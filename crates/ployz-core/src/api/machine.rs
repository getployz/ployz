use crate::deploy::ZfsPoolName;
use crate::ids::{MachineId, OperationId};
use crate::install::InstallArtifactVersion;
use crate::intent::ActiveMachineState;
use crate::machine::{
    GatewayStatusObservation, MachineContainerUnavailableReason, MachineDiskSpace,
    MachineEndpointObservation, StorageCapability, StrandedVolumeAlarm,
};
use crate::operation::{CancellationReason, EventSequence};

use super::ops::{AcceptedOperation, OperationApiResponse};
use serde::{Deserialize, Serialize};

pub type MachineListResponse = OperationApiResponse<MachineListResult, MachineListError>;

pub type MachineInspectResponse = OperationApiResponse<MachineSnapshot, MachineInspectError>;

pub type MachineUpdateResponse = OperationApiResponse<AcceptedOperation, MachineUpdateError>;
pub type MachineStoragePrepareResponse =
    OperationApiResponse<AcceptedOperation, MachineStoragePrepareError>;
pub type MachineStoragePrepareCancelResponse =
    OperationApiResponse<AcceptedOperation, MachineStoragePrepareCancelError>;
pub type MachineBuildCachePruneResponse =
    OperationApiResponse<AcceptedOperation, MachineBuildCachePruneError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineInspectRequest {
    pub machine_id: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineUpdateRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target_version: InstallArtifactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineStoragePrepareRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pool: Option<ZfsPoolName>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineStoragePrepareCancelRequest {
    pub operation_id: OperationId,
    pub reason: CancellationReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineBuildCachePruneRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
}

/// One request shape for both lifecycle endpoints: the endpoint carries the
/// verb (drain or resume), the body only names the operation and machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineLifecycleRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
}
pub type MachineDrainResponse = OperationApiResponse<AcceptedOperation, MachineLifecycleError>;
pub type MachineResumeResponse = OperationApiResponse<AcceptedOperation, MachineLifecycleError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineLifecycleError {
    #[error("no such machine {} for operation {}", .machine_id.as_str(), .operation_id.as_str())]
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error("machine lifecycle {} unavailable: {message}", .operation_id.as_str())]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineListResult {
    pub machines: Vec<MachineSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineSnapshot {
    pub active: ActiveMachineState,
    pub testimony: MachineTestimony,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub storage_alarms: Vec<StrandedVolumeAlarm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineTestimony {
    Answered {
        endpoints: Option<MachineEndpointObservation>,
        gateway: Option<Box<GatewayStatusObservation>>,
        containers: MachineContainerAvailability,
        disk_space: MachineDiskSpace,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        storage: Option<StorageCapability>,
        /// When this machine last self-reported, as display evidence for the
        /// operator. Never an input to behavior: liveness surfaces at the point
        /// of use (ADR 0040).
        last_observed_at_unix_seconds: u64,
    },
    NoAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineContainerAvailability {
    Answered {
        observed_count: usize,
    },
    Unavailable {
        reason: MachineContainerUnavailableReason,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineListError {
    #[error("machine list unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineInspectError {
    #[error("no such machine {}", .machine_id.as_str())]
    NoSuchMachine { machine_id: MachineId },
    #[error("machine inspect unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineUpdateError {
    #[error("no such machine {} for operation {}", .machine_id.as_str(), .operation_id.as_str())]
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error(
        "operation {}: updating the current machine {} is unsupported",
        .operation_id.as_str(),
        .machine_id.as_str()
    )]
    CurrentMachineUnsupported {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error(
        "machine substrate for {} is busy with operation {}",
        .machine_id.as_str(),
        .owner_operation_id.as_str()
    )]
    MachineSubstrateBusy {
        operation_id: OperationId,
        machine_id: MachineId,
        owner_operation_id: OperationId,
    },
    #[error("machine update {} unavailable: {message}", .operation_id.as_str())]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineStoragePrepareError {
    #[error("no such machine {} for operation {}", .machine_id.as_str(), .operation_id.as_str())]
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error(
        "machine substrate for {} is busy with operation {}",
        .machine_id.as_str(),
        .owner_operation_id.as_str()
    )]
    MachineSubstrateBusy {
        operation_id: OperationId,
        machine_id: MachineId,
        owner_operation_id: OperationId,
    },
    #[error("machine storage prepare {} unavailable: {message}", .operation_id.as_str())]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineStoragePrepareCancelError {
    #[error("no such storage prepare operation {}", .operation_id.as_str())]
    NoSuchOperation { operation_id: OperationId },
    #[error("machine storage prepare cancellation {} unavailable: {message}", .operation_id.as_str())]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineBuildCachePruneError {
    #[error("no such machine {} for operation {}", .machine_id.as_str(), .operation_id.as_str())]
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error("machine build cache prune {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
    #[error("operation {} already recorded a different event at sequence {}", .operation_id.as_str(), .sequence.get())]
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}
