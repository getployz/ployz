use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::ops::OperationApiResponse;

pub type ServiceListResponse = OperationApiResponse<ServiceListResult, ServiceListError>;

pub type ServiceInspectResponse = OperationApiResponse<ServiceSnapshot, ServiceInspectError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceInspectRequest {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceListResult {
    pub services: Vec<ServiceSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceSnapshot {
    pub active: ServingTargetEntry,
    pub route_bindings: Vec<RouteBindingState>,
    pub testimony: ServiceTestimony,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceTestimony {
    pub ready_container_count: usize,
    pub observed_container_count: usize,
    pub machines: Vec<ServiceMachineTestimony>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceContainerTestimony {
    pub observation: ManagedContainerObservation,
    pub membership: ServiceContainerMembership,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ServiceContainerMembership {
    ServingTargetMember,
    RetainedEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum ServiceListError {
    #[error("service list unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum ServiceInspectError {
    #[error("no such service {}", .service_id.as_str())]
    NoSuchService { service_id: ServiceId },
    #[error("service inspect unavailable: {message}")]
    Unavailable { message: String },
}
