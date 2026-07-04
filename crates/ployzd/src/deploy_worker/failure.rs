use ployz_core::dataplane::DataplanePrepareError;
use ployz_core::deploy::DeployPlanError;
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceRevisionId, OperationId, ServiceId, StepId, SubjectTokenError,
};
use ployz_core::ops::{
    ControlPlaneCommitScope, DeployOperationFailure, FailureMessage, HealthCheckFailure,
    OperatorHint, RetainedArtifact, RouteCutoverFailureReason,
};
use ployz_nats::core_state::CoreStateStoreError;
use ployz_nats::operations::{RecordDeployEvidenceError, RecordDeployTransitionError};
use std::future::Future;
use std::time::Duration;

use ployz_core::machine_runtime::ManagedContainerIdentity;

use super::{
    DeployContainer, DeployExecutionCommand, DeployOperationRecorder, RouteBindingCommitError,
};

fn failure_service_id(command: &DeployExecutionCommand) -> ServiceId {
    command.request.status_service_id()
}

fn failure_namespace_revision_id(command: &DeployExecutionCommand) -> NamespaceRevisionId {
    command.request.namespace_revision_id()
}

/// Scope for a control-plane commit failure. Empty-manifest deploys commit
/// no service entry, so their record failures are namespace-scoped instead
/// of borrowing a counterfeit entry digest.
fn failure_commit_scope(command: &DeployExecutionCommand) -> ControlPlaneCommitScope {
    let Some(service) = command.services().first() else {
        return ControlPlaneCommitScope::Namespace {
            namespace_revision_id: command.request.namespace_revision_id(),
        };
    };
    ControlPlaneCommitScope::ServiceEntry {
        service_id: service.request.service_id.clone(),
        namespace_revision_entry_id: service.request.namespace_revision_entry_id.clone(),
    }
}

#[derive(Debug)]
pub(super) struct DeployExecutionFailure {
    source: DeployExecutionError,
    operation_failure: DeployOperationFailure,
    retained_stop_targets: Vec<DeployContainer>,
}

impl DeployExecutionFailure {
    pub(super) fn new(
        command: &DeployExecutionCommand,
        source: DeployExecutionError,
        deploy_containers: &[DeployContainer],
    ) -> Self {
        Self::with_stop_targets(command, source, deploy_containers, deploy_containers)
    }

    pub(super) fn with_stop_targets(
        command: &DeployExecutionCommand,
        source: DeployExecutionError,
        deploy_containers: &[DeployContainer],
        retained_stop_targets: &[DeployContainer],
    ) -> Self {
        let operation_failure =
            source.deploy_failure(command, retained_artifacts(deploy_containers));
        Self {
            source,
            operation_failure,
            retained_stop_targets: retained_stop_targets.to_vec(),
        }
    }

    pub(super) fn retained_stop_targets(&self) -> &[DeployContainer] {
        &self.retained_stop_targets
    }

    pub(super) fn add_retained_artifacts(&mut self, artifacts: Vec<RetainedArtifact>) {
        if artifacts.is_empty() {
            return;
        }
        add_retained_artifacts(&mut self.operation_failure, artifacts);
    }
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
    PrepareDataplane(DataplanePrepareError),
    RunContainer(MachineContainerRuntimeError),
    WaitHealthy(DeployHealthCheckError),
    CommitRoute(RouteBindingCommitError),
    CommitServingTarget(ServingTargetCommitError),
    Failed {
        failure: DeployOperationFailure,
        source: Box<DeployExecutionError>,
        failure_record_error: Option<DeployFailureRecordError>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployExecutionStep {
    RecordOperationEvent,
    EnsureEndpointNetwork { machine_id: MachineId },
    PrepareDataplane { machines: Vec<MachineId> },
    RunContainer { machine_id: MachineId },
    WaitHealthy,
    CommitRoute { route: ployz_core::ops::RouteTarget },
    RemoveRoute { route: ployz_core::ops::RouteTarget },
    CommitServingTarget,
    RemoveServingTarget { scope: ControlPlaneCommitScope },
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
            Self::EnsureEndpointNetwork { .. } => "ensure_endpoint_network",
            Self::PrepareDataplane { .. } => "prepare_dataplane",
            Self::RunContainer { .. } => "run_container",
            Self::WaitHealthy => "wait_healthy",
            Self::CommitRoute { .. } => "commit_route",
            Self::RemoveRoute { .. } => "remove_route",
            Self::RemoveServingTarget { .. } => "remove_serving_target",
            Self::CommitServingTarget => "commit_serving_target_entry",
        }
    }

