//! Owned deploy execution launched by the control service.

use crate::controllers::{AcceptedDeployOperation, OperationControllers};
use crate::deploy_launcher::{
    DeployLaunchError, DeployLaunchPorts, DeployLaunchStores, run_deploy_operation,
};
use crate::deploy_worker::{
    DeployContainer, DeployExecutionNodeScope, DeployExecutionOutcome, DeployHealthCheckError,
    DeployHealthChecker,
};
use crate::node_rpc::{NatsNodeContainerRuntime, NatsNodeWireGuardEbpfPreparer};
use ployz_core::ops::{FailureMessage, OperatorHint};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;

const DEPLOY_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct OwnedDeployLauncher {
    client: async_nats::Client,
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
    controllers: OperationControllers,
    node_scope: DeployExecutionNodeScope,
    step_timeout: Duration,
    task_registry: DeployTaskRegistry,
}

impl OwnedDeployLauncher {
    #[must_use]
    pub fn new(
        client: async_nats::Client,
        core_state: AsyncNatsCoreStateStore,
        observations: AsyncNatsObservationStore,
        controllers: OperationControllers,
        node_scope: DeployExecutionNodeScope,
        step_timeout: Duration,
        task_registry: DeployTaskRegistry,
    ) -> Self {
        Self {
            client,
            core_state,
            observations,
            controllers,
            node_scope,
            step_timeout,
            task_registry,
        }
    }

    pub fn launch(&self, accepted: AcceptedDeployOperation) {
        let launcher = self.clone();
        self.task_registry.spawn(async move {
            let _outcome = launcher.run(accepted).await;
        });
    }

    pub async fn run(
        self,
        accepted: AcceptedDeployOperation,
    ) -> Result<DeployExecutionOutcome, DeployLaunchError> {
        let mut wireguard_ebpf = NatsNodeWireGuardEbpfPreparer::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut node_runtime = NatsNodeContainerRuntime::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut health_checker =
            ObservationHealthChecker::new(self.observations.clone(), DEPLOY_HEALTH_POLL_INTERVAL);

        run_deploy_operation(
            accepted,
            self.node_scope,
            DeployLaunchStores {
                core_state: self.core_state,
                observations: self.observations,
                controllers: self.controllers,
            },
            DeployLaunchPorts {
                wireguard_ebpf: &mut wireguard_ebpf,
                node_runtime: &mut node_runtime,
                health_checker: &mut health_checker,
            },
            self.step_timeout,
        )
        .await
    }
}

#[derive(Debug, Clone, Default)]
pub struct DeployTaskRegistry {
    handles: Arc<Mutex<Vec<JoinHandle<()>>>>,
}

impl DeployTaskRegistry {
    pub fn spawn(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut handles = self
            .handles
            .lock()
            .expect("deploy task registry lock is not poisoned");
        handles.retain(|handle| !handle.is_finished());
        handles.push(tokio::spawn(future));
    }

    pub fn abort_all(&self) {
        let mut handles = self
            .handles
            .lock()
            .expect("deploy task registry lock is not poisoned");
        for handle in handles.drain(..) {
            handle.abort();
        }
    }
}

pub struct ObservationHealthChecker {
    observations: AsyncNatsObservationStore,
    poll_interval: Duration,
}

impl ObservationHealthChecker {
    #[must_use]
    pub fn new(observations: AsyncNatsObservationStore, poll_interval: Duration) -> Self {
        Self {
            observations,
            poll_interval,
        }
    }
}

