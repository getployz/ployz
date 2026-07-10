//! Owned deploy execution started by the control service.

use crate::intent::namespace_intent::NamespaceIntentStore;
use crate::intent::service::NatsIntentReader;
use crate::operation_api::admission::OperationControllers;
use crate::operations::deploy::{
    DataplanePreparer, DeployContainer, DeployExecutionError, DeployExecutionOutcome,
    DeployExecutionPorts, DeployFactLoadError, DeployHealthCheckError, DeployHealthChecker,
    DeployMachineCandidates, MachineContainerRuntime, NamespaceCommitError,
    NamespaceStateCommitter, execute_deploy_operation, load_deploy_execution_facts_from_nats,
    prepare_deploy_execution_command,
};
use crate::operations::log::{
    AcceptedDeploySubmission, OperationStatusWrite, RecordDeployTransitionError,
    RecordOperationEventError,
};
use crate::roles::machine::client::{
    MachineContainerInspectError, NatsMachineContainerRuntime, NatsMachineDataplanePreparer,
    NatsMachineFactsReader,
};
use crate::roles::machine::protocol::MachineContainerInspectRpcRequest;
use crate::tasks::TaskRegistry;
use ployz_core::machine_runtime::{ContainerHealth, ContainerRuntimeState};
use ployz_core::ops::{
    DeployOperationFailure, DeployTransition, FailureMessage, OperatorHint, StatusProjectionError,
};
use ployz_core::subjects::INTENT_CHANGED;
use std::time::Duration;

const DEPLOY_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub async fn run_deploy_operation<D, N, H>(
    accepted: AcceptedDeploySubmission,
    machine_candidates: DeployMachineCandidates,
    stores: DeployOperationStores,
    ports: DeployOperationPorts<'_, D, N, H>,
    step_timeout: Duration,
) -> Result<DeployExecutionOutcome, DeployOperationRunError>
where
    D: DataplanePreparer,
    N: MachineContainerRuntime,
    H: DeployHealthChecker,
{
    let DeployOperationStores {
        intent_change_client,
        namespace_intent,
        controllers,
    } = stores;
    let DeployOperationPorts {
        facts_reader,
        intent_reader,
        dataplane,
        machine_runtime,
        health_checker,
    } = ports;
    let request = accepted.target.clone();

    let facts = match load_deploy_execution_facts_from_nats(
        &request,
        machine_candidates,
        intent_reader,
        facts_reader,
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
    let command =
        prepare_deploy_execution_command(accepted.operation_id.clone(), request.clone(), facts);
    let mut recorder = controllers;
    let mut namespace_state = NamespaceIntentCommitter::new(intent_change_client, namespace_intent);
    execute_deploy_operation(
        command,
        DeployExecutionPorts {
            recorder: &mut recorder,
            dataplane,
            machine_runtime,
            health_checker,
            namespace_state: &mut namespace_state,
        },
    )
    .await
    .map_err(DeployOperationRunError::Execute)
}

async fn record_operation_failure(
    controllers: &OperationControllers,
    accepted: &AcceptedDeploySubmission,
    failure: DeployOperationFailure,
) -> Result<(), RecordDeployTransitionError> {
    controllers
        .repository()
        .record_deploy_transition(&accepted.operation_id, DeployTransition::Failed { failure })
        .await
        .map(|_| ())
}

fn fact_load_failure(
    request: &ployz_core::deploy::DeployRequest,
    source: &DeployFactLoadError,
) -> DeployOperationFailure {
    DeployOperationFailure::PlanningFailed {
        service_id: request.status_service_id(),
        namespace_revision_id: request.namespace_revision_id(),
        message: FailureMessage::try_new(source.to_string())
            .expect("rendered fact load failure message is non-empty"),
    }
}

#[derive(Debug, Clone)]
pub struct DeployOperationStores {
    pub intent_change_client: async_nats::Client,
    pub namespace_intent: NamespaceIntentStore,
    pub controllers: OperationControllers,
}

pub struct DeployOperationPorts<'a, D, N, H> {
    pub facts_reader: &'a NatsMachineFactsReader,
    pub intent_reader: &'a NatsIntentReader,
    pub dataplane: &'a mut D,
    pub machine_runtime: &'a mut N,
    pub health_checker: &'a mut H,
}

