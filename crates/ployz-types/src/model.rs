use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use derive_more::Display;
use ipnet::Ipv4Net;
use schemars::JsonSchema;
use serde::de::{self, Visitor};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt::{self, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use strum::EnumString;

use crate::spec::{Namespace, VolumeScope};

#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Display, JsonSchema,
)]
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct RegionName(pub String);

impl RegionName {
    #[must_use]
    pub fn local() -> Self {
        Self("local".into())
    }

    pub fn new(value: impl AsRef<str>) -> Result<Self, String> {
        normalize_topology_label(value.as_ref(), "region").map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for RegionName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = deserializer.deserialize_str(TopologyLabelVisitor { field: "region" })?;
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, JsonSchema)]
pub struct AvailabilityZoneName(pub String);

impl AvailabilityZoneName {
    pub fn new(value: impl AsRef<str>) -> Result<Self, String> {
        normalize_topology_label(value.as_ref(), "availability_zone").map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for AvailabilityZoneName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = deserializer.deserialize_str(TopologyLabelVisitor {
            field: "availability_zone",
        })?;
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineTopology {
    pub region: RegionName,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability_zone: Option<AvailabilityZoneName>,
}

impl MachineTopology {
    #[must_use]
    pub fn local() -> Self {
        Self {
            region: RegionName::local(),
            availability_zone: None,
        }
    }

    pub fn new(
        region: impl AsRef<str>,
        availability_zone: Option<impl AsRef<str>>,
    ) -> Result<Self, String> {
        Ok(Self {
            region: RegionName::new(region)?,
            availability_zone: availability_zone
                .map(AvailabilityZoneName::new)
                .transpose()?,
        })
    }
}

struct TopologyLabelVisitor {
    field: &'static str,
}

impl Visitor<'_> for TopologyLabelVisitor {
    type Value = String;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "a non-empty {} label", self.field)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        normalize_topology_label(value, self.field).map_err(E::custom)
    }
}

