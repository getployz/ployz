//! Composition layer that wires Ployz product ports to adapters.
//!
//! Feature modules should depend on Ployz-owned traits. This module is allowed
//! to assemble concrete adapters and pass them into product orchestration.

use crate::adapters::polis::{PolisDomainStatus, PolisMachineMembership, PolisServingSnapshots};
use crate::domain::DomainStatusPort;
use crate::machine::MachineMembershipPort;
use crate::operation::ScopeId;
use crate::serving::ServingSnapshotPort;

#[must_use]
pub fn in_memory_machine_membership() -> impl MachineMembershipPort {
    PolisMachineMembership::in_memory()
}

#[must_use]
pub fn in_memory_domain_status(scope: ScopeId) -> impl DomainStatusPort {
    PolisDomainStatus::in_memory(scope)
}

#[must_use]
pub fn in_memory_serving_snapshots(scope: ScopeId) -> impl ServingSnapshotPort {
    PolisServingSnapshots::in_memory(scope)
}
