//! Build operation states, typed failures, evidence, and projection.

use serde::{Deserialize, Serialize};

use crate::build::{
    BuildAdapter, BuildExecutorAssignment, BuildPlatformExecutorAssignment, BuildPlatforms,
    BuildTarget, GitSourceEvidence,
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildPlatformFailure {
    MachineUnavailable {
        message: FailureMessage,
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildToolchainEvidence {
    pub buildkit_image: crate::image::OciDigest,
    pub adapter: BuildAdapterToolchainEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildCleanupEvidence {
    NotRequired,
    Completed { machine_ids: Vec<MachineId> },
    Unconfirmed { machine_ids: Vec<MachineId> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildTimeoutFailure {
    DeadlineExceeded { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
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
        source: GitSourceEvidence,
        adapter: BuildAdapter,
        platforms: BuildPlatforms,
    },
    Evidence(BuildEvidence),
    Transition(BuildTransition),
}

pub(super) struct BuildFields<'a> {
    pub id: &'a OperationId,
    pub target: &'a BuildTarget,
    pub source: &'a GitSourceEvidence,
    pub adapter: &'a BuildAdapter,
    pub platforms: &'a BuildPlatforms,
    pub executor_assignments: &'a [BuildPlatformExecutorAssignment],
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
                && !all_platforms_placed(fields.platforms, fields.executor_assignments)
            {
                return Err(invalid_transition(&fields));
            }
            if attempted.is_terminal()
                && !terminal_provenance_matches(fields.executor_assignments, &attempted)
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
                        fields.executor_assignments.to_vec(),
                        sequence,
                    )
                },
            )
        }
    }
}

fn terminal_provenance_matches(
    assignments: &[BuildPlatformExecutorAssignment],
    attempted: &BuildOperationState,
) -> bool {
    match attempted {
        BuildOperationState::Completed { receipt } => {
            receipt.platforms().len() == assignments.len()
                && assignments.iter().all(|assignment| {
                    receipt.platform(&assignment.platform).is_some_and(|image| {
                        image.seed == *assignment_image_seed(&assignment.executor)
                    })
                })
        }
        BuildOperationState::Failed {
            failure:
                BuildOperationFailure::PlatformFailed {
                    platform,
                    machine_id,
                    failure: _,
                },
        } => assignments.iter().any(|assignment| {
            assignment.platform == *platform
                && assignment_image_seed(&assignment.executor) == machine_id
        }),
        BuildOperationState::Cancelled { cleanup, .. }
        | BuildOperationState::TimedOut { cleanup, .. } => cleanup_matches(assignments, cleanup),
        BuildOperationState::Accepted
        | BuildOperationState::Placing
        | BuildOperationState::Building
        | BuildOperationState::Failed { .. }
        | BuildOperationState::Interrupted { .. } => true,
    }
}

fn cleanup_matches(
    assignments: &[BuildPlatformExecutorAssignment],
    cleanup: &BuildCleanupEvidence,
) -> bool {
    let machine_ids = match cleanup {
        BuildCleanupEvidence::NotRequired => return true,
        BuildCleanupEvidence::Completed { machine_ids }
        | BuildCleanupEvidence::Unconfirmed { machine_ids } => machine_ids,
    };
    machine_ids.iter().all(|machine_id| {
        assignments
            .iter()
            .any(|assignment| assignment_image_seed(&assignment.executor) == machine_id)
    })
}

fn assignment_image_seed(assignment: &BuildExecutorAssignment) -> &MachineId {
    match assignment {
        BuildExecutorAssignment::Cluster { machine_id } => machine_id,
        BuildExecutorAssignment::External { image_seed, .. } => image_seed,
    }
}

