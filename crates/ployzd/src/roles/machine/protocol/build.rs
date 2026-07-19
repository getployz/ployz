use ployz_core::build::{
    BuildExecutorCancelDomainError, BuildExecutorCancelOk, BuildExecutorCancelOutcome,
    BuildExecutorCancelRequest, BuildExecutorCleanupOutcome, BuildExecutorLogFrame,
    BuildExecutorStartDomainError, BuildExecutorStartOk, BuildExecutorStartRequest,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::rpc::MachineRpcResponse;
use ployz_core::operation::FailureMessage;
use serde::{Deserialize, Serialize};

pub type MachineBuildStartRpcRequest = BuildExecutorStartRequest;
pub type MachineBuildStartRpcOk = BuildExecutorStartOk;
pub type MachineBuildStartDomainError = BuildExecutorStartDomainError;
pub type MachineBuildCleanupOutcome = BuildExecutorCleanupOutcome;
pub type MachineBuildStartRpcResponse =
    MachineRpcResponse<MachineBuildStartRpcOk, MachineBuildStartDomainError>;

pub type MachineBuildCancelRpcRequest = BuildExecutorCancelRequest;
pub type MachineBuildCancelRpcOk = BuildExecutorCancelOk;
pub type MachineBuildCancelOutcome = BuildExecutorCancelOutcome;
pub type MachineBuildCancelDomainError = BuildExecutorCancelDomainError;
pub type MachineBuildCancelRpcResponse =
    MachineRpcResponse<MachineBuildCancelRpcOk, MachineBuildCancelDomainError>;

pub type MachineBuildLogFrame = BuildExecutorLogFrame;

pub use ployz_core::build::{BuildExecutorAcceptance, BuildExecutorOrigin, BuildLogSummary};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineBuildCachePruneRpcRequest {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineBuildCachePruneRpcOk {
    pub machine_id: MachineId,
    pub evidence: ployz_core::operation::BuildCachePruneEvidence,
}

impl ployz_core::machine::rpc::MachineRpcResponder for MachineBuildCachePruneRpcOk {
    fn responder_machine_id(&self) -> &MachineId {
        &self.machine_id
    }
}

pub type MachineBuildCachePruneRpcResponse =
    MachineRpcResponse<MachineBuildCachePruneRpcOk, MachineBuildCachePruneDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineBuildCachePruneDomainError {
    PruneFailed { message: FailureMessage },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::{GitSource, VerifiedGitCommit};
    use ployz_core::deploy::{ImageAvailabilityExpiresAt, PlatformImage};
    use ployz_core::image::{OciDigest, OciPlatform};
    use ployz_core::operation::{BuildAdapterToolchainEvidence, BuildToolchainEvidence};

    fn acceptance(machine_id: &MachineId) -> BuildExecutorAcceptance {
        BuildExecutorAcceptance {
            operation_id: OperationId::try_new("build-1").expect("operation id"),
            origin: BuildExecutorOrigin::Cluster {
                machine_id: machine_id.clone(),
            },
            image_seed: machine_id.clone(),
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        }
    }

    #[test]
    fn successful_response_keeps_flat_log_summary_wire_shape() {
        let digest = OciDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        let source = GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "redacted-test-value",
            None::<String>,
        )
        .expect("source");
        let machine_id = MachineId::try_new("machine-a").expect("machine");
        let response = MachineBuildStartRpcOk {
            machine_id: machine_id.clone(),
            acceptance: acceptance(&machine_id),
            image: PlatformImage {
                seed: machine_id,
                manifest_digest: digest.clone(),
                image_id: digest.clone(),
                availability_expires_at: ImageAvailabilityExpiresAt::try_new(4_102_444_800)
                    .expect("expiry"),
            },
            verified_commit: VerifiedGitCommit::from_source(&source),
            toolchain: BuildToolchainEvidence {
                buildkit_image: digest,
                adapter: BuildAdapterToolchainEvidence::Dockerfile,
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
    fn timed_out_response_preserves_typed_cleanup_outcome() {
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        let error = MachineBuildStartDomainError::TimedOut {
            acceptance: acceptance(&machine_id),
            message: FailureMessage::try_new("deadline exceeded").expect("message"),
            cleanup: MachineBuildCleanupOutcome::Unconfirmed,
            log_summary: BuildLogSummary::new(3, 5),
        };
        let encoded = serde_json::to_value(&error).expect("encode timeout");
        assert_eq!(
            encoded,
            serde_json::json!({
                "error": "timed_out",
                "acceptance": {
                    "operation_id": "build-1",
                    "origin": {"origin": "cluster", "machine_id": "machine-a"},
                    "image_seed": "machine-a",
                    "platform": {"os": "linux", "architecture": "amd64"},
                },
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
}
