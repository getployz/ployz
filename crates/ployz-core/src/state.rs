//! Current-state records stored in JetStream KV.

use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

use crate::ids::{MachineId, NamespaceId, NamespaceRevisionEntryId, OperationId, ServiceId};
use crate::machine::MachineName;
use crate::nats_config::NatsAuthorizedUser;
use crate::ops::{MachineSubstrateVersions, RoutePort, RouteTarget};
use crate::state_key::{id_prefixed_state_key, state_key_path};
use std::net::{IpAddr, SocketAddr};

pub const KV_CORE_BUCKET: &str = "KV_CORE";
pub const KV_OPS_BUCKET: &str = "KV_OPS";
pub const KV_OBS_BUCKET: &str = "KV_OBS";

pub const SERVING_TARGET_ENTRY_PREFIX: &str = "services";
pub const ACTIVE_MACHINE_STATE_PREFIX: &str = "machines";
pub const NAMESPACE_LOCK_STATE_PREFIX: &str = "namespace_locks";
/// `KV_CORE` prefix of the durable NATS authorized-principal records
/// (ADR-0001: their recovery evidence is `authorized-users.conf`).
pub const NATS_AUTHORIZED_USER_PREFIX: &str = "nats_authorized_user";
pub const ROUTE_BINDING_STATE_PREFIX: &str = "routes";
pub const MACHINE_CONTAINER_OBSERVATION_PREFIX: &str = "containers";
pub const MACHINE_PUBLIC_IP_OBSERVATION_PREFIX: &str = "machines";
pub const GATEWAY_STATUS_OBSERVATION_PREFIX: &str = "gateways";

/// Persisted `KV_CORE.services.*` value.
///
/// Changing this shape intentionally breaks existing clusters unless paired
/// with a KV cleanup or migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ServingTargetEntry {
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub namespace_revision_entry_id: NamespaceRevisionEntryId,
}

/// Persisted `KV_CORE.routes.*` value.
///
/// Changing this shape intentionally breaks existing clusters unless paired
/// with a KV cleanup or migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RouteBindingState {
    pub namespace_id: NamespaceId,
    pub target: RouteTarget,
    pub endpoint_port: RoutePort,
    pub service_id: ServiceId,
}

/// Persisted `KV_CORE.machines.*` value.
///
/// Changing this shape intentionally breaks existing clusters unless paired
/// with a KV cleanup or migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveMachineState {
    pub machine_id: MachineId,
    pub name: MachineName,
    pub activated_by: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub substrate_versions: Option<MachineSubstrateVersions>,
    /// Durable operator intent for this machine (Machine Lifecycle in the
    /// glossary). Absent in records written before lifecycle existed, so the
    /// default is active.
    #[serde(default)]
    pub lifecycle: MachineLifecycle,
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

/// Why a machine is excluded from new workload placement. Only operator
/// intent excludes today; future reasons (placement constraints) join as
/// their signals land. Liveness is never a reason (ADR 0027): a dead machine
/// answers at the point of use — it does not reply to a placement RPC, and
/// its upstreams fail at dial time. This control-side gate is interim: once
/// placement is bid-based, a draining machine declines its own bids and the
/// check moves into the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// KV key for one serving target entry, scoped by namespace so equally
/// named services in different namespaces never share a record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingTargetEntryKey(String);

impl ServingTargetEntryKey {
    /// Segment count shared by the constructor and the wildcard pattern:
    /// arity is stated once, so the two renderings cannot drift.
    const ARITY: usize = 2;

    #[must_use]
    pub fn from_namespace_service(namespace_id: &NamespaceId, service_id: &ServiceId) -> Self {
        let segments: [&str; Self::ARITY] = [namespace_id.as_str(), service_id.as_str()];
        Self(state_key_path(SERVING_TARGET_ENTRY_PREFIX, &segments))
    }

