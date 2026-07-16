//! Build operation states, typed failures, evidence, and projection.

use serde::{Deserialize, Serialize};

use crate::build::{BuildAdapter, BuildPlatforms, GitSourceEvidence, VerifiedGitCommit};
use crate::deploy::{PlatformImage, PushedImageReceipt};
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
pub enum BuildEvidence {
    VerifiedCommit {
        platform: OciPlatform,
        machine_id: MachineId,
        commit: VerifiedGitCommit,
    },
    PlatformPlaced {
        platform: OciPlatform,
        machine_id: MachineId,
    },
    ToolchainVerified {
        platform: OciPlatform,
        machine_id: MachineId,
        toolchain: BuildToolchainEvidence,
    },
    PlatformLog {
        platform: OciPlatform,
        machine_id: MachineId,
        chunk: BuildLogChunk,
    },
    PlatformLogTruncated {
        platform: OciPlatform,
        machine_id: MachineId,
        omitted_bytes: u64,
    },
    PlatformCompleted {
        platform: OciPlatform,
        machine_id: MachineId,
        image: PlatformImage,
    },
    PlatformFailed {
        platform: OciPlatform,
        machine_id: MachineId,
        failure: BuildPlatformFailure,
    },
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
        source: GitSourceEvidence,
        adapter: BuildAdapter,
        platforms: BuildPlatforms,
    },
    Evidence(BuildEvidence),
    Transition(BuildTransition),
}

pub(super) struct BuildFields<'a> {
    pub id: &'a OperationId,
    pub source: &'a GitSourceEvidence,
    pub adapter: &'a BuildAdapter,
    pub platforms: &'a BuildPlatforms,
    pub state: &'a BuildOperationState,
}

pub(super) fn project_event(
    fields: BuildFields<'_>,
    event: BuildEvent,
    sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        BuildEvent::Submitted {
            source,
            adapter,
            platforms,
        } => {
            if fields.source != &source
                || fields.adapter != &adapter
                || fields.platforms != &platforms
            {
                return Err(invalid_transition(&fields));
            }
            Ok(OperationProjection::AlreadySatisfied)
        }
        BuildEvent::Evidence(evidence) => project_evidence(&fields, evidence, sequence),
        BuildEvent::Transition(transition) => project_transition(
            fields.id,
            fields.state,
            transition.state(),
            BuildOperationState::is_terminal,
            transition_allowed,
            ProjectionOperationState::Build,
            |state| status(&fields, state, sequence),
        ),
    }
}

fn project_evidence(
    fields: &BuildFields<'_>,
    evidence: BuildEvidence,
    sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let (platform, allowed) = match &evidence {
        BuildEvidence::PlatformPlaced {
            platform,
            machine_id: _,
        }
        | BuildEvidence::ToolchainVerified {
            platform,
            machine_id: _,
            toolchain: _,
        }
        | BuildEvidence::VerifiedCommit {
            platform,
            machine_id: _,
            commit: _,
        } => (
            platform,
            matches!(fields.state, BuildOperationState::Placing),
        ),
        BuildEvidence::PlatformLog {
            platform,
            machine_id: _,
            chunk: _,
        }
        | BuildEvidence::PlatformLogTruncated {
            platform,
            machine_id: _,
            omitted_bytes: _,
        }
        | BuildEvidence::PlatformCompleted {
            platform,
            machine_id: _,
            image: _,
        }
        | BuildEvidence::PlatformFailed {
            platform,
            machine_id: _,
            failure: _,
        } => (
            platform,
            matches!(fields.state, BuildOperationState::Building),
        ),
    };

    if !allowed || !fields.platforms.contains(platform) {
        return Err(invalid_transition(fields));
    }

    if let BuildEvidence::VerifiedCommit { commit, .. } = &evidence
        && (commit.url != fields.source.url
            || commit.commit != fields.source.commit
            || commit.subdir != fields.source.subdir)
    {
        return Err(invalid_transition(fields));
    }

    if let BuildEvidence::ToolchainVerified { toolchain, .. } = &evidence
        && !matches!(
            (fields.adapter, &toolchain.adapter),
            (
                BuildAdapter::Dockerfile {
                    dockerfile: _,
                    target: _,
                },
                BuildAdapterToolchainEvidence::Dockerfile,
            ) | (
                BuildAdapter::Railpack { cache_scope: _ },
                BuildAdapterToolchainEvidence::Railpack {
                    helper_version: _,
                    helper_sha256: _,
                    frontend_image: _,
                },
            )
        )
    {
        return Err(invalid_transition(fields));
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(status(fields, fields.state.clone(), sequence)),
    })
}

