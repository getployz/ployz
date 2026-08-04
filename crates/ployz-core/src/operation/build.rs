//! Build operation states, typed failures, evidence, and projection.

use serde::{Deserialize, Serialize};

use crate::build::{
    BuildAdapter, BuildExecutorAssignments, BuildExecutorEvidence, BuildPlatforms,
    BuildSourceEvidence, BuildTarget,
};
use crate::deploy::PushedImageReceipt;
use crate::ids::{MachineId, OperationId};
use crate::image::OciPlatform;
use crate::install::{InstallArtifactVersion, InstallSha256Digest};

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
};
use super::{
    BuildInterruptionStage, CancellationReason, EventSequence, FailureMessage,
    OperationInterruptionCause, OperationInterruptionEvidence, OperationInterruptionStage,
    OperationStatus, UnusableMachine,
};

mod evidence;
pub use evidence::BuildEvidence;

pub const MAX_BUILD_LOG_CHUNK_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildOperationState {
    Accepted,
    Placing,
    Building,
    Completed {
        receipt: PushedImageReceipt,
    },
    Failed {
        failure: BuildOperationFailure,
    },
    Cancelled {
        reason: CancellationReason,
        cleanup: BuildCleanupEvidence,
    },
    TimedOut {
        failure: BuildTimeoutFailure,
        cleanup: BuildCleanupEvidence,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

/// A build status whose executor provenance has been checked against the
/// admitted target, platforms, and current phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(try_from = "BuildOperationStatusWire", deny_unknown_fields)]
pub struct BuildOperationStatus {
    id: OperationId,
    target: BuildTarget,
    source: BuildSourceEvidence,
    adapter: BuildAdapter,
    platforms: BuildPlatforms,
    executor_assignments: BuildExecutorAssignments,
    state: BuildOperationState,
    last_event_sequence: EventSequence,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BuildOperationStatusWire {
    pub(super) id: OperationId,
    pub(super) target: BuildTarget,
    pub(super) source: BuildSourceEvidence,
    pub(super) adapter: BuildAdapter,
    pub(super) platforms: BuildPlatforms,
    pub(super) executor_assignments: BuildExecutorAssignments,
    pub(super) state: BuildOperationState,
    pub(super) last_event_sequence: EventSequence,
}

impl TryFrom<BuildOperationStatusWire> for BuildOperationStatus {
    type Error = BuildOperationStatusError;

    fn try_from(wire: BuildOperationStatusWire) -> Result<Self, Self::Error> {
        let status = Self {
            id: wire.id,
            target: wire.target,
            source: wire.source,
            adapter: wire.adapter,
            platforms: wire.platforms,
            executor_assignments: wire.executor_assignments,
            state: wire.state,
            last_event_sequence: wire.last_event_sequence,
        };
        if status.is_valid() {
            Ok(status)
        } else {
            Err(BuildOperationStatusError)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("build status executor provenance does not match its contract or phase")]
pub(super) struct BuildOperationStatusError;

impl BuildOperationStatus {
    pub(super) fn new(wire: BuildOperationStatusWire) -> Self {
        let status = Self {
            id: wire.id,
            target: wire.target,
            source: wire.source,
            adapter: wire.adapter,
            platforms: wire.platforms,
            executor_assignments: wire.executor_assignments,
            state: wire.state,
            last_event_sequence: wire.last_event_sequence,
        };
        assert!(
            status.is_valid(),
            "build status constructor received contradictory provenance"
        );
        status
    }

    fn is_valid(&self) -> bool {
        if !self
            .executor_assignments
            .matches_contract(&self.target, &self.platforms)
        {
            return false;
        }
        match &self.state {
            BuildOperationState::Accepted => self.executor_assignments.is_empty(),
            BuildOperationState::Placing => true,
            BuildOperationState::Building => self.executor_assignments.is_complete(&self.platforms),
            BuildOperationState::Completed { .. }
            | BuildOperationState::Failed { .. }
            | BuildOperationState::Cancelled { .. }
            | BuildOperationState::TimedOut { .. } => {
                self.executor_assignments
                    .terminal_provenance_matches(&self.state)
                    && (!matches!(self.state, BuildOperationState::Completed { .. })
                        || self.executor_assignments.is_complete(&self.platforms))
            }
            BuildOperationState::Interrupted { evidence } => match evidence.last_durable_stage() {
                super::OperationInterruptionStage::Build {
                    stage: super::BuildInterruptionStage::Accepted,
                } => self.executor_assignments.is_empty(),
                super::OperationInterruptionStage::Build {
                    stage: super::BuildInterruptionStage::Placing,
                } => true,
                super::OperationInterruptionStage::Build {
                    stage: super::BuildInterruptionStage::Building,
                } => self.executor_assignments.is_complete(&self.platforms),
                super::OperationInterruptionStage::Deploy { .. }
                | super::OperationInterruptionStage::IngressConfigureAccepted
                | super::OperationInterruptionStage::MachineUpdateAccepted
                | super::OperationInterruptionStage::MachineUpdateRunning
                | super::OperationInterruptionStage::MachineStoragePrepareAccepted
                | super::OperationInterruptionStage::MachineStoragePreparePreparing
                | super::OperationInterruptionStage::MachineBuildCachePruneAccepted
                | super::OperationInterruptionStage::MachineBuildCachePrunePruning
                | super::OperationInterruptionStage::MachineLifecycleAccepted
                | super::OperationInterruptionStage::NetworkRepairAccepted
                | super::OperationInterruptionStage::NetworkRepairRunning { .. }
                | super::OperationInterruptionStage::ServiceRestartAccepted
                | super::OperationInterruptionStage::ServiceRestartRunning { .. }
                | super::OperationInterruptionStage::NamespaceRemoveAccepted
                | super::OperationInterruptionStage::NamespaceRemoveRunning { .. }
                | super::OperationInterruptionStage::VolumeRemoveAccepted
                | super::OperationInterruptionStage::VolumeRemoveRunning { .. }
                | super::OperationInterruptionStage::VolumeCreateAccepted
                | super::OperationInterruptionStage::VolumeCreatePlanning
                | super::OperationInterruptionStage::VolumeCreateRunning { .. } => false,
            },
        }
    }

    #[must_use]
    pub const fn id(&self) -> &OperationId {
        &self.id
    }

    #[must_use]
    pub const fn target(&self) -> &BuildTarget {
        &self.target
    }

    #[must_use]
    pub const fn source(&self) -> &BuildSourceEvidence {
        &self.source
    }

    #[must_use]
    pub const fn adapter(&self) -> &BuildAdapter {
        &self.adapter
    }

    #[must_use]
    pub const fn platforms(&self) -> &BuildPlatforms {
        &self.platforms
    }

    #[must_use]
    pub const fn executor_assignments(&self) -> &BuildExecutorAssignments {
        &self.executor_assignments
    }

    #[must_use]
    pub const fn state(&self) -> &BuildOperationState {
        &self.state
    }

    #[must_use]
    pub const fn last_event_sequence(&self) -> EventSequence {
        self.last_event_sequence
    }

    pub(super) fn interrupt(
        &mut self,
        evidence: OperationInterruptionEvidence,
        event_sequence: EventSequence,
    ) {
        self.state = BuildOperationState::interrupted(evidence);
        self.last_event_sequence = event_sequence;
        assert!(
            self.is_valid(),
            "build interruption produced contradictory provenance"
        );
    }
}

impl BuildOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. }
                | Self::Failed { .. }
                | Self::Cancelled { .. }
                | Self::TimedOut { .. }
                | Self::Interrupted { .. }
        )
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        let stage = match self {
            Self::Accepted => BuildInterruptionStage::Accepted,
            Self::Placing => BuildInterruptionStage::Placing,
            Self::Building => BuildInterruptionStage::Building,
            Self::Completed { .. }
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::TimedOut { .. }
            | Self::Interrupted { .. } => return None,
        };
        Some(OperationInterruptionEvidence::new(
            cause,
            OperationInterruptionStage::Build { stage },
        ))
    }

    pub(super) const fn interrupted(evidence: OperationInterruptionEvidence) -> Self {
        Self::Interrupted { evidence }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildOperationFailure {
    NoEligibleMachine {
        platform: OciPlatform,
        unusable: Vec<UnusableMachine>,
    },
    PlatformFailed {
        platform: OciPlatform,
        machine_id: MachineId,
        failure: BuildPlatformFailure,
    },
    ExternalPlatformFailed {
        platform: OciPlatform,
        executor: BuildExecutorEvidence,
        failure: BuildPlatformFailure,
    },
    ReceiptAssemblyFailed {
        message: FailureMessage,
    },
    EvidenceRecordingFailed {
        message: FailureMessage,
    },
    ControlUnavailable {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildPlatformFailure {
    MachineUnavailable {
        message: FailureMessage,
    },
    ExecutorUnavailable {
        message: FailureMessage,
    },
    ImageSeedUnavailable {
        image_seed: MachineId,
    },
    BuildkitDigestMismatch {
        expected: crate::image::OciDigest,
        actual: crate::image::OciDigest,
    },
    HelperDigestMismatch {
        expected: InstallSha256Digest,
        actual: InstallSha256Digest,
    },
    FrontendDigestMismatch {
        expected: crate::image::OciDigest,
        actual: crate::image::OciDigest,
    },
    PlatformMismatch {
        expected: OciPlatform,
        actual: OciPlatform,
    },
    InsufficientHostDisk {
        available_bytes: u64,
        required_free_bytes: u64,
    },
    SourceFetchFailed {
        message: FailureMessage,
    },
    AdapterFailed {
        message: FailureMessage,
    },
    ImagePushFailed {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildToolchainEvidence {
    pub buildkit_image: crate::image::OciDigest,
    pub adapter: BuildAdapterToolchainEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "adapter", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildAdapterToolchainEvidence {
    Dockerfile,
    Railpack {
        helper_version: InstallArtifactVersion,
        helper_sha256: InstallSha256Digest,
        frontend_image: crate::image::OciDigest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildCleanupEvidence {
    NotRequired,
    Completed {
        machine_ids: Vec<MachineId>,
    },
    Unconfirmed {
        machine_ids: Vec<MachineId>,
    },
    ExternalCompleted {
        executors: Vec<BuildExecutorEvidence>,
    },
    ExternalUnconfirmed {
        executors: Vec<BuildExecutorEvidence>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildTimeoutFailure {
    DeadlineExceeded { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(try_from = "String", into = "String")]
pub struct BuildLogChunk(String);
impl BuildLogChunk {
    pub fn try_new(value: impl Into<String>) -> Result<Self, BuildLogChunkError> {
        let value = value.into();
        if value.is_empty() {
            return Err(BuildLogChunkError::Empty);
        }
        if value.len() > MAX_BUILD_LOG_CHUNK_BYTES {
            return Err(BuildLogChunkError::TooLarge {
                actual: value.len(),
                maximum: MAX_BUILD_LOG_CHUNK_BYTES,
            });
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for BuildLogChunk {
    type Error = BuildLogChunkError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}
impl From<BuildLogChunk> for String {
    fn from(value: BuildLogChunk) -> Self {
        value.0
    }
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildLogChunkError {
    #[error("build log chunk must not be empty")]
    Empty,
    #[error("build log chunk is {actual} bytes; maximum is {maximum}")]
    TooLarge { actual: usize, maximum: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildTransition {
    Placing,
    Building,
    Completed {
        receipt: PushedImageReceipt,
    },
    Failed {
        failure: BuildOperationFailure,
    },
    Cancelled {
        reason: CancellationReason,
        cleanup: BuildCleanupEvidence,
    },
    TimedOut {
        failure: BuildTimeoutFailure,
        cleanup: BuildCleanupEvidence,
    },
}
impl BuildTransition {
    #[must_use]
    pub fn state(&self) -> BuildOperationState {
        match self {
            Self::Placing => BuildOperationState::Placing,
            Self::Building => BuildOperationState::Building,
            Self::Completed { receipt } => BuildOperationState::Completed {
                receipt: receipt.clone(),
            },
            Self::Failed { failure } => BuildOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason, cleanup } => BuildOperationState::Cancelled {
                reason: reason.clone(),
                cleanup: cleanup.clone(),
            },
            Self::TimedOut { failure, cleanup } => BuildOperationState::TimedOut {
                failure: failure.clone(),
                cleanup: cleanup.clone(),
            },
        }
    }
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Placing => OperationEvent::BuildPlacementStarted {
                operation_id: operation_id.clone(),
            },
            Self::Building => OperationEvent::BuildRunning {
                operation_id: operation_id.clone(),
            },
            Self::Completed { receipt } => OperationEvent::BuildCompleted {
                operation_id: operation_id.clone(),
                receipt: receipt.clone(),
            },
            Self::Failed { failure } => OperationEvent::BuildFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason, cleanup } => OperationEvent::BuildCancelled {
                operation_id: operation_id.clone(),
                reason: reason.clone(),
                cleanup: cleanup.clone(),
            },
            Self::TimedOut { failure, cleanup } => OperationEvent::BuildTimedOut {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
                cleanup: cleanup.clone(),
            },
        }
    }
}

pub(super) enum BuildEvent {
    Submitted {
        target: BuildTarget,
        source: BuildSourceEvidence,
        adapter: BuildAdapter,
        platforms: BuildPlatforms,
    },
    Evidence(BuildEvidence),
    Transition(BuildTransition),
}

pub(super) struct BuildFields<'a> {
    pub id: &'a OperationId,
    pub target: &'a BuildTarget,
    pub source: &'a BuildSourceEvidence,
    pub adapter: &'a BuildAdapter,
    pub platforms: &'a BuildPlatforms,
    pub executor_assignments: &'a BuildExecutorAssignments,
    pub state: &'a BuildOperationState,
}

pub(super) fn project_event(
    fields: BuildFields<'_>,
    event: BuildEvent,
    sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        BuildEvent::Submitted {
            target,
            source,
            adapter,
            platforms,
        } => {
            if fields.target != &target
                || fields.source != &source
                || fields.adapter != &adapter
                || fields.platforms != &platforms
            {
                return Err(invalid_transition(&fields));
            }
            Ok(OperationProjection::AlreadySatisfied)
        }
        BuildEvent::Evidence(evidence) => evidence::project(&fields, evidence, sequence),
        BuildEvent::Transition(transition) => {
            let attempted = transition.state();
            if matches!(attempted, BuildOperationState::Building)
                && !fields.executor_assignments.is_complete(fields.platforms)
            {
                return Err(invalid_transition(&fields));
            }
            if attempted.is_terminal()
                && !fields
                    .executor_assignments
                    .terminal_provenance_matches(&attempted)
            {
                return Err(invalid_transition(&fields));
            }
            project_transition(
                fields.id,
                fields.state,
                attempted,
                BuildOperationState::is_terminal,
                transition_allowed,
                ProjectionOperationState::Build,
                |state| {
                    status(
                        &fields,
                        state,
                        fields.executor_assignments.clone(),
                        sequence,
                    )
                },
            )
        }
    }
}

impl BuildExecutorAssignments {
    fn terminal_provenance_matches(&self, attempted: &BuildOperationState) -> bool {
        match attempted {
            BuildOperationState::Completed { receipt } => {
                receipt.platforms().len() == self.len()
                    && self.iter().all(|assignment| {
                        receipt
                            .platform(&assignment.platform)
                            .is_some_and(|image| image.seed == *assignment.executor.image_seed())
                    })
            }
            BuildOperationState::Failed {
                failure:
                    BuildOperationFailure::PlatformFailed {
                        platform,
                        machine_id,
                        failure: _,
                    },
            } => self.iter().any(|assignment| {
                assignment.platform == *platform
                    && matches!(
                        &assignment.executor,
                        crate::build::BuildExecutorAssignment::Cluster {
                            machine_id: assigned_machine_id,
                        } if assigned_machine_id == machine_id
                    )
            }),
            BuildOperationState::Failed {
                failure:
                    BuildOperationFailure::ExternalPlatformFailed {
                        platform,
                        executor,
                        failure: _,
                    },
            } => self.iter().any(|assignment| {
                assignment.platform == *platform
                    && matches!(
                        executor.assignment(),
                        crate::build::BuildExecutorAssignment::External { .. }
                    )
                    && assignment.executor == *executor.assignment()
            }),
            BuildOperationState::Cancelled { cleanup, .. }
            | BuildOperationState::TimedOut { cleanup, .. } => match cleanup {
                BuildCleanupEvidence::NotRequired => true,
                BuildCleanupEvidence::Completed { machine_ids }
                | BuildCleanupEvidence::Unconfirmed { machine_ids } => {
                    machine_ids.iter().all(|machine_id| {
                        self.iter().any(|assignment| {
                            matches!(
                                &assignment.executor,
                                crate::build::BuildExecutorAssignment::Cluster {
                                    machine_id: assigned_machine_id,
                                } if assigned_machine_id == machine_id
                            )
                        })
                    })
                }
                BuildCleanupEvidence::ExternalCompleted { executors }
                | BuildCleanupEvidence::ExternalUnconfirmed { executors } => {
                    executors.iter().all(|executor| {
                        matches!(
                            executor.assignment(),
                            crate::build::BuildExecutorAssignment::External { .. }
                        ) && self.contains_executor(executor.assignment())
                    })
                }
            },
            BuildOperationState::Accepted
            | BuildOperationState::Placing
            | BuildOperationState::Building
            | BuildOperationState::Failed { .. }
            | BuildOperationState::Interrupted { .. } => true,
        }
    }
}

pub(super) fn invalid_transition(fields: &BuildFields<'_>) -> StatusProjectionError {
    StatusProjectionError::InvalidTransition {
        operation_id: fields.id.clone(),
        current: Box::new(ProjectionOperationState::Build(fields.state.clone())),
        attempted: Box::new(ProjectionOperationState::Build(fields.state.clone())),
    }
}

pub(super) fn status(
    fields: &BuildFields<'_>,
    state: BuildOperationState,
    executor_assignments: BuildExecutorAssignments,
    sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::Build {
        status: BuildOperationStatus::new(BuildOperationStatusWire {
            id: fields.id.clone(),
            target: fields.target.clone(),
            source: fields.source.clone(),
            adapter: fields.adapter.clone(),
            platforms: fields.platforms.clone(),
            executor_assignments,
            state,
            last_event_sequence: sequence,
        }),
    }
}

fn transition_allowed(current: &BuildOperationState, attempted: &BuildOperationState) -> bool {
    match (current, attempted) {
        (
            BuildOperationState::Accepted,
            BuildOperationState::Placing
            | BuildOperationState::Cancelled { .. }
            | BuildOperationState::Failed { .. }
            | BuildOperationState::TimedOut { .. },
        )
        | (
            BuildOperationState::Placing,
            BuildOperationState::Building
            | BuildOperationState::Failed { .. }
            | BuildOperationState::Cancelled { .. }
            | BuildOperationState::TimedOut { .. },
        )
        | (
            BuildOperationState::Building,
            BuildOperationState::Completed { .. }
            | BuildOperationState::Failed { .. }
            | BuildOperationState::Cancelled { .. }
            | BuildOperationState::TimedOut { .. },
        ) => true,
        (
            BuildOperationState::Accepted
            | BuildOperationState::Placing
            | BuildOperationState::Building
            | BuildOperationState::Completed { .. }
            | BuildOperationState::Failed { .. }
            | BuildOperationState::Cancelled { .. }
            | BuildOperationState::TimedOut { .. }
            | BuildOperationState::Interrupted { .. },
            BuildOperationState::Interrupted { .. },
        )
        | (
            BuildOperationState::Accepted,
            BuildOperationState::Accepted
            | BuildOperationState::Building
            | BuildOperationState::Completed { .. },
        )
        | (
            BuildOperationState::Placing,
            BuildOperationState::Accepted
            | BuildOperationState::Placing
            | BuildOperationState::Completed { .. },
        )
        | (
            BuildOperationState::Building,
            BuildOperationState::Accepted
            | BuildOperationState::Placing
            | BuildOperationState::Building,
        )
        | (
            BuildOperationState::Completed { .. }
            | BuildOperationState::Failed { .. }
            | BuildOperationState::Cancelled { .. }
            | BuildOperationState::TimedOut { .. }
            | BuildOperationState::Interrupted { .. },
            BuildOperationState::Accepted
            | BuildOperationState::Placing
            | BuildOperationState::Building
            | BuildOperationState::Completed { .. }
            | BuildOperationState::Failed { .. }
            | BuildOperationState::Cancelled { .. }
            | BuildOperationState::TimedOut { .. },
        ) => false,
    }
}

#[cfg(test)]
mod tests;