    /// The NATS subject pattern spanning every serving target entry key,
    /// rendered by the same helper and arity as the constructor.
    #[must_use]
    pub fn wildcard_pattern() -> String {
        state_key_path(SERVING_TARGET_ENTRY_PREFIX, &["*"; Self::ARITY])
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteBindingStateKey(String);

impl RouteBindingStateKey {
    /// Segment count shared by the constructor and the wildcard pattern:
    /// arity is stated once, so the two renderings cannot drift.
    const ARITY: usize = 2;

    #[must_use]
    pub fn from_target(target: &RouteTarget) -> Self {
        let hostname_token = route_hostname_key_token(target);
        let port_token = target.port.get().to_string();
        let segments: [&str; Self::ARITY] = [hostname_token.as_str(), port_token.as_str()];
        Self(state_key_path(ROUTE_BINDING_STATE_PREFIX, &segments))
    }

    /// The NATS subject pattern spanning every route binding key, rendered
    /// by the same helper and arity as the constructor.
    #[must_use]
    pub fn wildcard_pattern() -> String {
        state_key_path(ROUTE_BINDING_STATE_PREFIX, &["*"; Self::ARITY])
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn route_hostname_key_token(target: &RouteTarget) -> String {
    let hostname = target.hostname.as_str();
    let mut token = String::with_capacity(hostname.len() * 2);
    for byte in hostname.bytes() {
        write!(&mut token, "{byte:02x}").expect("writing to string cannot fail");
    }
    token
}

id_prefixed_state_key! { pub struct ActiveMachineStateKey; prefix: ACTIVE_MACHINE_STATE_PREFIX; fn from_machine_id(&MachineId); }
id_prefixed_state_key! { pub struct NamespaceLockStateKey; prefix: NAMESPACE_LOCK_STATE_PREFIX; fn from_namespace_id(&NamespaceId); }

/// KV key for one durable authorized-principal record. The record key
/// nests further segments (`user.<nkey>`, `machine_<id>`), so the spanning
/// pattern is a `>` scope rather than per-segment stars; both still render
/// against the same prefix here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsAuthorizedUserKey(String);

impl NatsAuthorizedUserKey {
    #[must_use]
    pub fn from_user(user: &NatsAuthorizedUser) -> Self {
        Self(state_key_path(
            NATS_AUTHORIZED_USER_PREFIX,
            &[user.authority_record_key().as_str()],
        ))
    }

    /// The NATS subject pattern spanning every authorized-user key.
    #[must_use]
    pub fn wildcard_pattern() -> String {
        format!("{NATS_AUTHORIZED_USER_PREFIX}.>")
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Every KV_CORE write family. `permissions.rs` matches this exhaustively to
/// build the controller grant, so a new key type cannot ship without an
/// authority decision. Add new variants here, next to their key types, and
/// extend [`CoreStateKeyFamily::ALL`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoreStateKeyFamily {
    ServingTargetEntry,
    RouteBinding,
    ActiveMachine,
    NamespaceLock,
    NatsAuthorizedUser,
}

impl CoreStateKeyFamily {
    pub const ALL: [Self; 5] = [
        Self::ServingTargetEntry,
        Self::RouteBinding,
        Self::ActiveMachine,
        Self::NamespaceLock,
        Self::NatsAuthorizedUser,
    ];

    /// The NATS subject pattern spanning every key this family writes. Each
    /// arm delegates to the key type's own pattern so the grant and the key
    /// format come from one renderer; authorized-user records span nested
    /// segments, so that family is an explicit `>` scope.
    #[must_use]
    pub fn wildcard_pattern(self) -> String {
        match self {
            Self::ServingTargetEntry => ServingTargetEntryKey::wildcard_pattern(),
            Self::RouteBinding => RouteBindingStateKey::wildcard_pattern(),
            Self::ActiveMachine => ActiveMachineStateKey::wildcard_pattern(),
            Self::NamespaceLock => NamespaceLockStateKey::wildcard_pattern(),
            Self::NatsAuthorizedUser => NatsAuthorizedUserKey::wildcard_pattern(),
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
