use ployz_core::dataplane::WireGuardEbpfPrepareError;
use ployz_core::deploy::DeployPlanError;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, StepId, SubjectTokenError};
use ployz_core::ops::{
    ActiveServiceCommitFailure, DeployOperationFailure, FailureMessage, HealthCheckFailure,
    OperatorHint, RetainedArtifact, RouteCutoverFailureReason,
};
use ployz_core::state::{ActiveRouteState, ExpectedActiveRoute, ExpectedActiveService};
use ployz_nats::core_state::CoreStateStoreError;
use ployz_nats::operations::{RecordDeployEvidenceError, RecordDeployTransitionError};
use std::future::Future;
use std::time::Duration;

use crate::docker::labels::ManagedContainerLabels;

use super::{
    ActiveRouteCommitError, DeployContainer, DeployExecutionCommand, DeployOperationRecorder,
};

#[derive(Debug)]
pub(super) struct DeployExecutionFailure {
    source: DeployExecutionError,
    operation_failure: DeployOperationFailure,
}

impl DeployExecutionFailure {
    fn new(
        command: &DeployExecutionCommand,
        source: DeployExecutionError,
        deploy_containers: &[DeployContainer],
    ) -> Self {
        let operation_failure =
            source.deploy_failure(command, retained_artifacts(deploy_containers));
        Self {
            source,
            operation_failure,
        }
    }
}

pub(super) fn failure(
    command: &DeployExecutionCommand,
    source: DeployExecutionError,
    deploy_containers: &[DeployContainer],
) -> DeployExecutionFailure {
    DeployExecutionFailure::new(command, source, deploy_containers)
}