fn invalid_transition(fields: &BuildFields<'_>) -> StatusProjectionError {
    StatusProjectionError::InvalidTransition {
        operation_id: fields.id.clone(),
        current: Box::new(ProjectionOperationState::Build(fields.state.clone())),
        attempted: Box::new(ProjectionOperationState::Build(fields.state.clone())),
    }
}

fn status(
    fields: &BuildFields<'_>,
    state: BuildOperationState,
    sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::Build {
        id: fields.id.clone(),
        source: fields.source.clone(),
        adapter: fields.adapter.clone(),
        platforms: fields.platforms.clone(),
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
    use crate::build::{BuildCacheScope, GitSource};
    use crate::deploy::PlatformImage;
    use crate::image::{OciDigest, OciPlatform};

    fn id() -> OperationId {
        OperationId::try_new("build-test").expect("id")
    }
    fn status0() -> OperationStatus {
        OperationStatus::build_accepted(
            id(),
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
    fn receipt() -> PushedImageReceipt {
        PushedImageReceipt::try_new([(
            OciPlatform::try_new("linux", "amd64").expect("platform"),
            PlatformImage {
                seed: MachineId::try_new("machine-a").expect("machine"),
                manifest_digest: OciDigest::try_new(format!("sha256:{}", "1".repeat(64)))
                    .expect("digest"),
                image_id: OciDigest::try_new(format!("sha256:{}", "2".repeat(64))).expect("digest"),
            },
        )])
        .expect("receipt")
    }

    #[test]
    fn projector_accepts_only_ordered_transitions_and_terminal_is_final() {
        let accepted = status0();
        let OperationProjection::StatusChanged { status: placing } = project_event_from_status(
            &accepted,
            BuildTransition::Placing.event(&id()),
            EventSequence::try_new(2).expect("seq"),
        )
        .expect("placing") else {
            panic!("changed")
        };
        let OperationProjection::StatusChanged { status: building } = project_event_from_status(
            &placing,
            BuildTransition::Building.event(&id()),
            EventSequence::try_new(3).expect("seq"),
        )
        .expect("building") else {
            panic!("changed")
        };
        let OperationProjection::StatusChanged { status: completed } = project_event_from_status(
            &building,
            BuildTransition::Completed { receipt: receipt() }.event(&id()),
            EventSequence::try_new(4).expect("seq"),
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
                EventSequence::try_new(5).expect("seq")
            )
            .is_err()
        );
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
            source: evidence.clone(),
            adapter: adapter.clone(),
            platforms: platforms.clone(),
        };
        let status = OperationStatus::build_accepted(
            id(),
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
                    machine_id: MachineId::try_new("machine-arm").expect("machine"),
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
                    machine_id: MachineId::try_new("machine-amd").expect("machine"),
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
                    machine_id: MachineId::try_new("machine-amd").expect("machine"),
                    toolchain: BuildToolchainEvidence {
                        buildkit_image: OciDigest::try_new(format!("sha256:{}", "3".repeat(64)))
                            .expect("digest"),
                        adapter: BuildAdapterToolchainEvidence::Dockerfile,
                    },
                },
                EventSequence::try_new(3).expect("sequence"),
            )
            .is_err()
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

    fn project_event_from_status(
        current: &OperationStatus,
        event: OperationEvent,
        sequence: EventSequence,
    ) -> Result<OperationProjection, StatusProjectionError> {
        super::super::project_operation_event(current, event, sequence)
    }
}
