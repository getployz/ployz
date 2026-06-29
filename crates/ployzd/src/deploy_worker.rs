//! Deploy operation execution over explicit runtime ports.

mod facts;
mod failure;
mod ports;
mod preparation;
mod types;

use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployPlanStep, DeployPlanningInput, ReplicaSlot,
    plan_service_deploy,
};
use ployz_core::ids::{OperationId, StepId, SubjectTokenError};
use ployz_core::machine_runtime::ManagedContainerKind;
use ployz_core::ops::{
    DeployCleanupFailure, DeployEvidence, DeployRunningStage, DeployTransition, FailureMessage,
    OperatorHint, RetainedArtifact, RoutePort,
};

pub use facts::{
    DeployExecutionMachineScope, DeployFactLoadError, load_deploy_execution_facts_from_nats,
};
pub use failure::{
    ActiveServiceCommitError, DeployExecutionError, DeployExecutionStep, DeployFailureRecordError,
    DeployHealthCheckError, DeployOperationRecordError, MachineContainerRuntimeError,
    MachineRuntimeUnavailableReason,
};
use failure::{DeployExecutionFailure, fail_deploy, with_step_timeout};
pub use ports::{
    ActiveRouteCommitError, ActiveRouteCommitter, ActiveServiceCommitter, DataplanePreparer,
    DeployHealthChecker, DeployOperationRecorder, MachineContainerRuntime,
};
pub use preparation::{
    DeployCommandPreparationError, DeployExecutionFacts, prepare_deploy_execution_command,
};

use crate::docker::labels::ManagedContainerIdentity;
use crate::machine_runtime::protocol::{
    ContainerEndpointRequest, MachineContainerRemoveRpcRequest, MachineContainerRunRpcRequest,
    MachineContainerRunSpec, MachineContainerStopRpcRequest,
    MachineEnsureEndpointNetworkRpcRequest,
};
pub use types::{
    DeployCleanupResult, DeployContainer, DeployExecutionCommand, DeployExecutionOutcome,
    DeployExecutionPorts, DeployTerminalEvent,
};

pub async fn execute_deploy_operation<R, D, N, H, C, A>(
    command: DeployExecutionCommand,
    ports: DeployExecutionPorts<'_, R, D, N, H, C, A>,
) -> Result<DeployExecutionOutcome, DeployExecutionError>
where
    R: DeployOperationRecorder,
    D: DataplanePreparer,
    N: MachineContainerRuntime,
    H: DeployHealthChecker,
    C: ActiveRouteCommitter,
    A: ActiveServiceCommitter,
{
    let mut ports = ports;
    match execute_deploy(&command, &mut ports).await {
        Ok(outcome) => Ok(outcome),
        Err(mut failure) => {
            let stop_artifacts = stop_retained_containers(
                &command,
                &mut *ports.machine_runtime,
                failure.retained_stop_targets(),
            )
            .await;
            failure.add_retained_artifacts(stop_artifacts);
            fail_deploy(command, &mut *ports.recorder, failure).await
        }
    }
}

/// In-flight deploy execution state: the command plus the containers started
/// so far, which become retained failure evidence when a stage fails.
struct DeployRun<'a> {
    command: &'a DeployExecutionCommand,
    started_containers: Vec<DeployContainer>,
}

impl<'a> DeployRun<'a> {
    fn new(command: &'a DeployExecutionCommand) -> Self {
        Self {
            command,
            started_containers: Vec::new(),
        }
    }

    fn container_started(&mut self, started: DeployContainer) {
        self.started_containers.push(started);
    }

    fn fail(&self, source: DeployExecutionError) -> DeployExecutionFailure {
        DeployExecutionFailure::new(self.command, source, &self.started_containers)
    }

    fn fail_run_container(
        &self,
        slot: ReplicaSlot,
        source: DeployExecutionError,
    ) -> DeployExecutionFailure {
        let retained = retained_containers_after_run_error(
            self.command,
            &self.started_containers,
            slot,
            &source,
        );
        DeployExecutionFailure::with_stop_targets(
            self.command,
            source,
            &self.started_containers,
            &retained,
        )
    }
}

