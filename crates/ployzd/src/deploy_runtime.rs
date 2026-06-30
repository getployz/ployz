//! Owned deploy execution started by the control service.

use crate::controllers::OperationControllers;
use crate::deploy_worker::{
    DataplanePreparer, DeployCommandPreparationError, DeployContainer, DeployExecutionError,
    DeployExecutionMachineScope, DeployExecutionOutcome, DeployExecutionPorts, DeployFactLoadError,
    DeployHealthCheckError, DeployHealthChecker, MachineContainerRuntime, execute_deploy_operation,
    load_deploy_execution_facts_from_nats, prepare_deploy_execution_command,
};
use crate::machine_runtime::client::{NatsMachineContainerRuntime, NatsMachineDataplanePreparer};
use crate::tasks::TaskRegistry;
use ployz_core::ops::{
    DeployOperationFailure, DeployTransition, FailureMessage, OperatorHint, StatusProjectionError,
};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::operations::{
    AcceptedDeploySubmission, OperationStatusWrite, RecordDeployTransitionError,
    RecordOperationEventError,
};
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

const DEPLOY_HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(100);
const DEPLOY_HEALTH_INITIAL_EXIT_GRACE: Duration = Duration::from_secs(3);

pub async fn run_deploy_operation<D, N, H>(
    accepted: AcceptedDeploySubmission,
    machine_scope: DeployExecutionMachineScope,
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
        core_state,
        observations,
        controllers,
    } = stores;
    let DeployOperationPorts {
        dataplane,
        machine_runtime,
        health_checker,
    } = ports;
    let request = accepted.target.clone();

    let facts = match load_deploy_execution_facts_from_nats(
        &request,
        machine_scope,
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
            let failure_record_error =
                record_operation_failure(&controllers, &accepted, preparation_failure(&request))
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
            dataplane,
            machine_runtime,
            health_checker,
            route_state: &mut route_state,
            active_state: &mut active_state,
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
    let service = request
        .primary_service()
        .expect("accepted deploy request has at least one service");
    DeployOperationFailure::PlanningFailed {
        service_id: service.service_id.clone(),
        revision_id: request.target_revision.clone(),
        message: FailureMessage::try_new(source.to_string())
            .expect("rendered fact load failure message is non-empty"),
    }
}

fn preparation_failure(request: &ployz_core::deploy::DeployRequest) -> DeployOperationFailure {
    let service = request
        .primary_service()
        .expect("accepted deploy request has at least one service");
    DeployOperationFailure::PlanningFailed {
        service_id: service.service_id.clone(),
        revision_id: request.target_revision.clone(),
        message: FailureMessage::try_new("deploy command could not be prepared")
            .expect("static operation failure message is non-empty"),
    }
}

#[derive(Debug, Clone)]
pub struct DeployOperationStores {
    pub core_state: AsyncNatsCoreStateStore,
    pub observations: AsyncNatsObservationStore,
    pub controllers: OperationControllers,
}

pub struct DeployOperationPorts<'a, D, N, H> {
    pub dataplane: &'a mut D,
    pub machine_runtime: &'a mut N,
    pub health_checker: &'a mut H,
}

