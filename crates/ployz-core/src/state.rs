//! Runtime and intent state records.

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId};
use crate::machine::{IssuedJoinToken, MachineName};
use crate::nats_config::NatsAuthorizedUser;
use crate::ops::{RoutePort, RouteTarget};
use crate::roles::InstallRolePolicy;
use std::net::{IpAddr, SocketAddr};

/// Core-owned serving-target intent value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServingTargetEntry {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
}

/// Core-owned route-binding intent value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RouteBindingState {
    pub namespace_id: NamespaceId,
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
    pub service_id: ServiceId,
}

/// Core-owned active-machine roster value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveMachineState {
    pub machine_id: MachineId,
    pub name: MachineName,
    pub activated_by: OperationId,
    /// Durable operator intent for this machine (Machine Lifecycle in the
    /// glossary). Absent in records written before lifecycle existed, so the
    /// default is active.
    #[serde(default)]
    pub lifecycle: MachineLifecycle,
    /// Public NATS/operator endpoints recorded from machine testimony. Promotion
    /// requires at least one of these; mesh-private addresses never become
    /// promotion authority.
    pub control_endpoints: Vec<IpAddr>,
    /// WireGuard dial candidates recorded from machine testimony. The first is
    /// programmed initially; later candidates are for endpoint rotation.
    pub mesh_endpoints: Vec<SocketAddr>,
}

/// Epoch-stamped non-secret pending machine-add recovery hints mirrored for
/// core promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingMachineJoinRecoverySnapshot {
    pub epoch: ControlPlaneEpoch,
    pub pending: Vec<PendingMachineJoinRecovery>,
}

/// Non-secret pending machine-add recovery hint mirrored for core promotion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingMachineJoinRecovery {
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_token: IssuedJoinToken,
}

/// Monotonic control-plane generation, advertised with intent. A machine tells a
/// promoted core (higher epoch) from a healed old one (lower epoch) by comparing
/// it; the old core is repaired by an explicit operator core replacement command
/// after a partition. Owned by the core and bumped only on operator promotion (ADR
/// 0030/0031) — NATS carries the value, it does not define it (core NATS has no
/// epoch primitive; the ones that exist live in the JetStream we exited).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ControlPlaneEpoch(u64);

impl ControlPlaneEpoch {
    /// The epoch a core mints before it has ever been promoted.
    #[must_use]
    pub const fn initial() -> Self {
        Self(1)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The next generation, minted by a promotion to fence the core it succeeds
    /// (ADR 0031). `#[must_use]` because a bump only matters once persisted as the
    /// new epoch.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

/// Full operator intent visible to readers, stamped with the epoch it reflects.
/// The authorized-users grant set rides here too (ADR 0031): a promoted core
/// reuses it verbatim rather than re-deriving grants from the roster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IntentSnapshot {
    pub epoch: ControlPlaneEpoch,
    pub core_machine_id: MachineId,
    pub active_machines: Vec<ActiveMachineState>,
    pub route_bindings: Vec<RouteBindingState>,
    pub serving_target_entries: Vec<ServingTargetEntry>,
    pub authorized_users: Vec<NatsAuthorizedUser>,
}

impl IntentSnapshot {
    /// A specific machine's advertised control endpoints, if the core recorded any.
    #[must_use]
    pub fn control_endpoints_of(&self, machine_id: &MachineId) -> Option<&[IpAddr]> {
        self.active_machines
            .iter()
            .find(|machine| &machine.machine_id == machine_id)
            .map(|machine| machine.control_endpoints.as_slice())
            .filter(|endpoints| !endpoints.is_empty())
    }
}

/// The durable operator-intent state of a current machine identity. Controls
/// placement policy; runtime readiness comes from observations, never from
/// lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineLifecycle {
    #[default]
    Active,
    Draining,
}

/// Why a machine is excluded from new workload placement for one operation.
/// Operator intent excludes durably; unavailable machine facts exclude only
/// the current operation runtime snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUsabilityReason {
    Draining,
    FactsUnavailable,
}

#[must_use]
pub fn placement_rejection(lifecycle: MachineLifecycle) -> Option<MachineUsabilityReason> {
    match lifecycle {
        MachineLifecycle::Active => None,
        MachineLifecycle::Draining => Some(MachineUsabilityReason::Draining),
    }
}

/// Machine-owned endpoint facts reported with machine facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineEndpointObservation {
    pub machine_id: MachineId,
    pub control_endpoints: Vec<IpAddr>,
    pub mesh_endpoints: Vec<SocketAddr>,
}

/// Gateway role status fact reported by the gateway process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct GatewayStatusObservation {
    pub machine_id: MachineId,
    pub listen_addr: SocketAddr,
    pub serving: GatewayServingStatus,
    pub route_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GatewayServingStatus {
    Current,
    LastKnownGood,
    Unavailable,
}

#[cfg(test)]
mod tests {
    use super::{MachineLifecycle, MachineUsabilityReason, placement_rejection};

    #[test]
    fn only_draining_excludes_placement() {
        assert_eq!(placement_rejection(MachineLifecycle::Active), None);
        assert_eq!(
            placement_rejection(MachineLifecycle::Draining),
            Some(MachineUsabilityReason::Draining)
        );
    }
}