struct NamespaceIntentCommitter {
    intent_change_client: async_nats::Client,
    namespace_intent: NamespaceIntentStore,
}

impl NamespaceIntentCommitter {
    fn new(
        intent_change_client: async_nats::Client,
        namespace_intent: NamespaceIntentStore,
    ) -> Self {
        Self {
            intent_change_client,
            namespace_intent,
        }
    }

    async fn publish_intent_changed(&self) {
        let _ = self
            .intent_change_client
            .publish(INTENT_CHANGED, Vec::new().into())
            .await;
    }
}

impl NamespaceStateCommitter for NamespaceIntentCommitter {
    async fn replace_route_binding(
        &mut self,
        state: ployz_core::state::RouteBindingState,
    ) -> Result<(), NamespaceCommitError> {
        let target = state.target.clone();
        self.namespace_intent
            .replace_route_binding(state)
            .await
            .map_err(|error| NamespaceCommitError::RouteStore {
                target,
                message: error.to_string(),
            })?;
        self.publish_intent_changed().await;
        Ok(())
    }

    async fn remove_route_binding(
        &mut self,
        target: ployz_core::ops::RouteTarget,
    ) -> Result<(), NamespaceCommitError> {
        self.namespace_intent
            .remove_route_binding(&target)
            .await
            .map_err(|error| NamespaceCommitError::RouteStore {
                target,
                message: error.to_string(),
            })?;
        self.publish_intent_changed().await;
        Ok(())
    }

    async fn replace_serving_target_entry(
        &mut self,
        state: ployz_core::state::ServingTargetEntry,
    ) -> Result<(), NamespaceCommitError> {
        let scope = ployz_core::ops::ControlPlaneCommitScope::ServiceEntry {
            service_id: state.service_id.clone(),
            namespace_revision_entry_id: state.namespace_revision_entry_id.clone(),
        };
        self.namespace_intent
            .replace_serving_target_entry(state)
            .await
            .map_err(|error| NamespaceCommitError::ServingTargetStore {
                scope,
                message: error.to_string(),
            })?;
        self.publish_intent_changed().await;
        Ok(())
    }

    async fn remove_serving_target_entry(
        &mut self,
        entry: ployz_core::state::ServingTargetEntry,
    ) -> Result<(), NamespaceCommitError> {
        let scope = ployz_core::ops::ControlPlaneCommitScope::ServiceEntry {
            service_id: entry.service_id.clone(),
            namespace_revision_entry_id: entry.namespace_revision_entry_id.clone(),
        };
        self.namespace_intent
            .remove_serving_target_entry(&entry)
            .await
            .map_err(|error| NamespaceCommitError::ServingTargetStore {
                scope,
                message: error.to_string(),
            })?;
        self.publish_intent_changed().await;
        Ok(())
    }

    async fn replace_volume_pin(
        &mut self,
        state: ployz_core::state::VolumePinState,
    ) -> Result<(), NamespaceCommitError> {
        self.namespace_intent
            .replace_volume_pin(state.clone())
            .await
            .map_err(|error| NamespaceCommitError::VolumePinStore {
                state,
                message: error.to_string(),
            })?;
        self.publish_intent_changed().await;
        Ok(())
    }
}

#[derive(Debug)]
pub enum DeployOperationRunError {
    AlreadyStarted,
    ClaimStart(RecordDeployTransitionError),
    LoadFacts {
        source: DeployFactLoadError,
        failure_record_error: Option<RecordDeployTransitionError>,
    },
    Execute(DeployExecutionError),
}

#[derive(Debug, Clone)]
pub struct DeployOperationDriver {
    client: async_nats::Client,
    namespace_intent: NamespaceIntentStore,
    controllers: OperationControllers,
    machine_candidates: DeployMachineCandidates,
    step_timeout: Duration,
    task_registry: TaskRegistry,
}