fn all_platforms_placed(
    platforms: &BuildPlatforms,
    assignments: &[BuildPlatformExecutorAssignment],
) -> bool {
    assignments.len() == platforms.iter().len()
        && platforms.iter().all(|platform| {
            assignments
                .iter()
                .any(|placed| placed.platform == *platform)
        })
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
    executor_assignments: Vec<BuildPlatformExecutorAssignment>,
    sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::Build {
        id: fields.id.clone(),
        target: fields.target.clone(),
        source: fields.source.clone(),
        adapter: fields.adapter.clone(),
        platforms: fields.platforms.clone(),
        executor_assignments,
        state,
        last_event_sequence: sequence,
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
mod tests {
    use super::*;
    use crate::build::{
        BuildCacheScope, BuildExecutorAssignment, BuildExecutorEvidence, GitCommit,
        GitRepositoryUrl, GitSource, VerifiedGitCommit,
    };
    use crate::deploy::PlatformImage;
    use crate::image::{OciDigest, OciPlatform};

    fn id() -> OperationId {
        OperationId::try_new("build-test").expect("id")
    }
    fn cluster_evidence(machine_id: &MachineId) -> BuildExecutorEvidence {
        BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::Cluster {
            machine_id: machine_id.clone(),
        })
    }
    fn status0() -> OperationStatus {
        status_for_target(BuildTarget::Cluster)
    }
    fn status_for_target(target: BuildTarget) -> OperationStatus {
        OperationStatus::build_accepted(
            id(),
            target,
            GitSource::try_new(
                "https://example.com/repo.git",
                "0123456789abcdef0123456789abcdef01234567",
                "git",
                "secret",
                None::<String>,
            )
            .expect("source")
            .evidence(),
            BuildAdapter::Railpack {
                cache_scope: BuildCacheScope::try_new("test").expect("scope"),
            },
            BuildPlatforms::try_new([OciPlatform::try_new("linux", "amd64").expect("platform")])
                .expect("platforms"),
            EventSequence::try_new(1).expect("sequence"),
        )
    }
    fn building_status(target: BuildTarget) -> OperationStatus {
        let accepted = status_for_target(target.clone());
        let OperationProjection::StatusChanged { status: placing } = project_event_from_status(
            &accepted,
            BuildTransition::Placing.event(&id()),
            EventSequence::try_new(2).expect("sequence"),
        )
        .expect("placing") else {
            panic!("changed")
        };
        let executor = match &target {
            BuildTarget::Cluster => {
                cluster_evidence(&MachineId::try_new("machine-a").expect("machine"))
            }
            BuildTarget::External { pool_id } => {
                BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::External {
                    pool_id: pool_id.clone(),
                    executor_id: crate::ids::BuildExecutorId::try_new("executor-a")
                        .expect("executor"),
                    image_seed: MachineId::try_new("seed-a").expect("seed"),
                })
            }
        };
        let OperationProjection::StatusChanged { status: placed } = project_event_from_status(
            &placing,
            OperationEvent::BuildPlatformPlaced {
                operation_id: id(),
                platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
                executor,
            },
            EventSequence::try_new(3).expect("sequence"),
        )
        .expect("placed") else {
            panic!("changed")
        };
        let OperationProjection::StatusChanged { status: building } = project_event_from_status(
            &placed,
            BuildTransition::Building.event(&id()),
            EventSequence::try_new(4).expect("sequence"),
        )
        .expect("building") else {
            panic!("changed")
        };
        *building
    }
    fn receipt() -> PushedImageReceipt {
        receipt_for(
            OciPlatform::try_new("linux", "amd64").expect("platform"),
            MachineId::try_new("machine-a").expect("machine"),
        )
    }

    fn receipt_for(platform: OciPlatform, seed: MachineId) -> PushedImageReceipt {
        PushedImageReceipt::try_new([(
            platform,
            PlatformImage {
                seed,
                manifest_digest: OciDigest::try_new(format!("sha256:{}", "1".repeat(64)))
                    .expect("digest"),
                image_id: OciDigest::try_new(format!("sha256:{}", "2".repeat(64))).expect("digest"),
                availability_expires_at: crate::deploy::ImageAvailabilityExpiresAt::try_new(
                    4_102_444_800,
                )
                .expect("expiry"),
            },
        )])
        .expect("receipt")
    }

    fn railpack_toolchain() -> BuildToolchainEvidence {
        BuildToolchainEvidence {
            buildkit_image: OciDigest::try_new(format!("sha256:{}", "3".repeat(64)))
                .expect("digest"),
            adapter: BuildAdapterToolchainEvidence::Railpack {
                helper_version: InstallArtifactVersion::try_new("v0.31.0").expect("version"),
                helper_sha256: InstallSha256Digest::try_new("4".repeat(64)).expect("digest"),
                frontend_image: OciDigest::try_new(format!("sha256:{}", "5".repeat(64)))
                    .expect("digest"),
            },
        }
    }

    #[test]
    fn projector_accepts_only_ordered_transitions_and_terminal_is_final() {
        let building = building_status(BuildTarget::Cluster);
        let OperationProjection::StatusChanged { status: completed } = project_event_from_status(
            &building,
            BuildTransition::Completed { receipt: receipt() }.event(&id()),
            EventSequence::try_new(5).expect("seq"),
        )
        .expect("completed") else {
            panic!("changed")
        };
        assert!(completed.is_terminal());
        assert!(
            project_event_from_status(
                &completed,
                BuildTransition::Failed {
                    failure: BuildOperationFailure::ReceiptAssemblyFailed {
                        message: FailureMessage::try_new("late").expect("message")
                    }
                }
                .event(&id()),
                EventSequence::try_new(6).expect("seq")
            )
            .is_err()
        );
    }

    #[test]
    fn completed_receipt_must_match_placed_platform() {
        let pool_id = crate::ids::BuildPoolId::try_new("pool-a").expect("pool");
        let building = building_status(BuildTarget::External { pool_id });
        let receipt = receipt_for(
            OciPlatform::try_new("linux", "arm64").expect("platform"),
            MachineId::try_new("seed-a").expect("seed"),
        );
        assert!(
            project_event_from_status(
                &building,
                BuildTransition::Completed { receipt }.event(&id()),
                EventSequence::try_new(5).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn completed_receipt_image_seed_must_match_placed_executor() {
        let pool_id = crate::ids::BuildPoolId::try_new("pool-a").expect("pool");
        let building = building_status(BuildTarget::External { pool_id });
        let receipt = receipt_for(
            OciPlatform::try_new("linux", "amd64").expect("platform"),
            MachineId::try_new("seed-b").expect("seed"),
        );
        assert!(
            project_event_from_status(
                &building,
                BuildTransition::Completed { receipt }.event(&id()),
                EventSequence::try_new(5).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn platform_failure_must_match_placed_platform() {
        let building = building_status(BuildTarget::Cluster);
        let failure = BuildPlatformFailure::MachineUnavailable {
            message: FailureMessage::try_new("failed").expect("message"),
        };
        assert!(
            project_event_from_status(
                &building,
                BuildTransition::Failed {
                    failure: BuildOperationFailure::PlatformFailed {
                        platform: OciPlatform::try_new("linux", "arm64").expect("platform"),
                        machine_id: MachineId::try_new("machine-a").expect("machine"),
                        failure,
                    },
                }
                .event(&id()),
                EventSequence::try_new(5).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn platform_failure_image_seed_must_match_placed_executor() {
        let building = building_status(BuildTarget::Cluster);
        let failure = BuildPlatformFailure::MachineUnavailable {
            message: FailureMessage::try_new("failed").expect("message"),
        };
        assert!(
            project_event_from_status(
                &building,
                BuildTransition::Failed {
                    failure: BuildOperationFailure::PlatformFailed {
                        platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
                        machine_id: MachineId::try_new("machine-b").expect("machine"),
                        failure,
                    },
                }
                .event(&id()),
                EventSequence::try_new(5).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn terminal_cleanup_must_not_name_unplaced_image_seeds() {
        let building = building_status(BuildTarget::Cluster);
        let cleanup = BuildCleanupEvidence::Completed {
            machine_ids: vec![MachineId::try_new("machine-b").expect("machine")],
        };
        assert!(
            project_event_from_status(
                &building,
                BuildTransition::Cancelled {
                    reason: CancellationReason::try_new("cancelled").expect("reason"),
                    cleanup,
                }
                .event(&id()),
                EventSequence::try_new(5).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn accepted_build_cleanup_must_be_provenance_free() {
        let accepted = status0();
        assert!(
            project_event_from_status(
                &accepted,
                BuildTransition::TimedOut {
                    failure: BuildTimeoutFailure::DeadlineExceeded {
                        message: FailureMessage::try_new("timed out").expect("message"),
                    },
                    cleanup: BuildCleanupEvidence::Unconfirmed {
                        machine_ids: vec![MachineId::try_new("machine-a").expect("machine")],
                    },
                }
                .event(&id()),
                EventSequence::try_new(2).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn log_gap_is_nonterminal_and_preserves_completed_machine_outcome() {
        let building = building_status(BuildTarget::Cluster);
        let OperationProjection::StatusChanged {
            status: gap_recorded,
        } = project_event_from_status(
            &building,
            OperationEvent::BuildPlatformLogGap {
                operation_id: id(),
                platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
                executor: cluster_evidence(&MachineId::try_new("machine-a").expect("machine")),
                expected_sequence: 2,
                final_sequence: 3,
            },
            EventSequence::try_new(5).expect("sequence"),
        )
        .expect("gap evidence")
        else {
            panic!("changed")
        };

        let OperationProjection::StatusChanged { status: completed } = project_event_from_status(
            &gap_recorded,
            BuildTransition::Completed { receipt: receipt() }.event(&id()),
            EventSequence::try_new(6).expect("sequence"),
        )
        .expect("completed") else {
            panic!("changed")
        };
        assert!(matches!(
            *completed,
            OperationStatus::Build {
                state: BuildOperationState::Completed { .. },
                ..
            }
        ));
    }

    #[test]
    fn submitted_event_and_status_are_credential_free() {
        let source = GitSource::try_new(
            "https://example.com/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "do-not-persist",
            None::<String>,
        )
        .expect("source");
        let evidence = source.evidence();
        let adapter = BuildAdapter::Railpack {
            cache_scope: BuildCacheScope::try_new("test").expect("scope"),
        };
        let platforms =
            BuildPlatforms::try_new([OciPlatform::try_new("linux", "amd64").expect("platform")])
                .expect("platforms");
        let event = OperationEvent::BuildSubmitted {
            operation_id: id(),
            target: BuildTarget::Cluster,
            source: evidence.clone(),
            adapter: adapter.clone(),
            platforms: platforms.clone(),
        };
        let status = OperationStatus::build_accepted(
            id(),
            BuildTarget::Cluster,
            evidence,
            adapter,
            platforms,
            EventSequence::try_new(1).expect("sequence"),
        );
        assert!(
            !serde_json::to_string(&event)
                .expect("event")
                .contains("do-not-persist")
        );
        assert!(
            !serde_json::to_string(&status)
                .expect("status")
                .contains("do-not-persist")
        );
    }

    #[test]
    fn submitted_event_must_match_the_admitted_build_contract() {
        let accepted = status0();
        let different_source = GitSource::try_new(
            "https://example.com/other.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            None::<String>,
        )
        .expect("source");
        let OperationStatus::Build {
            adapter, platforms, ..
        } = &accepted
        else {
            panic!("build status")
        };

        assert!(
            project_event_from_status(
                &accepted,
                OperationEvent::BuildSubmitted {
                    operation_id: id(),
                    target: BuildTarget::Cluster,
                    source: different_source.evidence(),
                    adapter: adapter.clone(),
                    platforms: platforms.clone(),
                },
                EventSequence::try_new(2).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn evidence_must_name_a_declared_platform_in_the_correct_stage() {
        let accepted = status0();
        let OperationProjection::StatusChanged { status: placing } = project_event_from_status(
            &accepted,
            BuildTransition::Placing.event(&id()),
            EventSequence::try_new(2).expect("sequence"),
        )
        .expect("placing") else {
            panic!("changed")
        };

        assert!(
            project_event_from_status(
                &placing,
                OperationEvent::BuildPlatformPlaced {
                    operation_id: id(),
                    platform: OciPlatform::try_new("linux", "arm64").expect("platform"),
                    executor: cluster_evidence(
                        &MachineId::try_new("machine-arm").expect("machine"),
                    ),
                },
                EventSequence::try_new(3).expect("sequence"),
            )
            .is_err()
        );
        assert!(
            project_event_from_status(
                &placing,
                OperationEvent::BuildPlatformLog {
                    operation_id: id(),
                    platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
                    executor: cluster_evidence(
                        &MachineId::try_new("machine-amd").expect("machine"),
                    ),
                    chunk: BuildLogChunk::try_new("too early").expect("chunk"),
                },
                EventSequence::try_new(3).expect("sequence"),
            )
            .is_err()
        );
        assert!(
            project_event_from_status(
                &placing,
                OperationEvent::BuildPlatformToolchainVerified {
                    operation_id: id(),
                    platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
                    executor: cluster_evidence(
                        &MachineId::try_new("machine-amd").expect("machine"),
                    ),
                    toolchain: railpack_toolchain(),
                },
                EventSequence::try_new(3).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn cluster_evidence_rejects_origin_and_image_seed_contradictions() {
        let building = building_status(BuildTarget::Cluster);
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let machine_id = MachineId::try_new("machine-a").expect("machine");
        let other_machine = MachineId::try_new("machine-b").expect("machine");

        let invalid_wire = serde_json::json!({
            "machine_id": machine_id,
            "executor_origin": {"origin": "cluster", "machine_id": other_machine},
        });
        assert!(serde_json::from_value::<BuildExecutorEvidence>(invalid_wire).is_err());

        let event = OperationEvent::BuildPlatformCompleted {
            operation_id: id(),
            platform: platform.clone(),
            executor: cluster_evidence(&machine_id),
            image: PlatformImage {
                seed: other_machine.clone(),
                manifest_digest: OciDigest::try_new(format!("sha256:{}", "1".repeat(64)))
                    .expect("digest"),
                image_id: OciDigest::try_new(format!("sha256:{}", "2".repeat(64))).expect("digest"),
                availability_expires_at: crate::deploy::ImageAvailabilityExpiresAt::try_new(
                    4_102_444_800,
                )
                .expect("expiry"),
            },
        };
        assert!(
            project_event_from_status(
                &building,
                event,
                EventSequence::try_new(5).expect("sequence"),
            )
            .is_err()
        );
    }

    #[test]
    fn external_evidence_requires_the_admitted_pool_and_external_origin() {
        let pool_id = crate::ids::BuildPoolId::try_new("pool-a").expect("pool");
        let building = building_status(BuildTarget::External {
            pool_id: pool_id.clone(),
        });
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let machine_id = MachineId::try_new("seed-a").expect("machine");
        let failure = BuildPlatformFailure::MachineUnavailable {
            message: FailureMessage::try_new("failed").expect("message"),
        };

        for executor in [
            cluster_evidence(&machine_id),
            BuildExecutorEvidence::from_assignment(&BuildExecutorAssignment::External {
                pool_id: crate::ids::BuildPoolId::try_new("pool-b").expect("pool"),
                executor_id: crate::ids::BuildExecutorId::try_new("executor-a").expect("executor"),
                image_seed: machine_id.clone(),
            }),
        ] {
            assert!(
                project_event_from_status(
                    &building,
                    OperationEvent::BuildPlatformFailed {
                        operation_id: id(),
                        platform: platform.clone(),
                        executor,
                        failure: failure.clone(),
                    },
                    EventSequence::try_new(5).expect("sequence"),
                )
                .is_err()
            );
        }
    }

    #[test]
    fn verified_build_evidence_is_accepted_while_building() {
        let building = building_status(BuildTarget::Cluster);
        let platform = OciPlatform::try_new("linux", "amd64").expect("platform");
        let machine_id = MachineId::try_new("machine-a").expect("machine");
        let OperationProjection::StatusChanged { status: verified } = project_event_from_status(
            &building,
            OperationEvent::BuildCommitVerified {
                operation_id: id(),
                platform: platform.clone(),
                executor: cluster_evidence(&machine_id),
                commit: VerifiedGitCommit {
                    url: GitRepositoryUrl::try_new("https://example.com/repo.git").expect("url"),
                    commit: GitCommit::try_new("0123456789abcdef0123456789abcdef01234567")
                        .expect("commit"),
                    subdir: None,
                },
            },
            EventSequence::try_new(5).expect("sequence"),
        )
        .expect("verified commit") else {
            panic!("changed")
        };

        assert!(
            project_event_from_status(
                &verified,
                OperationEvent::BuildPlatformToolchainVerified {
                    operation_id: id(),
                    platform,
                    executor: cluster_evidence(&machine_id),
                    toolchain: railpack_toolchain(),
                },
                EventSequence::try_new(6).expect("sequence"),
            )
            .is_ok()
        );
    }

    #[test]
    fn core_process_loss_terminally_interrupts_a_build() {
        let accepted = status0();
        let evidence = accepted
            .interruption_evidence(OperationInterruptionCause::PriorCoreProcessLoss)
            .expect("interruption evidence");
        let OperationProjection::StatusChanged { status } = project_event_from_status(
            &accepted,
            OperationEvent::OperationInterrupted {
                operation_id: id(),
                evidence: evidence.clone(),
            },
            EventSequence::try_new(2).expect("sequence"),
        )
        .expect("interrupted") else {
            panic!("changed")
        };

        assert!(status.is_terminal());
        assert_eq!(status.terminal_interruption_evidence(), Some(&evidence));
    }

    #[test]
    fn log_chunks_are_bounded() {
        assert!(BuildLogChunk::try_new("x".repeat(MAX_BUILD_LOG_CHUNK_BYTES)).is_ok());
        assert!(matches!(
            BuildLogChunk::try_new("x".repeat(MAX_BUILD_LOG_CHUNK_BYTES + 1)),
            Err(BuildLogChunkError::TooLarge { .. })
        ));
    }

    #[test]
    fn insufficient_host_disk_has_actionable_wire_evidence() {
        let failure = BuildPlatformFailure::InsufficientHostDisk {
            available_bytes: 1,
            required_free_bytes: 2,
        };
        assert_eq!(
            serde_json::to_value(failure).expect("serialize failure"),
            serde_json::json!({
                "kind": "insufficient_host_disk",
                "available_bytes": 1,
                "required_free_bytes": 2
            })
        );
    }

    fn project_event_from_status(
        current: &OperationStatus,
        event: OperationEvent,
        sequence: EventSequence,
    ) -> Result<OperationProjection, StatusProjectionError> {
        super::super::project_operation_event(current, event, sequence)
    }
}
