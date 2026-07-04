//! Machine placement usability: the one rule that excludes a machine from
//! new workload placement — durable operator intent (Machine Lifecycle).
//!
//! Liveness is never inferred here or anywhere else (ADR 0027): a dead
//! machine answers at the point of use — it does not reply to a placement
//! RPC, and its upstreams fail at dial time. Observation age is display
//! evidence for operators, not an input to behavior. The control-side gate
//! below is interim: once placement is bid-based, a draining machine
//! declines its own bids and this check moves into the machine.

use crate::state::MachineLifecycle;
use std::time::Duration;

/// How often each machine publishes its own reality (container snapshot,
/// public ip, role status) into the observation KV.
pub const OBSERVATION_PUBLISH_INTERVAL: Duration = Duration::from_secs(30);

/// Why a machine is excluded from placement. Only operator intent excludes
/// today; future reasons (placement constraints) join as their signals land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUsabilityReason {
    Draining,
}

#[must_use]
pub fn placement_rejection(lifecycle: MachineLifecycle) -> Option<MachineUsabilityReason> {
    match lifecycle {
        MachineLifecycle::Active => None,
        MachineLifecycle::Draining => Some(MachineUsabilityReason::Draining),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_draining_excludes_placement() {
        assert_eq!(placement_rejection(MachineLifecycle::Active), None);
        assert_eq!(
            placement_rejection(MachineLifecycle::Draining),
            Some(MachineUsabilityReason::Draining)
        );
    }
}
