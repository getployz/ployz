//! Runtime and intent state records.

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId};
use crate::machine::MachineName;
use crate::ops::{RoutePort, RouteTarget};
use crate::state_key::id_prefixed_state_key;
use std::net::{IpAddr, SocketAddr};

pub const KV_CORE_BUCKET: &str = "KV_CORE";
pub const KV_OPS_BUCKET: &str = "KV_OPS";
pub const KV_OBS_BUCKET: &str = "KV_OBS";

pub const NAMESPACE_LOCK_STATE_PREFIX: &str = "namespace_locks";
pub const MACHINE_CONTAINER_OBSERVATION_PREFIX: &str = "containers";
pub const MACHINE_PUBLIC_IP_OBSERVATION_PREFIX: &str = "machines";
pub const GATEWAY_STATUS_OBSERVATION_PREFIX: &str = "gateways";

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
}

/// Full operator intent visible to readers. Authorized users stay private to
/// the core authorization renderer and are intentionally absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IntentSnapshot {
    pub active_machines: Vec<ActiveMachineState>,
    pub route_bindings: Vec<RouteBindingState>,
    pub serving_target_entries: Vec<ServingTargetEntry>,
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

/// Persisted `KV_CORE.namespace_locks.*` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct NamespaceLockState {
    pub namespace_id: NamespaceId,
    pub operation_id: OperationId,
    pub expires_at_unix_ms: u64,
}

/// Persisted `KV_OBS.machines.*.public_ip` value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachinePublicIpObservation {
    pub machine_id: MachineId,
    pub public_ip: IpAddr,
}

/// Persisted `KV_OBS.gateways.*.status` value.
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

id_prefixed_state_key! { pub struct NamespaceLockStateKey; prefix: NAMESPACE_LOCK_STATE_PREFIX; fn from_namespace_id(&NamespaceId); }

/// Every remaining KV_CORE write family. `permissions.rs` matches this
/// exhaustively to build the controller grant, so a new key type cannot ship
/// without an authority decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreStateKeyFamily {
    NamespaceLock,
}

impl CoreStateKeyFamily {
    pub const ALL: [Self; 1] = [Self::NamespaceLock];

    /// The NATS subject pattern spanning every key this family writes. Each
    /// arm delegates to the key type's own pattern so the grant and the key
    /// format come from one renderer.
    #[must_use]
    pub fn wildcard_pattern(self) -> String {
        match self {
            Self::NamespaceLock => NamespaceLockStateKey::wildcard_pattern(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachineContainerObservationKey(String);

impl MachineContainerObservationKey {
    #[must_use]
    pub fn from_machine_id(machine_id: &MachineId) -> Self {
        Self(format!(
            "{MACHINE_CONTAINER_OBSERVATION_PREFIX}.{}",
            machine_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MachinePublicIpObservationKey(String);

impl MachinePublicIpObservationKey {
    #[must_use]
    pub fn from_machine_id(machine_id: &MachineId) -> Self {
        Self(format!(
            "{MACHINE_PUBLIC_IP_OBSERVATION_PREFIX}.{}.public_ip",
            machine_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn matches(value: &str) -> bool {
        value.starts_with(&format!("{MACHINE_PUBLIC_IP_OBSERVATION_PREFIX}."))
            && value.ends_with(".public_ip")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GatewayStatusObservationKey(String);

impl GatewayStatusObservationKey {
    #[must_use]
    pub fn from_machine_id(machine_id: &MachineId) -> Self {
        Self(format!(
            "{GATEWAY_STATUS_OBSERVATION_PREFIX}.{}.status",
            machine_id.as_str()
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
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