async fn execute_deploy<R, D, N, H, C, A>(
    command: &DeployExecutionCommand,
    ports: &mut DeployExecutionPorts<'_, R, D, N, H, C, A>,
) -> Result<DeployExecutionOutcome, DeployExecutionFailure>
where
    R: DeployOperationRecorder,
    D: DataplanePreparer,
    N: MachineContainerRuntime,
    H: DeployHealthChecker,
    C: ActiveRouteCommitter,
    A: ActiveServiceCommitter,
{
    let mut containers = Vec::new();
    let mut run = DeployRun::new(command);
    record_stage(command, &mut *ports.recorder, DeployTransition::Planning)
        .await
        .map_err(|source| run.fail(source))?;
    let plan = plan_service_deploy(DeployPlanningInput {
        request: command.request.clone(),
        eligible_machines: command.eligible_machines.clone(),
        existing_replicas: command.existing_replicas.clone(),
        cleanup_candidates: command.cleanup_candidates.clone(),
    })
    .map_err(|source| run.fail(source.into()))?;
    record_evidence(
        command,
        &mut *ports.recorder,
        DeployEvidence::PlanCreated { plan: plan.clone() },
    )
    .await
    .map_err(|source| run.fail(source))?;
    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::PreparingDataplane,
    )
    .await
    .map_err(|source| run.fail(source))?;
    ensure_endpoint_networks(command, &plan, &mut *ports.machine_runtime)
        .await
        .map_err(|source| run.fail(source))?;
    let dataplane = prepare_dataplane(command, &plan, &mut *ports.dataplane)
        .await
        .map_err(|source| run.fail(source))?;
    record_evidence(
        command,
        &mut *ports.recorder,
        DeployEvidence::DataplanePrepared { report: dataplane },
    )
    .await
    .map_err(|source| run.fail(source))?;
    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::StartingContainers,
    )
    .await
    .map_err(|source| run.fail(source))?;

    for step in &plan.steps {
        match step {
            DeployPlanStep::UseExistingContainer {
                machine_id,
                container_id,
                slot,
            } => containers.push(DeployContainer {
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
                step_id: deploy_step_id(*slot)
                    .map_err(|source| run.fail(DeployExecutionError::StepId(source)))?,
                required_endpoint_port: required_endpoint_port(command),
            }),
            DeployPlanStep::RunContainer { machine_id, slot } => {
                let run_result = with_step_timeout(
                    command,
                    DeployExecutionStep::RunContainer {
                        machine_id: machine_id.clone(),
                    },
                    run_deploy_step(&mut *ports.machine_runtime, command, machine_id, *slot),
                )
                .await;
                let started = match run_result {
                    Ok(started) => started,
                    Err(source) => return Err(run.fail_run_container(*slot, source)),
                };
                containers.push(started.clone());
                run.container_started(started.clone());
                record_evidence(
                    command,
                    &mut *ports.recorder,
                    DeployEvidence::ContainerStarted {
                        machine_id: started.machine_id.clone(),
                        container_id: started.container_id.clone(),
                    },
                )
                .await
                .map_err(|source| run.fail(source))?;
            }
        }
    }

    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::WaitingForHealth,
    )
    .await
    .map_err(|source| run.fail(source))?;

    record_evidence(
        command,
        &mut *ports.recorder,
        DeployEvidence::HealthCheckStarted,
    )
    .await
    .map_err(|source| run.fail(source))?;

    let health_result = with_step_timeout(
        command,
        DeployExecutionStep::WaitHealthy,
        (*ports.health_checker).wait_healthy(&containers),
    )
    .await;
    if let Err(source) = health_result {
        return Err(run.fail(source));
    }

    if command.active_route_commit_request().is_some() {
        record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::RouteCutover,
        )
        .await
        .map_err(|source| run.fail(source))?;

        cutover_route(command, &mut *ports.route_state)
            .await
            .map_err(|source| run.fail(source))?;
    }
    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::ActiveServiceCommit,
    )
    .await
    .map_err(|source| run.fail(source))?;

    commit_active_service(command, &mut *ports.active_state)
        .await
        .map_err(|source| run.fail(source))?;
    if !plan.cleanup_containers.is_empty() {
        let _ = record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::RemovingSupersededContainers,
        )
        .await;
    }
    let cleanup = cleanup_superseded_containers(command, &mut *ports.machine_runtime, &plan).await;
    let terminal_event = record_terminal_state(command, &mut *ports.recorder, &cleanup).await;

    let outcome = DeployExecutionOutcome {
        service_id: plan.service_id,
        target_revision: plan.target_revision,
        containers,
        cleanup,
        terminal_event,
    };

    Ok(outcome)
}

