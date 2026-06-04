use ployz_core::deploy::DeployPlanError;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, StepId, SubjectTokenError};
use ployz_core::ops::{
    ActiveServiceCommitFailure, DeployOperationFailure, ExpectedActiveServiceFailure,
    FailureMessage, HealthCheckFailure, OperatorHint, RetainedArtifact,
};
use ployz_core::state::{ActiveServiceStaleReason, ExpectedActiveService};
use ployz_nats::core_state::CoreStateStoreError;
use ployz_nats::operations::{RecordDeployEvidenceError, RecordDeployTransitionError};
use std::future::Future;
use std::time::Duration;

use crate::docker::labels::ManagedContainerLabels;

use super::{DeployExecutionCommand, DeployOperationRecorder, StartedDeployContainer};

#[derive(Debug)]
pub(super) struct DeployExecutionFailure {
    pub(super) source: DeployExecutionError,
    pub(super) started_containers: Vec<StartedDeployContainer>,
}

impl DeployExecutionFailure {
    fn new(source: DeployExecutionError, started_containers: &[StartedDeployContainer]) -> Self {
        Self {
            source,
            started_containers: started_containers.to_vec(),
        }
    }
}

pub(super) fn failure(
    source: DeployExecutionError,
    started_containers: &[StartedDeployContainer],
) -> DeployExecutionFailure {
    DeployExecutionFailure::new(source, started_containers)
}

pub(super) async fn fail_deploy<R>(
    command: DeployExecutionCommand,
    recorder: &mut R,
    failure: DeployExecutionFailure,
) -> Result<super::DeployExecutionOutcome, DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    let operation_failure = failure
        .source
        .deploy_failure(&command, retained_artifacts(&failure.started_containers));
    let failure_record_error = record_failed_transition(
        command.step_timeout(),
        recorder,
        &command,
        &operation_failure,
    )
    .await;

    Err(DeployExecutionError::Failed {
        failure: operation_failure,
        source: Box::new(failure.source),
        failure_record_error,
    })
}

async fn record_failed_transition<R>(
    timeout: Duration,
    recorder: &mut R,
    command: &DeployExecutionCommand,
    operation_failure: &DeployOperationFailure,
) -> Option<DeployFailureRecordError>
where
    R: DeployOperationRecorder,
{
    match tokio::time::timeout(
        timeout,
        recorder.record_deploy_transition(
            &command.operation_id,
            ployz_core::ops::DeployTransition::Failed {
                failure: operation_failure.clone(),
            },
        ),
    )
    .await
    {
        Ok(Ok(())) => None,
        Ok(Err(source)) => Some(DeployFailureRecordError::Record(source)),
        Err(_) => Some(DeployFailureRecordError::TimedOut { timeout }),
    }
}

pub(super) async fn with_step_timeout<T, E, F>(
    command: &DeployExecutionCommand,
    step: DeployExecutionStep,
    future: F,
) -> Result<T, DeployExecutionError>
where
    F: Future<Output = Result<T, E>>,
    E: Into<DeployExecutionError>,
{
    let timeout = command.step_timeout();
    tokio::time::timeout(timeout, future)
        .await
        .map_err(|_| DeployExecutionError::StepTimedOut { step, timeout })?
        .map_err(Into::into)
}

