use ployz_core::build::{
    BuildCachePruneAcceptance, BuildCachePruneCancelOk, BuildCachePruneCancelRequest,
    BuildCachePruneStartRequest, BuildCachePruneStatus, BuildCachePruneStatusRequest,
    BuildExecutorCancelDomainError, BuildExecutorCancelOk, BuildExecutorCancelOutcome,
    BuildExecutorCancelRequest, BuildExecutorCleanupOutcome, BuildExecutorLogFrame,
    BuildExecutorStartDomainError, BuildExecutorStartOk, BuildExecutorStartRequest,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::rpc::MachineRpcResponse;
use ployz_core::operation::FailureMessage;
use serde::{Deserialize, Serialize};

pub type MachineBuildStartRpcRequest = BuildExecutorStartRequest;
pub type MachineBuildStartDomainError = BuildExecutorStartDomainError;
pub type MachineBuildCleanupOutcome = BuildExecutorCleanupOutcome;
pub type MachineBuildStartRpcOk = MachineBuildExecutorRpcOk<BuildExecutorStartOk>;
pub type MachineBuildStartRpcResponse =
    MachineRpcResponse<MachineBuildStartRpcOk, MachineBuildStartDomainError>;

pub type MachineBuildCancelRpcRequest = BuildExecutorCancelRequest;
pub type MachineBuildCancelOutcome = BuildExecutorCancelOutcome;
pub type MachineBuildCancelDomainError = BuildExecutorCancelDomainError;
pub type MachineBuildCancelRpcOk = MachineBuildExecutorRpcOk<BuildExecutorCancelOk>;
pub type MachineBuildCancelRpcResponse =
    MachineRpcResponse<MachineBuildCancelRpcOk, MachineBuildCancelDomainError>;

pub type MachineBuildLogFrame = BuildExecutorLogFrame;

pub use ployz_core::build::{BuildExecutorAcceptance, BuildExecutorAssignment, BuildLogSummary};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineBuildExecutorRpcOk<T> {
    pub machine_id: MachineId,
    pub executor: T,
}

impl<T> MachineBuildExecutorRpcOk<T> {
    #[must_use]
    pub fn into_executor(self) -> T {
        self.executor
    }
}

impl<T> From<(MachineId, T)> for MachineBuildExecutorRpcOk<T> {
    fn from((machine_id, executor): (MachineId, T)) -> Self {
        Self {
            machine_id,
            executor,
        }
    }
}

impl<T> ployz_core::machine::rpc::MachineRpcResponder for MachineBuildExecutorRpcOk<T> {
    fn responder_machine_id(&self) -> &MachineId {
        &self.machine_id
    }
}

pub type MachineBuildCachePruneStartRpcRequest = BuildCachePruneStartRequest;
pub type MachineBuildCachePruneStartRpcOk = MachineBuildExecutorRpcOk<BuildCachePruneAcceptance>;
pub type MachineBuildCachePruneStartRpcResponse =
    MachineRpcResponse<MachineBuildCachePruneStartRpcOk, MachineBuildCachePruneStartDomainError>;

pub type MachineBuildCachePruneStatusRpcRequest = BuildCachePruneStatusRequest;
pub type MachineBuildCachePruneStatusRpcOk = MachineBuildExecutorRpcOk<BuildCachePruneStatus>;
pub type MachineBuildCachePruneStatusRpcResponse =
    MachineRpcResponse<MachineBuildCachePruneStatusRpcOk, MachineBuildCachePruneStatusDomainError>;

pub type MachineBuildCachePruneCancelRpcRequest = BuildCachePruneCancelRequest;
pub type MachineBuildCachePruneCancelRpcOk = MachineBuildExecutorRpcOk<BuildCachePruneCancelOk>;
pub type MachineBuildCachePruneCancelRpcResponse =
    MachineRpcResponse<MachineBuildCachePruneCancelRpcOk, MachineBuildCachePruneCancelDomainError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineBuildCachePruneStartDomainError {
    RuntimeUnavailable,
    RuntimeStopped,
    AlreadyRunning { operation_id: OperationId },
    StartFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineBuildCachePruneStatusDomainError {
    ReadFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineBuildCachePruneCancelDomainError {
    CancelFailed { message: FailureMessage },
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::{
        BuildCachePruneCancelOutcome, BuildExecutorSuccessCleanupEvidence, GitSource,
        VerifiedBuildSource,
    };
    use ployz_core::deploy::{ImageAvailabilityExpiresAt, PlatformImage};
    use ployz_core::image::{OciDigest, OciPlatform};
    use ployz_core::operation::{BuildAdapterToolchainEvidence, BuildToolchainEvidence};
    use serde::de::DeserializeOwned;

    fn assert_strict<T: Serialize + DeserializeOwned>(value: &T) {
        let encoded = serde_json::to_value(value).expect("encode");
        serde_json::from_value::<T>(encoded.clone()).expect("decode");
        let mut unknown = encoded.clone();
        unknown
            .as_object_mut()
            .expect("object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<T>(unknown).is_err());
        let mut unknown_executor = encoded;
        unknown_executor
            .get_mut("executor")
            .and_then(serde_json::Value::as_object_mut)
            .expect("executor")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<T>(unknown_executor).is_err());
    }

    fn assert_closed<T: Serialize + DeserializeOwned>(value: &T) {
        let encoded = serde_json::to_value(value).expect("encode");
        serde_json::from_value::<T>(encoded.clone()).expect("decode");
        let mut unknown = encoded;
        unknown
            .as_object_mut()
            .expect("object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<T>(unknown).is_err());
    }

    fn acceptance(machine_id: &MachineId) -> BuildExecutorAcceptance {
        BuildExecutorAcceptance {
            operation_id: OperationId::try_new("build-1").expect("operation id"),
            assignment: BuildExecutorAssignment::Cluster {
                machine_id: machine_id.clone(),
            },
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
        }
    }

    #[test]
    fn cache_prune_start_status_and_cancel_envelopes_are_strict_and_provenanced() {
        let machine_id = MachineId::try_new("machine-a").expect("machine");
        let operation_id = OperationId::try_new("prune-1").expect("operation");
        let start = MachineBuildCachePruneStartRpcOk::from((
            machine_id.clone(),
            BuildCachePruneAcceptance {
                operation_id: operation_id.clone(),
            },
        ));
        let status = MachineBuildCachePruneStatusRpcOk::from((
            machine_id.clone(),
            BuildCachePruneStatus::Queued {
                operation_id: operation_id.clone(),
                progress: 1,
            },
        ));
        let cancel = MachineBuildCachePruneCancelRpcOk::from((
            machine_id,
            BuildCachePruneCancelOk {
                operation_id: operation_id.clone(),
                outcome: BuildCachePruneCancelOutcome::Requested,
            },
        ));

        assert_strict(&start);
        assert_strict(&status);
        assert_strict(&cancel);
        assert_eq!(start.executor.operation_id, operation_id);
    }

    #[test]
    fn cache_prune_endpoint_domain_errors_are_typed_and_strict() {
        let operation_id = OperationId::try_new("prune-1").expect("operation");
        let start = MachineBuildCachePruneStartDomainError::AlreadyRunning { operation_id };
        let status = MachineBuildCachePruneStatusDomainError::ReadFailed {
            message: FailureMessage::try_new("status unavailable").expect("message"),
        };
        let cancel = MachineBuildCachePruneCancelDomainError::CancelFailed {
            message: FailureMessage::try_new("cancel unavailable").expect("message"),
        };
        assert_eq!(
            serde_json::to_value(&start).expect("start"),
            serde_json::json!({"error": "already_running", "operation_id": "prune-1"})
        );
        assert_closed(&start);
        assert_closed(&status);
        assert_closed(&cancel);
    }

    #[test]
    fn successful_start_response_has_a_strict_nested_executor_envelope() {
        let digest = OciDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest");
        let source = GitSource::try_new(
            "https://example.test/repo.git",
            "0123456789abcdef0123456789abcdef01234567",
            "git",
            "redacted-test-value",
            None::<String>,
        )
        .expect("source")
        .into();
        let machine_id = MachineId::try_new("machine-a").expect("machine");
        let response = MachineBuildStartRpcOk::from((
            machine_id.clone(),
            BuildExecutorStartOk {
                acceptance: acceptance(&machine_id),
                cleanup: BuildExecutorSuccessCleanupEvidence::confirmed(),
                image: PlatformImage {
                    seed: machine_id,
                    manifest_digest: digest.clone(),
                    image_id: digest.clone(),
                    availability_expires_at: ImageAvailabilityExpiresAt::try_new(4_102_444_800)
                        .expect("expiry"),
                },
                verified_source: VerifiedBuildSource::from_source(&source),
                toolchain: BuildToolchainEvidence {
                    buildkit_image: digest,
                    adapter: BuildAdapterToolchainEvidence::Dockerfile,
                },
                log_summary: BuildLogSummary::new(8, 13),
            },
        ));

        let encoded = serde_json::to_value(&response).expect("encode success");
        assert_eq!(
            encoded,
            serde_json::json!({
                "machine_id": "machine-a",
                "executor": {
                    "acceptance": {
                        "operation_id": "build-1",
                        "assignment": {"executor": "cluster", "machine_id": "machine-a"},
                        "platform": {"os": "linux", "architecture": "amd64"},
                    },
                    "cleanup": {"outcome": "confirmed"},
                    "image": {
                        "seed": "machine-a",
                        "manifest_digest": format!("sha256:{}", "a".repeat(64)),
                        "image_id": format!("sha256:{}", "a".repeat(64)),
                        "availability_expires_at": "4102444800",
                    },
                    "verified_source": {
                        "source": "git",
                        "url": "https://example.test/repo.git",
                        "commit": "0123456789abcdef0123456789abcdef01234567",
                    },
                    "toolchain": {
                        "buildkit_image": format!("sha256:{}", "a".repeat(64)),
                        "adapter": {"adapter": "dockerfile"},
                    },
                    "final_log_sequence": 8,
                    "omitted_log_bytes": 13,
                },
            })
        );
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

        let mut unknown_executor = serde_json::to_value(&response).expect("encode success");
        unknown_executor
            .get_mut("executor")
            .and_then(serde_json::Value::as_object_mut)
            .expect("executor object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<MachineBuildStartRpcOk>(unknown_executor).is_err());

        let mut unconfirmed_cleanup = serde_json::to_value(&response).expect("encode success");
        unconfirmed_cleanup
            .get_mut("executor")
            .and_then(|executor| executor.get_mut("cleanup"))
            .and_then(serde_json::Value::as_object_mut)
            .expect("cleanup object")
            .insert("outcome".to_owned(), serde_json::json!("unconfirmed"));
        assert!(serde_json::from_value::<MachineBuildStartRpcOk>(unconfirmed_cleanup).is_err());

        let mut unknown_cleanup = serde_json::to_value(&response).expect("encode success");
        unknown_cleanup
            .get_mut("executor")
            .and_then(|executor| executor.get_mut("cleanup"))
            .and_then(serde_json::Value::as_object_mut)
            .expect("cleanup object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<MachineBuildStartRpcOk>(unknown_cleanup).is_err());
    }

    #[test]
    fn successful_cancel_response_has_a_strict_nested_executor_envelope() {
        let machine_id = MachineId::try_new("machine-a").expect("machine");
        let response = MachineBuildCancelRpcOk::from((
            machine_id.clone(),
            BuildExecutorCancelOk {
                assignment: BuildExecutorAssignment::Cluster { machine_id },
                outcome: BuildExecutorCancelOutcome::Requested,
            },
        ));
        let encoded = serde_json::to_value(&response).expect("encode success");
        assert_eq!(
            encoded,
            serde_json::json!({
                "machine_id": "machine-a",
                "executor": {
                    "assignment": {"executor": "cluster", "machine_id": "machine-a"},
                    "outcome": {"outcome": "requested"},
                },
            })
        );
        assert_eq!(
            serde_json::from_value::<MachineBuildCancelRpcOk>(encoded.clone())
                .expect("decode success"),
            response
        );
        let mut unknown = encoded;
        unknown
            .as_object_mut()
            .expect("success object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<MachineBuildCancelRpcOk>(unknown).is_err());
    }

    #[test]
    fn timed_out_response_preserves_typed_cleanup_outcome() {
        let machine_id = MachineId::try_new("machine-a").expect("machine id");
        let error = MachineBuildStartDomainError::TimedOut {
            acceptance: Box::new(acceptance(&machine_id)),
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
                    "assignment": {"executor": "cluster", "machine_id": "machine-a"},
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