impl DeployHealthChecker for ObservationHealthChecker {
    async fn wait_healthy(
        &mut self,
        containers: &[DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        loop {
            let mut all_running = true;
            for container in containers {
                match self
                    .observations
                    .container(&container.node_id, &container.container_id)
                    .await
                {
                    Ok(Some(observation)) => {
                        match observed_container_health(container, &observation) {
                            ObservedContainerHealth::Healthy => {}
                            ObservedContainerHealth::Pending => all_running = false,
                            ObservedContainerHealth::Failed(message) => {
                                return Err(unhealthy_container(container, message));
                            }
                        }
                    }
                    Ok(None) => all_running = false,
                    Err(error) => {
                        return Err(unhealthy_container(container, health_read_error(error)));
                    }
                }
            }

            if all_running {
                return Ok(());
            }

            tokio::time::sleep(self.poll_interval).await;
        }
    }
}

fn observed_container_health(
    container: &DeployContainer,
    observation: &ployz_core::node::ManagedContainerObservation,
) -> ObservedContainerHealth {
    if !observation.state.is_running() {
        return ObservedContainerHealth::Failed("container exited");
    }

    let Some(required_port) = container.required_endpoint_port else {
        return ObservedContainerHealth::Healthy;
    };

    let Some(endpoint) = observation.running_service_endpoint() else {
        return ObservedContainerHealth::Pending;
    };

    if endpoint.port != required_port {
        return ObservedContainerHealth::Pending;
    }

    ObservedContainerHealth::Healthy
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedContainerHealth {
    Healthy,
    Pending,
    Failed(&'static str),
}

fn unhealthy_container(
    container: &DeployContainer,
    message: impl Into<String>,
) -> DeployHealthCheckError {
    let message = FailureMessage::try_new(message).expect("health failure message is non-empty");
    let log_hint = OperatorHint::try_new(format!("ployz logs {}", container.container_id.as_str()))
        .expect("generated log hint is non-empty");
    DeployHealthCheckError::Unhealthy {
        node_id: container.node_id.clone(),
        container_id: container.container_id.clone(),
        message,
        log_hint,
    }
}

fn health_read_error(error: ObservationStoreError) -> String {
    format!("container observation could not be read: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
    use ployz_core::node::{
        ContainerEndpoint, ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    };
    use ployz_core::ops::RoutePort;

    #[test]
    fn routed_health_waits_for_endpoint_evidence() {
        assert_eq!(
            observed_container_health(
                &deploy_container("node_a", "ctr_1", Some(route_port(8080))),
                &observation(
                    "node_a",
                    "ctr_1",
                    ContainerRuntimeState::running_unroutable()
                ),
            ),
            ObservedContainerHealth::Pending
        );
    }

    #[test]
    fn unrouted_health_accepts_running_without_endpoint() {
        assert_eq!(
            observed_container_health(
                &deploy_container("node_a", "ctr_1", None),
                &observation(
                    "node_a",
                    "ctr_1",
                    ContainerRuntimeState::running_unroutable()
                ),
            ),
            ObservedContainerHealth::Healthy
        );
    }

    #[test]
    fn routed_health_accepts_running_endpoint() {
        assert_eq!(
            observed_container_health(
                &deploy_container("node_a", "ctr_1", Some(route_port(8080))),
                &observation(
                    "node_a",
                    "ctr_1",
                    ContainerRuntimeState::running_at(endpoint("10.0.0.2", 8080)),
                ),
            ),
            ObservedContainerHealth::Healthy
        );
    }

    #[test]
    fn routed_health_waits_for_matching_endpoint_port() {
        assert_eq!(
            observed_container_health(
                &deploy_container("node_a", "ctr_1", Some(route_port(8080))),
                &observation(
                    "node_a",
                    "ctr_1",
                    ContainerRuntimeState::running_at(endpoint("10.0.0.2", 3000)),
                ),
            ),
            ObservedContainerHealth::Pending
        );
    }

    #[test]
    fn health_fails_exited_container() {
        assert_eq!(
            observed_container_health(
                &deploy_container("node_a", "ctr_1", None),
                &observation("node_a", "ctr_1", ContainerRuntimeState::Exited),
            ),
            ObservedContainerHealth::Failed("container exited")
        );
    }

    fn deploy_container(
        node_id_value: &str,
        container_id_value: &str,
        required_endpoint_port: Option<RoutePort>,
    ) -> DeployContainer {
        DeployContainer {
            node_id: node_id(node_id_value),
            container_id: container_id(container_id_value),
            required_endpoint_port,
        }
    }

    fn observation(
        node_id_value: &str,
        container_id_value: &str,
        state: ContainerRuntimeState,
    ) -> ManagedContainerObservation {
        ManagedContainerObservation {
            node_id: node_id(node_id_value),
            container_id: container_id(container_id_value),
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
            operation_id: operation_id("op_123"),
            step_id: step_id("run_1"),
            kind: ManagedContainerKind::Service,
            state,
        }
    }

    fn endpoint(ip: &str, port: u16) -> ContainerEndpoint {
        ContainerEndpoint {
            ip: ip.parse().expect("valid endpoint ip"),
            port: route_port(port),
        }
    }

    fn route_port(port: u16) -> RoutePort {
        RoutePort::try_new(port).expect("valid endpoint port")
    }

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    fn service_id(value: &str) -> ServiceId {
        ServiceId::try_new(value).expect("valid service id")
    }

    fn revision_id(value: &str) -> RevisionId {
        RevisionId::try_new(value).expect("valid revision id")
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn step_id(value: &str) -> StepId {
        StepId::try_new(value).expect("valid step id")
    }
}