#[derive(Debug)]
pub enum DeployExecutionError {
    Plan(DeployPlanError),
    StepId(SubjectTokenError),
    StepTimedOut {
        step: DeployExecutionStep,
        timeout: Duration,
    },
    RecordTransition(DeployOperationRecordError),
    RecordEvidence(DeployOperationRecordError),
    RunContainer(NodeContainerRuntimeError),
    WaitHealthy(DeployHealthCheckError),
    CommitActiveService(ActiveServiceCommitError),
    ActiveServiceCommitRejected {
        reason: ActiveServiceCommitRejection,
    },
    CompletionRecordPending {
        attempts: usize,
        last_error: CompletionRecordAttemptError,
    },
    Failed {
        failure: DeployOperationFailure,
        source: Box<DeployExecutionError>,
        failure_record_error: Option<DeployFailureRecordError>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployExecutionStep {
    RecordPlanning,
    RecordPlanCreated,
    RecordExecutingPlan,
    RunContainer { node_id: NodeId },
    RecordContainerStarted,
    RecordHealthCheckStarted,
    RecordWaitingForHealth,
    WaitHealthy,
    RecordActiveServiceCommitCheckpoint,
    RecordCompleted,
    CommitActiveService,
    RecordFailed,
}

impl From<DeployPlanError> for DeployExecutionError {
    fn from(value: DeployPlanError) -> Self {
        Self::Plan(value)
    }
}

impl DeployExecutionStep {
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::RecordPlanning => "record_planning",
            Self::RecordPlanCreated => "record_plan_created",
            Self::RecordExecutingPlan => "record_executing_plan",
            Self::RunContainer { .. } => "run_container",
            Self::RecordContainerStarted => "record_container_started",
            Self::RecordHealthCheckStarted => "record_health_check_started",
            Self::RecordWaitingForHealth => "record_waiting_for_health",
            Self::WaitHealthy => "wait_healthy",
            Self::RecordActiveServiceCommitCheckpoint => "record_active_service_commit_checkpoint",
            Self::RecordCompleted => "record_completed",
            Self::CommitActiveService => "commit_active_service",
            Self::RecordFailed => "record_failed",
        }
    }

    fn deploy_timeout_failure(
        &self,
        command: &DeployExecutionCommand,
        timeout: Duration,
        retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        match self {
            Self::RunContainer { node_id } => DeployOperationFailure::RuntimeUnavailable {
                node_id: node_id.clone(),
                message: timeout_failure_message("node runtime", timeout),
                retained_artifacts,
            },
            Self::WaitHealthy => DeployOperationFailure::HealthCheckFailed {
                health_check: HealthCheckFailure::TimedOut {
                    timeout_seconds: timeout_seconds(timeout),
                },
                retained_artifacts,
            },
            Self::CommitActiveService => DeployOperationFailure::ControlPlaneCommitFailed {
                service_id: command.request.service_id.clone(),
                revision_id: command.request.target_revision.clone(),
                message: timeout_failure_message("active service commit", timeout),
                retained_artifacts,
            },
            Self::RecordPlanning
            | Self::RecordPlanCreated
            | Self::RecordExecutingPlan
            | Self::RecordContainerStarted
            | Self::RecordHealthCheckStarted
            | Self::RecordWaitingForHealth
            | Self::RecordActiveServiceCommitCheckpoint
            | Self::RecordCompleted
            | Self::RecordFailed => DeployOperationFailure::ControlPlaneCommitFailed {
                service_id: command.request.service_id.clone(),
                revision_id: command.request.target_revision.clone(),
                message: timeout_failure_message(self.as_str(), timeout),
                retained_artifacts,
            },
        }
    }
}

impl From<DeployHealthCheckError> for DeployExecutionError {
    fn from(value: DeployHealthCheckError) -> Self {
        Self::WaitHealthy(value)
    }
}

