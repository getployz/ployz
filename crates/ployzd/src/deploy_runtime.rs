//! Owned deploy execution started by the control service.

use crate::controllers::OperationControllers;
use crate::deploy_worker::{
    DataplanePreparer, DeployCommandPreparationError, DeployContainer, DeployExecutionError,
    DeployExecutionMachineScope, DeployExecutionOutcome, DeployExecutionPorts, DeployFactLoadError,
    DeployHealthCheckError, DeployHealthChecker, MachineContainerRuntime, RouteBindingCommitError,
    RouteBindingCommitter, ServingTargetCommitError, ServingTargetCommitter,
    execute_deploy_operation, load_deploy_execution_facts_from_nats,
    prepare_deploy_execution_command,
};
use crate::machine_runtime::client::{NatsMachineContainerRuntime, NatsMachineDataplanePreparer};
use crate::tasks::TaskRegistry;
use ployz_core::ops::{
    DeployOperationFailure, DeployTransition, FailureMessage, OperatorHint, StatusProjectionError,
};
use ployz_nats::core_state::{
    AsyncNatsCoreStateStore, CoreStateStoreError, NAMESPACE_LOCK_RENEW_INTERVAL_MS,
    NamespaceLockRenew,
};
use ployz_nats::observations::{AsyncNatsObservationStore, ObservationStoreError};
use ployz_nats::operations::{
    AcceptedDeploySubmission, OperationStatusWrite, RecordDeployTransitionError,
    RecordOperationEventError,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
        namespace_lock_lost,
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
    if let Some(lock_lost) = namespace_lock_lost {
        let mut route_state = LockCheckedCoreState::new(core_state.clone(), Arc::clone(&lock_lost));
        let mut active_state = LockCheckedCoreState::new(core_state, lock_lost);
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
    } else {
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
        namespace_revision_id: request.namespace_revision_id.clone(),
        message: FailureMessage::try_new(source.to_string())
            .expect("rendered fact load failure message is non-empty"),
    }
}

fn preparation_failure(request: &ployz_core::deploy::DeployRequest) -> DeployOperationFailure {
    DeployOperationFailure::PlanningFailed {
        service_id: request.status_service_id(),
        namespace_revision_id: request.namespace_revision_id.clone(),
        message: FailureMessage::try_new("deploy command could not be prepared")
            .expect("static operation failure message is non-empty"),
    }
}

#[derive(Debug, Clone)]
pub struct DeployOperationStores {
    pub core_state: AsyncNatsCoreStateStore,
    pub observations: AsyncNatsObservationStore,
    pub controllers: OperationControllers,
    pub namespace_lock_lost: Option<Arc<AtomicBool>>,
}

pub struct DeployOperationPorts<'a, D, N, H> {
    pub dataplane: &'a mut D,
    pub machine_runtime: &'a mut N,
    pub health_checker: &'a mut H,
}

struct LockCheckedCoreState {
    core_state: AsyncNatsCoreStateStore,
    namespace_lock_lost: Arc<AtomicBool>,
}

impl LockCheckedCoreState {
    fn new(core_state: AsyncNatsCoreStateStore, namespace_lock_lost: Arc<AtomicBool>) -> Self {
        Self {
            core_state,
            namespace_lock_lost,
        }
    }

    fn lock_is_lost(&self) -> bool {
        self.namespace_lock_lost.load(Ordering::Relaxed)
    }
}

impl RouteBindingCommitter for LockCheckedCoreState {
    async fn replace_route_binding(
        &mut self,
        state: ployz_core::state::RouteBindingState,
    ) -> Result<(), RouteBindingCommitError> {
        let target = state.target.clone();
        if self.lock_is_lost() {
            return Err(RouteBindingCommitError::NamespaceLockLost { target });
        }

        AsyncNatsCoreStateStore::replace_route_binding(&self.core_state, &state)
            .await
            .map_err(|error| RouteBindingCommitError::Store { target, error })
    }

    async fn remove_route_binding(
        &mut self,
        target: ployz_core::ops::RouteTarget,
    ) -> Result<(), RouteBindingCommitError> {
        if self.lock_is_lost() {
            return Err(RouteBindingCommitError::NamespaceLockLost { target });
        }

        AsyncNatsCoreStateStore::remove_route_binding(&self.core_state, &target)
            .await
            .map_err(|error| RouteBindingCommitError::Store { target, error })
    }
}

impl ServingTargetCommitter for LockCheckedCoreState {
    async fn replace_serving_target_entry(
        &mut self,
        state: ployz_core::state::ServingTargetEntry,
    ) -> Result<(), ServingTargetCommitError> {
        if self.lock_is_lost() {
            return Err(ServingTargetCommitError::NamespaceLockLost);
        }

        AsyncNatsCoreStateStore::replace_serving_target_entry(&self.core_state, &state)
            .await
            .map_err(ServingTargetCommitError::Store)
    }

    async fn remove_serving_target_entry(
        &mut self,
        namespace_id: ployz_core::ids::NamespaceId,
        service_id: ployz_core::ids::ServiceId,
    ) -> Result<(), ServingTargetCommitError> {
        if self.lock_is_lost() {
            return Err(ServingTargetCommitError::NamespaceLockLost);
        }

        AsyncNatsCoreStateStore::remove_serving_target_entry(
            &self.core_state,
            &namespace_id,
            &service_id,
        )
        .await
        .map_err(ServingTargetCommitError::Store)
    }
}

#[derive(Debug)]
pub enum DeployOperationRunError {
    AlreadyStarted,
    ClaimStart(RecordDeployTransitionError),
    Clock {
        message: String,
    },
    NamespaceLock(CoreStateStoreError),
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

impl From<CoreStateStoreError> for DeployOperationRunError {
    fn from(value: CoreStateStoreError) -> Self {
        Self::NamespaceLock(value)
    }
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

