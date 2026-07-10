use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::{EventSequence, MachineId, OperationId};
use crate::ops::{AcceptedOperation, OperationApiResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NetworkResolveRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NetworkResolveResult {
    pub name: String,
    pub addresses: Vec<String>,
    pub machines: Vec<NetworkResolveMachineTestimony>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkResolveMachineTestimony {
    Answered { machine_id: MachineId },
    NoAnswer { machine_id: MachineId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkResolveError {
    #[error("invalid internal service name {name:?}")]
    InvalidName { name: String },
    #[error("network resolve unavailable: {message}")]
    Unavailable { message: String },
}

pub type NetworkResolveResponse = OperationApiResponse<NetworkResolveResult, NetworkResolveError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NetworkRepairRequest {
    pub operation_id: OperationId,
}

pub type NetworkRepairResponse = OperationApiResponse<AcceptedOperation, NetworkRepairError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairError {
    #[error("network repair {} unavailable: {message}", .operation_id.as_str())]
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