impl DeployExecutionError {
    #[must_use]
    pub fn deploy_failure(
        &self,
        command: &DeployExecutionCommand,
        retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        match self {
            Self::Plan(_) | Self::StepId(_) => DeployOperationFailure::PlanningFailed {
                service_id: command.request.service_id.clone(),
                revision_id: command.request.target_revision.clone(),
                message: failure_message("deploy planning failed"),
            },
            Self::StepTimedOut { step, timeout } => {
                step.deploy_timeout_failure(command, *timeout, retained_artifacts)
            }
            Self::RecordTransition(_) | Self::RecordEvidence(_) => {
                DeployOperationFailure::ControlPlaneCommitFailed {
                    service_id: command.request.service_id.clone(),
                    revision_id: command.request.target_revision.clone(),
                    message: failure_message("operation progress could not be recorded"),
                    retained_artifacts,
                }
            }
            Self::RunContainer(error) => error.deploy_failure(retained_artifacts),
            Self::WaitHealthy(error) => error.deploy_failure(retained_artifacts),
            Self::CommitActiveService(error) => error.deploy_failure(command, retained_artifacts),
            Self::ActiveServiceCommitRejected { reason } => {
                DeployOperationFailure::ActiveServiceCommitRejected {
                    service_id: command.request.service_id.clone(),
                    revision_id: command.request.target_revision.clone(),
                    reason: reason.clone().into_failure(),
                    retained_artifacts,
                }
            }
            Self::CompletionRecordPending { .. } => {
                DeployOperationFailure::CompletionRecordFailedAfterActiveCommit {
                    service_id: command.request.service_id.clone(),
                    revision_id: command.request.target_revision.clone(),
                    message: failure_message("completion record could not be recorded"),
                    retained_artifacts,
                }
            }
            Self::Failed { failure, .. } => failure.clone(),
        }
    }
}

#[derive(Debug)]
pub enum CompletionRecordAttemptError {
    TimedOut { timeout: Duration },
    Record(DeployOperationRecordError),
}

impl From<ActiveServiceCommitError> for DeployExecutionError {
    fn from(value: ActiveServiceCommitError) -> Self {
        Self::CommitActiveService(value)
    }
}

#[derive(Debug)]
pub enum ActiveServiceCommitError {
    Store(CoreStateStoreError),
}

