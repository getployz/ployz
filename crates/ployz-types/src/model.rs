use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use derive_more::Display;
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::net::{Ipv4Addr, Ipv6Addr};
use strum::EnumString;

use crate::spec::Namespace;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Display)]
pub struct MachineId(pub String);

impl AsRef<str> for MachineId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct NetworkName(pub String);

impl AsRef<str> for NetworkName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct NetworkId(pub String);

impl AsRef<str> for NetworkId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl NetworkId {
    #[must_use]
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        rand::fill(&mut bytes);
        let mut value = String::with_capacity(32);
        for b in &bytes {
            let _ = write!(&mut value, "{b:02x}");
        }
        Self(value)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(bytes) = self;
        let [b0, b1, b2, b3, ..] = bytes;
        write!(f, "PublicKey({b0:02x}{b1:02x}{b2:02x}{b3:02x}..)")
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateKey(pub [u8; 32]);

impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self(_) = self;
        f.write_str("PrivateKey(***)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
#[display("{_0}")]
pub struct OverlayIp(pub Ipv6Addr);

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, Default,
)]
pub enum MachineLifecycle {
    #[default]
    #[display("standby")]
    #[strum(serialize = "standby")]
    Standby,
    #[display("active")]
    #[strum(serialize = "active")]
    Active,
    #[display("draining")]
    #[strum(serialize = "draining")]
    Draining,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, Default,
)]
pub enum NetworkLifecycle {
    #[default]
    #[display("stopped")]
    #[strum(serialize = "stopped")]
    Stopped,
    #[display("running")]
    #[strum(serialize = "running")]
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineMembership {
    pub id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_target: Option<String>,
    pub subnet: Option<Ipv4Net>,
    pub bridge_ip: Option<OverlayIp>,
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub lifecycle: MachineLifecycle,
    pub created_at: u64,
    pub updated_at: u64,
    pub labels: BTreeMap<String, String>,
}

impl MachineMembership {
    /// Create a minimal seed record for bootstrap/peer-discovery purposes.
    ///
    /// Control-plane fields (`lifecycle`, timestamps, `labels`)
    /// are zeroed — the real values arrive once the store is online.
    #[must_use]
    pub fn seed(
        id: MachineId,
        public_key: PublicKey,
        overlay_ip: OverlayIp,
        subnet: Option<Ipv4Net>,
        endpoints: Vec<String>,
    ) -> Self {
        Self {
            id,
            public_key,
            overlay_ip,
            control_target: None,
            subnet,
            bridge_ip: None,
            endpoints,
            lifecycle: MachineLifecycle::Standby,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    /// All CIDRs this peer should route, used by both host and docker WireGuard adapters.
    #[must_use]
    pub fn allowed_cidrs(&self) -> Vec<String> {
        let mut cidrs = vec![format!("{}/128", self.overlay_ip.0)];
        if let Some(subnet) = &self.subnet {
            cidrs.push(subnet.to_string());
        }
        if let Some(bridge_ip) = &self.bridge_ip {
            cidrs.push(format!("{}/128", bridge_ip.0));
        }
        cidrs
    }

    #[must_use]
    pub fn identity(&self) -> MachineIdentity {
        MachineIdentity {
            id: self.id.clone(),
            public_key: self.public_key.clone(),
            overlay_ip: self.overlay_ip,
        }
    }

    #[must_use]
    pub fn placement_candidate(&self) -> PlacementCandidate {
        PlacementCandidate {
            id: self.id.clone(),
            lifecycle: self.lifecycle,
            labels: self.labels.clone(),
        }
    }

    #[must_use]
    pub fn wireguard_peer_spec(&self) -> WireGuardPeerSpec {
        WireGuardPeerSpec {
            identity: self.identity(),
            subnet: self.subnet,
            bridge_ip: self.bridge_ip,
            endpoints: self.endpoints.clone(),
        }
    }

    #[must_use]
    pub fn observation(&self) -> MachineObservation {
        MachineObservation {
            identity: self.identity(),
            subnet: self.subnet,
            bridge_ip: self.bridge_ip,
            endpoints: self.endpoints.clone(),
        }
    }
}

/// Immutable identity assigned at join — the (id, key, overlay_ip) triple every
/// other view type carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineIdentity {
    pub id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
}

/// Everything a WireGuard adapter needs to render a peer. No lifecycle,
/// timestamps, control_target, or labels — those don't influence WG config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardPeerSpec {
    pub identity: MachineIdentity,
    pub subnet: Option<Ipv4Net>,
    pub bridge_ip: Option<OverlayIp>,
    pub endpoints: Vec<String>,
}

impl WireGuardPeerSpec {
    #[must_use]
    pub fn id(&self) -> &MachineId {
        let Self { identity, .. } = self;
        &identity.id
    }

