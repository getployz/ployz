//! Owned deploy execution started by the control service.

use crate::controllers::{AcceptedDeployOperation, OperationControllers};
use crate::deploy_worker::{
    DeployCommandPreparationError, DeployContainer, DeployExecutionError, DeployExecutionNodeScope,
    DeployExecutionOutcome, DeployExecutionPorts, DeployFactLoadError, DeployHealthCheckError,
    DeployHealthChecker, NodeContainerRuntime, WireGuardEbpfPreparer, execute_deploy_operation,
    load_deploy_execution_facts_from_nats, prepare_deploy_execution_command,
};
use crate::node_rpc::{NatsNodeContainerRuntime, NatsNodeWireGuardEbpfPreparer};
use crate::operation_lease::with_advisory_operation_lease;
use ployz_core::ids::OperationOwnerId;
use ployz_core::ops::{
    DeployOperationFailure, DeployTransition, FailureMessage, OperationOwnerLease, OperatorHint,
};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::operations::{OperationStatusStoreError, RecordDeployTransitionError};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::task::JoinHandle;

const DEPLOY_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub async fn run_deploy_operation<D, N, H>(
    accepted: AcceptedDeployOperation,
    node_scope: DeployExecutionNodeScope,
    stores: DeployOperationStores,
    ports: DeployOperationPorts<'_, D, N, H>,
    step_timeout: Duration,
) -> Result<DeployExecutionOutcome, DeployOperationRunError>
where
    D: WireGuardEbpfPreparer,
    N: NodeContainerRuntime,
    H: DeployHealthChecker,
{
    let DeployOperationStores {
        core_state,
        observations,
        controllers,
    } = stores;
    let DeployOperationPorts {
        wireguard_ebpf,
        node_runtime,
        health_checker,
    } = ports;
    renew_operation_owner_lease(&controllers, &accepted)
        .await
        .map_err(DeployOperationRunError::LeaseNotHeld)?;
    let lease_policy = controllers.lease_policy();
    let lease_renewer = controllers.clone();
    let operation_id = accepted.operation_id.clone();
    let request = accepted.target.clone();

    with_advisory_operation_lease(operation_id, lease_policy, lease_renewer, async move {
        let facts = match load_deploy_execution_facts_from_nats(
            &request,
            node_scope,
            &core_state,
            &observations,
            step_timeout,
        )
        .await
        {
            Ok(facts) => facts,
            Err(source) => {
                let failure_record_error = record_operation_failure(
                    &controllers,
                    &accepted,
                    fact_load_failure(&request, &source),
                )
                .await
                .err();
                return Err(DeployOperationRunError::LoadFacts {
                    source,
                    failure_record_error,
                });
            }
        };
        let command = match prepare_deploy_execution_command(
            accepted.operation_id.clone(),
            request.clone(),
            facts,
        ) {
            Ok(command) => command,
            Err(source) => {
                let failure_record_error = record_operation_failure(
                    &controllers,
                    &accepted,
                    preparation_failure(&request),
                )
                .await
                .err();
                return Err(DeployOperationRunError::PrepareCommand {
                    source,
                    failure_record_error,
                });
            }
        };
        let mut recorder = controllers;
        let mut route_state = core_state.clone();
        let mut active_state = core_state;

        execute_deploy_operation(
            command,
            DeployExecutionPorts {
                recorder: &mut recorder,
                wireguard_ebpf,
                node_runtime,
                health_checker,
                route_state: &mut route_state,
                active_state: &mut active_state,
            },
        )
        .await
        .map_err(DeployOperationRunError::Execute)
    })
    .await
}

async fn renew_operation_owner_lease(
    controllers: &OperationControllers,
    accepted: &AcceptedDeployOperation,
) -> Result<OperationOwnerLease, DeployOperationLeaseError> {
    verify_accepted_deploy_lease(&accepted.lease, &accepted.operation_id)?;
    let Some(lease) = controllers
        .renew_owner_lease(&accepted.operation_id)
        .await
        .map_err(DeployOperationLeaseError::Renew)?
    else {
        return Err(DeployOperationLeaseError::NoCurrentLease {
            operation_id: accepted.operation_id.clone(),
            expected_owner: controllers.owner_id().clone(),
        });
    };
    verify_accepted_deploy_lease(&lease, &accepted.operation_id)?;
    if lease.owner_id != *controllers.owner_id() {
        return Err(DeployOperationLeaseError::NotCurrentOwner {
            lease,
            expected_owner: controllers.owner_id().clone(),
        });
    }

    Ok(lease)
}

