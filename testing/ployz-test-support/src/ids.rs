//! Typed-id constructors for test literals: panic loudly when a test
//! literal does not satisfy the id's invariants.

use ployz_core::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId, StepId,
};
use ployz_core::operation::{RouteHostname, RoutePort};

#[must_use]
pub fn machine_id(value: &str) -> MachineId {
    MachineId::try_new(value).expect("valid machine id")
}

#[must_use]
pub fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

#[must_use]
pub fn namespace_id(value: &str) -> NamespaceId {
    NamespaceId::try_new(value).expect("valid namespace id")
}

#[must_use]
pub fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

#[must_use]
pub fn namespace_revision_entry_id(value: &str) -> NamespaceRevisionEntryId {
    NamespaceRevisionEntryId::try_new(value).expect("valid namespace revision entry id")
}

#[must_use]
pub fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

#[must_use]
pub fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}

#[must_use]
pub fn route_hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

#[must_use]
pub fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
}