    #[must_use]
    pub fn public_key(&self) -> &PublicKey {
        let Self { identity, .. } = self;
        &identity.public_key
    }

    #[must_use]
    pub fn overlay_ip(&self) -> OverlayIp {
        let Self { identity, .. } = self;
        identity.overlay_ip
    }

    /// All CIDRs this peer should route, used by both host and docker WireGuard adapters.
    #[must_use]
    pub fn allowed_cidrs(&self) -> Vec<String> {
        let Self {
            identity,
            subnet,
            bridge_ip,
            endpoints: _,
        } = self;
        let mut cidrs = vec![format!("{}/128", identity.overlay_ip.0)];
        if let Some(subnet) = subnet {
            cidrs.push(subnet.to_string());
        }
        if let Some(bridge_ip) = bridge_ip {
            cidrs.push(format!("{}/128", bridge_ip.0));
        }
        cidrs
    }
}

impl From<&MachineMembership> for WireGuardPeerSpec {
    fn from(record: &MachineMembership) -> Self {
        record.wireguard_peer_spec()
    }
}

/// Placement and coordination policy input — all `machine_policy.rs` reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementCandidate {
    pub id: MachineId,
    pub lifecycle: MachineLifecycle,
    pub labels: BTreeMap<String, String>,
}

impl From<&MachineMembership> for PlacementCandidate {
    fn from(record: &MachineMembership) -> Self {
        record.placement_candidate()
    }
}

/// Transient observation pushed into peer-state from `endpoint_maintainer` and
/// `peer_sync`. Mirrors the fields that `PeerState` actually keeps.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineObservation {
    pub identity: MachineIdentity,
    pub subnet: Option<Ipv4Net>,
    pub bridge_ip: Option<OverlayIp>,
    pub endpoints: Vec<String>,
}

impl MachineObservation {
    /// Build a transient observation for bootstrap/peer-discovery purposes.
    #[must_use]
    pub fn seed(
        id: MachineId,
        public_key: PublicKey,
        overlay_ip: OverlayIp,
        subnet: Option<Ipv4Net>,
        endpoints: Vec<String>,
    ) -> Self {
        Self {
            identity: MachineIdentity {
                id,
                public_key,
                overlay_ip,
            },
            subnet,
            bridge_ip: None,
            endpoints,
        }
    }

    #[must_use]
    pub fn id(&self) -> &MachineId {
        let Self { identity, .. } = self;
        &identity.id
    }
}

