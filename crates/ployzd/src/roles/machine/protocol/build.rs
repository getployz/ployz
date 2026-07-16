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
    pub final_log_sequence: u64,
    pub omitted_log_bytes: u64,
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
        final_log_sequence: u64,
        omitted_log_bytes: u64,
    },
    Cancelled {
        cleanup: MachineBuildCleanupOutcome,
        final_log_sequence: u64,
        omitted_log_bytes: u64,
    },
    TimedOut {
        message: FailureMessage,
        cleanup: MachineBuildCleanupOutcome,
        final_log_sequence: u64,
        omitted_log_bytes: u64,
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
            final_log_sequence: 3,
            omitted_log_bytes: 5,
        };
        let encoded = serde_json::to_value(&error).expect("encode timeout");
        assert_eq!(
            encoded.get("cleanup").expect("cleanup field"),
            "unconfirmed"
        );
        assert_eq!(
            serde_json::from_value::<MachineBuildStartDomainError>(encoded)
                .expect("decode timeout"),
            error
        );
    }

    #[test]
    fn cancelled_response_preserves_typed_cleanup_outcome() {
        let error = MachineBuildStartDomainError::Cancelled {
            cleanup: MachineBuildCleanupOutcome::Confirmed,
            final_log_sequence: 2,
            omitted_log_bytes: 7,
        };
        let encoded = serde_json::to_value(&error).expect("encode cancellation");
        assert_eq!(encoded.get("cleanup").expect("cleanup field"), "confirmed");
        assert_eq!(
            serde_json::from_value::<MachineBuildStartDomainError>(encoded)
                .expect("decode cancellation"),
            error
        );
    }
}
