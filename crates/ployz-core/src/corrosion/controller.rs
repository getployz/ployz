//! Preferred-controller identity and admission policy.

pub use crate::ids::ControllerRevision;
use crate::machine::MachineName;
use std::time::Duration;

use super::document::{ControllerDocument, CorrosionTimestamp};

/// Whether `machine_id` still owns the exact appointment in the current row.
#[must_use]
pub fn owns_current_controller_appointment(
    controller: &ControllerDocument,
    machine_id: &MachineName,
    appointment_id: &ControllerRevision,
) -> bool {
    &controller.preferred_machine_id == machine_id && &controller.appointment_id == appointment_id
}

/// Whether this machine has enough cluster visibility to run controller work.
///
/// `visible_members` includes the answering machine.
///
/// `ponytail:` two-node splits are accepted; larger rosters only block a node
/// that cannot see any peer.
#[must_use]
pub const fn controller_visibility_allows_work(
    accepted_roster_members: usize,
    visible_members: usize,
) -> bool {
    match accepted_roster_members {
        0 => false,
        1 | 2 => true,
        _ => visible_members >= 2,
    }
}

/// Whether an appointment has missed its full heartbeat timeout.
///
/// A future heartbeat remains fresh. Clock skew is intentionally not a
/// controller-election protocol; a later tick can recover from it.
#[must_use]
pub fn controller_heartbeat_is_stale(
    now: CorrosionTimestamp,
    heartbeat_at: CorrosionTimestamp,
    timeout: Duration,
) -> bool {
    now.saturating_since(heartbeat_at) > timeout
}
