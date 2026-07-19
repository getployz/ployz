//! Transport-neutral contracts for bounded build executors.

use serde::{Deserialize, Serialize};

use super::{BuildAdapter, GitSource, VerifiedGitCommit};
use crate::deploy::PlatformImage;
use crate::ids::{BuildExecutorId, BuildPoolId, MachineId, OperationId};
use crate::image::OciPlatform;
use crate::machine::rpc::MachineRpcResponder;
use crate::operation::{
    BuildLogChunk, BuildPlatformFailure, BuildToolchainEvidence, FailureMessage,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildTarget {
    #[default]
    Cluster,
    External {
        pool_id: BuildPoolId,
    },
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorStartRequest {
    pub operation_id: OperationId,
    pub origin: BuildExecutorOrigin,
    pub image_seed: MachineId,
    pub source: GitSource,
    pub adapter: BuildAdapter,
    pub platform: OciPlatform,
    pub timeout_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorAcceptance {
    pub operation_id: OperationId,
    pub origin: BuildExecutorOrigin,
    pub image_seed: MachineId,
    pub platform: OciPlatform,
}

impl BuildExecutorAcceptance {
    #[must_use]
    pub fn from_start_request(request: &BuildExecutorStartRequest) -> Self {
        Self {
            operation_id: request.operation_id.clone(),
            origin: request.origin.clone(),
            image_seed: request.image_seed.clone(),
            platform: request.platform.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorStartOk {
    pub machine_id: MachineId,
    pub acceptance: BuildExecutorAcceptance,
    pub image: PlatformImage,
    pub verified_commit: VerifiedGitCommit,
    pub toolchain: BuildToolchainEvidence,
    #[serde(flatten)]
    pub log_summary: BuildLogSummary,
}

impl MachineRpcResponder for BuildExecutorStartOk {
    fn responder_machine_id(&self) -> &MachineId {
        &self.machine_id
    }
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
    OriginMismatch {
        expected: BuildExecutorOrigin,
        actual: BuildExecutorOrigin,
    },
    ImageSeedMismatch {
        expected: MachineId,
        actual: MachineId,
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
    pub origin: BuildExecutorOrigin,
    pub image_seed: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorCancelOk {
    pub machine_id: MachineId,
    pub origin: BuildExecutorOrigin,
    pub outcome: BuildExecutorCancelOutcome,
}

impl MachineRpcResponder for BuildExecutorCancelOk {
    fn responder_machine_id(&self) -> &MachineId {
        &self.machine_id
    }
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
    OriginMismatch {
        expected: BuildExecutorOrigin,
        actual: BuildExecutorOrigin,
    },
    ImageSeedMismatch {
        expected: MachineId,
        actual: MachineId,
    },
    CancelFailed {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuildExecutorLogFrame {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub origin: BuildExecutorOrigin,
    pub platform: OciPlatform,
    pub sequence: u64,
    pub chunk: BuildLogChunk,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cluster_origin() -> BuildExecutorOrigin {
        BuildExecutorOrigin::Cluster {
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
            origin: cluster_origin(),
            image_seed: MachineId::try_new("machine-a").expect("machine id"),
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
    fn build_target_defaults_to_reserved_cluster_pool() {
        assert_eq!(BuildTarget::default(), BuildTarget::Cluster);
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
                origin: request.origin,
                image_seed: request.image_seed,
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
            "machine_id": "machine-a",
            "origin": {"origin": "cluster", "machine_id": "machine-a"},
            "platform": {"os": "linux", "architecture": "amd64"},
            "sequence": 1,
            "chunk": "hello",
            "credential": "must-not-fit"
        });
        assert!(serde_json::from_value::<BuildExecutorLogFrame>(frame).is_err());

        let frame = serde_json::json!({
            "operation_id": "build-1",
            "machine_id": "machine-a",
            "origin": {"origin": "cluster", "machine_id": "machine-a"},
            "platform": {"os": "linux", "architecture": "amd64"},
            "sequence": 1,
            "chunk": "x".repeat(crate::operation::MAX_BUILD_LOG_CHUNK_BYTES + 1)
        });
        assert!(serde_json::from_value::<BuildExecutorLogFrame>(frame).is_err());
    }
}
