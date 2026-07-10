//! The Deploy operation: a namespace manifest converges onto the cluster
//! through planning, staged execution, route cutover, and cleanup. States,
//! failures, transitions, evidence, and status projection live together
//! here.

use serde::{Deserialize, Serialize};

use crate::dataplane::{DataplaneProviderFailure, PloyzNativeMeshPrepareReport};
use crate::deploy::VolumeName;
use crate::deploy::{DeployCleanupContainer, DeployPlan};
use crate::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId,
    OperationId, ServiceId,
};
use crate::state::MachineUsabilityReason;

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, kind_mismatch,
};
use super::routes::RouteTarget;
use super::text::{CancellationReason, FailureMessage, OperatorHint};
use super::{EventSequence, OperationKind, OperationStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DeployRunningStage {
    PreparingDataplane,
    StartingContainers,
    WaitingForHealth,
    RouteCutover,
    ServingTargetCommit,
    RemovingSupersededContainers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DeployCompletionOutcome {
    Completed,
    CompletedWithWarnings,
    PartiallyCompleted,
    PartiallyCompletedWithWarnings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployOperationState {
    Accepted,
    Planning,
    Running { stage: DeployRunningStage },
    Completed { outcome: DeployCompletionOutcome },
    Failed { failure: DeployOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl DeployOperationState {
    #[must_use]
    pub const fn completed() -> Self {
        Self::Completed {
            outcome: DeployCompletionOutcome::Completed,
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted | Self::Planning | Self::Running { .. } => false,
        }
    }
}

/// One machine's placement rejection, carried on NoUsableMachines evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct UnusableMachine {
    pub machine_id: MachineId,
    pub reason: crate::state::MachineUsabilityReason,
}

/// What a failed control-plane commit was writing. Empty-manifest deploys
/// commit no service entry, so namespace-level record failures carry the
/// namespace revision id instead of a counterfeit entry digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlPlaneCommitScope {
    ServiceEntry {
        service_id: ServiceId,
        namespace_revision_entry_id: NamespaceRevisionEntryId,
    },
    Namespace {
        namespace_revision_id: NamespaceRevisionId,
    },
    VolumePin {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployFailureClass {
    PreconditionRejected,
    ImageResolvePullFailed,
    PreStartHookFailed,
    ContainerStartFailed,
    HealthGateFailed,
    MachineNoAnswer,
    Timeout,
    DataplanePrepareFailed,
    RuntimeUnavailable,
    ControlPlaneCommitFailed,
    RouteCutoverFailed,
}

impl DeployFailureClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreconditionRejected => "precondition-rejected",
            Self::ImageResolvePullFailed => "image-resolve-pull-failed",
            Self::PreStartHookFailed => "pre-start-hook-failed",
            Self::ContainerStartFailed => "container-start-failed",
            Self::HealthGateFailed => "health-gate-failed",
            Self::MachineNoAnswer => "machine-no-answer",
            Self::Timeout => "timeout",
            Self::DataplanePrepareFailed => "dataplane-prepare-failed",
            Self::RuntimeUnavailable => "runtime-unavailable",
            Self::ControlPlaneCommitFailed => "control-plane-commit-failed",
            Self::RouteCutoverFailed => "route-cutover-failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployOperationFailure {
    NoUsableMachines {
        /// Why each known machine was rejected for placement.
        reasons: Vec<UnusableMachine>,
    },
    PlanningFailed {
        service_id: ServiceId,
        namespace_revision_id: NamespaceRevisionId,
        message: FailureMessage,
    },
    AutoDnsWithoutLease {
        service_id: ServiceId,
        namespace_revision_id: NamespaceRevisionId,
        message: FailureMessage,
    },
    ArtifactUnavailable {
        service_id: ServiceId,
        namespace_revision_entry_id: NamespaceRevisionEntryId,
        reason: ArtifactUnavailableReason,
    },
    DataplaneUnavailable {
        machine_id: MachineId,
        provider_failure: DataplaneProviderFailure,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    DataplanePrepareTimedOut {
        machines: Vec<MachineId>,
        timeout_seconds: u32,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    DataplanePrepareInvalidReport {
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    RuntimeUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    ContainerStartFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    PreStartHookFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        exit_code: i64,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    HealthCheckFailed {
        health_check: HealthCheckFailure,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    ControlPlaneCommitFailed {
        scope: ControlPlaneCommitScope,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    RouteCutoverFailed {
        route: RouteTarget,
        reason: RouteCutoverFailureReason,
        retained_artifacts: Vec<RetainedArtifact>,
    },
}

impl DeployOperationFailure {
    #[must_use]
    pub fn failure_class(&self) -> DeployFailureClass {
        match self {
            Self::NoUsableMachines { reasons } => {
                if reasons
                    .iter()
                    .any(|reason| matches!(reason.reason, MachineUsabilityReason::FactsUnavailable))
                {
                    DeployFailureClass::MachineNoAnswer
                } else {
                    DeployFailureClass::PreconditionRejected
                }
            }
            Self::PlanningFailed { .. } | Self::AutoDnsWithoutLease { .. } => {
                DeployFailureClass::PreconditionRejected
            }
            Self::ArtifactUnavailable { .. } => DeployFailureClass::ImageResolvePullFailed,
            Self::DataplaneUnavailable { .. } | Self::DataplanePrepareInvalidReport { .. } => {
                DeployFailureClass::DataplanePrepareFailed
            }
            Self::DataplanePrepareTimedOut { .. } => DeployFailureClass::Timeout,
            Self::RuntimeUnavailable { .. } => DeployFailureClass::RuntimeUnavailable,
            Self::ContainerStartFailed { .. } => DeployFailureClass::ContainerStartFailed,
            Self::PreStartHookFailed { .. } => DeployFailureClass::PreStartHookFailed,
            Self::HealthCheckFailed { health_check, .. } => match health_check {
                HealthCheckFailure::ProbeFailed { .. } => DeployFailureClass::HealthGateFailed,
                HealthCheckFailure::TimedOut { .. } => DeployFailureClass::Timeout,
            },
            Self::ControlPlaneCommitFailed { .. } => DeployFailureClass::ControlPlaneCommitFailed,
            Self::RouteCutoverFailed { reason, .. } => match reason {
                RouteCutoverFailureReason::GatewayUnavailable { .. } => {
                    DeployFailureClass::MachineNoAnswer
                }
                RouteCutoverFailureReason::RouteRejected { .. }
                | RouteCutoverFailureReason::StateStoreFailed { .. } => {
                    DeployFailureClass::RouteCutoverFailed
                }
                RouteCutoverFailureReason::TimedOut { .. } => DeployFailureClass::Timeout,
            },
        }
    }

    /// The artifacts this failure retained for inspection; empty for failure
    /// classes that reject before any container work starts.
    #[must_use]
    pub fn retained_artifacts(&self) -> &[RetainedArtifact] {
        match self {
            Self::DataplaneUnavailable {
                retained_artifacts, ..
            }
            | Self::DataplanePrepareTimedOut {
                retained_artifacts, ..
            }
            | Self::DataplanePrepareInvalidReport {
                retained_artifacts, ..
            }
            | Self::RuntimeUnavailable {
                retained_artifacts, ..
            }
            | Self::ContainerStartFailed {
                retained_artifacts, ..
            }
            | Self::PreStartHookFailed {
                retained_artifacts, ..
            }
            | Self::HealthCheckFailed {
                retained_artifacts, ..
            }
            | Self::ControlPlaneCommitFailed {
                retained_artifacts, ..
            }
            | Self::RouteCutoverFailed {
                retained_artifacts, ..
            } => retained_artifacts,
            Self::NoUsableMachines { .. }
            | Self::PlanningFailed { .. }
            | Self::AutoDnsWithoutLease { .. }
            | Self::ArtifactUnavailable { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactUnavailableReason {
    BundleMissing,
    BundleUnreadable { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum HealthCheckFailure {
    ProbeFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
    TimedOut {
        timeout_seconds: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteCutoverFailureReason {
    GatewayUnavailable { machine_id: MachineId },
    RouteRejected { message: FailureMessage },
    StateStoreFailed { message: FailureMessage },
    TimedOut { timeout_seconds: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetainedArtifact {
    CreatedContainer {
        machine_id: MachineId,
        container_id: ContainerId,
        inspect_hint: OperatorHint,
    },
    StartedContainer {
        machine_id: MachineId,
        container_id: ContainerId,
        log_hint: OperatorHint,
    },
    ContainerStopFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

impl RetainedArtifact {
    /// Whether this artifact is a container the failed attempt left behind
    /// for inspection; false for records of a removal that failed on a
    /// pre-existing container.
    #[must_use]
    pub fn is_container(&self) -> bool {
        match self {
            RetainedArtifact::CreatedContainer {
                machine_id: _,
                container_id: _,
                inspect_hint: _,
            }
            | RetainedArtifact::StartedContainer {
                machine_id: _,
                container_id: _,
                log_hint: _,
            } => true,
            RetainedArtifact::ContainerStopFailed {
                machine_id: _,
                container_id: _,
                message: _,
                inspect_hint: _,
            } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployCleanupFailure {
    pub target: DeployCleanupContainer,
    pub message: FailureMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployTransition {
    Planning,
    Running { stage: DeployRunningStage },
    Completed { outcome: DeployCompletionOutcome },
    Failed { failure: DeployOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl DeployTransition {
    #[must_use]
    pub const fn completed() -> Self {
        Self::Completed {
            outcome: DeployCompletionOutcome::Completed,
        }
    }

    /// Renders this transition as the operation event it records.
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Planning => OperationEvent::DeployPlanningStarted {
                operation_id: operation_id.clone(),
            },
            Self::Running { stage } => OperationEvent::DeployRunning {
                operation_id: operation_id.clone(),
                stage: *stage,
            },
            Self::Completed { outcome } => OperationEvent::DeployCompleted {
                operation_id: operation_id.clone(),
                outcome: *outcome,
            },
            Self::Failed { failure } => OperationEvent::DeployFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::Deploy,
                reason: reason.clone(),
            },
        }
    }

    #[must_use]
    pub fn state(&self) -> DeployOperationState {
        match self {
            Self::Planning => DeployOperationState::Planning,
            Self::Running { stage } => DeployOperationState::Running { stage: *stage },
            Self::Completed { outcome } => DeployOperationState::Completed { outcome: *outcome },
            Self::Failed { failure } => DeployOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => DeployOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployEvidence {
    PlanCreated {
        plan: DeployPlan,
    },
    DataplanePrepared {
        report: PloyzNativeMeshPrepareReport,
    },
    ContainerStarted {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    HealthCheckStarted,
    CleanupFinished {
        removed: Vec<DeployCleanupContainer>,
        failed: Vec<DeployCleanupFailure>,
    },
}

impl DeployEvidence {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::PlanCreated { plan } => OperationEvent::DeployPlanCreated {
                operation_id: operation_id.clone(),
                plan: plan.clone(),
            },
            Self::DataplanePrepared { report } => OperationEvent::DeployDataplanePrepared {
                operation_id: operation_id.clone(),
                report: report.clone(),
            },
            Self::ContainerStarted {
                machine_id,
                container_id,
            } => OperationEvent::DeployContainerStarted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
            },
            Self::HealthCheckStarted => OperationEvent::DeployHealthCheckStarted {
                operation_id: operation_id.clone(),
            },
            Self::CleanupFinished { removed, failed } => OperationEvent::DeployCleanupFinished {
                operation_id: operation_id.clone(),
                removed: removed.clone(),
                failed: failed.clone(),
            },
        }
    }
}

/// Deploy events after classification from the flat [`OperationEvent`]
/// stream shape.
pub(super) enum DeployEvent {
    Submitted,
    Evidence(DeployEvidence),
    Transition(DeployTransition),
}

/// What a piece of deploy evidence requires of the operation state to count
/// as fresh. This is the single source of the evidence→stage mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceRequirement {
    Planning,
    RunningStage(DeployRunningStage),
    Cleanup,
}

const fn evidence_requirement(evidence: &DeployEvidence) -> EvidenceRequirement {
    match evidence {
        DeployEvidence::PlanCreated { .. } => EvidenceRequirement::Planning,
        DeployEvidence::DataplanePrepared { .. } => {
            EvidenceRequirement::RunningStage(DeployRunningStage::PreparingDataplane)
        }
        DeployEvidence::ContainerStarted { .. } => {
            EvidenceRequirement::RunningStage(DeployRunningStage::StartingContainers)
        }
        DeployEvidence::HealthCheckStarted => {
            EvidenceRequirement::RunningStage(DeployRunningStage::WaitingForHealth)
        }
        DeployEvidence::CleanupFinished { .. } => EvidenceRequirement::Cleanup,
    }
}

pub fn validate_fresh_deploy_evidence(
    current: &OperationStatus,
    evidence: &DeployEvidence,
) -> Result<(), StatusProjectionError> {
    let OperationStatus::Deploy { id, state, .. } = current else {
        return Err(kind_mismatch(current, OperationKind::Deploy));
    };
    let valid = match evidence_requirement(evidence) {
        EvidenceRequirement::Planning => matches!(state, DeployOperationState::Planning),
        EvidenceRequirement::RunningStage(stage) => {
            evidence_is_current_or_past_running_stage(state, stage)
        }
        EvidenceRequirement::Cleanup => cleanup_evidence_is_valid(state),
    };
    if valid {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: id.clone(),
        current: Box::new(ProjectionOperationState::Deploy(state.clone())),
        attempted: Box::new(ProjectionOperationState::Deploy(evidence_required_state(
            evidence,
        ))),
    })
}

fn evidence_is_current_or_past_running_stage(
    state: &DeployOperationState,
    evidence_stage: DeployRunningStage,
) -> bool {
    let DeployOperationState::Running { stage } = state else {
        return false;
    };

    stage_rank(*stage) >= stage_rank(evidence_stage)
}

fn cleanup_evidence_is_valid(state: &DeployOperationState) -> bool {
    matches!(
        state,
        DeployOperationState::Running {
            stage: DeployRunningStage::ServingTargetCommit
                | DeployRunningStage::RemovingSupersededContainers
        }
    )
}

fn evidence_required_state(evidence: &DeployEvidence) -> DeployOperationState {
    match evidence_requirement(evidence) {
        EvidenceRequirement::Planning => DeployOperationState::Planning,
        EvidenceRequirement::RunningStage(stage) => DeployOperationState::Running { stage },
        EvidenceRequirement::Cleanup => DeployOperationState::Running {
            stage: DeployRunningStage::RemovingSupersededContainers,
        },
    }
}

pub fn project_deploy_transition(
    current: &OperationStatus,
    transition: DeployTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let OperationStatus::Deploy {
        id,
        namespace_id,
        service_id,
        state: current_state,
        ..
    } = current
    else {
        return Err(kind_mismatch(current, OperationKind::Deploy));
    };

    project_state(
        id,
        namespace_id,
        service_id,
        current_state,
        transition.state(),
        event_sequence,
    )
}

pub(super) fn project_state(
    id: &OperationId,
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    current_state: &DeployOperationState,
    attempted: DeployOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if transition_satisfied(current_state, &attempted) {
        return Ok(OperationProjection::AlreadySatisfied);
    }

    validate_transition(id, current_state, &attempted)?;

    Ok(OperationProjection::StatusChanged {
        status: Box::new(OperationStatus::Deploy {
            id: id.clone(),
            namespace_id: namespace_id.clone(),
            service_id: service_id.clone(),
            state: attempted,
            last_event_sequence: event_sequence,
        }),
    })
}

pub(super) fn project_event(
    id: &OperationId,
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    state: &DeployOperationState,
    event: DeployEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        DeployEvent::Evidence(evidence) => {
            // Evidence records (advancing the status cursor without changing
            // state) once the operation has reached the phase that produces
            // it — including late arrivals after later stages or completion.
            // Evidence from a phase not yet reached is a stale duplicate and
            // is already satisfied.
            let records = match evidence_requirement(&evidence) {
                EvidenceRequirement::Planning => !matches!(state, DeployOperationState::Accepted),
                EvidenceRequirement::RunningStage(required) => {
                    evidence_is_current_or_past_running_stage(state, required)
                        || matches!(state, DeployOperationState::Completed { .. })
                }
                EvidenceRequirement::Cleanup => {
                    cleanup_evidence_is_valid(state)
                        || matches!(state, DeployOperationState::Completed { .. })
                }
            };
            if !records {
                return Ok(OperationProjection::AlreadySatisfied);
            }

            Ok(OperationProjection::StatusChanged {
                status: Box::new(evidence_status(
                    id,
                    namespace_id,
                    service_id,
                    state,
                    event_sequence,
                )),
            })
        }
        DeployEvent::Transition(transition) => project_state(
            id,
            namespace_id,
            service_id,
            state,
            transition.state(),
            event_sequence,
        ),
        DeployEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
    }
}

fn evidence_status(
    id: &OperationId,
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    state: &DeployOperationState,
    event_sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::Deploy {
        id: id.clone(),
        namespace_id: namespace_id.clone(),
        service_id: service_id.clone(),
        state: state.clone(),
        last_event_sequence: event_sequence,
    }
}

fn transition_satisfied(current: &DeployOperationState, attempted: &DeployOperationState) -> bool {
    match attempted {
        DeployOperationState::Accepted => matches!(current, DeployOperationState::Accepted),
        DeployOperationState::Planning => !matches!(current, DeployOperationState::Accepted),
        DeployOperationState::Running { stage: attempted } => match current {
            DeployOperationState::Running { stage: current } => {
                let current_rank = stage_rank(*current);
                let attempted_rank = stage_rank(*attempted);
                current_rank > attempted_rank
                    || current_rank == attempted_rank && current == attempted
            }
            DeployOperationState::Accepted
            | DeployOperationState::Planning
            | DeployOperationState::Completed { .. }
            | DeployOperationState::Failed { .. }
            | DeployOperationState::Cancelled { .. } => false,
        },
        DeployOperationState::Completed { outcome: attempted } => {
            matches!(current, DeployOperationState::Completed { outcome } if outcome == attempted)
        }
        DeployOperationState::Failed { failure: attempted } => {
            matches!(current, DeployOperationState::Failed { failure } if failure == attempted)
        }
        DeployOperationState::Cancelled { reason: attempted } => {
            matches!(current, DeployOperationState::Cancelled { reason } if reason == attempted)
        }
    }
}

fn validate_transition(
    operation_id: &OperationId,
    current: &DeployOperationState,
    attempted: &DeployOperationState,
) -> Result<(), StatusProjectionError> {
    if current.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: operation_id.clone(),
            current: Box::new(ProjectionOperationState::Deploy(current.clone())),
            attempted: Box::new(ProjectionOperationState::Deploy(attempted.clone())),
        });
    }

    if transition_allowed(current, attempted) {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(ProjectionOperationState::Deploy(current.clone())),
        attempted: Box::new(ProjectionOperationState::Deploy(attempted.clone())),
    })
}

fn transition_allowed(current: &DeployOperationState, attempted: &DeployOperationState) -> bool {
    match (current, attempted) {
        (DeployOperationState::Accepted, DeployOperationState::Planning)
        | (DeployOperationState::Accepted, DeployOperationState::Cancelled { .. })
        | (DeployOperationState::Accepted, DeployOperationState::Failed { .. })
        | (DeployOperationState::Planning, DeployOperationState::Cancelled { .. })
        | (DeployOperationState::Planning, DeployOperationState::Failed { .. })
        | (DeployOperationState::Running { .. }, DeployOperationState::Cancelled { .. })
        | (DeployOperationState::Running { .. }, DeployOperationState::Failed { .. }) => true,
        (
            DeployOperationState::Running {
                stage: DeployRunningStage::ServingTargetCommit,
            },
            DeployOperationState::Completed { .. },
        )
        | (
            DeployOperationState::Running {
                stage: DeployRunningStage::RemovingSupersededContainers,
            },
            DeployOperationState::Completed { .. },
        ) => true,
        (
            DeployOperationState::Planning,
            DeployOperationState::Running {
                stage: DeployRunningStage::PreparingDataplane,
            },
        ) => true,
        (
            DeployOperationState::Running { stage: current },
            DeployOperationState::Running { stage: attempted },
        ) => stage_is_next(*current, *attempted),
        (DeployOperationState::Accepted, _)
        | (DeployOperationState::Completed { .. }, _)
        | (DeployOperationState::Failed { .. }, _)
        | (DeployOperationState::Cancelled { .. }, _)
        | (DeployOperationState::Planning, _)
        | (DeployOperationState::Running { .. }, _) => false,
    }
}

fn stage_rank(stage: DeployRunningStage) -> u8 {
    match stage {
        DeployRunningStage::PreparingDataplane => 0,
        DeployRunningStage::StartingContainers => 1,
        DeployRunningStage::WaitingForHealth => 2,
        DeployRunningStage::RouteCutover => 3,
        DeployRunningStage::ServingTargetCommit => 4,
        DeployRunningStage::RemovingSupersededContainers => 5,
    }
}

fn stage_is_next(current: DeployRunningStage, attempted: DeployRunningStage) -> bool {
    matches!(
        (current, attempted),
        (
            DeployRunningStage::PreparingDataplane,
            DeployRunningStage::StartingContainers
        ) | (
            DeployRunningStage::StartingContainers,
            DeployRunningStage::WaitingForHealth
        ) | (
            DeployRunningStage::WaitingForHealth,
            DeployRunningStage::RouteCutover
        ) | (
            DeployRunningStage::RouteCutover,
            DeployRunningStage::ServingTargetCommit
        ) | (
            DeployRunningStage::ServingTargetCommit,
            DeployRunningStage::RemovingSupersededContainers
        ) | (
            DeployRunningStage::WaitingForHealth,
            DeployRunningStage::ServingTargetCommit
        )
    )
}