async fn cleanup_superseded_containers<N>(
    command: &DeployExecutionCommand,
    machine_runtime: &mut N,
    plan: &DeployPlan,
) -> Vec<DeployCleanupResult>
where
    N: MachineContainerRuntime,
{
    if plan.cleanup_containers.is_empty() {
        return Vec::new();
    }

    let mut cleanup = Vec::new();
    for target in &plan.cleanup_containers {
        let result = machine_runtime.remove_container(
            &target.machine_id,
            MachineContainerRemoveRpcRequest {
                operation_id: command.operation_id.clone(),
                container_id: target.container_id.clone(),
                expected_identity: cleanup_expected_identity(target),
            },
        );
        let result = tokio::time::timeout(command.step_timeout(), result).await;
        match result {
            Ok(Ok(())) => cleanup.push(DeployCleanupResult::Removed(target.clone())),
            Ok(Err(error)) => cleanup.push(DeployCleanupResult::Failed {
                target: target.clone(),
                message: cleanup_failure_message(error),
            }),
            Err(_) => cleanup.push(DeployCleanupResult::Failed {
                target: target.clone(),
                message: FailureMessage::try_new(format!(
                    "container cleanup timed out after {} seconds",
                    command.step_timeout().as_secs()
                ))
                .expect("generated cleanup failure message is non-empty"),
            }),
        }
    }

    cleanup
}

async fn stop_retained_containers<N>(
    command: &DeployExecutionCommand,
    machine_runtime: &mut N,
    containers: &[DeployContainer],
) -> Vec<RetainedArtifact>
where
    N: MachineContainerRuntime,
{
    let mut stop_failures = Vec::new();
    for container in containers {
        let stop = machine_runtime.stop_container(
            &container.machine_id,
            MachineContainerStopRpcRequest {
                operation_id: command.operation_id.clone(),
                container_id: container.container_id.clone(),
                expected_identity: retained_container_identity(command, container),
            },
        );
        match tokio::time::timeout(command.step_timeout(), stop).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                stop_failures.push(container_stop_failed_artifact(
                    container,
                    cleanup_failure_message(error),
                ));
            }
            Err(_) => {
                stop_failures.push(container_stop_failed_artifact(
                    container,
                    FailureMessage::try_new(format!(
                        "retained container stop timed out after {} seconds",
                        command.step_timeout().as_secs()
                    ))
                    .expect("generated retained stop failure message is non-empty"),
                ));
            }
        }
    }
    stop_failures
}

fn container_stop_failed_artifact(
    container: &DeployContainer,
    message: FailureMessage,
) -> RetainedArtifact {
    RetainedArtifact::ContainerStopFailed {
        machine_id: container.machine_id.clone(),
        container_id: container.container_id.clone(),
        message,
        inspect_hint: inspect_hint(&container.container_id),
    }
}

fn retained_containers_after_run_error(
    command: &DeployExecutionCommand,
    started_containers: &[DeployContainer],
    slot: ReplicaSlot,
    source: &DeployExecutionError,
) -> Vec<DeployContainer> {
    let mut retained = started_containers.to_vec();
    if let Some(container) = retained_created_container(command, slot, source)
        && !retained.contains(&container)
    {
        retained.push(container);
    }
    retained
}

fn retained_created_container(
    command: &DeployExecutionCommand,
    slot: ReplicaSlot,
    source: &DeployExecutionError,
) -> Option<DeployContainer> {
    let DeployExecutionError::RunContainer(
        MachineContainerRuntimeError::CreatedContainerStartFailed {
            machine_id,
            container_id,
            ..
        },
    ) = source
    else {
        return None;
    };
    let step_id = deploy_step_id(slot).ok()?;
    Some(DeployContainer {
        machine_id: machine_id.clone(),
        container_id: container_id.clone(),
        step_id,
        required_endpoint_port: required_endpoint_port(command),
    })
}

fn retained_container_identity(
    command: &DeployExecutionCommand,
    container: &DeployContainer,
) -> ManagedContainerIdentity {
    ManagedContainerIdentity {
        service_id: command.request.service_id.clone(),
        revision_id: command.request.target_revision.clone(),
        operation_id: command.operation_id.clone(),
        step_id: container.step_id.clone(),
        kind: ManagedContainerKind::Service,
    }
}

