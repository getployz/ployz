//! Deploy operation execution over explicit runtime ports.

mod facts;
mod failure;
mod finalization;
mod ports;
mod preparation;
mod types;

use ployz_core::deploy::{
    DeployPlan, DeployPlanStep, DeployPlanningInput, ReplicaSlot, plan_service_deploy,
};
use ployz_core::ids::{OperationId, StepId, SubjectTokenError};
use ployz_core::node::ManagedContainerKind;
use ployz_core::ops::{DeployEvidence, DeployRunningStage, DeployTransition};

use crate::docker::labels::ManagedContainerLabels;

pub use facts::{
    ActiveServiceReadFailure, DeployExecutionNodeScope, DeployFactLoadError,
    ObservationReadFailure, load_deploy_execution_facts_from_nats,
};
pub use failure::{
    ActiveServiceCommitError, ActiveServiceCommitRejection, CompletionRecordAttemptError,
    DeployExecutionError, DeployExecutionStep, DeployFailureRecordError, DeployHealthCheckError,
    DeployOperationRecordError, NodeContainerRuntimeError,
};
use failure::{DeployExecutionFailure, fail_deploy, failure, with_step_timeout};
use finalization::finalize_successful_deploy;
pub use ports::{
    ActiveServiceCommitter, DeployHealthChecker, DeployOperationRecorder, NodeContainerRuntime,
};
pub use preparation::{
    DeployCommandPreparationError, DeployExecutionFacts, prepare_deploy_execution_command,
};

pub use types::{
    DeployContainer, DeployExecutionCommand, DeployExecutionOutcome, DeployExecutionPorts,
    NodeRunContainerOutcome, NodeRunContainerRequest,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeployWorker;

impl DeployWorker {
    pub async fn execute<R, N, H, A>(
        &self,
        command: DeployExecutionCommand,
        ports: DeployExecutionPorts<'_, R, N, H, A>,
    ) -> Result<DeployExecutionOutcome, DeployExecutionError>
    where
        R: DeployOperationRecorder,
        N: NodeContainerRuntime,
        H: DeployHealthChecker,
        A: ActiveServiceCommitter,
    {
        let mut ports = ports;
        match execute_deploy(&command, &mut ports).await {
            Ok(outcome) => Ok(outcome),
            Err(failure) => fail_deploy(command, &mut *ports.recorder, failure).await,
        }
    }
}

async fn execute_deploy<R, N, H, A>(
    command: &DeployExecutionCommand,
    ports: &mut DeployExecutionPorts<'_, R, N, H, A>,
) -> Result<DeployExecutionOutcome, DeployExecutionFailure>
where
    R: DeployOperationRecorder,
    N: NodeContainerRuntime,
    H: DeployHealthChecker,
    A: ActiveServiceCommitter,
{
    let mut containers = Vec::new();
    let mut started_containers = Vec::new();
    record_stage(command, &mut *ports.recorder, DeployTransition::Planning)
        .await
        .map_err(|source| failure(source, &started_containers))?;
    let plan = plan_service_deploy(DeployPlanningInput {
        request: command.request.clone(),
        eligible_nodes: command.eligible_nodes.clone(),
        existing_replicas: command.existing_replicas.clone(),
    })
    .map_err(|source| failure(source.into(), &started_containers))?;
    record_plan_created(command, &mut *ports.recorder, &plan)
        .await
        .map_err(|source| failure(source, &started_containers))?;
    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployExecutionStep::RecordExecutingPlan,
        DeployRunningStage::StartingContainers,
    )
    .await
    .map_err(|source| failure(source, &started_containers))?;

    for step in &plan.steps {
        match step {
            DeployPlanStep::UseExistingContainer {
                node_id,
                container_id,
                ..
            } => containers.push(DeployContainer {
                node_id: node_id.clone(),
                container_id: container_id.clone(),
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
                .map_err(|source| failure(source, &started_containers))?;
                containers.push(started.clone());
                started_containers.push(started.clone());
                record_container_started(&mut *ports.recorder, command, &started)
                    .await
                    .map_err(|source| failure(source, &started_containers))?;
            }
        }
    }

    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployExecutionStep::RecordWaitingForHealth,
        DeployRunningStage::WaitingForHealth,
    )
    .await
    .map_err(|source| failure(source, &started_containers))?;

    record_health_check_started(command, &mut *ports.recorder)
        .await
        .map_err(|source| failure(source, &started_containers))?;

    with_step_timeout(
        command,
        DeployExecutionStep::WaitHealthy,
        (*ports.health_checker).wait_healthy(&containers),
    )
    .await
    .map_err(|source| failure(source, &started_containers))?;

    record_running_stage(
        command,
        &mut *ports.recorder,
        DeployExecutionStep::RecordActiveServiceCommitCheckpoint,
        DeployRunningStage::ActiveServiceCommit,
    )
    .await
    .map_err(|source| failure(source, &started_containers))?;

    let outcome = DeployExecutionOutcome {
        service_id: plan.service_id,
        target_revision: plan.target_revision,
        containers,
    };

    finalize_successful_deploy(command, &mut *ports.active_state, &mut *ports.recorder)
        .await
        .map_err(|source| source.into_execution_failure(&started_containers))?;

    Ok(outcome)
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
        DeployExecutionStep::RecordPlanning,
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
    with_step_timeout(command, DeployExecutionStep::RecordPlanCreated, async {
        recorder
            .record_deploy_evidence(
                &command.operation_id,
                DeployEvidence::PlanCreated { plan: plan.clone() },
            )
            .await
            .map_err(DeployExecutionError::RecordEvidence)
    })
    .await
}

async fn record_running_stage<R>(
    command: &DeployExecutionCommand,
    recorder: &mut R,
    step: DeployExecutionStep,
    stage: DeployRunningStage,
) -> Result<(), DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    with_step_timeout(
        command,
        step,
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
    with_step_timeout(
        command,
        DeployExecutionStep::RecordHealthCheckStarted,
        async {
            recorder
                .record_deploy_evidence(&command.operation_id, DeployEvidence::HealthCheckStarted)
                .await
                .map_err(DeployExecutionError::RecordEvidence)
        },
    )
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
    with_step_timeout(
        command,
        DeployExecutionStep::RecordContainerStarted,
        async {
            recorder
                .record_deploy_evidence(
                    &command.operation_id,
                    DeployEvidence::ContainerStarted {
                        node_id: started.node_id.clone(),
                        container_id: started.container_id.clone(),
                    },
                )
                .await
                .map_err(DeployExecutionError::RecordEvidence)
        },
    )
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
    let request = NodeRunContainerRequest {
        node_id: node_id.clone(),
        image: command.request.image.clone(),
        labels: ManagedContainerLabels {
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
        })
        .map_err(DeployExecutionError::RunContainer)
}

fn deploy_step_id(slot: ReplicaSlot) -> Result<StepId, SubjectTokenError> {
    StepId::try_new(format!("run_{}", slot.get()))
}
