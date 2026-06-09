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
use ployz_core::node::ManagedContainerKind;
use ployz_core::ops::{
    DeployCleanupFailure, DeployEvidence, DeployRunningStage, DeployTransition, FailureMessage,
    RoutePort,
};

pub use facts::{
    ActiveServiceReadFailure, DeployExecutionNodeScope, DeployFactLoadError,
    ObservationReadFailure, load_deploy_execution_facts_from_nats,
};
pub use failure::{
    ActiveServiceCommitError, DeployExecutionError, DeployExecutionStep, DeployFailureRecordError,
    DeployHealthCheckError, DeployOperationRecordError, NodeContainerRuntimeError,
    NodeRuntimeUnavailableReason,
};
use failure::{DeployExecutionFailure, fail_deploy, failure, with_step_timeout};
pub use ports::{
    ActiveRouteCommitError, ActiveRouteCommitter, ActiveServiceCommitter, DeployHealthChecker,
    DeployOperationRecorder, NodeContainerRuntime, WireGuardEbpfPreparer,
};
pub use preparation::{
    DeployCommandPreparationError, DeployExecutionFacts, prepare_deploy_execution_command,
};

use crate::docker::labels::ManagedContainerIdentity;
pub use crate::node_runtime_types::{
    ContainerEndpointRequest, NodeContainerRunSpec, NodeRemoveContainerRequest,
    NodeRunContainerOutcome, NodeRunContainerRequest,
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
    D: WireGuardEbpfPreparer,
    N: NodeContainerRuntime,
    H: DeployHealthChecker,
    C: ActiveRouteCommitter,
    A: ActiveServiceCommitter,
{
    let mut ports = ports;
    match execute_deploy(&command, &mut ports).await {
        Ok(outcome) => Ok(outcome),
        Err(failure) => fail_deploy(command, &mut *ports.recorder, failure).await,
    }
}

async fn execute_deploy<R, D, N, H, C, A>(
    command: &DeployExecutionCommand,
    ports: &mut DeployExecutionPorts<'_, R, D, N, H, C, A>,
) -> Result<DeployExecutionOutcome, DeployExecutionFailure>
where
    R: DeployOperationRecorder,
    D: WireGuardEbpfPreparer,
    N: NodeContainerRuntime,
    H: DeployHealthChecker,
    C: ActiveRouteCommitter,
    A: ActiveServiceCommitter,
{
    let mut containers = Vec::new();
    let mut started_containers = Vec::new();
    record_stage(command, &mut *ports.recorder, DeployTransition::Planning)
        .await
        .map_err(|source| failure(command, source, &started_containers))?;
    let plan = plan_service_deploy(DeployPlanningInput {
        request: command.request.clone(),
        eligible_nodes: command.eligible_nodes.clone(),
        existing_replicas: command.existing_replicas.clone(),
        cleanup_candidates: command.cleanup_candidates.clone(),
    })
    .map_err(|source| failure(command, source.into(), &started_containers))?;
    record_plan_created(command, &mut *ports.recorder, &plan)
        .await
        .map_err(|source| failure(command, source, &started_containers))?;
    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::PreparingWireGuardEbpf,
    )
    .await
    .map_err(|source| failure(command, source, &started_containers))?;
    let dataplane = prepare_wireguard_ebpf(command, &plan, &mut *ports.wireguard_ebpf)
        .await
        .map_err(|source| failure(command, source, &started_containers))?;
    record_wireguard_ebpf_prepared(command, &mut *ports.recorder, dataplane)
        .await
        .map_err(|source| failure(command, source, &started_containers))?;
    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::StartingContainers,
    )
    .await
    .map_err(|source| failure(command, source, &started_containers))?;

    for step in &plan.steps {
        match step {
            DeployPlanStep::UseExistingContainer {
                node_id,
                container_id,
                ..
            } => containers.push(DeployContainer {
                node_id: node_id.clone(),
                container_id: container_id.clone(),
                required_endpoint_port: required_endpoint_port(command),
            }),
            DeployPlanStep::RunContainer { node_id, slot } => {
                let started = with_step_timeout(
                    command,
                    DeployExecutionStep::RunContainer {
                        node_id: node_id.clone(),
                    },
                    run_deploy_step(&mut *ports.node_runtime, command, node_id, *slot),
                )
                .await
                .map_err(|source| failure(command, source, &started_containers))?;
                containers.push(started.clone());
                started_containers.push(started.clone());
                record_container_started(&mut *ports.recorder, command, &started)
                    .await
                    .map_err(|source| failure(command, source, &started_containers))?;
            }
        }
    }

    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::WaitingForHealth,
    )
    .await
    .map_err(|source| failure(command, source, &started_containers))?;

    record_health_check_started(command, &mut *ports.recorder)
        .await
        .map_err(|source| failure(command, source, &started_containers))?;

    with_step_timeout(
        command,
        DeployExecutionStep::WaitHealthy,
        (*ports.health_checker).wait_healthy(&containers),
    )
    .await
    .map_err(|source| failure(command, source, &started_containers))?;

    if command.active_route_commit_request().is_some() {
        record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::RouteCutover,
        )
        .await
        .map_err(|source| failure(command, source, &started_containers))?;

        cutover_route(command, &mut *ports.route_state)
            .await
            .map_err(|source| failure(command, source, &started_containers))?;
    }
    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployRunningStage::ActiveServiceCommit,
    )
    .await
    .map_err(|source| failure(command, source, &started_containers))?;

    commit_active_service(command, &mut *ports.active_state)
        .await
        .map_err(|source| failure(command, source, &started_containers))?;
    if !plan.cleanup_containers.is_empty() {
        let _ = record_running_stage(
            command,
            &mut *ports.recorder,
            DeployRunningStage::RemovingSupersededContainers,
        )
        .await;
    }
    let cleanup = cleanup_superseded_containers(command, &mut *ports.node_runtime, &plan).await;
    if !cleanup.is_empty() {
        let _ = record_cleanup_finished(command, &mut *ports.recorder, cleanup_evidence(&cleanup))
            .await;
    }
    let terminal_event = record_completion_best_effort(command, &mut *ports.recorder).await;

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
    node_runtime: &mut N,
    plan: &DeployPlan,
) -> Vec<DeployCleanupResult>
where
    N: NodeContainerRuntime,
{
    if plan.cleanup_containers.is_empty() {
        return Vec::new();
    }

    let mut cleanup = Vec::new();
    for target in &plan.cleanup_containers {
        let result = node_runtime.remove_container(NodeRemoveContainerRequest {
            node_id: target.node_id.clone(),
            operation_id: command.operation_id.clone(),
            container_id: target.container_id.clone(),
            expected_identity: cleanup_expected_identity(target),
        });
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

fn cleanup_expected_identity(target: &DeployCleanupContainer) -> ManagedContainerIdentity {
    ManagedContainerIdentity {
        service_id: target.service_id.clone(),
        revision_id: target.revision_id.clone(),
        operation_id: target.operation_id.clone(),
        step_id: target.step_id.clone(),
        kind: target.kind,
    }
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

fn cleanup_failure_message(error: NodeContainerRuntimeError) -> FailureMessage {
    match error {
        NodeContainerRuntimeError::Unavailable { reason, .. } => reason.failure_message(),
        NodeContainerRuntimeError::OperationStepConflict { .. } => {
            FailureMessage::try_new("cleanup found a conflicting operation-step container")
                .expect("generated cleanup failure message is non-empty")
        }
        NodeContainerRuntimeError::OperationStepAmbiguous { .. } => {
            FailureMessage::try_new("cleanup found multiple operation-step containers")
                .expect("generated cleanup failure message is non-empty")
        }
        NodeContainerRuntimeError::CreatedContainerStartFailed { message, .. }
        | NodeContainerRuntimeError::ExistingContainerStartFailed { message, .. }
        | NodeContainerRuntimeError::OperationStepContainerNotStartable { message, .. }
        | NodeContainerRuntimeError::StartedContainerUnhealthy { message, .. }
        | NodeContainerRuntimeError::RemoveContainerFailed { message, .. } => message,
    }
}

async fn record_completion_best_effort<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
) -> DeployTerminalEvent
where
    R: DeployOperationRecorder,
{
    match record_stage(command, recorder, DeployTransition::completed()).await {
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

async fn prepare_wireguard_ebpf<D>(
    command: &DeployExecutionCommand,
    plan: &DeployPlan,
    wireguard_ebpf: &mut D,
) -> Result<ployz_core::dataplane::WireGuardEbpfPrepareReport, DeployExecutionError>
where
    D: WireGuardEbpfPreparer,
{
    let request = command.wireguard_ebpf_prepare_request(plan);
    with_step_timeout(
        command,
        DeployExecutionStep::PrepareWireGuardEbpf {
            nodes: request.nodes.clone(),
        },
        wireguard_ebpf.prepare_wireguard_ebpf(request),
    )
    .await
}

async fn record_wireguard_ebpf_prepared<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    report: ployz_core::dataplane::WireGuardEbpfPrepareReport,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    with_step_timeout(command, DeployExecutionStep::RecordOperationEvent, async {
        recorder
            .record_deploy_evidence(
                &command.operation_id,
                DeployEvidence::WireGuardEbpfPrepared { report },
            )
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

async fn record_plan_created<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    plan: &DeployPlan,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    let evidence = DeployEvidence::PlanCreated { plan: plan.clone() };
    with_step_timeout(command, DeployExecutionStep::RecordOperationEvent, async {
        recorder
            .record_deploy_evidence(&command.operation_id, evidence)
            .await
            .map_err(DeployExecutionError::RecordEvidence)
    })
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

async fn record_health_check_started<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    let evidence = DeployEvidence::HealthCheckStarted;
    with_step_timeout(command, DeployExecutionStep::RecordOperationEvent, async {
        recorder
            .record_deploy_evidence(&command.operation_id, evidence)
            .await
            .map_err(DeployExecutionError::RecordEvidence)
    })
    .await
}

async fn record_cleanup_finished<R>(
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

async fn record_container_started<R>(
    recorder: &mut R,
    command: &DeployExecutionCommand,
    started: &DeployContainer,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    let evidence = DeployEvidence::ContainerStarted {
        node_id: started.node_id.clone(),
        container_id: started.container_id.clone(),
    };
    with_step_timeout(command, DeployExecutionStep::RecordOperationEvent, async {
        recorder
            .record_deploy_evidence(&command.operation_id, evidence)
            .await
            .map_err(DeployExecutionError::RecordEvidence)
    })
    .await
}

async fn run_deploy_step<N>(
    node_runtime: &mut N,
    command: &DeployExecutionCommand,
    node_id: &ployz_core::ids::NodeId,
    slot: ReplicaSlot,
) -> Result<DeployContainer, DeployExecutionError>
where
    N: NodeContainerRuntime,
{
    let step_id = deploy_step_id(slot).map_err(DeployExecutionError::StepId)?;
    let endpoint = command
        .request
        .route
        .as_ref()
        .map(|route| ContainerEndpointRequest {
            port: route.endpoint_port,
        });
    let request = NodeRunContainerRequest {
        node_id: node_id.clone(),
        image: command.request.image.clone(),
        endpoint,
        container: NodeContainerRunSpec {
            service_id: command.request.service_id.clone(),
            revision_id: command.request.target_revision.clone(),
            operation_id: command.operation_id.clone(),
            step_id,
            kind: ManagedContainerKind::Service,
        },
    };

    node_runtime
        .run_container(request)
        .await
        .map(|outcome| DeployContainer {
            node_id: node_id.clone(),
            container_id: outcome.container_id().clone(),
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
