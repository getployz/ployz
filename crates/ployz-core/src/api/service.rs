use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, NamespaceId, OperationId, ServiceId};
use crate::intent::{RouteBindingState, ServingTargetEntry};
use crate::machine::ManagedContainerObservation;
use crate::operation::EventSequence;

use super::ops::{AcceptedOperation, OperationApiResponse};

pub type ServiceListResponse = OperationApiResponse<ServiceListResult, ServiceListError>;

pub type ServiceInspectResponse = OperationApiResponse<ServiceSnapshot, ServiceInspectError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceInspectRequest {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceRestartRequest {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
}

pub type ServiceRestartResponse = OperationApiResponse<AcceptedOperation, ServiceRestartError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceListResult {
    pub services: Vec<ServiceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceSnapshot {
    pub active: ServingTargetEntry,
    pub route_bindings: Vec<RouteBindingState>,
    pub testimony: ServiceTestimony,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceTestimony {
    pub ready_container_count: usize,
    pub observed_container_count: usize,
    pub machines: Vec<ServiceMachineTestimony>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServiceContainerTestimony {
    pub observation: ManagedContainerObservation,
    pub membership: ServiceContainerMembership,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ServiceContainerMembership {
    ServingTargetMember,
    RetainedEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceMachineTestimony {
    Answered {
        machine_id: MachineId,
        containers: Vec<ServiceContainerTestimony>,
    },
    NoAnswer {
        machine_id: MachineId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum ServiceListError {
    #[error("service list unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum ServiceInspectError {
    #[error("no such service {}", .service_id.as_str())]
    NoSuchService { service_id: ServiceId },
    #[error("service inspect unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum ServiceRestartError {
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
    #[error("service restart {} unavailable: {message}", .operation_id.as_str())]
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
