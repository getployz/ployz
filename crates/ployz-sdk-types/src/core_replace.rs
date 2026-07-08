use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::ops::{AcceptedOperation, OperationApiResponse};

pub type CoreReplaceResponse = OperationApiResponse<AcceptedOperation, CoreReplaceError>;

pub type CoreReplaceReportResponse =
    OperationApiResponse<CoreReplaceReported, CoreReplaceReportError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CoreReplaceRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub successor_nats_url: MachineJoinRuntimeNatsUrl,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CoreReplaceReportRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub outcome: CoreReplaceReportOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreReplaceReportOutcome {
    Completed,
    Failed { failure: CoreReplaceFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CoreReplaceReported {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub last_event_sequence: EventSequence,
    pub outcome: CoreReplaceReportOutcome,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum CoreReplaceError {
    #[error("no such machine {} for operation {}", .machine_id.as_str(), .operation_id.as_str())]
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error("core replace {} unavailable: {message}", .operation_id.as_str())]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum CoreReplaceReportError {
    #[error("operation {} is unknown", .operation_id.as_str())]
    UnknownOperation { operation_id: OperationId },
    #[error("core replace report {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
}
