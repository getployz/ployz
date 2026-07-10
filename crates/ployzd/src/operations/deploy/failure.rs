use crate::operations::log::{RecordDeployEvidenceError, RecordDeployTransitionError};
use crate::roles::machine::response::log_hint;
use ployz_core::dataplane::DataplanePrepareError;
use ployz_core::deploy::DeployPlanError;
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceRevisionId, OperationId, ServiceId, StepId, SubjectTokenError,
};
use ployz_core::ops::{
    ControlPlaneCommitScope, DeployOperationFailure, FailureMessage, HealthCheckFailure,
    OperatorHint, PreStartHookFailure, RetainedArtifact, RouteCutoverFailureReason,
};
use std::future::Future;
use std::time::Duration;

use super::{
    DeployContainer, DeployExecutionCommand, DeployOperationRecorder, NamespaceCommitError,
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
    PlanInconsistent {
        service_id: ServiceId,
    },
    StepId(SubjectTokenError),
    StepTimedOut {
        step: DeployExecutionStep,
        timeout: Duration,
    },
    RecordTransition(DeployOperationRecordError),
    RecordEvidence(DeployOperationRecordError),
    PrepareDataplane(DataplanePrepareError),
    RunContainer(MachineContainerRuntimeError),
    PreStartHook(PreStartHookRuntimeError),
    PreStartHookExited {
        machine_id: MachineId,
        container_id: ContainerId,
        exit_code: i64,
        message: FailureMessage,
    },
    WaitHealthy(DeployHealthCheckError),
    CommitNamespaceState(NamespaceCommitError),
    Failed {
        failure: DeployOperationFailure,
        source: Box<DeployExecutionError>,
        failure_record_error: Option<DeployFailureRecordError>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreStartHookRuntimeError {
    Unavailable {
        machine_id: MachineId,
        reason: MachineRuntimeUnavailableReason,
    },
    OperationStepAmbiguous {
        machine_id: MachineId,
        operation_id: OperationId,
        step_id: StepId,
        container_ids: Vec<ContainerId>,
    },
    CreateFailed {
        machine_id: MachineId,
        message: FailureMessage,
    },
    StartFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    WaitFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
    TimedOut {
        machine_id: MachineId,
        container_id: ContainerId,
        timeout_millis: u64,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    CleanupFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

impl PreStartHookRuntimeError {
    fn deploy_failure(
        &self,
        mut retained_artifacts: Vec<RetainedArtifact>,
    ) -> DeployOperationFailure {
        let (machine_id, failure, retained) = match self {
            Self::Unavailable { machine_id, reason } => (
                machine_id,
                PreStartHookFailure::RuntimeUnavailable {
                    message: reason.failure_message(),
                },
                None,
            ),
            Self::OperationStepAmbiguous {
                machine_id,
                operation_id,
                step_id,
                container_ids,
            } => (
                machine_id,
                PreStartHookFailure::OperationStepAmbiguous {
                    operation_id: operation_id.clone(),
                    step_id: step_id.clone(),
                    container_ids: container_ids.clone(),
                },
                None,
            ),
            Self::CreateFailed {
                machine_id,
                message,
            } => (
                machine_id,
                PreStartHookFailure::CreateFailed {
                    message: message.clone(),
                },
                None,
            ),
            Self::StartFailed {
                machine_id,
                container_id,
                message,
                inspect_hint,
            } => (
                machine_id,
                PreStartHookFailure::StartFailed {
                    container_id: container_id.clone(),
                    message: message.clone(),
                    inspect_hint: inspect_hint.clone(),
                },
                Some(RetainedArtifact::CreatedContainer {
                    machine_id: machine_id.clone(),
                    container_id: container_id.clone(),
                    inspect_hint: inspect_hint.clone(),
                }),
            ),
            Self::WaitFailed {
                machine_id,
                container_id,
                message,
                log_hint,
            } => (
                machine_id,
                PreStartHookFailure::WaitFailed {
                    container_id: container_id.clone(),
                    message: message.clone(),
                    log_hint: log_hint.clone(),
                },
                Some(RetainedArtifact::StartedContainer {
                    machine_id: machine_id.clone(),
                    container_id: container_id.clone(),
                    log_hint: log_hint.clone(),
                }),
            ),
            Self::TimedOut {
                machine_id,
                container_id,
                timeout_millis,
                message,
                inspect_hint,
            } => (
                machine_id,
                PreStartHookFailure::TimedOut {
                    container_id: container_id.clone(),
                    timeout_millis: *timeout_millis,
                    message: message.clone(),
                    inspect_hint: inspect_hint.clone(),
                },
                Some(RetainedArtifact::CreatedContainer {
                    machine_id: machine_id.clone(),
                    container_id: container_id.clone(),
                    inspect_hint: inspect_hint.clone(),
                }),
            ),
            Self::CleanupFailed {
                machine_id,
                container_id,
                message,
                inspect_hint,
            } => (
                machine_id,
                PreStartHookFailure::CleanupFailed {
                    container_id: container_id.clone(),
                    message: message.clone(),
                    inspect_hint: inspect_hint.clone(),
                },
                Some(RetainedArtifact::CreatedContainer {
                    machine_id: machine_id.clone(),
                    container_id: container_id.clone(),
                    inspect_hint: inspect_hint.clone(),
                }),
            ),
        };
        if let Some(retained) = retained
            && !retained_artifacts.contains(&retained)
        {
            retained_artifacts.push(retained);
        }
        DeployOperationFailure::PreStartHookFailed {
            machine_id: machine_id.clone(),
            failure,
            retained_artifacts,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployExecutionStep {
    RecordOperationEvent,
    EnsureEndpointNetwork { machine_id: MachineId },
    PrepareDataplane { machines: Vec<MachineId> },
    RunContainer { machine_id: MachineId },
    RunPreStartHook { machine_id: MachineId },
    WaitHealthy,
    CommitVolumePins,
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
            Self::RunPreStartHook { .. } => "run_pre_start_hook",
            Self::WaitHealthy => "wait_healthy",
            Self::CommitVolumePins => "commit_volume_pins",
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
            Self::RunPreStartHook { machine_id } => DeployOperationFailure::PreStartHookFailed {
                machine_id: machine_id.clone(),
                failure: PreStartHookFailure::RuntimeUnavailable {
                    message: timeout_failure_message("pre-start hook request", timeout),
                },
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
            Self::CommitVolumePins => DeployOperationFailure::ControlPlaneCommitFailed {
                scope: failure_commit_scope(command),
                message: timeout_failure_message("volume pin commit", timeout),
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
            Self::Plan(DeployPlanError::NoEligibleMachines) => {
                DeployOperationFailure::NoUsableMachines {
                    reasons: command.unusable_machines.clone(),
                }
            }
            Self::Plan(DeployPlanError::ConflictingVolumePins { .. }) => {
                DeployOperationFailure::PlanningFailed {
                    service_id: failure_service_id(command),
                    namespace_revision_id: failure_namespace_revision_id(command),
                    message: failure_message("volume-backed service has conflicting volume pins"),
                }
            }
            Self::Plan(DeployPlanError::UnknownServiceDependency {
                service_id,
                dependency,
            }) => DeployOperationFailure::PlanningFailed {
                service_id: service_id.clone(),
                namespace_revision_id: failure_namespace_revision_id(command),
                message: failure_message(format!(
                    "service depends on unknown service {}",
                    dependency.as_str()
                )),
            },
            Self::Plan(DeployPlanError::ServiceDependencyCycle { .. }) => {
                DeployOperationFailure::PlanningFailed {
                    service_id: failure_service_id(command),
                    namespace_revision_id: failure_namespace_revision_id(command),
                    message: failure_message("service dependencies contain a cycle"),
                }
            }
            Self::PlanInconsistent { service_id } => DeployOperationFailure::PlanningFailed {
                service_id: service_id.clone(),
                namespace_revision_id: failure_namespace_revision_id(command),
                message: failure_message("deploy plan is inconsistent with its execution command"),
            },
            Self::StepId(_) => DeployOperationFailure::PlanningFailed {
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
            Self::PreStartHook(error) => error.deploy_failure(retained_artifacts),
            Self::PreStartHookExited {
                machine_id,
                container_id,
                exit_code,
                message,
            } => {
                let retained = RetainedArtifact::StartedContainer {
                    machine_id: machine_id.clone(),
                    container_id: container_id.clone(),
                    log_hint: log_hint(container_id),
                };
                let mut retained_artifacts = retained_artifacts;
                if !retained_artifacts.contains(&retained) {
                    retained_artifacts.push(retained);
                }
                DeployOperationFailure::PreStartHookFailed {
                    machine_id: machine_id.clone(),
                    failure: PreStartHookFailure::Exited {
                        container_id: container_id.clone(),
                        exit_code: *exit_code,
                        message: message.clone(),
                        log_hint: log_hint(container_id),
                    },
                    retained_artifacts,
                }
            }
            Self::WaitHealthy(error) => error.deploy_failure(retained_artifacts),
            Self::CommitNamespaceState(error) => error.deploy_failure(retained_artifacts),
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

impl From<NamespaceCommitError> for DeployExecutionError {
    fn from(value: NamespaceCommitError) -> Self {
        Self::CommitNamespaceState(value)
    }
}

impl NamespaceCommitError {
    fn deploy_failure(&self, retained_artifacts: Vec<RetainedArtifact>) -> DeployOperationFailure {
        match self {
            Self::RouteStore { target, message } => DeployOperationFailure::RouteCutoverFailed {
                route: target.clone(),
                reason: RouteCutoverFailureReason::StateStoreFailed {
                    message: failure_message(format!(
                        "route binding state write failed: {message}"
                    )),
                },
                retained_artifacts,
            },
            Self::RouteLockLost { target } => DeployOperationFailure::RouteCutoverFailed {
                route: target.clone(),
                reason: RouteCutoverFailureReason::StateStoreFailed {
                    message: failure_message("namespace lock was lost before route cutover"),
                },
                retained_artifacts,
            },
            Self::ServingTargetStore { scope, message } => {
                DeployOperationFailure::ControlPlaneCommitFailed {
                    scope: scope.clone(),
                    message: failure_message(format!(
                        "serving target entry state could not be committed: {message}"
                    )),
                    retained_artifacts,
                }
            }
            Self::ServingTargetLockLost { scope } => {
                DeployOperationFailure::ControlPlaneCommitFailed {
                    scope: scope.clone(),
                    message: failure_message(
                        "namespace lock was lost before serving target commit",
                    ),
                    retained_artifacts,
                }
            }
            Self::VolumePinStore { state, message } => {
                DeployOperationFailure::ControlPlaneCommitFailed {
                    scope: ControlPlaneCommitScope::VolumePin {
                        namespace_id: state.namespace_id.clone(),
                        volume_name: state.volume_name.clone(),
                    },
                    message: failure_message(format!(
                        "volume pin state could not be committed: {message}"
                    )),
                    retained_artifacts,
                }
            }
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
    RestartContainerFailed {
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

                DeployOperationFailure::ContainerStartFailed {
                    machine_id: machine_id.clone(),
                    container_id: container_id.clone(),
                    message: message.clone(),
                    retained_artifacts,
                }
            }
            Self::ExistingContainerStartFailed {
                machine_id,
                container_id,
                message,
                ..
            }
            | Self::OperationStepContainerNotStartable {
                machine_id,
                container_id,
                message,
                ..
            } => DeployOperationFailure::ContainerStartFailed {
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
                message: message.clone(),
                retained_artifacts,
            },
            Self::RemoveContainerFailed {
                machine_id,
                message,
                ..
            }
            | Self::RestartContainerFailed {
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
        | DeployOperationFailure::ContainerStartFailed {
            retained_artifacts, ..
        }
        | DeployOperationFailure::PreStartHookFailed {
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
        | DeployOperationFailure::AutoDnsWithoutLease { .. }
        | DeployOperationFailure::NoUsableMachines { .. }
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
