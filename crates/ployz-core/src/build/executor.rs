//! Transport-neutral contracts for bounded build executors.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{BuildAdapter, GitSource, VerifiedGitCommit};
use crate::deploy::PlatformImage;
use crate::ids::{BuildExecutorId, BuildPoolId, MachineId, OperationId};
use crate::image::OciPlatform;
use crate::operation::{
    BuildLogChunk, BuildPlatformFailure, BuildToolchainEvidence, FailureMessage,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildTarget {
    Cluster,
    External { pool_id: BuildPoolId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "executor", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorAssignment {
    Cluster {
        machine_id: MachineId,
    },
    External {
        pool_id: BuildPoolId,
        executor_id: BuildExecutorId,
        image_seed: MachineId,
    },
}

impl BuildExecutorAssignment {
    #[must_use]
    pub fn origin(&self) -> BuildExecutorOrigin {
        match self {
            Self::Cluster { machine_id } => BuildExecutorOrigin::Cluster {
                machine_id: machine_id.clone(),
            },
            Self::External {
                pool_id,
                executor_id,
                image_seed: _,
            } => BuildExecutorOrigin::External {
                pool_id: pool_id.clone(),
                executor_id: executor_id.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalBuildExecutorCandidate {
    pub pool_id: BuildPoolId,
    pub executor_id: BuildExecutorId,
    pub platform: OciPlatform,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildPlatformExecutorAssignment {
    pub platform: OciPlatform,
    pub executor: BuildExecutorAssignment,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalBuildPlacementError {
    NoCapableExecutor {
        pool_id: BuildPoolId,
        platform: OciPlatform,
    },
    NoReachableImageSeed {
        pool_id: BuildPoolId,
    },
}

/// Selects only from the capability inventory and reachable Machine seeds the
/// caller supplied. An unsolicited responder has no path into either set.
pub fn place_external_build_platforms(
    pool_id: &BuildPoolId,
    platforms: &super::BuildPlatforms,
    candidates: &[ExternalBuildExecutorCandidate],
    reachable_image_seeds: &BTreeSet<MachineId>,
) -> Result<Vec<BuildPlatformExecutorAssignment>, ExternalBuildPlacementError> {
    let mut assignments = Vec::new();
    for platform in platforms.iter() {
        let executor = candidates
            .iter()
            .filter(|candidate| candidate.pool_id == *pool_id && candidate.platform == *platform)
            .min_by(|left, right| left.executor_id.cmp(&right.executor_id))
            .ok_or_else(|| ExternalBuildPlacementError::NoCapableExecutor {
                pool_id: pool_id.clone(),
                platform: platform.clone(),
            })?;
        let image_seed = reachable_image_seeds
            .iter()
            .next()
            .cloned()
            .ok_or_else(|| ExternalBuildPlacementError::NoReachableImageSeed {
                pool_id: pool_id.clone(),
            })?;
        assignments.push(BuildPlatformExecutorAssignment {
            platform: platform.clone(),
            executor: BuildExecutorAssignment::External {
                pool_id: pool_id.clone(),
                executor_id: executor.executor_id.clone(),
                image_seed,
            },
        });
    }
    Ok(assignments)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "origin", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorOrigin {
    Cluster {
        machine_id: MachineId,
    },
    External {
        pool_id: BuildPoolId,
        executor_id: BuildExecutorId,
    },
}

/// Validated executor identity and image-seed provenance for build evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(
    try_from = "BuildExecutorEvidenceWire",
    into = "BuildExecutorEvidenceWire"
)]
pub struct BuildExecutorEvidence {
    machine_id: MachineId,
    executor_origin: BuildExecutorOrigin,
}

impl BuildExecutorEvidence {
    #[must_use]
    pub fn from_assignment(assignment: &BuildExecutorAssignment) -> Self {
        match assignment {
            BuildExecutorAssignment::Cluster { machine_id } => Self {
                machine_id: machine_id.clone(),
                executor_origin: BuildExecutorOrigin::Cluster {
                    machine_id: machine_id.clone(),
                },
            },
            BuildExecutorAssignment::External {
                pool_id,
                executor_id,
                image_seed,
            } => Self {
                machine_id: image_seed.clone(),
                executor_origin: BuildExecutorOrigin::External {
                    pool_id: pool_id.clone(),
                    executor_id: executor_id.clone(),
                },
            },
        }
    }

    pub fn try_new(
        machine_id: MachineId,
        executor_origin: BuildExecutorOrigin,
    ) -> Result<Self, BuildExecutorEvidenceError> {
        if let BuildExecutorOrigin::Cluster {
            machine_id: origin_machine_id,
        } = &executor_origin
            && origin_machine_id != &machine_id
        {
            return Err(BuildExecutorEvidenceError::ClusterSeedMismatch);
        }
        Ok(Self {
            machine_id,
            executor_origin,
        })
    }

    #[must_use]
    pub const fn machine_id(&self) -> &MachineId {
        &self.machine_id
    }

    #[must_use]
    pub const fn executor_origin(&self) -> &BuildExecutorOrigin {
        &self.executor_origin
    }

    #[must_use]
    pub fn assignment(&self) -> BuildExecutorAssignment {
        match &self.executor_origin {
            BuildExecutorOrigin::Cluster { machine_id } => BuildExecutorAssignment::Cluster {
                machine_id: machine_id.clone(),
            },
            BuildExecutorOrigin::External {
                pool_id,
                executor_id,
            } => BuildExecutorAssignment::External {
                pool_id: pool_id.clone(),
                executor_id: executor_id.clone(),
                image_seed: self.machine_id.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildExecutorEvidenceError {
    #[error("cluster build evidence seed does not match its executor machine")]
    ClusterSeedMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BuildExecutorEvidenceWire {
    machine_id: MachineId,
    executor_origin: BuildExecutorOrigin,
}

impl TryFrom<BuildExecutorEvidenceWire> for BuildExecutorEvidence {
    type Error = BuildExecutorEvidenceError;

    fn try_from(value: BuildExecutorEvidenceWire) -> Result<Self, Self::Error> {
        Self::try_new(value.machine_id, value.executor_origin)
    }
}

impl From<BuildExecutorEvidence> for BuildExecutorEvidenceWire {
    fn from(value: BuildExecutorEvidence) -> Self {
        Self {
            machine_id: value.machine_id,
            executor_origin: value.executor_origin,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorStartRequest {
    pub operation_id: OperationId,
    pub assignment: BuildExecutorAssignment,
    pub source: GitSource,
    pub adapter: BuildAdapter,
    pub platform: OciPlatform,
    pub timeout_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorAcceptance {
    pub operation_id: OperationId,
    pub assignment: BuildExecutorAssignment,
    pub platform: OciPlatform,
}

impl BuildExecutorAcceptance {
    #[must_use]
    pub fn from_start_request(request: &BuildExecutorStartRequest) -> Self {
        Self {
            operation_id: request.operation_id.clone(),
            assignment: request.assignment.clone(),
            platform: request.platform.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorStartOk {
    pub acceptance: BuildExecutorAcceptance,
    pub image: PlatformImage,
    pub verified_commit: VerifiedGitCommit,
    pub toolchain: BuildToolchainEvidence,
    #[serde(flatten)]
    pub log_summary: BuildLogSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildLogSummary {
    pub final_log_sequence: u64,
    pub omitted_log_bytes: u64,
}

impl BuildLogSummary {
    #[must_use]
    pub const fn new(final_log_sequence: u64, omitted_log_bytes: u64) -> Self {
        Self {
            final_log_sequence,
            omitted_log_bytes,
        }
    }

    #[must_use]
    pub const fn none() -> Self {
        Self::new(0, 0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorStartDomainError {
    AssignmentMismatch {
        expected: BuildExecutorAssignment,
        actual: BuildExecutorAssignment,
    },
    RuntimeUnavailable,
    RuntimeStopped,
    PlatformMismatch {
        expected: OciPlatform,
        actual: OciPlatform,
    },
    InvalidTimeout {
        timeout_millis: u64,
    },
    AlreadyRunning,
    PlatformFailed {
        acceptance: BuildExecutorAcceptance,
        failure: BuildPlatformFailure,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
    Cancelled {
        acceptance: BuildExecutorAcceptance,
        cleanup: BuildExecutorCleanupOutcome,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
    TimedOut {
        acceptance: BuildExecutorAcceptance,
        message: FailureMessage,
        cleanup: BuildExecutorCleanupOutcome,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildExecutorCleanupOutcome {
    Confirmed,
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorCancelRequest {
    pub operation_id: OperationId,
    pub assignment: BuildExecutorAssignment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorCancelOk {
    pub assignment: BuildExecutorAssignment,
    pub outcome: BuildExecutorCancelOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorCancelOutcome {
    Requested,
    NotRunning,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildExecutorCancelDomainError {
    AssignmentMismatch {
        expected: BuildExecutorAssignment,
        actual: BuildExecutorAssignment,
    },
    CancelFailed {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorLogFrame {
    pub operation_id: OperationId,
    pub assignment: BuildExecutorAssignment,
    pub platform: OciPlatform,
    pub sequence: u64,
    pub chunk: BuildLogChunk,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster_assignment() -> BuildExecutorAssignment {
        BuildExecutorAssignment::Cluster {
            machine_id: MachineId::try_new("machine-a").expect("machine id"),
        }
    }

    fn start_request() -> BuildExecutorStartRequest {
        let source = GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "redacted-test-value",
            None::<String>,
        )
        .expect("source");
        BuildExecutorStartRequest {
            operation_id: OperationId::try_new("build-1").expect("operation id"),
            assignment: cluster_assignment(),
            source,
            adapter: BuildAdapter::Dockerfile {
                dockerfile: super::super::BuildContextPath::try_new("Dockerfile")
                    .expect("dockerfile path"),
                target: None,
            },
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
            timeout_millis: 1_000,
        }
    }

    #[test]
    fn build_executor_ids_are_validated_subject_tokens() {
        assert!(BuildPoolId::try_new("pool-a").is_ok());
        assert!(BuildExecutorId::try_new("executor.a").is_err());
    }

    #[test]
    fn executor_origin_rejects_unknown_fields() {
        let origin = serde_json::json!({
            "origin": "cluster",
            "machine_id": "machine-a",
            "credential": "must-not-fit",
        });
        assert!(serde_json::from_value::<BuildExecutorOrigin>(origin).is_err());
    }

    #[test]
    fn acceptance_is_an_exact_start_request_projection() {
        let request = start_request();

        assert_eq!(
            BuildExecutorAcceptance::from_start_request(&request),
            BuildExecutorAcceptance {
                operation_id: request.operation_id,
                assignment: request.assignment,
                platform: request.platform,
            }
        );
    }

    #[test]
    fn start_request_rejects_unknown_fields() {
        let mut encoded = serde_json::to_value(start_request()).expect("encode request");
        encoded
            .as_object_mut()
            .expect("request object")
            .insert("credential".to_owned(), serde_json::json!("must-not-fit"));

        assert!(serde_json::from_value::<BuildExecutorStartRequest>(encoded).is_err());
    }

    #[test]
    fn log_frame_rejects_unknown_or_oversized_payloads() {
        let frame = serde_json::json!({
            "operation_id": "build-1",
            "assignment": {"executor": "cluster", "machine_id": "machine-a"},
            "platform": {"os": "linux", "architecture": "amd64"},
            "sequence": 1,
            "chunk": "hello",
            "credential": "must-not-fit"
        });
        assert!(serde_json::from_value::<BuildExecutorLogFrame>(frame).is_err());

        let frame = serde_json::json!({
            "operation_id": "build-1",
            "assignment": {"executor": "cluster", "machine_id": "machine-a"},
            "platform": {"os": "linux", "architecture": "amd64"},
            "sequence": 1,
            "chunk": "x".repeat(crate::operation::MAX_BUILD_LOG_CHUNK_BYTES + 1)
        });
        assert!(serde_json::from_value::<BuildExecutorLogFrame>(frame).is_err());
    }
}