fn normalize_topology_label(value: &str, field: &str) -> Result<String, String> {
    let normalized = value.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return Err(format!("{field} cannot be empty"));
    }
    let mut chars = normalized.chars();
    let Some(first) = chars.next() else {
        return Err(format!("{field} cannot be empty"));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(format!("{field} must start with an ASCII letter or digit"));
    }
    if !chars.all(|char| char.is_ascii_alphanumeric() || matches!(char, '-' | '_' | '.')) {
        return Err(format!(
            "{field} may only contain ASCII letters, digits, '-', '_', and '.'"
        ));
    }
    Ok(normalized)
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, JsonSchema)]
#[display("{_0}")]
pub struct OverlayIp(pub Ipv6Addr);

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    Display,
    EnumString,
    Default,
    JsonSchema,
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
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, JsonSchema,
)]
pub enum MachineRole {
    #[display("storage_candidate")]
    #[strum(serialize = "storage_candidate")]
    StorageCandidate,
    #[display("mirror")]
    #[strum(serialize = "mirror")]
    Mirror,
    #[display("leaf")]
    #[strum(serialize = "leaf")]
    Leaf,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineMembership {
    pub id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    pub topology: MachineTopology,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_target: Option<String>,
    #[schemars(with = "Option<String>")]
    pub subnet: Option<Ipv4Net>,
    pub bridge_ip: Option<OverlayIp>,
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub lifecycle: MachineLifecycle,
    pub role: MachineRole,
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
            topology: MachineTopology::local(),
            control_target: None,
            subnet,
            bridge_ip: None,
            endpoints,
            lifecycle: MachineLifecycle::Standby,
            role: MachineRole::StorageCandidate,
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

#[derive(Debug, Clone)]
pub enum CertificateEvent {
    Added(CertificateRecord),
    Updated(CertificateRecord),
    Removed(CertificateRecord),
}

#[derive(Debug, Clone)]
pub enum AcmeChallengeEvent {
    Added(AcmeChallengeRecord),
    Updated(AcmeChallengeRecord),
    Removed(AcmeChallengeRecord),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub machine_id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    pub topology: MachineTopology,
    pub role: MachineRole,
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

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display, JsonSchema)]
pub struct InstanceId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display, JsonSchema)]
pub struct DeployId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display, JsonSchema)]
pub struct SlotId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceRevisionRecord {
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
    pub spec_json: String,
    pub created_by: MachineId,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceReleaseRecord {
    pub namespace: Namespace,
    pub service: String,
    pub release: ServiceRelease,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceRelease {
    pub primary_revision_hash: String,
    pub referenced_revision_hashes: Vec<String>,
    pub routing: ServiceRoutingPolicy,
    pub slots: Vec<ServiceReleaseSlot>,
    pub updated_by_deploy_id: DeployId,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ServiceRoutingPolicy {
    Direct {
        revision_hash: String,
    },
    Split {
        allocations: Vec<ServiceTrafficAllocation>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceTrafficAllocation {
    pub revision_hash: String,
    pub percent: u8,
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ServiceReleaseSlot {
    pub slot_id: SlotId,
    pub machine_id: MachineId,
    pub active_instance_id: InstanceId,
    pub revision_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingState {
    pub machines: Vec<MachineMembership>,
    pub revisions: Vec<ServiceRevisionRecord>,
    pub releases: Vec<ServiceReleaseRecord>,
    pub instances: Vec<InstanceStatusRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoutingEvent {
    MachineAdded(MachineMembership),
    MachineUpdated {
        old: MachineMembership,
        new: MachineMembership,
    },
    MachineRemoved(MachineMembership),
    RevisionAdded(ServiceRevisionRecord),
    RevisionUpdated {
        old: ServiceRevisionRecord,
        new: ServiceRevisionRecord,
    },
    RevisionRemoved(ServiceRevisionRecord),
    ReleaseAdded(ServiceReleaseRecord),
    ReleaseUpdated {
        old: ServiceReleaseRecord,
        new: ServiceReleaseRecord,
    },
    ReleaseRemoved(ServiceReleaseRecord),
    InstanceAdded(InstanceStatusRecord),
    InstanceUpdated {
        old: InstanceStatusRecord,
        new: InstanceStatusRecord,
    },
    InstanceRemoved(InstanceStatusRecord),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeAccountRecord {
    pub account_id: String,
    pub issuer_url: String,
    pub contact_email: Option<String>,
    // SECURITY: serialized `instant_acme::AccountCredentials` containing the
    // account private key. Safe only while replication stays inside the
    // WireGuard mesh and local store files are not backed up unencrypted;
    // revisit if either assumption changes.
    pub account_credentials_json: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString)]
pub enum CertificateState {
    #[display("pending")]
    #[strum(serialize = "pending")]
    Pending,
    #[display("issuing")]
    #[strum(serialize = "issuing")]
    Issuing,
    #[display("active")]
    #[strum(serialize = "active")]
    Active,
    #[display("renewal_due")]
    #[strum(serialize = "renewal_due")]
    RenewalDue,
    #[display("failed")]
    #[strum(serialize = "failed")]
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateVersion {
    pub version_id: String,
    pub fullchain_pem: String,
    // SECURITY: leaf private key in PEM form, replicated as plaintext JSON
    // through the certificates table. Safe only under the WireGuard-only
    // replication + no-unencrypted-backup assumption documented on the
    // schema; revisit if either assumption changes.
    pub private_key_pem: String,
    pub not_before: Option<u64>,
    pub not_after: Option<u64>,
    pub issued_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateRecord {
    pub hostname: String,
    pub issuer_url: String,
    pub account_id: String,
    pub state: CertificateState,
    pub active_version_id: Option<String>,
    pub versions: Vec<CertificateVersion>,
    pub order_url: Option<String>,
    pub last_error: Option<String>,
    pub requested_at: u64,
    pub updated_at: u64,
    pub next_renewal_at: Option<u64>,
}

impl CertificateRecord {
    /// The currently-installable version, if any. Independent of `state`:
    /// renewal transitions a healthy cert through `RenewalDue → Issuing` and
    /// `active_version_id` keeps pointing at the existing leaf the whole way;
    /// a non-retryable finalize failure explicitly restores the previous
    /// `active_version_id` so callers can keep serving the old cert. TLS
    /// consumers should ask the type for material here, not gate on `state`.
    #[must_use]
    pub fn installed_version(&self) -> Option<&CertificateVersion> {
        let id = self.active_version_id.as_deref()?;
        self.versions
            .iter()
            .find(|version| version.version_id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeChallengeRecord {
    pub hostname: String,
    pub token: String,
    // SECURITY: HTTP-01 key authorization is the secret an ACME verifier must
    // echo back. Replicated as plaintext JSON. Safe only under the WireGuard-
    // only replication + no-unencrypted-backup assumption documented on the
    // schema; revisit if either assumption changes.
    pub key_authorization: String,
    pub expires_at: u64,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeChallengeReadinessRecord {
    pub hostname: String,
    pub token: String,
    pub machine_id: MachineId,
    pub observed_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainDnsAdvice {
    pub hostname: String,
    pub resolved_ips: Vec<IpAddr>,
    pub recommended_ips: Vec<IpAddr>,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, JsonSchema,
)]
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

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, EnumString, JsonSchema,
)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeRecord {
    pub namespace: Namespace,
    pub volume_name: String,
    pub scope: VolumeScope,
    pub machine_id: MachineId,
    pub quota: String,
    pub mode: String,
    pub owner: String,
    pub attached_services: Vec<String>,
    pub created_at: u64,
    pub created_by_deploy_id: DeployId,
    pub last_modified_at: u64,
    pub last_modified_by_deploy_id: DeployId,
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
            topology: self.topology,
            control_target: None,
            subnet: self.subnet,
            bridge_ip: None,
            endpoints: self.endpoints,
            lifecycle: MachineLifecycle::Standby,
            role: self.role,
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
            topology: MachineTopology::local(),
            role: MachineRole::Mirror,
            subnet: Some("10.42.1.0/24".parse().unwrap()),
            endpoints: vec!["1.2.3.4:51820".into()],
        };

        let encoded = resp.encode().unwrap();
        assert!(encoded.starts_with(JOIN_RESPONSE_PREFIX));

        let decoded = JoinResponse::decode(&encoded).unwrap();
        assert_eq!(decoded.machine_id, resp.machine_id);
        assert_eq!(decoded.public_key, resp.public_key);
        assert_eq!(decoded.overlay_ip, resp.overlay_ip);
        assert_eq!(decoded.topology, resp.topology);
        assert_eq!(decoded.role, resp.role);
        assert_eq!(decoded.subnet, resp.subnet);
        assert_eq!(decoded.endpoints, resp.endpoints);
    }

    #[test]
    fn join_response_into_seed_machine_membership() {
        let resp = JoinResponse {
            machine_id: MachineId("joiner-1".into()),
            public_key: PublicKey([0xab; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            topology: MachineTopology::local(),
            role: MachineRole::Mirror,
            subnet: None,
            endpoints: vec![],
        };
        let record = resp.into_seed_machine_membership();
        assert_eq!(record.id.0, "joiner-1");
        assert_eq!(record.role, MachineRole::Mirror);
        assert!(record.bridge_ip.is_none());
    }

    #[test]
    fn machine_record_without_topology_is_rejected() {
        let json = r#"{
            "id":"node-1",
            "public_key":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],
            "overlay_ip":"fd00::1",
            "subnet":null,
            "bridge_ip":null,
            "endpoints":[],
            "lifecycle":"Standby",
            "created_at":0,
            "updated_at":0,
            "labels":{}
        }"#;

        let error =
            serde_json::from_str::<MachineMembership>(json).expect_err("record should fail");

        assert!(error.to_string().contains("missing field `topology`"));
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

    // -------------------------------------------------------------------
    // CertificateRecord::installed_version — installable-material lookup
    //
    // These tests pin the contract that TLS consumers (gateway, doctor,
    // future status surfaces) must use when deciding whether to serve a
    // managed cert. The rule is:
    //
    //   "Installable" == there is a `CertificateVersion` whose `version_id`
    //   matches the record's `active_version_id`.
    //
    // It is deliberately independent of `state`. The renewal flow walks a
    // healthy cert through `Active → RenewalDue → Issuing` (and possibly
    // `→ Failed` on a non-retryable finalize) without clearing
    // `active_version_id`, so the existing leaf must remain serviceable
    // throughout. Gating on `state` would blackhole TLS handshakes during
    // every renewal window.
    // -------------------------------------------------------------------

    fn cert_version(id: &str) -> CertificateVersion {
        CertificateVersion {
            version_id: id.into(),
            fullchain_pem: format!(
                "-----BEGIN CERTIFICATE-----\n{id}\n-----END CERTIFICATE-----\n"
            ),
            private_key_pem: format!(
                "-----BEGIN PRIVATE KEY-----\n{id}\n-----END PRIVATE KEY-----\n"
            ),
            not_before: Some(0),
            not_after: Some(0),
            issued_at: 0,
        }
    }

    fn cert_record(state: CertificateState) -> CertificateRecord {
        CertificateRecord {
            hostname: "example.com".into(),
            issuer_url: "https://acme.example/directory".into(),
            account_id: "acct".into(),
            state,
            active_version_id: None,
            versions: Vec::new(),
            order_url: None,
            last_error: None,
            requested_at: 0,
            updated_at: 0,
            next_renewal_at: None,
        }
    }

    #[test]
    fn installed_version_returns_none_without_active_version_id() {
        // Brand-new Pending row with no successful issuance: nothing to serve.
        let record = cert_record(CertificateState::Pending);
        assert!(record.installed_version().is_none());
    }

    #[test]
    fn installed_version_returns_none_when_active_id_points_at_missing_version() {
        // The pointer is dangling — `versions` was rolled back or never
        // populated. Treat as "no installable material" rather than panicking.
        let mut record = cert_record(CertificateState::Active);
        record.active_version_id = Some("v-missing".into());
        assert!(record.installed_version().is_none());
    }

    #[test]
    fn installed_version_returns_match_for_active_record() {
        // Steady state: `state == Active`, single version, pointer matches.
        let mut record = cert_record(CertificateState::Active);
        record.versions.push(cert_version("v1"));
        record.active_version_id = Some("v1".into());
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v1")
        );
    }

    #[test]
    fn installed_version_serves_during_renewal_due() {
        // The renewal ticker flips `Active → RenewalDue` without touching the
        // cert material. The old leaf must remain installable until a fresh
        // version is committed; otherwise the gateway drops TLS during every
        // renewal window.
        let mut record = cert_record(CertificateState::RenewalDue);
        record.versions.push(cert_version("v1"));
        record.active_version_id = Some("v1".into());
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v1")
        );
    }

    #[test]
    fn installed_version_serves_during_issuing_renewal() {
        // `start_one` flips the row to `Issuing` for the renewal order while
        // `active_version_id` still points at the previous valid leaf. We
        // serve the old material until finalize replaces it.
        let mut record = cert_record(CertificateState::Issuing);
        record.versions.push(cert_version("v1"));
        record.active_version_id = Some("v1".into());
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v1")
        );
    }

    #[test]
    fn installed_version_serves_after_failed_renewal_when_previous_id_restored() {
        // `finalize_one` non-retryable error restores `previous_active_version_id`
        // before downgrading state to `Failed`, exactly so the gateway keeps
        // serving the previously-issued cert until the next reconcile attempt.
        // Old version is still in `versions`; new (failed) version is not added.
        let mut record = cert_record(CertificateState::Failed);
        record.versions.push(cert_version("v1"));
        record.active_version_id = Some("v1".into());
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v1")
        );
    }

    #[test]
    fn installed_version_picks_newest_when_multiple_versions_present() {
        // Successful renewal pushes a new `CertificateVersion`. The pointer
        // must determine which one is served — not insertion order.
        let mut record = cert_record(CertificateState::Active);
        record.versions.push(cert_version("v1"));
        record.versions.push(cert_version("v2"));
        record.active_version_id = Some("v2".into());
        assert_eq!(
            record.installed_version().map(|v| v.version_id.as_str()),
            Some("v2")
        );
    }

    fn sample_record() -> MachineMembership {
        let mut labels = BTreeMap::new();
        labels.insert("region".into(), "iad".into());
        MachineMembership {
            id: MachineId("m1".into()),
            public_key: PublicKey([0x11; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 7)),
            topology: MachineTopology::local(),
            control_target: Some("https://control.example".into()),
            subnet: Some("10.42.7.0/24".parse().unwrap()),
            bridge_ip: Some(OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 8))),
            endpoints: vec!["1.2.3.4:51820".into(), "5.6.7.8:51820".into()],
            lifecycle: MachineLifecycle::Active,
            role: MachineRole::StorageCandidate,
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
        assert_eq!(
            candidate.labels.get("region").map(String::as_str),
            Some("iad")
        );
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
