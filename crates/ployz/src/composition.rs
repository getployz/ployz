//! Composition layer that wires Ployz product ports to adapters.
//!
//! Feature modules should depend on Ployz-owned traits. This module is allowed
//! to assemble concrete adapters and pass them into product orchestration.

use crate::adapters::polis::PolisMachineMembership;
use crate::machine::MachineMembershipPort;

#[must_use]
pub fn in_memory_machine_membership() -> impl MachineMembershipPort {
    PolisMachineMembership::in_memory()
}