fn cleanup_expected_identity(target: &DeployCleanupContainer) -> ManagedContainerIdentity {
    ManagedContainerIdentity {
        service_id: target.service_id.clone(),
        revision_id: target.revision_id.clone(),
        operation_id: target.operation_id.clone(),
        step_id: target.step_id.clone(),
        kind: target.kind,
    }
}

fn inspect_hint(container_id: &ployz_core::ids::ContainerId) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {}", container_id.as_str()))
        .expect("generated inspect hint is non-empty")
}

fn cleanup_evidence(cleanup: &[DeployCleanupResult]) -> DeployEvidence {
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for result in cleanup {
        match result {
            DeployCleanupResult::Removed(target) => removed.push(target.clone()),
            DeployCleanupResult::Failed { target, message } => {
                failed.push(DeployCleanupFailure {
                    target: target.clone(),
                    message: message.clone(),
                });
            }
        }
    }

    DeployEvidence::CleanupFinished { removed, failed }
}

fn cleanup_failure_message(error: MachineContainerRuntimeError) -> FailureMessage {
    match error {
        MachineContainerRuntimeError::Unavailable { reason, .. } => reason.failure_message(),
        MachineContainerRuntimeError::OperationStepConflict { .. } => {
            FailureMessage::try_new("cleanup found a conflicting operation-step container")
                .expect("generated cleanup failure message is non-empty")
        }
        MachineContainerRuntimeError::OperationStepAmbiguous { .. } => {
            FailureMessage::try_new("cleanup found multiple operation-step containers")
                .expect("generated cleanup failure message is non-empty")
        }
        MachineContainerRuntimeError::CreatedContainerStartFailed { message, .. }
        | MachineContainerRuntimeError::ExistingContainerStartFailed { message, .. }
        | MachineContainerRuntimeError::OperationStepContainerNotStartable { message, .. }
        | MachineContainerRuntimeError::StopContainerFailed { message, .. }
        | MachineContainerRuntimeError::RemoveContainerFailed { message, .. } => message,
    }
}

async fn record_terminal_state<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    cleanup: &[DeployCleanupResult],
) -> DeployTerminalEvent
where
    R: DeployOperationRecorder,
{
    if !cleanup.is_empty() {
        let record_cleanup = record_evidence(command, recorder, cleanup_evidence(cleanup)).await;
        if DeployCleanupResult::has_failure(cleanup) && record_cleanup.is_err() {
            return DeployTerminalEvent::Missing;
        }
    }

    let outcome = DeployCleanupResult::completion_outcome(cleanup);
    match record_stage(command, recorder, DeployTransition::Completed { outcome }).await {
        Ok(()) => DeployTerminalEvent::Recorded,
        Err(_) => DeployTerminalEvent::Missing,
    }
}

async fn commit_active_service<A>(
    command: &DeployExecutionCommand,
    active_state: &mut A,
) -> Result<(), DeployExecutionError>
where
    A: ActiveServiceCommitter,
{
    let outcome = with_step_timeout(
        command,
        DeployExecutionStep::CommitActiveService,
        active_state.commit_active_service(command.active_service_commit_request()),
    )
    .await?;

    match outcome {
        ployz_core::state::ActiveServiceCommit::Stored { .. }
        | ployz_core::state::ActiveServiceCommit::AlreadyCommitted { .. } => Ok(()),
        ployz_core::state::ActiveServiceCommit::ActiveServiceChanged {
            expected_current,
            current_revision,
            attempted_revision,
        } => Err(DeployExecutionError::ActiveServiceCommitRejected {
            expected_current,
            current_revision,
            attempted_revision,
        }),
    }
}

pub(super) async fn cutover_route<C>(
    command: &DeployExecutionCommand,
    route_state: &mut C,
) -> Result<(), DeployExecutionError>
where
    C: ActiveRouteCommitter,
{
    let Some(request) = command.active_route_commit_request() else {
        return Ok(());
    };

    let outcome = with_step_timeout(
        command,
        DeployExecutionStep::CommitRoute {
            route: request.target.clone(),
        },
        route_state.commit_active_route(request),
    )
    .await?;

    match outcome {
        ployz_core::state::ActiveRouteCommit::Stored { .. }
        | ployz_core::state::ActiveRouteCommit::AlreadyCommitted { .. } => Ok(()),
        ployz_core::state::ActiveRouteCommit::ActiveRouteChanged {
            expected_current,
            current,
            attempted,
        } => Err(DeployExecutionError::ActiveRouteCommitRejected {
            expected_current,
            current,
            attempted,
        }),
    }
}