pub(super) async fn fail_deploy<R>(
    command: DeployExecutionCommand,
    recorder: &mut R,
    failure: DeployExecutionFailure,
) -> Result<super::DeployExecutionOutcome, DeployExecutionError>
where
    R: DeployOperationRecorder,
{
    let failure_record_error = record_failed_transition(
        command.step_timeout(),
        recorder,
        &command,
        &failure.operation_failure,
    )
    .await;

    Err(DeployExecutionError::Failed {
        failure: failure.operation_failure,
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
    PrepareWireGuardEbpf(WireGuardEbpfPrepareError),
    RunContainer(NodeContainerRuntimeError),
    WaitHealthy(DeployHealthCheckError),
    CommitRoute(ActiveRouteCommitError),
    CommitActiveService(ActiveServiceCommitError),
    ActiveRouteCommitRejected {
        expected_current: ExpectedActiveRoute,
        current: Option<ActiveRouteState>,
        attempted: ActiveRouteState,
    },
    ActiveServiceCommitRejected {
        expected_current: ExpectedActiveService,
        current_revision: Option<RevisionId>,
        attempted_revision: RevisionId,
    },
    Failed {
        failure: DeployOperationFailure,
        source: Box<DeployExecutionError>,
        failure_record_error: Option<DeployFailureRecordError>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployExecutionStep {
    RecordOperationEvent,
    PrepareWireGuardEbpf { nodes: Vec<NodeId> },
    RunContainer { node_id: NodeId },
    WaitHealthy,
    CommitRoute { route: ployz_core::ops::RouteTarget },
    CommitActiveService,
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
            Self::RecordOperationEvent => "record_operation_event",
            Self::PrepareWireGuardEbpf { .. } => "prepare_wireguard_ebpf",
            Self::RunContainer { .. } => "run_container",
            Self::WaitHealthy => "wait_healthy",
            Self::CommitRoute { .. } => "commit_route",
            Self::CommitActiveService => "commit_active_service",
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
            Self::PrepareWireGuardEbpf { nodes } => {
                DeployOperationFailure::WireGuardEbpfPreparationTimedOut {
                    nodes: nodes.clone(),
                    timeout_seconds: timeout_seconds(timeout),
                    retained_artifacts,
                }
            }
            Self::WaitHealthy => DeployOperationFailure::HealthCheckFailed {
                health_check: HealthCheckFailure::TimedOut {
                    timeout_seconds: timeout_seconds(timeout),
                },
                retained_artifacts,
            },
            Self::CommitRoute { route } => DeployOperationFailure::RouteCutoverFailed {
                route: route.clone(),
                reason: RouteCutoverFailureReason::TimedOut {
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
            Self::RecordOperationEvent => DeployOperationFailure::ControlPlaneCommitFailed {
                service_id: command.request.service_id.clone(),
                revision_id: command.request.target_revision.clone(),
                message: timeout_failure_message(self.as_str(), timeout),
                retained_artifacts,
            },
        }
    }
}

impl DeployExecutionError {
    fn record_failure(
        command: &DeployExecutionCommand,
        retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        DeployOperationFailure::ControlPlaneCommitFailed {
            service_id: command.request.service_id.clone(),
            revision_id: command.request.target_revision.clone(),
            message: failure_message("operation progress could not be recorded"),
            retained_artifacts,
        }
    }
}

impl From<DeployHealthCheckError> for DeployExecutionError {
    fn from(value: DeployHealthCheckError) -> Self {
        Self::WaitHealthy(value)
    }
}

impl From<WireGuardEbpfPrepareError> for DeployExecutionError {
    fn from(value: WireGuardEbpfPrepareError) -> Self {
        Self::PrepareWireGuardEbpf(value)
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
                Self::record_failure(command, retained_artifacts)
            }
            Self::PrepareWireGuardEbpf(error) => {
                wireguard_ebpf_deploy_failure(error, retained_artifacts)
            }
            Self::RunContainer(error) => error.deploy_failure(retained_artifacts),
            Self::WaitHealthy(error) => error.deploy_failure(retained_artifacts),
            Self::CommitRoute(error) => error.deploy_failure(command, retained_artifacts),
            Self::CommitActiveService(error) => error.deploy_failure(command, retained_artifacts),
            Self::ActiveRouteCommitRejected { attempted, .. } => {
                DeployOperationFailure::RouteCutoverFailed {
                    route: attempted.target.clone(),
                    reason: RouteCutoverFailureReason::RouteRejected {
                        message: failure_message("route changed before cutover"),
                    },
                    retained_artifacts,
                }
            }
            Self::ActiveServiceCommitRejected {
                expected_current,
                current_revision,
                attempted_revision,
            } => DeployOperationFailure::ActiveServiceCommitRejected {
                service_id: command.request.service_id.clone(),
                revision_id: command.request.target_revision.clone(),
                reason: ActiveServiceCommitFailure::ActiveServiceChanged {
                    expected_current: expected_current.clone(),
                    current_revision: current_revision.clone(),
                    attempted_revision: attempted_revision.clone(),
                },
                retained_artifacts,
            },
            Self::Failed { failure, .. } => failure.clone(),
        }
    }
}

fn wireguard_ebpf_deploy_failure(
    error: &WireGuardEbpfPrepareError,
    retained_artifacts: Vec<RetainedArtifact>,
) -> DeployOperationFailure {
    match error {
        WireGuardEbpfPrepareError::Unavailable {
            node_id,
            component,
            message,
        } => DeployOperationFailure::WireGuardEbpfUnavailable {
            node_id: node_id.clone(),
            component: *component,
            message: message.clone(),
            retained_artifacts,
        },
    }
}

impl From<ActiveRouteCommitError> for DeployExecutionError {
    fn from(value: ActiveRouteCommitError) -> Self {
        Self::CommitRoute(value)
    }
}

impl ActiveRouteCommitError {
    fn deploy_failure(
        &self,
        command: &DeployExecutionCommand,
        retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        let route = command
            .request
            .route
            .clone()
            .expect("route commit errors only occur for routed deploys");
        match self {
            Self::Store(error) => DeployOperationFailure::RouteCutoverFailed {
                route,
                reason: RouteCutoverFailureReason::StateStoreFailed {
                    message: failure_message(format!("active route state write failed: {error}")),
                },
                retained_artifacts,
            },
        }
    }
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
        reason: NodeRuntimeUnavailableReason,
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
            Self::Unavailable { node_id, reason } => DeployOperationFailure::RuntimeUnavailable {
                node_id: node_id.clone(),
                message: reason.failure_message(),
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
                        node_id: node_id.clone(),
                        container_id: container_id.clone(),
                        message: message.clone(),
                        log_hint: log_hint.clone(),
                    },
                    retained_artifacts,
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeRuntimeUnavailableReason {
    EncodeRequest { message: String },
    RequestTimedOut,
    NoResponders,
    InvalidSubject,
    MaxPayloadExceeded,
    RequestFailed { message: String },
    ServiceBadRequest { message: String },
    ServiceConflict { message: String },
    ServiceUnavailable { message: String },
    ServiceTimedOut { message: String },
    ServiceInternal { message: String },
    MalformedServiceError { message: String },
    DecodeResponse { message: String },
    WrongResponder { actual_node_id: NodeId },
}

impl NodeRuntimeUnavailableReason {
    pub(crate) fn failure_message(&self) -> FailureMessage {
        let message = match self {
            Self::EncodeRequest { message } => {
                format!("node runtime request could not be encoded: {message}")
            }
            Self::RequestTimedOut => "node runtime request timed out".to_owned(),
            Self::NoResponders => "node runtime has no responders".to_owned(),
            Self::InvalidSubject => "node runtime subject was invalid".to_owned(),
            Self::MaxPayloadExceeded => "node runtime request exceeded NATS max payload".to_owned(),
            Self::RequestFailed { message } => format!("node runtime request failed: {message}"),
            Self::ServiceBadRequest { message } => {
                format!("node runtime rejected the request: {message}")
            }
            Self::ServiceConflict { message } => {
                format!("node runtime reported a conflict: {message}")
            }
            Self::ServiceUnavailable { message } => {
                format!("node runtime service unavailable: {message}")
            }
            Self::ServiceTimedOut { message } => {
                format!("node runtime service timed out: {message}")
            }
            Self::ServiceInternal { message } => {
                format!("node runtime service failed internally: {message}")
            }
            Self::MalformedServiceError { message } => {
                format!("node runtime returned malformed service error headers: {message}")
            }
            Self::DecodeResponse { message } => {
                format!("node runtime response could not be decoded: {message}")
            }
            Self::WrongResponder { actual_node_id } => {
                format!(
                    "node runtime replied for a different node: {}",
                    actual_node_id.as_str()
                )
            }
        };
        FailureMessage::try_new(message).expect("generated runtime failure message is non-empty")
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
        retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        match self {
            Self::Unhealthy {
                node_id,
                container_id,
                message,
                log_hint,
            } => DeployOperationFailure::HealthCheckFailed {
                health_check: HealthCheckFailure::ProbeFailed {
                    node_id: node_id.clone(),
                    container_id: container_id.clone(),
                    message: message.clone(),
                    log_hint: log_hint.clone(),
                },
                retained_artifacts,
            },
        }
    }
}

fn retained_artifacts(containers: &[DeployContainer]) -> Vec<RetainedArtifact> {
    containers
        .iter()
        .map(DeployContainer::retained_artifact)
        .collect()
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("internal failure message is non-empty")
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