    fn deploy_timeout_failure(
        &self,
        command: &DeployExecutionCommand,
        timeout: Duration,
        retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        match self {
            Self::EnsureEndpointNetwork { machine_id } => {
                DeployOperationFailure::RuntimeUnavailable {
                    machine_id: machine_id.clone(),
                    message: timeout_failure_message("endpoint network ensure", timeout),
                    retained_artifacts,
                }
            }
            Self::RunContainer { machine_id } => DeployOperationFailure::RuntimeUnavailable {
                machine_id: machine_id.clone(),
                message: timeout_failure_message("machine runtime", timeout),
                retained_artifacts,
            },
            Self::PrepareDataplane { machines } => {
                DeployOperationFailure::DataplanePrepareTimedOut {
                    machines: machines.clone(),
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
            Self::CommitRoute { route } | Self::RemoveRoute { route } => {
                DeployOperationFailure::RouteCutoverFailed {
                    route: route.clone(),
                    reason: RouteCutoverFailureReason::TimedOut {
                        timeout_seconds: timeout_seconds(timeout),
                    },
                    retained_artifacts,
                }
            }
            Self::CommitServingTarget => DeployOperationFailure::ControlPlaneCommitFailed {
                scope: failure_commit_scope(command),
                message: timeout_failure_message("serving target commit", timeout),
                retained_artifacts,
            },
            Self::RemoveServingTarget { scope } => {
                DeployOperationFailure::ControlPlaneCommitFailed {
                    scope: scope.clone(),
                    message: timeout_failure_message("serving target unpublish", timeout),
                    retained_artifacts,
                }
            }
            Self::RecordOperationEvent => DeployOperationFailure::ControlPlaneCommitFailed {
                scope: failure_commit_scope(command),
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
            scope: failure_commit_scope(command),
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

impl From<DataplanePrepareError> for DeployExecutionError {
    fn from(value: DataplanePrepareError) -> Self {
        Self::PrepareDataplane(value)
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
                service_id: failure_service_id(command),
                namespace_revision_id: failure_namespace_revision_id(command),
                message: failure_message("deploy planning failed"),
            },
            Self::StepTimedOut { step, timeout } => {
                step.deploy_timeout_failure(command, *timeout, retained_artifacts)
            }
            Self::RecordTransition(_) | Self::RecordEvidence(_) => {
                Self::record_failure(command, retained_artifacts)
            }
            Self::PrepareDataplane(error) => dataplane_deploy_failure(error, retained_artifacts),
            Self::RunContainer(error) => error.deploy_failure(retained_artifacts),
            Self::WaitHealthy(error) => error.deploy_failure(retained_artifacts),
            Self::CommitRoute(error) => error.deploy_failure(retained_artifacts),
            Self::CommitServingTarget(error) => error.deploy_failure(retained_artifacts),
            Self::Failed { failure, .. } => failure.clone(),
        }
    }
}

fn dataplane_deploy_failure(
    error: &DataplanePrepareError,
    retained_artifacts: Vec<RetainedArtifact>,
) -> DeployOperationFailure {
    match error {
        DataplanePrepareError::Unavailable {
            machine_id,
            provider,
            message,
        } => DeployOperationFailure::DataplaneUnavailable {
            machine_id: machine_id.clone(),
            provider_failure: *provider,
            message: message.clone(),
            retained_artifacts,
        },
        DataplanePrepareError::InvalidReport { message } => {
            DeployOperationFailure::DataplanePrepareInvalidReport {
                message: message.clone(),
                retained_artifacts,
            }
        }
    }
}

impl From<RouteBindingCommitError> for DeployExecutionError {
    fn from(value: RouteBindingCommitError) -> Self {
        Self::CommitRoute(value)
    }
}

impl RouteBindingCommitError {
    fn deploy_failure(&self, retained_artifacts: Vec<RetainedArtifact>) -> DeployOperationFailure {
        match self {
            Self::Store { target, error } => DeployOperationFailure::RouteCutoverFailed {
                route: target.clone(),
                reason: RouteCutoverFailureReason::StateStoreFailed {
                    message: failure_message(format!("route binding state write failed: {error}")),
                },
                retained_artifacts,
            },
            Self::NamespaceLockLost { target } => DeployOperationFailure::RouteCutoverFailed {
                route: target.clone(),
                reason: RouteCutoverFailureReason::StateStoreFailed {
                    message: failure_message("namespace lock was lost before route cutover"),
                },
                retained_artifacts,
            },
        }
    }
}

impl From<ServingTargetCommitError> for DeployExecutionError {
    fn from(value: ServingTargetCommitError) -> Self {
        Self::CommitServingTarget(value)
    }
}

#[derive(Debug)]
pub enum ServingTargetCommitError {
    Store {
        scope: ControlPlaneCommitScope,
        error: CoreStateStoreError,
    },
    NamespaceLockLost {
        scope: ControlPlaneCommitScope,
    },
}

impl ServingTargetCommitError {
    fn deploy_failure(&self, retained_artifacts: Vec<RetainedArtifact>) -> DeployOperationFailure {
        match self {
            Self::Store { scope, error } => DeployOperationFailure::ControlPlaneCommitFailed {
                scope: scope.clone(),
                message: failure_message(format!(
                    "serving target entry state could not be committed: {error}"
                )),
                retained_artifacts,
            },
            Self::NamespaceLockLost { scope } => DeployOperationFailure::ControlPlaneCommitFailed {
                scope: scope.clone(),
                message: failure_message("namespace lock was lost before serving target commit"),
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
pub enum MachineContainerRuntimeError {
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
    OperationStepConflict {
        machine_id: MachineId,
        container_id: ContainerId,
        expected: ManagedContainerIdentity,
        actual: ManagedContainerIdentity,
    },
    OperationStepAmbiguous {
        machine_id: MachineId,
        operation_id: OperationId,
        step_id: StepId,
        container_ids: Vec<ContainerId>,
    },
    CreatedContainerStartFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    ExistingContainerStartFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    OperationStepContainerNotStartable {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    RemoveContainerFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    StopContainerFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

impl MachineContainerRuntimeError {
    #[must_use]
    pub fn deploy_failure(
        &self,
        retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        match self {
            Self::Unavailable { machine_id, reason } => {
                DeployOperationFailure::RuntimeUnavailable {
                    machine_id: machine_id.clone(),
                    message: reason.failure_message(),
                    retained_artifacts,
                }
            }
            Self::OperationStepConflict { machine_id, .. } => {
                DeployOperationFailure::RuntimeUnavailable {
                    machine_id: machine_id.clone(),
                    message: failure_message(
                        "machine runtime found a conflicting container for the operation step",
                    ),
                    retained_artifacts,
                }
            }
            Self::OperationStepAmbiguous { machine_id, .. } => {
                DeployOperationFailure::RuntimeUnavailable {
                    machine_id: machine_id.clone(),
                    message: failure_message(
                        "machine runtime found multiple containers for the operation step",
                    ),
                    retained_artifacts,
                }
            }
            Self::CreatedContainerStartFailed {
                machine_id,
                container_id,
                message,
                inspect_hint,
            } => {
                let retained_artifact = RetainedArtifact::CreatedContainer {
                    machine_id: machine_id.clone(),
                    container_id: container_id.clone(),
                    inspect_hint: inspect_hint.clone(),
                };
                let mut retained_artifacts = retained_artifacts;
                if !retained_artifacts.contains(&retained_artifact) {
                    retained_artifacts.push(retained_artifact);
                }

                DeployOperationFailure::RuntimeUnavailable {
                    machine_id: machine_id.clone(),
                    message: message.clone(),
                    retained_artifacts,
                }
            }
            Self::ExistingContainerStartFailed {
                machine_id,
                message,
                ..
            }
            | Self::OperationStepContainerNotStartable {
                machine_id,
                message,
                ..
            }
            | Self::RemoveContainerFailed {
                machine_id,
                message,
                ..
            }
            | Self::StopContainerFailed {
                machine_id,
                message,
                ..
            } => DeployOperationFailure::RuntimeUnavailable {
                machine_id: machine_id.clone(),
                message: message.clone(),
                retained_artifacts,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineRuntimeUnavailableReason {
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
    WrongResponder { actual_machine_id: MachineId },
}

impl MachineRuntimeUnavailableReason {
    pub(crate) fn failure_message(&self) -> FailureMessage {
        let message = match self {
            Self::EncodeRequest { message } => {
                format!("machine runtime request could not be encoded: {message}")
            }
            Self::RequestTimedOut => "machine runtime request timed out".to_owned(),
            Self::NoResponders => "machine runtime has no responders".to_owned(),
            Self::InvalidSubject => "machine runtime subject was invalid".to_owned(),
            Self::MaxPayloadExceeded => {
                "machine runtime request exceeded NATS max payload".to_owned()
            }
            Self::RequestFailed { message } => format!("machine runtime request failed: {message}"),
            Self::ServiceBadRequest { message } => {
                format!("machine runtime rejected the request: {message}")
            }
            Self::ServiceConflict { message } => {
                format!("machine runtime reported a conflict: {message}")
            }
            Self::ServiceUnavailable { message } => {
                format!("machine runtime service unavailable: {message}")
            }
            Self::ServiceTimedOut { message } => {
                format!("machine runtime service timed out: {message}")
            }
            Self::ServiceInternal { message } => {
                format!("machine runtime service failed internally: {message}")
            }
            Self::MalformedServiceError { message } => {
                format!("machine runtime returned malformed service error headers: {message}")
            }
            Self::DecodeResponse { message } => {
                format!("machine runtime response could not be decoded: {message}")
            }
            Self::WrongResponder { actual_machine_id } => {
                format!(
                    "machine runtime replied for a different machine: {}",
                    actual_machine_id.as_str()
                )
            }
        };
        FailureMessage::try_new(message).expect("generated runtime failure message is non-empty")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployHealthCheckError {
    Unhealthy {
        machine_id: MachineId,
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
                machine_id,
                container_id,
                message,
                log_hint,
            } => DeployOperationFailure::HealthCheckFailed {
                health_check: HealthCheckFailure::ProbeFailed {
                    machine_id: machine_id.clone(),
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

fn add_retained_artifacts(failure: &mut DeployOperationFailure, artifacts: Vec<RetainedArtifact>) {
    let retained_artifacts = match failure {
        DeployOperationFailure::DataplaneUnavailable {
            retained_artifacts, ..
        }
        | DeployOperationFailure::DataplanePrepareTimedOut {
            retained_artifacts, ..
        }
        | DeployOperationFailure::DataplanePrepareInvalidReport {
            retained_artifacts, ..
        }
        | DeployOperationFailure::RuntimeUnavailable {
            retained_artifacts, ..
        }
        | DeployOperationFailure::HealthCheckFailed {
            retained_artifacts, ..
        }
        | DeployOperationFailure::ControlPlaneCommitFailed {
            retained_artifacts, ..
        }
        | DeployOperationFailure::RouteCutoverFailed {
            retained_artifacts, ..
        } => retained_artifacts,
        DeployOperationFailure::PlanningFailed { .. }
        | DeployOperationFailure::ArtifactUnavailable { .. } => return,
    };

    for artifact in artifacts {
        if !retained_artifacts.contains(&artifact) {
            retained_artifacts.push(artifact);
        }
    }
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
