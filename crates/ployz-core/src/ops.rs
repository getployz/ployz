//! Operation state, events, and status models.

use serde::{Deserialize, Serialize};

use crate::cert::{ActiveCertState, CertBundleRef, CertValidityWindow};
use crate::dataplane::{DataplaneProviderFailure, PloyzNativeMeshPrepareReport};
use crate::deploy::{DeployCleanupContainer, DeployPlan};
use crate::ids::{
    CertId, ContainerId, MachineId, NamespaceRevisionEntryId, NamespaceRevisionId, OperationId,
    ServiceId, SubjectToken, SubjectTokenError,
};
use crate::install::InstallArtifactVersion;
use crate::machine::{IssuedJoinToken, MachineAddOperationState, MachineName};
use crate::roles::InstallRolePolicy;
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

mod accessors;
mod classification;
mod events;
mod projection;
mod replay;
mod routes;
mod text;

pub use classification::OperationSubjectRef;
pub use events::{OperationEvent, OperationSubject};
pub use projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_cert_transition,
    project_deploy_transition, project_operation_event, validate_cert_transition,
    validate_deploy_transition, validate_fresh_deploy_evidence,
};
pub use replay::{
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayLimitError,
    OperationEventReplayPage, OperationEventReplayRequest, ReplayedOperationEvent,
};
pub use routes::{RouteHostname, RouteHostnameError, RoutePort, RoutePortError, RouteTarget};
pub use text::{CancellationReason, FailureMessage, NonEmptyTextError, OperatorHint};

pub const MAX_OPERATION_EVENT_REPLAY_LIMIT: u16 = 512;

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

