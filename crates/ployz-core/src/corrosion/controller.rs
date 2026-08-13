//! Preferred-controller identity and admission policy.

use crate::machine::MachineName;
use std::time::Duration;

use super::document::{ControllerDocument, CorrosionTimestamp};

/// Whether `machine_id` is preferred in this node's current view.
#[must_use]
pub fn is_preferred_controller(controller: &ControllerDocument, machine_id: &MachineName) -> bool {
    &controller.preferred_machine_name == machine_id
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
