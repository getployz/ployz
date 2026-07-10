use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::ops::{AcceptedOperation, OperationApiResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeploySubmitRequest {
    pub idempotency_key: OperationIdempotencyKey,
    pub target: DeployRequest,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registry_credentials: Vec<DeployRegistryCredential>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct DeployRegistryCredential {
    pub service_id: ServiceId,
    pub credential: RegistryCredential,
}

pub type DeploySubmitResponse = OperationApiResponse<AcceptedOperation, DeploySubmitError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum DeploySubmitError {
    #[error("deploy target invalid for operation {}: {message}", .operation_id.as_str())]
    InvalidTarget {
        operation_id: OperationId,
        message: FailureMessage,
    },
    #[error(
        "namespace {} is busy with operation {}",
        .namespace_id.as_str(),
        .owner_operation_id.as_str()
    )]
    ResourceBusy {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        owner_operation_id: OperationId,
    },
    #[error("deploy submit {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
    #[error(
        "operation {} already recorded a different event at sequence {}",
        .operation_id.as_str(),
        .sequence.get()
    )]
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}
