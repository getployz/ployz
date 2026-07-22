use ployz_core::build::{
    BuildExecutorCancelDomainError, BuildExecutorCancelOk, BuildExecutorCancelOutcome,
    BuildExecutorCancelRequest, BuildExecutorCleanupOutcome, BuildExecutorLogFrame,
    BuildExecutorStartDomainError, BuildExecutorStartRequest, BuildExecutorStatus,
    BuildExecutorStatusDomainError, BuildExecutorStatusRequest,
};
use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::rpc::MachineRpcResponse;
use ployz_core::operation::FailureMessage;
use serde::{Deserialize, Serialize};

pub type MachineBuildStartRpcRequest = BuildExecutorStartRequest;
pub type MachineBuildStartDomainError = BuildExecutorStartDomainError;
pub type MachineBuildCleanupOutcome = BuildExecutorCleanupOutcome;
pub type MachineBuildStartRpcOk = MachineBuildExecutorRpcOk<BuildExecutorAcceptance>;
pub type MachineBuildStartRpcResponse =
    MachineRpcResponse<MachineBuildStartRpcOk, MachineBuildStartDomainError>;

pub type MachineBuildStatusRpcRequest = BuildExecutorStatusRequest;
pub type MachineBuildStatusDomainError = BuildExecutorStatusDomainError;
pub type MachineBuildStatusRpcOk = MachineBuildExecutorRpcOk<BuildExecutorStatus>;
pub type MachineBuildStatusRpcResponse =
    MachineRpcResponse<MachineBuildStatusRpcOk, MachineBuildStatusDomainError>;

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
    use ployz_core::image::OciPlatform;

    fn acceptance(machine_id: &MachineId) -> BuildExecutorAcceptance {
        BuildExecutorAcceptance {
            operation_id: OperationId::try_new("build-1").expect("operation id"),
            assignment: BuildExecutorAssignment::Cluster {
                machine_id: machine_id.clone(),
            },
            platform: OciPlatform::try_new("linux", "amd64").expect("platform"),
            request_commitment: ployz_core::build::BuildRequestCommitment::try_from("a".repeat(64))
                .expect("commitment"),
        }
    }

    #[test]
    fn successful_start_response_has_a_strict_nested_executor_envelope() {
        let machine_id = MachineId::try_new("machine-a").expect("machine");
        let response = MachineBuildStartRpcOk::from((machine_id.clone(), acceptance(&machine_id)));

        let encoded = serde_json::to_value(&response).expect("encode success");
        assert_eq!(
            encoded,
            serde_json::json!({
                "machine_id": "machine-a",
                "executor": {
                    "operation_id": "build-1",
                    "assignment": {"executor": "cluster", "machine_id": "machine-a"},
                    "platform": {"os": "linux", "architecture": "amd64"},
                    "request_commitment": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
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
    }

    #[test]
    fn running_status_response_has_strict_provenance_and_activity() {
        let machine_id = MachineId::try_new("machine-a").expect("machine");
        let response = MachineBuildStatusRpcOk::from((
            machine_id.clone(),
            BuildExecutorStatus {
                acceptance: acceptance(&machine_id),
                state: ployz_core::build::BuildExecutorState::Running {
                    log_summary: BuildLogSummary::new(8, 13),
                },
            },
        ));
        let encoded = serde_json::to_value(&response).expect("encode status");
        assert_eq!(
            encoded,
            serde_json::json!({
                "machine_id": "machine-a",
                "executor": {
                    "acceptance": {
                        "operation_id": "build-1",
                        "assignment": {"executor": "cluster", "machine_id": "machine-a"},
                        "platform": {"os": "linux", "architecture": "amd64"},
                        "request_commitment": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    },
                    "state": {
                        "status": "running",
                        "final_log_sequence": 8,
                        "omitted_log_bytes": 13,
                    },
                },
            })
        );
        assert_eq!(
            serde_json::from_value::<MachineBuildStatusRpcOk>(encoded.clone())
                .expect("decode status"),
            response
        );
        let mut unknown = encoded;
        unknown
            .get_mut("executor")
            .and_then(serde_json::Value::as_object_mut)
            .expect("executor object")
            .insert("unexpected".to_owned(), serde_json::json!(true));
        assert!(serde_json::from_value::<MachineBuildStatusRpcOk>(unknown).is_err());
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
                    "request_commitment": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
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
