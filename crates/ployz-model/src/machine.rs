use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineStorageAuthorityPeer {
    pub machine_id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subnet: Option<Ipv4Net>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_ip: Option<OverlayIp>,
    pub region_role: RegionRole,
    pub endpoints: Vec<String>,
}

impl From<&MachineMembership> for MachineStorageAuthorityPeer {
    fn from(record: &MachineMembership) -> Self {
        Self {
            machine_id: record.id.clone(),
            public_key: record.public_key.clone(),
            overlay_ip: record.overlay_ip,
            subnet: record.subnet,
            bridge_ip: record.bridge_ip,
            region_role: record.region_role,
            endpoints: record.endpoints.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MachineTransitionGoal {
    Activate,
    Drain,
    Standby,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "goal")]
pub enum MachineSelfTransition {
    Activate { assigned_subnet: Ipv4Net },
    Drain,
    Standby { force: bool },
}

impl MachineSelfTransition {
    #[must_use]
    pub fn goal(self) -> MachineTransitionGoal {
        match self {
            Self::Activate { .. } => MachineTransitionGoal::Activate,
            Self::Drain => MachineTransitionGoal::Drain,
            Self::Standby { .. } => MachineTransitionGoal::Standby,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display, JsonSchema)]
pub struct NetworkName(pub String);

impl AsRef<str> for NetworkName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

validated_string_id!(pub struct NetworkId("network id"););

impl NetworkId {
    #[must_use]
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        rand::fill(&mut bytes);
        let mut value = String::with_capacity(32);
        for b in &bytes {
            let _ = write!(&mut value, "{b:02x}");
        }
        Self::new(value)
    }
}

validated_string_id!(pub struct InstallationId("installation id"););

impl InstallationId {
    #[must_use]
    pub fn local() -> Self {
        Self::new("local")
    }
}

validated_string_id!(pub struct AuthorityId("authority id"););

impl AuthorityId {
    #[must_use]
    pub fn default_authority() -> Self {
        Self::new("auth-default")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
pub enum AuthorityTier {
    #[display("stable")]
    Stable,
    #[display("dev")]
    Dev,
    #[display("lab")]
    Lab,
    #[display("edge")]
    Edge,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthorityRecord {
    pub id: AuthorityId,
    pub tier: AuthorityTier,
    pub home_region: RegionName,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RegionRole {
    #[display("home_data")]
    HomeData,
    #[display("compute")]
    Compute,
    #[display("disabled")]
    Disabled,
    #[display("draining")]
    Draining,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RegionRecord {
    pub id: RegionName,
    pub role: RegionRole,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub authority: Option<AuthorityId>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
pub enum AuthorityParticipationRole {
    #[display("participant")]
    Participant,
    #[display("storage")]
    Storage,
    #[display("gateway")]
    Gateway,
    #[display("dns")]
    Dns,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AuthorityParticipationRecord {
    pub authority: AuthorityId,
    pub machine_id: MachineId,
    pub role: AuthorityParticipationRole,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StorageParticipation {
    Candidate,
    Authority { authority_id: AuthorityId },
}

impl StorageParticipation {
    #[must_use]
    pub fn default_authority() -> Self {
        Self::Authority {
            authority_id: AuthorityId::default_authority(),
        }
    }

    #[must_use]
    pub fn is_authority(&self) -> bool {
        matches!(self, Self::Authority { .. })
    }

    #[must_use]
    pub fn authority_id(&self) -> Option<&AuthorityId> {
        match self {
            Self::Candidate => None,
            Self::Authority { authority_id } => Some(authority_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineStorageRole {
    Compute,
    Candidate,
    Authority { authority_id: AuthorityId },
}

impl MachineStorageRole {
    #[must_use]
    pub fn default_authority() -> Self {
        Self::Authority {
            authority_id: AuthorityId::default_authority(),
        }
    }

    #[must_use]
    pub fn is_storage_capable(&self) -> bool {
        !matches!(self, Self::Compute)
    }

    #[must_use]
    pub fn authority_id(&self) -> Option<&AuthorityId> {
        match self {
            Self::Authority { authority_id } => Some(authority_id),
            Self::Compute | Self::Candidate => None,
        }
    }

    #[must_use]
    pub fn storage_participation(&self) -> StorageParticipation {
        match self {
            Self::Compute | Self::Candidate => StorageParticipation::Candidate,
            Self::Authority { authority_id } => StorageParticipation::Authority {
                authority_id: authority_id.clone(),
            },
        }
    }
}

impl From<StorageParticipation> for MachineStorageRole {
    fn from(value: StorageParticipation) -> Self {
        match value {
            StorageParticipation::Candidate => Self::Candidate,
            StorageParticipation::Authority { authority_id } => Self::Authority { authority_id },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StorageReplicaPolicy {
    #[display("single")]
    Single,
    #[display("r3")]
    R3,
    #[display("r5")]
    R5,
}

impl StorageReplicaPolicy {
    #[must_use]
    pub fn replicas(self) -> usize {
        match self {
            Self::Single => 1,
            Self::R3 => 3,
            Self::R5 => 5,
        }
    }

    pub fn try_from_replicas(replicas: usize) -> std::result::Result<Self, String> {
        match replicas {
            1 => Ok(Self::Single),
            3 => Ok(Self::R3),
            5 => Ok(Self::R5),
            _ => Err(format!(
                "storage replicas must be 1, 3, or 5 (got {replicas})"
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneDataBucket {
    #[display("stored_intent")]
    StoredIntent,
    #[display("projection")]
    Projection,
    #[display("live_facts")]
    LiveFacts,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Display, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlPlaneLossImpact {
    #[display("stored_truth_lost")]
    StoredTruthLost,
    #[display("no_stored_truth_lost")]
    NoStoredTruthLost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AuthorityNodeRole {
    AuthorityStorage { authority_id: AuthorityId },
    StorageCandidate,
    Compute,
}

impl fmt::Display for AuthorityNodeRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AuthorityStorage { authority_id } => {
                write!(f, "authority_storage:{authority_id}")
            }
            Self::StorageCandidate => f.write_str("storage_candidate"),
            Self::Compute => f.write_str("compute"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthorityNodePosture {
    AuthorityStorage { authority_id: AuthorityId },
    StorageCandidate,
    Compute,
}

impl AuthorityNodePosture {
    #[must_use]
    pub fn from_storage_participation(storage: bool, participation: &StorageParticipation) -> Self {
        match participation {
            StorageParticipation::Authority { authority_id } => Self::AuthorityStorage {
                authority_id: authority_id.clone(),
            },
            StorageParticipation::Candidate if storage => Self::StorageCandidate,
            StorageParticipation::Candidate => Self::Compute,
        }
    }

    #[must_use]
    pub fn from_machine_membership(machine: &MachineMembership) -> Self {
        match &machine.storage_role {
            MachineStorageRole::Authority { authority_id } => Self::AuthorityStorage {
                authority_id: authority_id.clone(),
            },
            MachineStorageRole::Candidate => Self::StorageCandidate,
            MachineStorageRole::Compute => Self::Compute,
        }
    }

    #[must_use]
    pub fn role(&self) -> AuthorityNodeRole {
        match self {
            Self::AuthorityStorage { authority_id } => AuthorityNodeRole::AuthorityStorage {
                authority_id: authority_id.clone(),
            },
            Self::StorageCandidate => AuthorityNodeRole::StorageCandidate,
            Self::Compute => AuthorityNodeRole::Compute,
        }
    }

    #[must_use]
    pub fn data_bucket(&self) -> ControlPlaneDataBucket {
        match self {
            Self::AuthorityStorage { .. } | Self::StorageCandidate => {
                ControlPlaneDataBucket::StoredIntent
            }
            Self::Compute => ControlPlaneDataBucket::LiveFacts,
        }
    }

    #[must_use]
    pub fn loss_impact(&self) -> ControlPlaneLossImpact {
        match self {
            Self::AuthorityStorage { .. } => ControlPlaneLossImpact::StoredTruthLost,
            Self::StorageCandidate | Self::Compute => ControlPlaneLossImpact::NoStoredTruthLost,
        }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MachineLifecycleTransition {
    pub goal: MachineLifecycleGoal,
    pub evidence: MachineTransitionEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "goal", rename_all = "snake_case")]
pub enum MachineLifecycleGoal {
    Activate {
        #[schemars(with = "String")]
        assigned_subnet: Ipv4Net,
    },
    Drain,
    Standby {
        clearance: StandbyTransitionClearance,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum StandbyTransitionClearance {
    DrainingComplete,
    OperatorForced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MachineTransitionEvidence {
    OperatorCommand { command: String },
    BootstrapActivation { operation_id: Option<String> },
    MeshStop { network: NetworkName },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct MachineTransitionError {
    code: &'static str,
    message: String,
}

impl MachineTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: "INVALID_TRANSITION",
            message: message.into(),
        }
    }
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
pub struct NetworkLifecycleTransition {
    pub goal: NetworkLifecycleGoal,
    pub evidence: NetworkTransitionEvidence,
    pub at_unix_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NetworkLifecycleGoal {
    Start,
    Stop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NetworkTransitionEvidence {
    OperatorCommand { command: String },
    BootstrapJoin { network: NetworkName },
    StartupResumeFailure { network: NetworkName },
    MeshTeardown { network: NetworkName },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkTransitionOutcome {
    Applied,
    AlreadyInState,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct NetworkTransitionError {
    code: &'static str,
    message: String,
}

impl NetworkTransitionError {
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl NetworkLifecycle {
    pub fn apply_transition(
        &mut self,
        transition: NetworkLifecycleTransition,
    ) -> Result<NetworkTransitionOutcome, NetworkTransitionError> {
        let NetworkLifecycleTransition {
            goal,
            evidence: _,
            at_unix_secs: _,
        } = transition;
        match (*self, goal) {
            (NetworkLifecycle::Stopped, NetworkLifecycleGoal::Start) => {
                *self = NetworkLifecycle::Running;
                Ok(NetworkTransitionOutcome::Applied)
            }
            (NetworkLifecycle::Running, NetworkLifecycleGoal::Stop) => {
                *self = NetworkLifecycle::Stopped;
                Ok(NetworkTransitionOutcome::Applied)
            }
            (NetworkLifecycle::Running, NetworkLifecycleGoal::Start)
            | (NetworkLifecycle::Stopped, NetworkLifecycleGoal::Stop) => {
                Ok(NetworkTransitionOutcome::AlreadyInState)
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct MachineMembership {
    pub id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    pub topology: MachineTopology,
    pub region_role: RegionRole,
    #[schemars(with = "Option<String>")]
    pub subnet: Option<Ipv4Net>,
    pub bridge_ip: Option<OverlayIp>,
    pub endpoints: Vec<String>,
    #[serde(default)]
    pub lifecycle: MachineLifecycle,
    pub storage_role: MachineStorageRole,
    pub created_at: u64,
    pub updated_at: u64,
    pub labels: BTreeMap<String, String>,
}

impl<'de> Deserialize<'de> for MachineMembership {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawMachineMembership {
            id: MachineId,
            public_key: PublicKey,
            overlay_ip: OverlayIp,
            topology: MachineTopology,
            region_role: RegionRole,
            subnet: Option<Ipv4Net>,
            bridge_ip: Option<OverlayIp>,
            endpoints: Vec<String>,
            #[serde(default)]
            lifecycle: MachineLifecycle,
            storage_role: MachineStorageRole,
            created_at: u64,
            updated_at: u64,
            labels: BTreeMap<String, String>,
        }

        let raw = RawMachineMembership::deserialize(deserializer)?;
        match (raw.lifecycle, raw.subnet) {
            (MachineLifecycle::Active | MachineLifecycle::Draining, None) => {
                return Err(de::Error::custom(format!(
                    "{} machine '{}' must carry an assigned subnet",
                    raw.lifecycle, raw.id
                )));
            }
            (MachineLifecycle::Standby, Some(_)) => {
                return Err(de::Error::custom(format!(
                    "standby machine '{}' cannot carry an assigned subnet",
                    raw.id
                )));
            }
            _ => {}
        }

        Ok(Self {
            id: raw.id,
            public_key: raw.public_key,
            overlay_ip: raw.overlay_ip,
            topology: raw.topology,
            region_role: raw.region_role,
            subnet: raw.subnet,
            bridge_ip: raw.bridge_ip,
            endpoints: raw.endpoints,
            lifecycle: raw.lifecycle,
            storage_role: raw.storage_role,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
            labels: raw.labels,
        })
    }
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
        let lifecycle = if subnet.is_some() {
            MachineLifecycle::Active
        } else {
            MachineLifecycle::Standby
        };
        Self {
            id,
            public_key,
            overlay_ip,
            topology: MachineTopology::local(),
            region_role: RegionRole::Compute,
            subnet,
            bridge_ip: None,
            endpoints,
            lifecycle,
            storage_role: MachineStorageRole::Candidate,
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
    pub fn storage(&self) -> bool {
        self.storage_role.is_storage_capable()
    }

    #[must_use]
    pub fn storage_participation(&self) -> StorageParticipation {
        self.storage_role.storage_participation()
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
            region_role: self.region_role,
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

    pub fn apply_lifecycle_transition(
        &mut self,
        transition: MachineLifecycleTransition,
    ) -> Result<MachineTransitionOutcome, MachineTransitionError> {
        let MachineLifecycleTransition {
            goal,
            evidence: _,
            at_unix_secs,
        } = transition;

        match goal {
            MachineLifecycleGoal::Activate { assigned_subnet } => {
                if self.lifecycle == MachineLifecycle::Active
                    && self.subnet == Some(assigned_subnet)
                {
                    return Ok(MachineTransitionOutcome::AlreadyInState);
                }
                if self.lifecycle == MachineLifecycle::Draining {
                    return Err(MachineTransitionError::invalid(
                        "cannot activate a draining machine without first entering standby",
                    ));
                }
                self.lifecycle = MachineLifecycle::Active;
                self.subnet = Some(assigned_subnet);
            }
            MachineLifecycleGoal::Drain => {
                if self.lifecycle == MachineLifecycle::Draining {
                    return Ok(MachineTransitionOutcome::AlreadyInState);
                }
                if self.lifecycle == MachineLifecycle::Standby {
                    return Err(MachineTransitionError::invalid(
                        "cannot drain a standby machine",
                    ));
                }
                self.lifecycle = MachineLifecycle::Draining;
            }
            MachineLifecycleGoal::Standby { clearance } => {
                if self.lifecycle == MachineLifecycle::Standby && self.subnet.is_none() {
                    return Ok(MachineTransitionOutcome::AlreadyInState);
                }
                match (self.lifecycle, clearance) {
                    (MachineLifecycle::Draining, StandbyTransitionClearance::DrainingComplete)
                    | (MachineLifecycle::Standby, StandbyTransitionClearance::OperatorForced)
                    | (MachineLifecycle::Active, StandbyTransitionClearance::OperatorForced)
                    | (MachineLifecycle::Draining, StandbyTransitionClearance::OperatorForced) => {
                        self.lifecycle = MachineLifecycle::Standby;
                        self.subnet = None;
                    }
                    (
                        MachineLifecycle::Standby | MachineLifecycle::Active,
                        StandbyTransitionClearance::DrainingComplete,
                    ) => {
                        return Err(MachineTransitionError::invalid(
                            "machine must be draining before standby",
                        ));
                    }
                }
            }
        }

        self.updated_at = at_unix_secs;
        Ok(MachineTransitionOutcome::Applied)
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
/// timestamps, or labels — those don't influence WG config.
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
    pub region_role: RegionRole,
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
    pub status: InviteStatus,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InviteStatus {
    Active,
    Consumed {
        consumed_by: MachineId,
        consumed_at: u64,
    },
    Revoked {
        revoked_at: u64,
    },
}

impl InviteStatus {
    #[must_use]
    pub fn consumed_by(&self) -> Option<&MachineId> {
        match self {
            Self::Consumed { consumed_by, .. } => Some(consumed_by),
            Self::Active | Self::Revoked { .. } => None,
        }
    }

    #[must_use]
    pub fn is_consumed_by(&self, machine_id: &MachineId) -> bool {
        self.consumed_by() == Some(machine_id)
    }

    #[must_use]
    pub fn is_consumed(&self) -> bool {
        matches!(self, Self::Consumed { .. })
    }

    #[must_use]
    pub fn is_revoked(&self) -> bool {
        matches!(self, Self::Revoked { .. })
    }
}

#[derive(Debug, Clone)]
pub enum MachineEvent {
    Upsert(MachineMembership),
    Removed { id: MachineId },
}

#[derive(Debug, Clone)]
pub enum CertificateEvent {
    Upsert(CertificateRecord),
    Removed { hostname: String },
}

#[derive(Debug, Clone)]
pub enum AcmeChallengeEvent {
    Upsert(AcmeChallengeRecord),
    Removed { hostname: String, token: String },
}

/// Derive a deterministic overlay IP from a public key (fd00::/8 ULA + first 15 key bytes).
#[must_use]
pub fn management_ip_from_key(key: &PublicKey) -> OverlayIp {
    let mut octets = [0u8; 16];
    octets[0] = 0xfd;
    octets[1..16].copy_from_slice(&key.0[..15]);
    OverlayIp(Ipv6Addr::from(octets))
}
