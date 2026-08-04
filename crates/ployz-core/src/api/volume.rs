use serde::{Deserialize, Serialize};

use super::core_types::*;
use super::ops::{AcceptedOperation, OperationApiResponse};

pub use crate::operation::VolumeCreateRequest;

pub type VolumeCreateResponse = OperationApiResponse<AcceptedOperation, VolumeCreateError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeCreateError {
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
    #[error("volume create {} unavailable: {message}", .operation_id.as_str())]
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
pub struct VolumeListRequest {}

pub type VolumeListResponse = OperationApiResponse<VolumeListResult, VolumeListError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct VolumeListResult {
    pub volumes: Vec<VolumeSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct VolumeSnapshot {
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
    pub machine_id: MachineId,
    pub kind: VolumeKind,
    /// Service ids are sorted and deduplicated by the projection owner.
    pub referencing_services: Vec<ServiceId>,
    pub testimony: VolumeTestimony,
    pub status: VolumeStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeTestimony {
    Available {
        used_bytes: u64,
        last_write_unix_seconds: u64,
    },
    Unavailable,
    NoAnswer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum VolumeStatus {
    InUse,
    Orphaned,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeListError {
    #[error("volume list unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct VolumeRemoveRequest {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
}

pub type VolumeRemoveResponse = OperationApiResponse<AcceptedOperation, VolumeRemoveError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeRemoveError {
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
    #[error("volume remove {} unavailable: {message}", .operation_id.as_str())]
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