impl From<&MachineMembership> for MachineObservation {
    fn from(record: &MachineMembership) -> Self {
        record.observation()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRecord {
    pub invite_id: String,
    pub network_id: NetworkId,
    pub issuer_machine_id: MachineId,
    pub issuer_verify_key: String,
    pub expires_at: u64,
    pub consumed_by: Option<MachineId>,
    pub consumed_at: Option<u64>,
    pub revoked_at: Option<u64>,
    pub signature: String,
}

pub type InviteReservation = InviteRecord;

#[derive(Debug, Clone)]
pub enum MachineEvent {
    Added(MachineMembership),
    Updated(MachineMembership),
    Removed(MachineMembership),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub machine_id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    pub subnet: Option<Ipv4Net>,
    pub endpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct SidecarId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidecarRecord {
    pub id: SidecarId,
    pub machine_id: MachineId,
    pub overlay_ip: Ipv4Addr,
    pub public_key: PublicKey,
    pub sidecar_container: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct InstanceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct DeployId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct SlotId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRevisionRecord {
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
    pub spec_json: String,
    pub created_by: MachineId,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceReleaseRecord {
    pub namespace: Namespace,
    pub service: String,
    pub release: ServiceRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRelease {
    pub primary_revision_hash: String,
    pub referenced_revision_hashes: Vec<String>,
    pub routing: ServiceRoutingPolicy,
    pub slots: Vec<ServiceReleaseSlot>,
    pub updated_by_deploy_id: DeployId,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceRoutingPolicy {
    Direct {
        revision_hash: String,
    },
    Split {
        allocations: Vec<ServiceTrafficAllocation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceTrafficAllocation {
    pub revision_hash: String,
    pub percent: u8,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceReleaseSlot {
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub active_instance_id: InstanceId,
    pub revision_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingState {
    pub revisions: Vec<ServiceRevisionRecord>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub instances: Vec<InstanceStatusRecord>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
pub enum InstancePhase {
    #[display("pending")]
    #[strum(serialize = "pending")]
    Pending,
    #[display("starting")]
    #[strum(serialize = "starting")]
    Starting,
    #[display("ready")]
    #[strum(serialize = "ready")]
    Ready,
    #[display("failed")]
    #[strum(serialize = "failed")]
    Failed,
    #[display("draining")]
    #[strum(serialize = "draining")]
    Draining,
    #[display("removed")]
    #[strum(serialize = "removed")]
    Removed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
pub enum DrainState {
    #[display("none")]
    #[strum(serialize = "none")]
    None,
    #[display("requested")]
    #[strum(serialize = "requested")]
    Requested,
    #[display("complete")]
    #[strum(serialize = "complete")]
    Complete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceStatusRecord {
    pub instance_id: InstanceId,
    pub namespace: Namespace,
    pub service: String,
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub revision_hash: String,
    pub deploy_id: DeployId,
    pub docker_container_id: String,
    pub overlay_ip: Option<Ipv4Addr>,
    pub backend_ports: BTreeMap<String, u16>,
    pub phase: InstancePhase,
    pub ready: bool,
    pub drain_state: DrainState,
    pub error: Option<String>,
    pub started_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
pub enum DeployState {
    #[display("planning")]
    #[strum(serialize = "planning")]
    Planning,
    #[display("applying")]
    #[strum(serialize = "applying")]
    Applying,
    #[display("committed")]
    #[strum(serialize = "committed")]
    Committed,
    #[display("cleanup_pending")]
    #[strum(serialize = "cleanup_pending")]
    CleanupPending,
    #[display("failed")]
    #[strum(serialize = "failed")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRecord {
    pub deploy_id: DeployId,
    pub namespace: Namespace,
    pub coordinator_machine_id: MachineId,
    pub manifest_hash: String,
    pub state: DeployState,
    pub started_at: u64,
    pub committed_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub summary_json: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeployChangeKind {
    Create,
    Replace,
    Remove,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotPlan {
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub current_instance_id: Option<InstanceId>,
    pub next_instance_id: Option<InstanceId>,
    pub current_revision_hash: Option<String>,
    pub next_revision_hash: Option<String>,
    pub action: DeployChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServicePlan {
    pub service: String,
    pub current_revision_hash: Option<String>,
    pub next_revision_hash: Option<String>,
    pub slots: Vec<SlotPlan>,
    pub action: DeployChangeKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployPreview {
    pub namespace: Namespace,
    pub manifest_hash: String,
    pub participants: Vec<MachineId>,
    pub services: Vec<ServicePlan>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployEvent {
    pub step: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployApplyResult {
    pub deploy_id: DeployId,
    pub preview: DeployPreview,
    pub state: DeployState,
    pub events: Vec<DeployEvent>,
}

pub const JOIN_RESPONSE_PREFIX: &str = "PLOYZ_JOIN_RESPONSE:";

impl JoinResponse {
    pub fn encode(&self) -> Result<String, String> {
        let json = serde_json::to_string(self).map_err(|e| format!("serialize: {e}"))?;
        Ok(format!(
            "{}{}",
            JOIN_RESPONSE_PREFIX,
            URL_SAFE_NO_PAD.encode(json.as_bytes())
        ))
    }

    pub fn decode(s: &str) -> Result<Self, String> {
        let payload = s
            .strip_prefix(JOIN_RESPONSE_PREFIX)
            .ok_or_else(|| format!("missing prefix '{JOIN_RESPONSE_PREFIX}'"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|e| format!("base64 decode: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("json decode: {e}"))
    }

    #[must_use]
    pub fn into_seed_machine_membership(self) -> MachineMembership {
        MachineMembership {
            id: self.machine_id,
            public_key: self.public_key,
            overlay_ip: self.overlay_ip,
            control_target: None,
            subnet: self.subnet,
            bridge_ip: None,
            endpoints: self.endpoints,
            lifecycle: MachineLifecycle::Standby,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }
}

/// Derive a deterministic overlay IP from a public key (fd00::/8 ULA + first 15 key bytes).
#[must_use]
pub fn management_ip_from_key(key: &PublicKey) -> OverlayIp {
    let mut octets = [0u8; 16];
    octets[0] = 0xfd;
    octets[1..16].copy_from_slice(&key.0[..15]);
    OverlayIp(Ipv6Addr::from(octets))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn management_ip_deterministic() {
        let key = PublicKey([0xab; 32]);
        let ip1 = management_ip_from_key(&key);
        let ip2 = management_ip_from_key(&key);
        assert_eq!(ip1, ip2);
        assert!(ip1.0.segments()[0] >> 8 == 0xfd);
    }

    #[test]
    fn different_keys_different_ips() {
        let k1 = PublicKey([0x01; 32]);
        let k2 = PublicKey([0x02; 32]);
        assert_ne!(management_ip_from_key(&k1), management_ip_from_key(&k2));
    }

    #[test]
    fn join_response_encode_decode_roundtrip() {
        let resp = JoinResponse {
            machine_id: MachineId("joiner-1".into()),
            public_key: PublicKey([0xab; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            subnet: Some("10.42.1.0/24".parse().unwrap()),
            endpoints: vec!["1.2.3.4:51820".into()],
        };

        let encoded = resp.encode().unwrap();
        assert!(encoded.starts_with(JOIN_RESPONSE_PREFIX));

        let decoded = JoinResponse::decode(&encoded).unwrap();
        assert_eq!(decoded.machine_id, resp.machine_id);
        assert_eq!(decoded.public_key, resp.public_key);
        assert_eq!(decoded.overlay_ip, resp.overlay_ip);
        assert_eq!(decoded.subnet, resp.subnet);
        assert_eq!(decoded.endpoints, resp.endpoints);
    }

    #[test]
    fn join_response_into_seed_machine_membership() {
        let resp = JoinResponse {
            machine_id: MachineId("joiner-1".into()),
            public_key: PublicKey([0xab; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            subnet: None,
            endpoints: vec![],
        };
        let record = resp.into_seed_machine_membership();
        assert_eq!(record.id.0, "joiner-1");
        assert!(record.bridge_ip.is_none());
    }

    #[test]
    fn machine_lifecycle_display_is_explicit() {
        assert_eq!(MachineLifecycle::Standby.to_string(), "standby");
    }

    #[test]
    fn machine_lifecycle_from_str_is_explicit() {
        assert_eq!(
            MachineLifecycle::from_str("active"),
            Ok(MachineLifecycle::Active)
        );
    }

    #[test]
    fn network_lifecycle_display_is_explicit() {
        assert_eq!(NetworkLifecycle::Stopped.to_string(), "stopped");
    }

    #[test]
    fn network_lifecycle_from_str_is_explicit() {
        assert_eq!(
            NetworkLifecycle::from_str("running"),
            Ok(NetworkLifecycle::Running)
        );
    }

    fn sample_record() -> MachineMembership {
        let mut labels = BTreeMap::new();
        labels.insert("region".into(), "iad".into());
        MachineMembership {
            id: MachineId("m1".into()),
            public_key: PublicKey([0x11; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 7)),
            control_target: Some("https://control.example".into()),
            subnet: Some("10.42.7.0/24".parse().unwrap()),
            bridge_ip: Some(OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 8))),
            endpoints: vec!["1.2.3.4:51820".into(), "5.6.7.8:51820".into()],
            lifecycle: MachineLifecycle::Active,
            created_at: 100,
            updated_at: 200,
            labels,
        }
    }

    #[test]
    fn machine_record_identity_carries_id_key_overlay() {
        let record = sample_record();
        let identity = record.identity();
        assert_eq!(identity.id, record.id);
        assert_eq!(identity.public_key, record.public_key);
        assert_eq!(identity.overlay_ip, record.overlay_ip);
    }

    #[test]
    fn machine_record_placement_candidate_only_carries_policy_fields() {
        let record = sample_record();
        let candidate = record.placement_candidate();
        assert_eq!(candidate.id, record.id);
        assert_eq!(candidate.lifecycle, MachineLifecycle::Active);
        assert_eq!(candidate.labels.get("region").map(String::as_str), Some("iad"));
    }

    #[test]
    fn machine_record_wireguard_peer_spec_drops_control_plane_fields() {
        let record = sample_record();
        let spec = record.wireguard_peer_spec();
        assert_eq!(spec.id(), &record.id);
        assert_eq!(spec.public_key(), &record.public_key);
        assert_eq!(spec.overlay_ip(), record.overlay_ip);
        assert_eq!(spec.subnet, record.subnet);
        assert_eq!(spec.bridge_ip, record.bridge_ip);
        assert_eq!(spec.endpoints, record.endpoints);
    }

    #[test]
    fn wireguard_peer_spec_allowed_cidrs_matches_record_helper() {
        let record = sample_record();
        let spec = record.wireguard_peer_spec();
        assert_eq!(spec.allowed_cidrs(), record.allowed_cidrs());
    }

    #[test]
    fn machine_observation_carries_observable_fields() {
        let record = sample_record();
        let observation = record.observation();
        assert_eq!(observation.id(), &record.id);
        assert_eq!(observation.identity.public_key, record.public_key);
        assert_eq!(observation.subnet, record.subnet);
        assert_eq!(observation.bridge_ip, record.bridge_ip);
        assert_eq!(observation.endpoints, record.endpoints);
    }

    #[test]
    fn machine_observation_seed_omits_bridge_ip() {
        let observation = MachineObservation::seed(
            MachineId("m9".into()),
            PublicKey([0x22; 32]),
            OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 9)),
            None,
            vec!["1.1.1.1:51820".into()],
        );
        assert_eq!(observation.id().0, "m9");
        assert!(observation.bridge_ip.is_none());
        assert!(observation.subnet.is_none());
        assert_eq!(observation.endpoints, vec!["1.1.1.1:51820"]);
    }
}
