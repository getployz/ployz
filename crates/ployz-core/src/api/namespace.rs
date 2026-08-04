use serde::{Deserialize, Serialize};

use crate::ids::{NamespaceId, OperationId};
use crate::operation::EventSequence;

use super::ops::{AcceptedOperation, OperationApiResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct NamespaceRemoveRequest {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
}

pub type NamespaceRemoveResponse = OperationApiResponse<AcceptedOperation, NamespaceRemoveError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum NamespaceRemoveError {
    #[error("namespace {} is reserved for Ployz system services", .namespace_id.as_str())]
    ReservedSystemNamespace {
        operation_id: OperationId,
        namespace_id: NamespaceId,
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
    #[error("namespace remove {} unavailable: {message}", .operation_id.as_str())]
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