        let namespace_id = accepted.target.namespace_id.clone();
        let operation_id = accepted.operation_id.clone();
        let lock_lost = Arc::new(AtomicBool::new(false));
        let renew_core_state = self.core_state.clone();
        let run_core_state = self.core_state.clone();
        let renew_task = tokio::spawn(renew_namespace_lock_until_lost(
            renew_core_state,
            namespace_id.clone(),
            operation_id.clone(),
            Arc::clone(&lock_lost),
        ));
        let result = run_deploy_operation(
            accepted,
            self.machine_scope,
            DeployOperationStores {
                core_state: run_core_state,
                observations: self.observations,
                controllers: self.controllers,
                namespace_lock_lost: Some(lock_lost),
            },
            DeployOperationPorts {
                dataplane: &mut dataplane,
                machine_runtime: &mut machine_runtime,
                health_checker: &mut health_checker,
            },
            self.step_timeout,
        )
        .await;
        renew_task.abort();
        let _ = renew_task.await;
        let _ = self
            .core_state
            .release_namespace_lock(&namespace_id, &operation_id)
            .await;
        result
    }
}

async fn renew_namespace_lock_until_lost(
    core_state: AsyncNatsCoreStateStore,
    namespace_id: ployz_core::ids::NamespaceId,
    operation_id: ployz_core::ids::OperationId,
    lock_lost: Arc<AtomicBool>,
) {
    let mut interval =
        tokio::time::interval(Duration::from_millis(NAMESPACE_LOCK_RENEW_INTERVAL_MS));
    let mut failures = 0_u8;
    loop {
        interval.tick().await;
        match core_state
            .renew_namespace_lock(
                &namespace_id,
                &operation_id,
                current_unix_ms().unwrap_or(u64::MAX),
            )
            .await
        {
            Ok(NamespaceLockRenew::Renewed) => failures = 0,
            Ok(NamespaceLockRenew::Lost) => {
                lock_lost.store(true, Ordering::Relaxed);
                return;
            }
            Err(_) => {
                failures = failures.saturating_add(1);
                if failures >= 3 {
                    lock_lost.store(true, Ordering::Relaxed);
                    return;
                }
            }
        }
    }
}

fn current_unix_ms() -> Result<u64, DeployOperationRunError> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| DeployOperationRunError::Clock {
            message: error.to_string(),
        })?
        .as_millis();

    Ok(u64::try_from(millis).unwrap_or(u64::MAX))
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
                    Ok(Some(observation)) => match observed_container_health(&observation) {
                        ObservedContainerHealth::Healthy => {
                            memory.record_running(container);
                        }
                        ObservedContainerHealth::Failed(message) => {
                            if memory.should_wait_for_fresh_start_observation(container) {
                                all_running = false;
                                continue;
                            }
                            return Err(unhealthy_container(container, message));
                        }
                    },
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

/// Running is healthy: deploy input defines no healthchecks yet, and when it
/// does they will gate only first container creation (ADR 0025) - reused
/// containers and phase continuation never re-run health gates.
fn observed_container_health(
    observation: &ployz_core::machine_runtime::ManagedContainerObservation,
) -> ObservedContainerHealth {
    if !observation.state.is_running() {
        return ObservedContainerHealth::Failed("container exited");
    }

    ObservedContainerHealth::Healthy
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservedContainerHealth {
    Healthy,
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
    use ployz_core::ids::{
        ContainerId, MachineId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId,
    };
    use ployz_core::machine_runtime::{
        ContainerRuntimeState, ManagedContainerIdentity, ManagedContainerKind,
        ManagedContainerObservation,
    };

    #[test]
    fn health_accepts_running_without_endpoint() {
        assert_eq!(
            observed_container_health(&observation(
                "machine_a",
                "ctr_1",
                ContainerRuntimeState::running_unroutable()
            ),),
            ObservedContainerHealth::Healthy
        );
    }

    #[test]
    fn health_accepts_running_endpoint() {
        assert_eq!(
            observed_container_health(&observation(
                "machine_a",
                "ctr_1",
                ContainerRuntimeState::running_at(endpoint_ip("10.0.0.2")),
            ),),
            ObservedContainerHealth::Healthy
        );
    }

    #[test]
    fn health_accepts_running_endpoint_without_matching_route_port() {
        assert_eq!(
            observed_container_health(&observation(
                "machine_a",
                "ctr_1",
                ContainerRuntimeState::running_at(endpoint_ip("10.0.0.2")),
            ),),
            ObservedContainerHealth::Healthy
        );
    }

    #[test]
    fn health_fails_exited_container() {
        assert_eq!(
            observed_container_health(&observation(
                "machine_a",
                "ctr_1",
                ContainerRuntimeState::Exited
            ),),
            ObservedContainerHealth::Failed("container exited")
        );
    }

    #[test]
    fn health_graces_initial_exited_observation_until_running_is_seen() {
        let container = deploy_container("machine_a", "ctr_1");
        let mut memory = HealthObservationMemory::default();

        assert!(memory.should_wait_for_fresh_start_observation(&container));

        memory.record_running(&container);

        assert!(!memory.should_wait_for_fresh_start_observation(&container));
    }

    fn deploy_container(machine_id_value: &str, container_id_value: &str) -> DeployContainer {
        DeployContainer {
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_target"),
            machine_id: machine_id(machine_id_value),
            container_id: container_id(container_id_value),
            step_id: step_id("run_1"),
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
            identity: ManagedContainerIdentity {
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                namespace_revision_entry_id: namespace_revision_entry_id("entry_observed"),
                operation_id: operation_id("op_123"),
                step_id: step_id("run_1"),
                kind: ManagedContainerKind::Service,
            },
            state,
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