#[derive(Debug)]
pub enum DeployOperationRunError {
    AlreadyStarted,
    ClaimStart(RecordDeployTransitionError),
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

#[derive(Debug, Clone)]
pub struct DeployOperationRuntime {
    client: async_nats::Client,
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
    controllers: OperationControllers,
    machine_scope: DeployExecutionMachineScope,
    step_timeout: Duration,
    task_registry: TaskRegistry,
}

impl DeployOperationRuntime {
    #[must_use]
    pub fn new(
        client: async_nats::Client,
        core_state: AsyncNatsCoreStateStore,
        observations: AsyncNatsObservationStore,
        controllers: OperationControllers,
        machine_scope: DeployExecutionMachineScope,
        step_timeout: Duration,
        task_registry: TaskRegistry,
    ) -> Self {
        Self {
            client,
            core_state,
            observations,
            controllers,
            machine_scope,
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

    pub async fn run(
        self,
        accepted: AcceptedDeploySubmission,
    ) -> Result<DeployExecutionOutcome, DeployOperationRunError> {
        if !claim_deploy_execution(&self.controllers, &accepted.operation_id).await? {
            return Err(DeployOperationRunError::AlreadyStarted);
        }

        let mut dataplane =
            NatsMachineDataplanePreparer::new(self.client.clone(), self.observations.clone())
                .with_request_timeout(self.step_timeout);
        let mut machine_runtime = NatsMachineContainerRuntime::new(self.client.clone())
            .with_request_timeout(self.step_timeout);
        let mut health_checker =
            ObservationHealthChecker::new(self.observations.clone(), DEPLOY_HEALTH_POLL_INTERVAL);

        run_deploy_operation(
            accepted,
            self.machine_scope,
            DeployOperationStores {
                core_state: self.core_state,
                observations: self.observations,
                controllers: self.controllers,
            },
            DeployOperationPorts {
                dataplane: &mut dataplane,
                machine_runtime: &mut machine_runtime,
                health_checker: &mut health_checker,
            },
            self.step_timeout,
        )
        .await
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
        Ok(OperationStatusWrite::Stored { .. }) => Ok(true),
        Ok(OperationStatusWrite::AlreadySatisfied { .. } | OperationStatusWrite::Stale { .. }) => {
            Ok(false)
        }
        Err(RecordOperationEventError::ProjectStatus(
            StatusProjectionError::InvalidTransition { .. }
            | StatusProjectionError::TerminalState { .. },
        )) => Ok(false),
        Err(error) => Err(DeployOperationRunError::ClaimStart(error)),
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
        let mut memory = HealthObservationMemory::default();
        loop {
            let mut all_running = true;
            for container in containers {
                match self
                    .observations
                    .container(&container.machine_id, &container.container_id)
                    .await
                {
                    Ok(Some(observation)) => {
                        match observed_container_health(container, &observation) {
                            ObservedContainerHealth::Healthy => {
                                memory.record_running(container);
                            }
                            ObservedContainerHealth::Pending => {
                                if observation.state.is_running() {
                                    memory.record_running(container);
                                }
                                all_running = false;
                            }
                            ObservedContainerHealth::Failed(message) => {
                                if memory.should_wait_for_fresh_start_observation(container) {
                                    all_running = false;
                                    continue;
                                }
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

#[derive(Default)]
struct HealthObservationMemory {
    seen_running: BTreeSet<HealthContainerKey>,
    initial_exit_seen_at: BTreeMap<HealthContainerKey, Instant>,
}

impl HealthObservationMemory {
    fn record_running(&mut self, container: &DeployContainer) {
        let key = health_container_key(container);
        self.seen_running.insert(key.clone());
        self.initial_exit_seen_at.remove(&key);
    }

    fn should_wait_for_fresh_start_observation(&mut self, container: &DeployContainer) -> bool {
        let key = health_container_key(container);
        if self.seen_running.contains(&key) {
            return false;
        }

        let now = Instant::now();
        let first_seen = self.initial_exit_seen_at.entry(key).or_insert(now);
        now.duration_since(*first_seen) < DEPLOY_HEALTH_INITIAL_EXIT_GRACE
    }
}

type HealthContainerKey = (ployz_core::ids::MachineId, ployz_core::ids::ContainerId);

fn health_container_key(container: &DeployContainer) -> HealthContainerKey {
    (container.machine_id.clone(), container.container_id.clone())
}

fn observed_container_health(
    container: &DeployContainer,
    observation: &ployz_core::machine_runtime::ManagedContainerObservation,
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
        machine_id: container.machine_id.clone(),
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
    use ployz_core::ids::{ContainerId, MachineId, OperationId, RevisionId, ServiceId, StepId};
    use ployz_core::machine_runtime::{
        ContainerEndpoint, ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    };
    use ployz_core::ops::RoutePort;

    #[test]
    fn routed_health_waits_for_endpoint_evidence() {
        assert_eq!(
            observed_container_health(
                &deploy_container("machine_a", "ctr_1", Some(route_port(8080))),
                &observation(
                    "machine_a",
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
                &deploy_container("machine_a", "ctr_1", None),
                &observation(
                    "machine_a",
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
                &deploy_container("machine_a", "ctr_1", Some(route_port(8080))),
                &observation(
                    "machine_a",
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
                &deploy_container("machine_a", "ctr_1", Some(route_port(8080))),
                &observation(
                    "machine_a",
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
                &deploy_container("machine_a", "ctr_1", None),
                &observation("machine_a", "ctr_1", ContainerRuntimeState::Exited),
            ),
            ObservedContainerHealth::Failed("container exited")
        );
    }

    #[test]
    fn health_graces_initial_exited_observation_until_running_is_seen() {
        let container = deploy_container("machine_a", "ctr_1", Some(route_port(8080)));
        let mut memory = HealthObservationMemory::default();

        assert!(memory.should_wait_for_fresh_start_observation(&container));

        memory.record_running(&container);

        assert!(!memory.should_wait_for_fresh_start_observation(&container));
    }

    fn deploy_container(
        machine_id_value: &str,
        container_id_value: &str,
        required_endpoint_port: Option<RoutePort>,
    ) -> DeployContainer {
        DeployContainer {
            machine_id: machine_id(machine_id_value),
            container_id: container_id(container_id_value),
            step_id: step_id("run_1"),
            required_endpoint_port,
        }
    }

    fn observation(
        machine_id_value: &str,
        container_id_value: &str,
        state: ContainerRuntimeState,
    ) -> ManagedContainerObservation {
        ManagedContainerObservation {
            machine_id: machine_id(machine_id_value),
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

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
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