impl DeployOperationDriver {
    #[must_use]
    pub fn new(
        client: async_nats::Client,
        namespace_intent: NamespaceIntentStore,
        controllers: OperationControllers,
        machine_candidates: DeployMachineCandidates,
        step_timeout: Duration,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            client,
            namespace_intent,
            controllers,
            machine_candidates,
            step_timeout,
            task_registry,
        }
    }

    pub fn start(&self, accepted: AcceptedDeploySubmission) {
        if !accepted.should_start_execution {
            return;
        }

        let runtime = self.clone();
        self.task_registry.spawn(async move {
            let _outcome = runtime.run(accepted).await;
        });
    }

    #[must_use]
    pub(crate) const fn step_timeout(&self) -> Duration {
        self.step_timeout
    }

    pub async fn run(
        self,
        accepted: AcceptedDeploySubmission,
    ) -> Result<DeployExecutionOutcome, DeployOperationRunError> {
        if !claim_deploy_execution(&self.controllers, &accepted.operation_id).await? {
            return Err(DeployOperationRunError::AlreadyStarted);
        }

        let mut dataplane = NatsMachineDataplanePreparer::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut machine_runtime = NatsMachineContainerRuntime::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let facts_reader = NatsMachineFactsReader::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let health_runtime = NatsMachineContainerRuntime::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut health_checker =
            LiveContainerHealthChecker::new(health_runtime, DEPLOY_HEALTH_POLL_INTERVAL);
        let intent_reader =
            NatsIntentReader::new(self.client.clone()).with_request_timeout(self.step_timeout);

        let namespace_id = accepted.target.namespace_id.clone();
        let operation_id = accepted.operation_id.clone();
        let namespace_intent = self.namespace_intent.clone();
        let controllers = self.controllers.clone();
        let result = run_deploy_operation(
            accepted,
            self.machine_candidates,
            DeployOperationStores {
                intent_change_client: self.client.clone(),
                namespace_intent,
                controllers: controllers.clone(),
            },
            DeployOperationPorts {
                facts_reader: &facts_reader,
                intent_reader: &intent_reader,
                dataplane: &mut dataplane,
                machine_runtime: &mut machine_runtime,
                health_checker: &mut health_checker,
            },
            self.step_timeout,
        )
        .await;
        controllers
            .release_namespace(&namespace_id, &operation_id)
            .await;
        result
    }
}

async fn claim_deploy_execution(
    controllers: &OperationControllers,
    operation_id: &ployz_core::ids::OperationId,
) -> Result<bool, DeployOperationRunError> {
    match controllers
        .repository()
        .record_deploy_transition(operation_id, DeployTransition::Planning)
        .await
    {
        Ok(OperationStatusWrite::Stored) => Ok(true),
        Ok(OperationStatusWrite::AlreadySatisfied { .. }) => Ok(false),
        Err(RecordOperationEventError::ProjectStatus(
            StatusProjectionError::InvalidTransition { .. }
            | StatusProjectionError::TerminalState { .. },
        )) => Ok(false),
        Err(error) => Err(DeployOperationRunError::ClaimStart(error)),
    }
}

pub struct LiveContainerHealthChecker {
    runtime: NatsMachineContainerRuntime,
    poll_interval: Duration,
}

impl LiveContainerHealthChecker {
    #[must_use]
    pub fn new(runtime: NatsMachineContainerRuntime, poll_interval: Duration) -> Self {
        Self {
            runtime,
            poll_interval,
        }
    }
}

