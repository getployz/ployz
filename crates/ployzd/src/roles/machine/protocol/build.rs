use ployz_core::build::{BuildAdapter, GitSource, VerifiedGitCommit};
use ployz_core::deploy::PlatformImage;
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::image::OciPlatform;
use ployz_core::machine::rpc::{MachineRpcResponder, MachineRpcResponse};
use ployz_core::operation::{
    BuildLogChunk, BuildPlatformFailure, BuildToolchainEvidence, FailureMessage,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineBuildStartRpcRequest {
    pub operation_id: OperationId,
    pub source: GitSource,
    pub adapter: BuildAdapter,
    pub platform: OciPlatform,
    pub timeout_millis: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineBuildStartRpcOk {
    pub machine_id: MachineId,
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

impl MachineRpcResponder for MachineBuildStartRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self { machine_id, .. } = self;
        machine_id
    }
}

pub type MachineBuildStartRpcResponse =
    MachineRpcResponse<MachineBuildStartRpcOk, MachineBuildStartDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineBuildStartDomainError {
    AlreadyRunning,
    PlatformFailed {
        failure: BuildPlatformFailure,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
    Cancelled {
        cleanup: MachineBuildCleanupOutcome,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
    TimedOut {
        message: FailureMessage,
        cleanup: MachineBuildCleanupOutcome,
        #[serde(flatten)]
        log_summary: BuildLogSummary,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineBuildCleanupOutcome {
    Confirmed,
    Unconfirmed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineBuildCancelRpcRequest {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineBuildCancelRpcOk {
    pub machine_id: MachineId,
    pub outcome: MachineBuildCancelOutcome,
}

impl MachineRpcResponder for MachineBuildCancelRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        let Self {
            machine_id,
            outcome: _,
        } = self;
        machine_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineBuildCancelOutcome {
    Requested,
    NotRunning,
}

pub type MachineBuildCancelRpcResponse =
    MachineRpcResponse<MachineBuildCancelRpcOk, MachineBuildCancelDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineBuildCancelDomainError {
    CancelFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineBuildLogFrame {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub platform: OciPlatform,
    pub sequence: u64,
    pub chunk: BuildLogChunk,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_response_keeps_flat_log_summary_wire_shape() {
        let digest = ployz_core::image::OciDigest::try_new(format!("sha256:{}", "a".repeat(64)))
            .expect("digest");
        let source = GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "secret",
            None::<String>,
        )
        .expect("source");
        let response = MachineBuildStartRpcOk {
            machine_id: MachineId::try_new("machine-a").expect("machine"),
            image: PlatformImage {
                seed: MachineId::try_new("machine-a").expect("machine"),
                manifest_digest: digest.clone(),
                image_id: digest.clone(),
                availability_expires_at: ployz_core::deploy::ImageAvailabilityExpiresAt::try_new(
                    4_102_444_800,
                )
                .expect("expiry"),
            },
            verified_commit: VerifiedGitCommit::from_source(&source),
            toolchain: BuildToolchainEvidence {
                buildkit_image: digest,
                adapter: ployz_core::operation::BuildAdapterToolchainEvidence::Dockerfile,
            },
            log_summary: BuildLogSummary::new(8, 13),
        };

        let encoded = serde_json::to_value(&response).expect("encode success");
        assert_eq!(
            encoded.get("final_log_sequence"),
            Some(&serde_json::json!(8))
        );
        assert_eq!(
            encoded.get("omitted_log_bytes"),
            Some(&serde_json::json!(13))
        );
        assert!(encoded.get("log_summary").is_none());
        assert_eq!(
            serde_json::from_value::<MachineBuildStartRpcOk>(encoded.clone())
                .expect("decode success"),
            response
        );
        let mut unknown = encoded;
        unknown
            .as_object_mut()
            .expect("success object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<MachineBuildStartRpcOk>(unknown).is_err());
    }

    #[test]
    fn build_log_frame_rejects_unknown_or_oversized_payloads() {
        let frame = serde_json::json!({
            "operation_id": "build-1",
            "machine_id": "machine-a",
            "platform": {"os": "linux", "architecture": "amd64"},
            "sequence": 1,
            "chunk": "hello",
            "credential": "must-not-fit"
        });
        assert!(serde_json::from_value::<MachineBuildLogFrame>(frame).is_err());

        let frame = serde_json::json!({
            "operation_id": "build-1",
            "machine_id": "machine-a",
            "platform": {"os": "linux", "architecture": "amd64"},
            "sequence": 1,
            "chunk": "x".repeat(ployz_core::operation::MAX_BUILD_LOG_CHUNK_BYTES + 1)
        });
        assert!(serde_json::from_value::<MachineBuildLogFrame>(frame).is_err());
    }

    #[test]
    fn timed_out_response_preserves_typed_cleanup_outcome() {
        let error = MachineBuildStartDomainError::TimedOut {
            message: FailureMessage::try_new("deadline exceeded").expect("message"),
            cleanup: MachineBuildCleanupOutcome::Unconfirmed,
            log_summary: BuildLogSummary::new(3, 5),
        };
        let encoded = serde_json::to_value(&error).expect("encode timeout");
        assert_eq!(
            encoded,
            serde_json::json!({
                "error": "timed_out",
                "message": "deadline exceeded",
                "cleanup": "unconfirmed",
                "final_log_sequence": 3,
                "omitted_log_bytes": 5,
            })
        );
        assert_eq!(
            serde_json::from_value::<MachineBuildStartDomainError>(encoded.clone())
                .expect("decode timeout"),
            error
        );
        let mut unknown = encoded;
        unknown
            .as_object_mut()
            .expect("timeout object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<MachineBuildStartDomainError>(unknown).is_err());
    }

    #[test]
    fn cancelled_response_preserves_typed_cleanup_outcome() {
        let error = MachineBuildStartDomainError::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            log_summary: BuildLogSummary::new(2, 7),
        };
        let encoded = serde_json::to_value(&error).expect("encode cancellation");
        assert_eq!(
            encoded,
            serde_json::json!({
                "error": "cancelled",
                "cleanup": "confirmed",
                "final_log_sequence": 2,
                "omitted_log_bytes": 7,
            })
        );
        assert_eq!(
            serde_json::from_value::<MachineBuildStartDomainError>(encoded)
                .expect("decode cancellation"),
            error
        );
    }
}