impl DeployCompletionOutcome {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CertRunningStage {
    ChallengePublished,
    ValidationStarted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Deploy,
    Cert,
    MachineAdd,
    MachineUpdate,
    MachineLifecycle,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertOperationState {
    Accepted,
    Running { stage: CertRunningStage },
    Completed,
    Failed { failure: CertOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl CertOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted | Self::Running { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUpdateOperationState {
    Accepted,
    Running,
    Completed { reported: MachineSubstrateVersions },
    Failed { failure: MachineUpdateFailure },
    Cancelled { reason: CancellationReason },
}

impl MachineUpdateOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted | Self::Running => false,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineLifecycleOperationState {
    Accepted,
    Completed,
    Failed { failure: MachineLifecycleFailure },
    Cancelled { reason: CancellationReason },
}

impl MachineLifecycleOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineLifecycleFailure {
    NoSuchMachine { machine_id: MachineId },
    EvidenceWriteFailed { message: FailureMessage },
    StateCommitFailed { message: FailureMessage },
}

/// Worker-recorded terminal outcomes. Cancellation is not one: it arrives
/// through the generic [`OperationEvent::Cancelled`] path, never from the
/// lifecycle worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineLifecycleTransition {
    Completed,
    Failed { failure: MachineLifecycleFailure },
}

impl MachineLifecycleTransition {
    #[must_use]
    pub fn state(&self) -> MachineLifecycleOperationState {
        match self {
            Self::Completed => MachineLifecycleOperationState::Completed,
            Self::Failed { failure } => MachineLifecycleOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }

    /// Renders this transition as the operation event it records.
    #[must_use]
    pub fn event(&self, operation_id: &OperationId, machine_id: &MachineId) -> OperationEvent {
        match self {
            Self::Completed => OperationEvent::MachineLifecycleCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::MachineLifecycleFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure: failure.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineSubstrateVersions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ployzd: Option<InstallArtifactVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keeper: Option<InstallArtifactVersion>,
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertOperationFailure {
    ChallengePublishFailed {
        cert_id: CertId,
        message: FailureMessage,
    },
    AcmeValidationFailed {
        cert_id: CertId,
        message: FailureMessage,
        retained_active_cert: Option<ActiveCertState>,
    },
    ActiveCertCommitFailed {
        cert_id: CertId,
        bundle_ref: CertBundleRef,
        validity: CertValidityWindow,
        message: FailureMessage,
        retained_active_cert: Option<ActiveCertState>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUpdateFailure {
    MachineUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    UpdateRejected {
        machine_id: MachineId,
        message: FailureMessage,
    },
    VersionNotReported {
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
        reported: MachineSubstrateVersions,
    },
    StateCommitFailed {
        machine_id: MachineId,
        message: FailureMessage,
    },
}

impl CertOperationFailure {
    #[must_use]
    pub const fn cert_id(&self) -> &CertId {
        match self {
            Self::ChallengePublishFailed { cert_id, .. }
            | Self::AcmeValidationFailed { cert_id, .. }
            | Self::ActiveCertCommitFailed { cert_id, .. } => cert_id,
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

/// Persisted `KV_OPS.status.*` value.
///
/// Changing this shape intentionally breaks operation status recovery unless
/// paired with KV cleanup or migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationStatus {
    Deploy {
        id: OperationId,
        service_id: ServiceId,
        state: DeployOperationState,
        last_event_sequence: EventSequence,
    },
    Cert {
        id: OperationId,
        cert_id: CertId,
        state: CertOperationState,
        last_event_sequence: EventSequence,
    },
    MachineAdd {
        id: OperationId,
        machine_id: MachineId,
        name: MachineName,
        roles: InstallRolePolicy,
        state: MachineAddOperationState,
        last_event_sequence: EventSequence,
    },
    MachineUpdate {
        id: OperationId,
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
        state: MachineUpdateOperationState,
        last_event_sequence: EventSequence,
    },
    MachineLifecycle {
        id: OperationId,
        machine_id: MachineId,
        target: crate::state::MachineLifecycle,
        state: MachineLifecycleOperationState,
        last_event_sequence: EventSequence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct OperationStatusSnapshot {
    pub status: OperationStatus,
}

impl OperationStatusSnapshot {
    #[must_use]
    pub fn new(status: OperationStatus) -> Self {
        Self { status }
    }
}

impl OperationStatus {
    #[must_use]
    pub fn deploy_accepted(
        id: OperationId,
        service_id: ServiceId,
        event_sequence: EventSequence,
    ) -> Self {
        Self::Deploy {
            id,
            service_id,
            state: DeployOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn cert_accepted(id: OperationId, cert_id: CertId, event_sequence: EventSequence) -> Self {
        Self::Cert {
            id,
            cert_id,
            state: CertOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn machine_add_pending(
        id: OperationId,
        machine_id: MachineId,
        name: MachineName,
        roles: InstallRolePolicy,
        join_token: IssuedJoinToken,
        event_sequence: EventSequence,
    ) -> Self {
        Self::MachineAdd {
            id,
            machine_id,
            name,
            roles,
            state: MachineAddOperationState::Pending { join_token },
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn machine_update_accepted(
        id: OperationId,
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
        event_sequence: EventSequence,
    ) -> Self {
        Self::MachineUpdate {
            id,
            machine_id,
            target_version,
            state: MachineUpdateOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn machine_lifecycle_accepted(
        id: OperationId,
        machine_id: MachineId,
        target: crate::state::MachineLifecycle,
        event_sequence: EventSequence,
    ) -> Self {
        Self::MachineLifecycle {
            id,
            machine_id,
            target,
            state: MachineLifecycleOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Deploy { state, .. } => state.is_terminal(),
            Self::Cert { state, .. } => state.is_terminal(),
            Self::MachineAdd { state, .. } => state.is_terminal(),
            Self::MachineUpdate { state, .. } => state.is_terminal(),
            Self::MachineLifecycle { state, .. } => state.is_terminal(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineUpdateTransition {
    Running,
    Completed { reported: MachineSubstrateVersions },
    Failed { failure: MachineUpdateFailure },
    Cancelled { reason: CancellationReason },
}

impl MachineUpdateTransition {
    #[must_use]
    pub fn state(&self) -> MachineUpdateOperationState {
        match self {
            Self::Running => MachineUpdateOperationState::Running,
            Self::Completed { reported } => MachineUpdateOperationState::Completed {
                reported: reported.clone(),
            },
            Self::Failed { failure } => MachineUpdateOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => MachineUpdateOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
    /// Renders this transition as the operation event it records.
    #[must_use]
    pub fn event(&self, operation_id: &OperationId, machine_id: &MachineId) -> OperationEvent {
        match self {
            Self::Running => OperationEvent::MachineUpdateRunning {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
            Self::Completed { reported } => OperationEvent::MachineUpdateCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                reported: reported.clone(),
            },
            Self::Failed { failure } => OperationEvent::MachineUpdateFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::MachineUpdate,
                reason: reason.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployTransition {
    Planning,
    Running { stage: DeployRunningStage },
    Completed { outcome: DeployCompletionOutcome },
    Failed { failure: DeployOperationFailure },
    Cancelled { reason: CancellationReason },
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployCleanupFailure {
    pub target: DeployCleanupContainer,
    pub message: FailureMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertTransition {
    Running { stage: CertRunningStage },
    Completed,
    Failed { failure: CertOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl CertTransition {
    #[must_use]
    pub fn state(&self) -> CertOperationState {
        match self {
            Self::Running { stage } => CertOperationState::Running { stage: *stage },
            Self::Completed => CertOperationState::Completed,
            Self::Failed { failure } => CertOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => CertOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"OperationIdempotencyKey\">")
)]
#[serde(transparent)]
pub struct OperationIdempotencyKey(SubjectToken);

impl OperationIdempotencyKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

positive_u64_wire_newtype! {
    pub struct EventSequence;
    ts_brand: "Brand<string, \"EventSequence\">";
    accessor: get;
    error: EventSequenceError;
}

positive_u64_wire_error! {
    pub enum EventSequenceError;
    noun: "event sequence";
}