impl DeployHealthChecker for LiveContainerHealthChecker {
    async fn wait_healthy(
        &mut self,
        containers: &[DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        loop {
            let mut all_running = true;
            for container in containers {
                let observation = self
                    .runtime
                    .inspect_container(
                        &container.machine_id,
                        MachineContainerInspectRpcRequest {
                            container_id: container.container_id.clone(),
                        },
                    )
                    .await
                    .map_err(|error| unhealthy_container(container, health_read_error(error)))?;
                match observation {
                    Some(observation) => match observed_container_health(
                        &observation,
                        container.requires_docker_healthcheck,
                    ) {
                        ObservedContainerHealth::Healthy => {}
                        ObservedContainerHealth::Pending => all_running = false,
                        ObservedContainerHealth::Failed(message) => {
                            return Err(unhealthy_container(container, message));
                        }
                    },
                    None => all_running = false,
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
    observation: &ployz_core::machine_runtime::ManagedContainerObservation,
    requires_docker_healthcheck: bool,
) -> ObservedContainerHealth {
    match &observation.state {
        ContainerRuntimeState::Running { health, .. } => match health {
            ContainerHealth::Starting => ObservedContainerHealth::Pending,
            ContainerHealth::Healthy => ObservedContainerHealth::Healthy,
            ContainerHealth::Unhealthy => ObservedContainerHealth::Failed("unhealthy"),
            ContainerHealth::None if requires_docker_healthcheck => {
                ObservedContainerHealth::Pending
            }
            ContainerHealth::None => ObservedContainerHealth::Healthy,
        },
        ContainerRuntimeState::Exited => ObservedContainerHealth::Failed("container exited"),
    }
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
        machine_id: container.machine_id.clone(),
        container_id: container.container_id.clone(),
        message,
        log_hint,
    }
}

fn health_read_error(error: MachineContainerInspectError) -> String {
    match error {
        MachineContainerInspectError::InspectFailed { message, .. } => {
            format!("container status could not be read: {}", message.as_str())
        }
        MachineContainerInspectError::Unavailable { reason, .. } => {
            format!(
                "container status unavailable: {}",
                reason.failure_message().as_str()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::ids::{
        ContainerId, MachineId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId,
    };
    use ployz_core::machine_runtime::{
        ContainerHealth, ContainerRuntimeState, ManagedContainerIdentity, ManagedContainerKind,
        ManagedContainerObservation,
    };

    #[test]
    fn health_accepts_running_without_endpoint() {
        assert_eq!(
            observed_container_health(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::running_unroutable()
                ),
                false,
            ),
            ObservedContainerHealth::Healthy
        );
    }

    #[test]
    fn health_waits_for_missing_configured_docker_health() {
        assert_eq!(
            observed_container_health(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::running_unroutable()
                ),
                true,
            ),
            ObservedContainerHealth::Pending
        );
    }

    #[test]
    fn health_accepts_running_endpoint() {
        assert_eq!(
            observed_container_health(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::running_at(endpoint_ip("10.0.0.2")),
                ),
                false,
            ),
            ObservedContainerHealth::Healthy
        );
    }

    #[test]
    fn health_accepts_running_endpoint_without_matching_route_port() {
        assert_eq!(
            observed_container_health(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::running_at(endpoint_ip("10.0.0.2")),
                ),
                false,
            ),
            ObservedContainerHealth::Healthy
        );
    }

    #[test]
    fn health_fails_exited_container() {
        assert_eq!(
            observed_container_health(
                &observation("machine_a", "ctr_1", ContainerRuntimeState::Exited),
                false,
            ),
            ObservedContainerHealth::Failed("container exited")
        );
    }

    #[test]
    fn health_waits_for_starting_and_fails_unhealthy() {
        assert_eq!(
            observed_container_health(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::Running {
                        ip: None,
                        health: ContainerHealth::Starting,
                    }
                ),
                true,
            ),
            ObservedContainerHealth::Pending
        );
        assert_eq!(
            observed_container_health(
                &observation(
                    "machine_a",
                    "ctr_1",
                    ContainerRuntimeState::Running {
                        ip: None,
                        health: ContainerHealth::Unhealthy,
                    }
                ),
                true,
            ),
            ObservedContainerHealth::Failed("unhealthy")
        );
    }

    fn observation(
        machine_id_value: &str,
        container_id_value: &str,
        state: ContainerRuntimeState,
    ) -> ManagedContainerObservation {
        ManagedContainerObservation {
            machine_id: machine_id(machine_id_value),
            container_id: container_id(container_id_value),
            identity: ManagedContainerIdentity {
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                namespace_revision_entry_id: namespace_revision_entry_id("entry_observed"),
                operation_id: operation_id("op_123"),
                step_id: step_id("run_1"),
                kind: ManagedContainerKind::Service,
            },
            state,
            health_status: None,
            resolved_image_identity: None,
            created_at_unix_seconds: None,
        }
    }

    fn endpoint_ip(ip: &str) -> std::net::IpAddr {
        ip.parse().expect("valid endpoint ip")
    }

    fn namespace_id(value: &str) -> ployz_core::ids::NamespaceId {
        ployz_core::ids::NamespaceId::try_new(value).expect("valid namespace id")
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn container_id(value: &str) -> ContainerId {
        ContainerId::try_new(value).expect("valid container id")
    }

    fn service_id(value: &str) -> ServiceId {
        ServiceId::try_new(value).expect("valid service id")
    }

    fn namespace_revision_entry_id(value: &str) -> NamespaceRevisionEntryId {
        NamespaceRevisionEntryId::try_new(value).expect("valid namespace revision entry id")
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }

    fn step_id(value: &str) -> StepId {
        StepId::try_new(value).expect("valid step id")
    }
}