impl ActiveServiceCommitError {
    fn deploy_failure(
        &self,
        command: &DeployExecutionCommand,
        retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        match self {
            Self::Store(_) => DeployOperationFailure::ControlPlaneCommitFailed {
                service_id: command.request.service_id.clone(),
                revision_id: command.request.target_revision.clone(),
                message: failure_message("active service state could not be committed"),
                retained_artifacts,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActiveServiceCommitRejection {
    Stale {
        reason: ActiveServiceStaleReason,
    },
    Contended {
        current_revision: RevisionId,
        attempted_revision: RevisionId,
        expected_current: ExpectedActiveService,
    },
}

impl ActiveServiceCommitRejection {
    pub(crate) fn into_failure(self) -> ActiveServiceCommitFailure {
        match self {
            Self::Stale { reason } => match reason {
                ActiveServiceStaleReason::Missing { expected_revision } => {
                    ActiveServiceCommitFailure::MissingExpectedRevision { expected_revision }
                }
                ActiveServiceStaleReason::Mismatch {
                    expected_revision,
                    current_revision,
                } => ActiveServiceCommitFailure::RevisionMismatch {
                    expected_revision,
                    current_revision,
                },
                ActiveServiceStaleReason::UnexpectedCurrent { current_revision } => {
                    ActiveServiceCommitFailure::UnexpectedCurrentRevision { current_revision }
                }
            },
            Self::Contended {
                current_revision,
                attempted_revision,
                expected_current,
            } => ActiveServiceCommitFailure::Contended {
                current_revision,
                attempted_revision,
                expected_current: expected_active_service_failure(expected_current),
            },
        }
    }
}

#[derive(Debug)]
pub enum DeployOperationRecordError {
    RecordTransition(RecordDeployTransitionError),
    RecordEvidence(RecordDeployEvidenceError),
    Synthetic { message: &'static str },
}

#[derive(Debug)]
pub enum DeployFailureRecordError {
    TimedOut { timeout: Duration },
    Record(DeployOperationRecordError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeContainerRuntimeError {
    Unavailable {
        node_id: NodeId,
    },
    OperationStepConflict {
        node_id: NodeId,
        container_id: ContainerId,
        expected: ManagedContainerLabels,
        actual: ManagedContainerLabels,
    },
    OperationStepAmbiguous {
        node_id: NodeId,
        operation_id: OperationId,
        step_id: StepId,
        container_ids: Vec<ContainerId>,
    },
    StartedContainerUnhealthy {
        node_id: NodeId,
        container_id: ployz_core::ids::ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
}

impl NodeContainerRuntimeError {
    #[must_use]
    pub fn deploy_failure(
        &self,
        retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        match self {
            Self::Unavailable { node_id } => DeployOperationFailure::RuntimeUnavailable {
                node_id: node_id.clone(),
                message: failure_message("node runtime unavailable while starting container"),
                retained_artifacts,
            },
            Self::OperationStepConflict { node_id, .. } => {
                DeployOperationFailure::RuntimeUnavailable {
                    node_id: node_id.clone(),
                    message: failure_message(
                        "node runtime found a conflicting container for the operation step",
                    ),
                    retained_artifacts,
                }
            }
            Self::OperationStepAmbiguous { node_id, .. } => {
                DeployOperationFailure::RuntimeUnavailable {
                    node_id: node_id.clone(),
                    message: failure_message(
                        "node runtime found multiple containers for the operation step",
                    ),
                    retained_artifacts,
                }
            }
            Self::StartedContainerUnhealthy {
                node_id,
                container_id,
                message,
                log_hint,
            } => {
                let failing_artifact = RetainedArtifact::StartedContainer {
                    node_id: node_id.clone(),
                    container_id: container_id.clone(),
                    log_hint: log_hint.clone(),
                };
                let mut retained_artifacts = retained_artifacts;
                if !retained_artifacts.contains(&failing_artifact) {
                    retained_artifacts.push(failing_artifact);
                }

                DeployOperationFailure::HealthCheckFailed {
                    health_check: HealthCheckFailure::ProbeFailed {
                        message: message.clone(),
                    },
                    retained_artifacts,
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployHealthCheckError {
    Unhealthy {
        node_id: NodeId,
        container_id: ployz_core::ids::ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
}

impl DeployHealthCheckError {
    #[must_use]
    pub fn deploy_failure(
        &self,
        mut retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        match self {
            Self::Unhealthy {
                node_id,
                container_id,
                message,
                log_hint,
            } => {
                let failing_artifact = RetainedArtifact::StartedContainer {
                    node_id: node_id.clone(),
                    container_id: container_id.clone(),
                    log_hint: log_hint.clone(),
                };
                if !retained_artifacts.contains(&failing_artifact) {
                    retained_artifacts.push(failing_artifact);
                }

                DeployOperationFailure::HealthCheckFailed {
                    health_check: HealthCheckFailure::ProbeFailed {
                        message: message.clone(),
                    },
                    retained_artifacts,
                }
            }
        }
    }
}

fn retained_artifacts(containers: &[StartedDeployContainer]) -> Vec<RetainedArtifact> {
    containers
        .iter()
        .map(StartedDeployContainer::retained_artifact)
        .collect()
}

fn failure_message(message: &'static str) -> FailureMessage {
    FailureMessage::try_new(message).expect("internal failure message is non-empty")
}

fn expected_active_service_failure(
    expected_current: ExpectedActiveService,
) -> ExpectedActiveServiceFailure {
    match expected_current {
        ExpectedActiveService::Absent => ExpectedActiveServiceFailure::Absent,
        ExpectedActiveService::Revision(revision_id) => {
            ExpectedActiveServiceFailure::Revision { revision_id }
        }
    }
}

fn timeout_failure_message(scope: &'static str, timeout: Duration) -> FailureMessage {
    FailureMessage::try_new(format!("{scope} timed out after {}ms", timeout.as_millis()))
        .expect("generated timeout message is non-empty")
}

fn timeout_seconds(timeout: Duration) -> u32 {
    let seconds = timeout
        .as_secs()
        .saturating_add(u64::from(timeout.subsec_nanos() > 0));
    u32::try_from(seconds).unwrap_or(u32::MAX).max(1)
}