async fn prepare_dataplane<D>(
    command: &DeployExecutionCommand,
    plan: &DeployPlan,
    dataplane: &mut D,
) -> Result<ployz_core::dataplane::PloyzNativeMeshPrepareReport, DeployExecutionError>
where
    D: DataplanePreparer,
{
    let request = command.dataplane_prepare_request(plan);
    with_step_timeout(
        command,
        DeployExecutionStep::PrepareDataplane {
            machines: request.machines(),
        },
        dataplane.prepare_dataplane(request),
    )
    .await
}

async fn ensure_endpoint_networks<N>(
    command: &DeployExecutionCommand,
    plan: &DeployPlan,
    machine_runtime: &mut N,
) -> Result<(), DeployExecutionError>
where
    N: MachineContainerRuntime,
{
    let request = command.dataplane_prepare_request(plan);
    for member in request.membership {
        let machine_id = member.machine_id;
        let network_request = MachineEnsureEndpointNetworkRpcRequest {
            operation_id: command.operation_id.clone(),
        };
        with_step_timeout(
            command,
            DeployExecutionStep::EnsureEndpointNetwork {
                machine_id: machine_id.clone(),
            },
            async {
                machine_runtime
                    .ensure_endpoint_network(&machine_id, network_request)
                    .await
                    .map_err(DeployExecutionError::RunContainer)
            },
        )
        .await?;
    }
    Ok(())
}

async fn record_evidence<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    evidence: DeployEvidence,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    with_step_timeout(command, DeployExecutionStep::RecordOperationEvent, async {
        recorder
            .record_deploy_evidence(&command.operation_id, evidence)
            .await
            .map_err(DeployExecutionError::RecordEvidence)
    })
    .await
}

async fn record_stage<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    transition: DeployTransition,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    with_step_timeout(
        command,
        DeployExecutionStep::RecordOperationEvent,
        record(recorder, &command.operation_id, transition),
    )
    .await
}

async fn record_running_stage<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    stage: DeployRunningStage,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    with_step_timeout(
        command,
        DeployExecutionStep::RecordOperationEvent,
        record(
            recorder,
            &command.operation_id,
            DeployTransition::Running { stage },
        ),
    )
    .await
}

async fn record<R>(
    recorder: &mut R,
    operation_id: &OperationId,
    transition: DeployTransition,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    recorder
        .record_deploy_transition(operation_id, transition)
        .await
        .map_err(DeployExecutionError::RecordTransition)
}

async fn run_deploy_step<N>(
    machine_runtime: &mut N,
    command: &DeployExecutionCommand,
    machine_id: &ployz_core::ids::MachineId,
    slot: ReplicaSlot,
) -> Result<DeployContainer, DeployExecutionError>
where
    N: MachineContainerRuntime,
{
    let step_id = deploy_step_id(slot).map_err(DeployExecutionError::StepId)?;
    let endpoint = command
        .request
        .route
        .as_ref()
        .map(|route| ContainerEndpointRequest {
            port: route.endpoint_port,
        });
    let request = MachineContainerRunRpcRequest {
        image: command.request.image.clone(),
        endpoint,
        container: MachineContainerRunSpec {
            service_id: command.request.service_id.clone(),
            revision_id: command.request.target_revision.clone(),
            operation_id: command.operation_id.clone(),
            step_id: step_id.clone(),
            kind: ManagedContainerKind::Service,
        },
    };

    machine_runtime
        .run_container(machine_id, request)
        .await
        .map(|outcome| DeployContainer {
            machine_id: machine_id.clone(),
            container_id: outcome.container_id().clone(),
            step_id,
            required_endpoint_port: required_endpoint_port(command),
        })
        .map_err(DeployExecutionError::RunContainer)
}

fn required_endpoint_port(command: &DeployExecutionCommand) -> Option<RoutePort> {
    command
        .request
        .route
        .as_ref()
        .map(|route| route.endpoint_port)
}

fn deploy_step_id(slot: ReplicaSlot) -> Result<StepId, SubjectTokenError> {
    StepId::try_new(format!("run_{}", slot.get()))
}
