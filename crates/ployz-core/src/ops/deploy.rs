//! The Deploy operation: a namespace manifest converges onto the cluster
//! through planning, staged execution, route cutover, and cleanup. States,
//! failures, transitions, evidence, and status projection live together
//! here.

use serde::{Deserialize, Serialize};

use crate::dataplane::{DataplaneProviderFailure, PloyzNativeMeshPrepareReport};
use crate::deploy::{DeployCleanupContainer, DeployPlan};
use crate::ids::{
    ContainerId, MachineId, NamespaceRevisionEntryId, NamespaceRevisionId, OperationId, ServiceId,
};

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
        attempted: Box::new(ProjectionOperationState::Deploy(
            deploy_evidence_required_state(evidence),
        )),
    })
}

fn evidence_is_current_or_past_running_stage(
    state: &DeployOperationState,
    evidence_stage: DeployRunningStage,
) -> bool {
    let DeployOperationState::Running { stage } = state else {
        return false;
    };

    deploy_stage_rank(*stage) >= deploy_stage_rank(evidence_stage)
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

fn deploy_evidence_required_state(evidence: &DeployEvidence) -> DeployOperationState {
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
        service_id,
        state: current_state,
        ..
    } = current
    else {
        return Err(kind_mismatch(current, OperationKind::Deploy));
    };

    project_deploy_state(
        id,
        service_id,
        current_state,
        transition.state(),
        event_sequence,
    )
}

pub(super) fn project_deploy_state(
    id: &OperationId,
    service_id: &ServiceId,
    current_state: &DeployOperationState,
    attempted: DeployOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if deploy_transition_satisfied(current_state, &attempted) {
        return Ok(OperationProjection::AlreadySatisfied);
    }

    validate_deploy_transition(id, current_state, &attempted)?;

    Ok(OperationProjection::StatusChanged {
        status: Box::new(OperationStatus::Deploy {
            id: id.clone(),
            service_id: service_id.clone(),
            state: attempted,
            last_event_sequence: event_sequence,
        }),
    })
}

pub(super) fn project_deploy_event(
    id: &OperationId,
    service_id: &ServiceId,
    state: &DeployOperationState,
    event: DeployEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        DeployEvent::Evidence(evidence) => {
            let requirement = evidence_requirement(&evidence);
            let fresh = match requirement {
                EvidenceRequirement::Planning => {
                    matches!(state, DeployOperationState::Planning)
                }
                EvidenceRequirement::RunningStage(required) => matches!(
                    state,
                    DeployOperationState::Running { stage } if *stage == required
                ),
                EvidenceRequirement::Cleanup => cleanup_evidence_is_valid(state),
            };
            if fresh {
                return Ok(OperationProjection::StatusChanged {
                    status: Box::new(evidence_status(id, service_id, state, event_sequence)),
                });
            }

            let satisfied = match requirement {
                EvidenceRequirement::Planning => !matches!(state, DeployOperationState::Accepted),
                EvidenceRequirement::RunningStage(required) => {
                    evidence_is_satisfied_after_stage(state, required)
                }
                EvidenceRequirement::Cleanup => evidence_is_satisfied_after_stage(
                    state,
                    DeployRunningStage::RemovingSupersededContainers,
                ),
            };
            if !satisfied {
                return Ok(OperationProjection::AlreadySatisfied);
            }

            Ok(OperationProjection::StatusChanged {
                status: Box::new(evidence_status(id, service_id, state, event_sequence)),
            })
        }
        DeployEvent::Transition(transition) => {
            project_deploy_state(id, service_id, state, transition.state(), event_sequence)
        }
        DeployEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
    }
}

fn evidence_is_satisfied_after_stage(
    state: &DeployOperationState,
    evidence_stage: DeployRunningStage,
) -> bool {
    match state {
        DeployOperationState::Running { stage } => {
            deploy_stage_rank(*stage) > deploy_stage_rank(evidence_stage)
        }
        DeployOperationState::Completed { .. } => true,
        DeployOperationState::Accepted
        | DeployOperationState::Planning
        | DeployOperationState::Failed { .. }
        | DeployOperationState::Cancelled { .. } => false,
    }
}

fn evidence_status(
    id: &OperationId,
    service_id: &ServiceId,
    state: &DeployOperationState,
    event_sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::Deploy {
        id: id.clone(),
        service_id: service_id.clone(),
        state: state.clone(),
        last_event_sequence: event_sequence,
    }
}

fn deploy_transition_satisfied(
    current: &DeployOperationState,
    attempted: &DeployOperationState,
) -> bool {
    match attempted {
        DeployOperationState::Accepted => matches!(current, DeployOperationState::Accepted),
        DeployOperationState::Planning => !matches!(current, DeployOperationState::Accepted),
        DeployOperationState::Running { stage: attempted } => match current {
            DeployOperationState::Running { stage: current } => {
                let current_rank = deploy_stage_rank(*current);
                let attempted_rank = deploy_stage_rank(*attempted);
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

pub fn validate_deploy_transition(
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

    if deploy_transition_allowed(current, attempted) {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(ProjectionOperationState::Deploy(current.clone())),
        attempted: Box::new(ProjectionOperationState::Deploy(attempted.clone())),
    })
}

fn deploy_transition_allowed(
    current: &DeployOperationState,
    attempted: &DeployOperationState,
) -> bool {
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
        ) => deploy_stage_is_next(*current, *attempted),
        (DeployOperationState::Accepted, _)
        | (DeployOperationState::Completed { .. }, _)
        | (DeployOperationState::Failed { .. }, _)
        | (DeployOperationState::Cancelled { .. }, _)
        | (DeployOperationState::Planning, _)
        | (DeployOperationState::Running { .. }, _) => false,
    }
}

fn deploy_stage_rank(stage: DeployRunningStage) -> u8 {
    match stage {
        DeployRunningStage::PreparingDataplane => 0,
        DeployRunningStage::StartingContainers => 1,
        DeployRunningStage::WaitingForHealth => 2,
        DeployRunningStage::RouteCutover => 3,
        DeployRunningStage::ServingTargetCommit => 4,
        DeployRunningStage::RemovingSupersededContainers => 5,
    }
}

fn deploy_stage_is_next(current: DeployRunningStage, attempted: DeployRunningStage) -> bool {
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