fn verify_accepted_deploy_lease(
    lease: &OperationOwnerLease,
    operation_id: &ployz_core::ids::OperationId,
) -> Result<(), DeployOperationLeaseError> {
    if &lease.operation_id != operation_id {
        return Err(DeployOperationLeaseError::OperationMismatch {
            lease: lease.clone(),
            expected_operation_id: operation_id.clone(),
        });
    }

    Ok(())
}

async fn record_operation_failure(
    controllers: &OperationControllers,
    accepted: &AcceptedDeployOperation,
    failure: DeployOperationFailure,
) -> Result<(), RecordDeployTransitionError> {
    controllers
        .record_deploy_transition(&accepted.operation_id, DeployTransition::Failed { failure })
        .await
        .map(|_| ())
}

fn fact_load_failure(
    request: &ployz_core::deploy::DeployRequest,
    source: &DeployFactLoadError,
) -> DeployOperationFailure {
    DeployOperationFailure::PlanningFailed {
        service_id: request.service_id.clone(),
        revision_id: request.target_revision.clone(),
        message: operation_failure_message(fact_load_failure_message(source)),
    }
}

fn preparation_failure(request: &ployz_core::deploy::DeployRequest) -> DeployOperationFailure {
    DeployOperationFailure::PlanningFailed {
        service_id: request.service_id.clone(),
        revision_id: request.target_revision.clone(),
        message: operation_failure_message("deploy command could not be prepared"),
    }
}

fn fact_load_failure_message(source: &DeployFactLoadError) -> &'static str {
    match source {
        DeployFactLoadError::ActiveServiceRead { .. } => "active service state could not be loaded",
        DeployFactLoadError::ActiveRouteRead { .. } => "active route state could not be loaded",
        DeployFactLoadError::ActiveMachineRead { .. } => "active machines could not be loaded",
        DeployFactLoadError::NodeObservationRead { .. } => "node observations could not be loaded",
        DeployFactLoadError::NodePublicIpObservationRead { .. } => {
            "node public ip observations could not be loaded"
        }
        DeployFactLoadError::MissingNodePublicIpObservation { .. } => {
            "node public ip observation is missing"
        }
    }
}

fn operation_failure_message(message: &'static str) -> FailureMessage {
    FailureMessage::try_new(message).expect("static operation failure message is non-empty")
}

#[derive(Debug, Clone)]
pub struct DeployOperationStores {
    pub core_state: AsyncNatsCoreStateStore,
    pub observations: AsyncNatsObservationStore,
    pub controllers: OperationControllers,
}

pub struct DeployOperationPorts<'a, D, N, H> {
    pub wireguard_ebpf: &'a mut D,
    pub node_runtime: &'a mut N,
    pub health_checker: &'a mut H,
}

#[derive(Debug)]
pub enum DeployOperationRunError {
    LeaseNotHeld(DeployOperationLeaseError),
    LoadFacts {
        source: DeployFactLoadError,
        failure_record_error: Option<RecordDeployTransitionError>,
    },
    PrepareCommand {
        source: DeployCommandPreparationError,
        failure_record_error: Option<RecordDeployTransitionError>,
    },
    Execute(DeployExecutionError),
}

#[derive(Debug)]
pub enum DeployOperationLeaseError {
    OperationMismatch {
        lease: OperationOwnerLease,
        expected_operation_id: ployz_core::ids::OperationId,
    },
    NoCurrentLease {
        operation_id: ployz_core::ids::OperationId,
        expected_owner: OperationOwnerId,
    },
    NotCurrentOwner {
        lease: OperationOwnerLease,
        expected_owner: OperationOwnerId,
    },
    Renew(OperationStatusStoreError),
}

#[derive(Debug, Clone)]
pub struct DeployOperationRuntime {
    client: async_nats::Client,
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
    controllers: OperationControllers,
    node_scope: DeployExecutionNodeScope,
    step_timeout: Duration,
    task_registry: DeployTaskRegistry,
}

impl DeployOperationRuntime {
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

    pub fn start(&self, accepted: AcceptedDeployOperation) {
        let runtime = self.clone();
        self.task_registry.spawn(async move {
            let _outcome = runtime.run(accepted).await;
        });
    }

    pub async fn run(
        self,
        accepted: AcceptedDeployOperation,
    ) -> Result<DeployExecutionOutcome, DeployOperationRunError> {
        let mut wireguard_ebpf = NatsNodeWireGuardEbpfPreparer::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut node_runtime = NatsNodeContainerRuntime::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut health_checker =
            ObservationHealthChecker::new(self.observations.clone(), DEPLOY_HEALTH_POLL_INTERVAL);

        run_deploy_operation(
            accepted,
            self.node_scope,
            DeployOperationStores {
                core_state: self.core_state,
                observations: self.observations,
                controllers: self.controllers,
            },
            DeployOperationPorts {
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
