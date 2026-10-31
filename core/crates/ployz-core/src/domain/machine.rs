use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::{IpAddr, SocketAddr},
    num::{NonZeroU8, NonZeroU64},
    str::FromStr,
};
use ts_rs::TS;

use serde::{Deserialize, Serialize};

use super::NameMatches;
use crate::{
    AdvertisedEndpoint, FanoutSelector, MachineId, MachineLabelKey, MachineLabelValue, MachineName,
    MachineSubnet, MachineTarget, ManagementAddress, ManagementIdentity, Placement, Sealing,
    SelectedEndpoint, ValueError, WireGuardPublicKey,
};

pub(super) fn resolve_machine_text<'a>(
    text: &str,
    visible: impl IntoIterator<Item = &'a Machine>,
) -> NameMatches<&'a Machine> {
    let mut exact_id = None;
    let mut names = Vec::new();
    for machine in visible {
        if machine.id.as_str() == text {
            exact_id = Some(machine);
        }
        if machine.name.as_str() == text {
            names.push(machine);
        }
    }
    exact_id.map_or_else(|| NameMatches::from_matches(names), NameMatches::One)
}

/// Complete admission policy committed with a Machine's initial assignment.
/// Every field is required on the wire; defaults are explicit caller choices.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct InitialMachinePolicy {
    /// Operator classifications used by placement constraints.
    pub labels: BTreeMap<MachineLabelKey, MachineLabelValue>,
    /// Whether to admit new Builds; revocation preserves existing work.
    pub accepts_builds: bool,
    /// Whether to admit new application Services; revocation preserves existing work.
    pub accepts_services: bool,
    /// Whether to admit the trusted Ingress Proxy; revocation preserves existing work.
    pub accepts_ingress: bool,
}

impl InitialMachinePolicy {
    /// Whether this complete policy matches the currently observed Machine.
    #[must_use]
    pub fn matches(&self, machine: &Machine) -> bool {
        self.labels == machine.labels
            && self.accepts_builds == machine.accepts_builds
            && self.accepts_services == machine.accepts_services
            && self.accepts_ingress == machine.accepts_ingress
    }
}

impl Default for InitialMachinePolicy {
    fn default() -> Self {
        Self {
            labels: BTreeMap::new(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
        }
    }
}

/// One Machine's durable advertised record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Machine {
    /// Operator classifications used by placement constraints.
    pub labels: BTreeMap<MachineLabelKey, MachineLabelValue>,
    /// Whether to admit new Builds; revocation preserves existing work.
    pub accepts_builds: bool,
    /// Whether to admit new application Services; revocation preserves existing work.
    pub accepts_services: bool,
    /// Whether to admit the trusted Ingress Proxy; revocation preserves existing work.
    pub accepts_ingress: bool,
    pub id: MachineId,
    pub name: MachineName,
    pub subnet: MachineSubnet,
    pub public_key: WireGuardPublicKey,
    #[serde(default)]
    pub public_ip: Option<IpAddr>,
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub runtime: MachineRuntime,
    /// Builds this Machine runs at once; absent means automatic.
    #[serde(default)]
    pub build_concurrency: Option<BuildConcurrency>,
}

impl Machine {
    /// The set build concurrency, or the automatic value from this record.
    #[must_use]
    pub fn effective_build_concurrency(&self) -> BuildConcurrency {
        self.build_concurrency.unwrap_or_else(|| {
            BuildConcurrency::automatic(self.accepts_services, self.runtime.memory_total_bytes)
        })
    }

    /// Management-plane address derived solely from this Machine's public key.
    #[must_use]
    pub fn management_address(&self) -> ManagementAddress {
        management_address(self.public_key)
    }
}

/// Deterministic management-plane address for a WireGuard identity.
#[must_use]
pub fn management_address(public_key: WireGuardPublicKey) -> ManagementAddress {
    let mut address = [0_u8; 16];
    address[..2].copy_from_slice(&[0xfd, 0xcc]);
    address[2..].copy_from_slice(&public_key.0[..14]);
    ManagementAddress(std::net::Ipv6Addr::from(address))
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct MachineRuntime {
    pub daemon_version: String,
    pub docker_version: String,
    pub hostname: String,
    pub architecture: String,
    pub os_pretty_name: String,
    pub kernel_version: String,
    /// Host memory, absent when the daemon could not observe it.
    #[ts(optional = nullable)]
    pub memory_total_bytes: Option<u64>,
    /// Builds this Machine's daemon is running now; live, never persisted.
    pub running_builds: u32,
    /// Bytes of Provisioned Volume bounds this Machine's Pool could still take on, net
    /// of the bounds its copies commit, growing its backing where it can: a whole-GiB hint
    /// kept below the exact edge, which the Machine's own admission still decides. Live,
    /// never persisted. Absent when unobserved and from older daemons.
    #[ts(optional = nullable)]
    pub pool_committable_bytes: Option<u64>,
    /// What this Machine holds of the Cluster's Sealing Key. Live, never persisted.
    /// Absent when unobserved and from older daemons.
    #[ts(optional = nullable)]
    pub sealing: Option<Sealing>,
    /// Filesystem type under the daemon's replicated state, such as `ext4` or `zfs`, or
    /// its magic number in hex when the daemon does not name it. Absent when unobserved
    /// and from older daemons.
    #[ts(optional = nullable)]
    pub store_filesystem: Option<String>,
    /// Whether the replicated store syncs each commit to disk before peers see it, so a
    /// power cut loses none of this Machine's changes. Absent from older daemons, so
    /// absence claims nothing.
    #[ts(optional = nullable)]
    pub store_syncs_commits: Option<bool>,
    /// Every capability name the daemon binary serves. Empty from a daemon
    /// that predates the field, and when an older entry relays a newer peer's
    /// record, so empty claims nothing.
    #[ts(as = "Option<BTreeSet<String>>", optional)]
    pub capabilities: BTreeSet<String>,
    /// The daemon's Management Identity as 64 hex digits, so a member can tell a
    /// Machine that asks whether the Cluster forgot it. Absent from a record written
    /// by a daemon that predates the field, so absence claims nothing.
    #[serde(default, with = "crate::management_identity_hex")]
    #[ts(skip)]
    pub management_identity: Option<ManagementIdentity>,
    /// Whether this Machine is leaving the Cluster: its own daemon sets it once a Leave
    /// is admitted, and its row goes when the Leave completes. False claims nothing: a
    /// daemon from before the field never sets it, and an older entry Machine drops it
    /// when it relays the row.
    #[ts(as = "Option<bool>", optional)]
    pub leaving: bool,
}

/// How many Builds one Machine runs at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct BuildConcurrency(NonZeroU8);

impl BuildConcurrency {
    /// One Build at a time.
    pub const ONE: Self = Self(NonZeroU8::MIN);
    const AUTOMATIC_MAX: u64 = 4;
    const AUTOMATIC_BYTES_PER_BUILD: u64 = 4_000_000_000;

    /// Parse an explicit count of simultaneous Builds.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] unless `value` is an integer from 1 to 255.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        value
            .parse::<NonZeroU8>()
            .map(Self)
            .map_err(|_| ValueError::new("Build concurrency", value, "an integer from 1 to 255"))
    }

    /// 1 for a Machine that accepts Services, otherwise one Build per 4 GB of
    /// RAM, clamped to 1–4. Unknown memory builds one at a time.
    #[must_use]
    pub fn automatic(accepts_services: bool, memory_total_bytes: Option<u64>) -> Self {
        let builds = match memory_total_bytes {
            Some(bytes) if !accepts_services => {
                (bytes / Self::AUTOMATIC_BYTES_PER_BUILD).clamp(1, Self::AUTOMATIC_MAX)
            }
            _ => 1,
        };
        Self(NonZeroU8::new(u8::try_from(builds).expect("clamped to 1–4")).expect("clamped to 1–4"))
    }
}

impl From<BuildConcurrency> for usize {
    fn from(value: BuildConcurrency) -> Self {
        usize::from(value.0.get())
    }
}

impl fmt::Display for BuildConcurrency {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for BuildConcurrency {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// Keep, clear to automatic, or set a Machine's build concurrency.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", tag = "action", content = "value")]
pub enum BuildConcurrencyUpdate {
    #[default]
    Keep,
    Automatic,
    Set(BuildConcurrency),
}

/// Storage preparation requested while enrolling one Machine.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StorageChoice {
    #[default]
    None,
    Zfs,
}

impl StorageChoice {
    /// Parse an installer storage choice.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] unless `value` is `none` or `zfs`.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        match value {
            "none" => Ok(Self::None),
            "zfs" => Ok(Self::Zfs),
            _ => Err(ValueError::new("Storage Choice", value, "none or zfs")),
        }
    }

    /// Installer environment spelling for this choice.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Zfs => "zfs",
        }
    }
}

impl fmt::Display for StorageChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for StorageChoice {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

/// An absolute device path chosen for ZFS preparation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ZfsDevice(std::path::PathBuf);

impl ZfsDevice {
    /// Validates an absolute device path without resolving it on the caller's host.
    ///
    /// # Errors
    /// Returns an error when the path is not absolute.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        let path = std::path::Path::new(value);
        if !path.is_absolute() {
            return Err(ValueError::new(
                "ZFS device",
                value,
                "an absolute device path",
            ));
        }
        Ok(Self(path.to_owned()))
    }

    /// The chosen device path, retaining stable symlinks.
    #[must_use]
    pub fn as_path(&self) -> &std::path::Path {
        &self.0
    }
}

impl fmt::Display for ZfsDevice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.display().fmt(formatter)
    }
}

/// Installer intent. The backing device stays outside durable Machine RPC records.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum StoragePreparation {
    #[default]
    None,
    Zfs {
        device: Option<ZfsDevice>,
    },
}

impl StoragePreparation {
    /// The storage mode carried by enrollment RPC and allocation history.
    #[must_use]
    pub const fn choice(&self) -> StorageChoice {
        match self {
            Self::None => StorageChoice::None,
            Self::Zfs { .. } => StorageChoice::Zfs,
        }
    }

    /// Parse `none`, `zfs`, or `zfs:<absolute device path>`.
    ///
    /// # Errors
    /// Returns an error for an unknown mode or a device path that is not absolute.
    pub fn parse(value: impl AsRef<str>) -> Result<Self, ValueError> {
        let value = value.as_ref();
        match value {
            "none" => Ok(Self::None),
            "zfs" => Ok(Self::Zfs { device: None }),
            _ => match value.strip_prefix("zfs:") {
                Some(device) => Ok(Self::Zfs {
                    device: Some(ZfsDevice::parse(device)?),
                }),
                _ => Err(ValueError::new(
                    "Storage Preparation",
                    value,
                    "none, zfs, or zfs:<absolute device path>",
                )),
            },
        }
    }
}

impl From<StorageChoice> for StoragePreparation {
    fn from(choice: StorageChoice) -> Self {
        match choice {
            StorageChoice::None => Self::None,
            StorageChoice::Zfs => Self::Zfs { device: None },
        }
    }
}

impl fmt::Display for StoragePreparation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => formatter.write_str("none"),
            Self::Zfs { device: None } => formatter.write_str("zfs"),
            Self::Zfs {
                device: Some(device),
            } => write!(formatter, "zfs:{device}"),
        }
    }
}

impl FromStr for StoragePreparation {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", tag = "mode", content = "address")]
pub enum PublicIpDiscovery {
    #[default]
    Auto,
    Disabled,
    Override(IpAddr),
}

/// Which of its own addresses a Machine's operator chose. Daemon start refreshes
/// only the unpinned ones from discovery. Kept on the Machine, never replicated.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(default)]
pub struct PinnedAddresses {
    pub public_ip: bool,
    pub advertised_endpoints: bool,
}

impl PinnedAddresses {
    /// Pins for what an enrollment asked for: any public IP choice but `auto`,
    /// and any explicit endpoint list.
    #[must_use]
    pub fn chosen(
        public_ip: &PublicIpDiscovery,
        advertised_endpoints: &[AdvertisedEndpoint],
    ) -> Self {
        Self {
            public_ip: *public_ip != PublicIpDiscovery::Auto,
            advertised_endpoints: !advertised_endpoints.is_empty(),
        }
    }

    /// These pins plus every address `update` sets.
    #[must_use]
    pub fn with_update(self, update: &MachineUpdate) -> Self {
        Self {
            public_ip: self.public_ip || update.public_ip != PublicIpUpdate::Keep,
            advertised_endpoints: self.advertised_endpoints
                || update.advertised_endpoints.is_some(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineToken {
    pub id: MachineId,
    pub public_key: WireGuardPublicKey,
    #[serde(default)]
    pub public_ip: Option<IpAddr>,
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub runtime: MachineRuntime,
    // Capacity is nullable when the daemon could not observe it.
    #[serde(default)]
    pub memory_total_bytes: Option<u64>,
    #[serde(default)]
    pub disk_total_bytes: Option<u64>,
    #[serde(default)]
    pub disk_available_bytes: Option<u64>,
}

#[cfg(test)]
mod machine_token_tests {
    use super::*;

    #[test]
    fn capacity_is_nullable_and_missing_fields_decode_for_older_daemons() {
        let token = MachineToken {
            id: MachineId::random(),
            public_key: WireGuardPublicKey([0; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: MachineRuntime::default(),
            memory_total_bytes: None,
            disk_total_bytes: None,
            disk_available_bytes: None,
        };
        let mut wire = serde_json::to_value(&token).unwrap();
        assert_eq!(
            wire.get("memory_total_bytes"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(wire.get("disk_total_bytes"), Some(&serde_json::Value::Null));
        assert_eq!(
            wire.get("disk_available_bytes"),
            Some(&serde_json::Value::Null)
        );

        let object = wire.as_object_mut().unwrap();
        object.remove("memory_total_bytes");
        object.remove("disk_total_bytes");
        object.remove("disk_available_bytes");
        assert_eq!(serde_json::from_value::<MachineToken>(wire).unwrap(), token);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineIdentity {
    pub id: MachineId,
    pub name: MachineName,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case", tag = "action", content = "value")]
pub enum PublicIpUpdate {
    #[default]
    Keep,
    Remove,
    Set(IpAddr),
}

/// One atomic metadata edit; omitted fields and Label keys preserve current values.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineUpdate {
    /// One change per Label key: a value sets it, `None` removes it.
    #[serde(default)]
    pub label_changes: BTreeMap<MachineLabelKey, Option<MachineLabelValue>>,
    /// Change Build acceptance independently; `None` preserves it and existing work remains.
    #[serde(default)]
    pub accepts_builds: Option<bool>,
    /// Change application Service acceptance independently; `None` preserves it.
    #[serde(default)]
    pub accepts_services: Option<bool>,
    /// Change trusted Ingress acceptance independently; `None` preserves it.
    #[serde(default)]
    pub accepts_ingress: Option<bool>,
    /// Replace the Machine Name, or preserve it when omitted.
    #[serde(default)]
    pub name: Option<MachineName>,
    /// Explicitly preserve, remove, or replace the advertised public IP.
    #[serde(default)]
    pub public_ip: PublicIpUpdate,
    /// Replace all Advertised Endpoints, or preserve them when omitted.
    #[serde(default)]
    pub advertised_endpoints: Option<Vec<AdvertisedEndpoint>>,
    /// Explicitly preserve, clear to automatic, or set build concurrency.
    #[serde(default)]
    pub build_concurrency: BuildConcurrencyUpdate,
}

impl MachineUpdate {
    /// Whether the patch requests no metadata changes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.label_changes.is_empty()
            && self.accepts_builds.is_none()
            && self.accepts_services.is_none()
            && self.accepts_ingress.is_none()
            && self.name.is_none()
            && self.public_ip == PublicIpUpdate::Keep
            && self.advertised_endpoints.is_none()
            && self.build_concurrency == BuildConcurrencyUpdate::Keep
    }
}

/// A metadata edit that cannot be applied to the observed Machine set.
#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum MachineUpdateError {
    /// Another observed Machine already uses the requested Name.
    #[error("Machine name is already visible on another Machine")]
    DuplicateName,
    /// Replacing Advertised Endpoints with an empty list is not allowed.
    #[error("at least one Advertised Endpoint is required")]
    MissingEndpoints,
}

/// Apply one complete edit without modifying the source Machine.
///
/// # Errors
/// Rejects a Name used by another visible Machine or an empty endpoint replacement.
pub fn apply_machine_update(
    machine: &Machine,
    visible: &[Machine],
    update: MachineUpdate,
) -> Result<Machine, MachineUpdateError> {
    if update.name.as_ref().is_some_and(|name| {
        name != &machine.name
            && visible
                .iter()
                .any(|other| other.id != machine.id && &other.name == name)
    }) {
        return Err(MachineUpdateError::DuplicateName);
    }
    if update
        .advertised_endpoints
        .as_ref()
        .is_some_and(Vec::is_empty)
    {
        return Err(MachineUpdateError::MissingEndpoints);
    }

    let mut updated = machine.clone();
    for (key, value) in update.label_changes {
        if let Some(value) = value {
            updated.labels.insert(key, value);
        } else {
            updated.labels.remove(&key);
        }
    }
    if let Some(accepts) = update.accepts_builds {
        updated.accepts_builds = accepts;
    }
    if let Some(accepts) = update.accepts_services {
        updated.accepts_services = accepts;
    }
    if let Some(accepts) = update.accepts_ingress {
        updated.accepts_ingress = accepts;
    }
    if let Some(name) = update.name {
        updated.name = name;
    }
    match update.public_ip {
        PublicIpUpdate::Keep => {}
        PublicIpUpdate::Remove => updated.public_ip = None,
        PublicIpUpdate::Set(address) => updated.public_ip = Some(address),
    }
    if let Some(endpoints) = update.advertised_endpoints {
        updated.advertised_endpoints = endpoints;
    }
    match update.build_concurrency {
        BuildConcurrencyUpdate::Keep => {}
        BuildConcurrencyUpdate::Automatic => updated.build_concurrency = None,
        BuildConcurrencyUpdate::Set(concurrency) => {
            updated.build_concurrency = Some(concurrency);
        }
    }
    Ok(updated)
}

/// Current storage evidence observed from one Machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MachineStorageObservation {
    /// Usable ZFS support was not observed.
    Stateless,
    /// ZFS is usable and no Machine Pool is imported.
    Ready,
    /// Exactly one writable `ONLINE` or `DEGRADED` Machine Pool is imported.
    Pool {
        /// Current ZFS Pool size in bytes.
        size_bytes: NonZeroU64,
        /// Current allocated ZFS Pool bytes.
        used_bytes: u64,
        /// Current free ZFS Pool bytes.
        free_bytes: u64,
    },
}

/// An observer-relative view layered over a Machine's advertised record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineObservation {
    pub machine: Machine,
    pub membership: MembershipObservation,
    /// Raw admin evidence, absent for synthetic membership and older responders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub membership_evidence: Option<MembershipEvidence>,
    /// Current storage evidence, absent when this observer could not obtain it.
    #[serde(default)]
    pub storage: Option<MachineStorageObservation>,
    #[serde(default)]
    pub selected_endpoint: Option<SelectedEndpoint>,
    /// Entry-local RTT. `ListMachines` omits it; Runtime Watch may include it.
    #[serde(default)]
    pub rtt: Option<RttStatistics>,
    /// The entry's mesh path to this Machine. Absent from the entry's own row, before the
    /// entry has probed the path, and from daemons before 0.3.7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub path: Option<PathObservation>,
}

impl MachineObservation {
    /// Recognized responding peers invite RPC; unrecognized admin values never do.
    #[must_use]
    pub fn invites_rpc(&self) -> bool {
        self.membership_evidence.as_ref().map_or_else(
            || self.membership.invites_rpc(),
            MembershipEvidence::invites_rpc,
        )
    }

    /// An Unarrived Machine: omitted, never recorded by this observer's admin, and with
    /// no daemon version in its row. Cloud's Register writes an empty runtime, and a
    /// daemon publishes its own row, version included, only once it participates, so
    /// this observer has no sign the Machine ever ran or held a Service Container.
    #[must_use]
    pub fn unarrived(&self) -> bool {
        !self.invites_rpc()
            && self.membership_evidence.is_none()
            && self.machine.runtime.daemon_version.is_empty()
    }

    /// Start an observer-relative Machine view with optional observations absent.
    #[must_use]
    pub fn new(machine: Machine, membership: MembershipObservation) -> Self {
        Self {
            machine,
            membership,
            membership_evidence: None,
            storage: None,
            selected_endpoint: None,
            rtt: None,
            path: None,
        }
    }
}

/// Match a Machine by exact ID or observer-relative Name. `all` is identity text.
#[must_use]
pub fn machine_matches_target(machine: &Machine, target: &MachineTarget) -> bool {
    machine.id.as_str() == target.as_str() || machine.name.as_str() == target.as_str()
}

/// Empty Placement is every Machine; otherwise every constraint must match.
#[must_use]
pub fn machine_matches_placement(machine: &Machine, placement: &Placement) -> bool {
    placement
        .constraints
        .iter()
        .all(|constraint| constraint.matches(machine))
}

/// Resolve fan-out selection to visible Machines. `*` selects every visible Machine;
/// other text is a Machine Target.
///
/// # Errors
///
/// Returns [`MachineSelectorError::NoTargets`] when `selectors` is empty,
/// [`MachineSelectorError::NotFound`] when a Machine Target matches nothing,
/// [`MachineSelectorError::Ambiguous`] when a name matches more than one Machine,
/// or [`MachineSelectorError::NoVisibleMachines`] when `*` matches no Machines.
pub fn resolve_machine_selectors(
    visible: &[Machine],
    selectors: &[FanoutSelector],
) -> Result<Vec<Machine>, MachineSelectorError> {
    if selectors.is_empty() {
        return Err(MachineSelectorError::NoTargets);
    }
    let mut targets = Vec::new();
    let mut seen = BTreeSet::new();
    let mut missing = Vec::new();
    for selector in selectors {
        match selector {
            FanoutSelector::All => {
                for machine in visible {
                    if seen.insert(machine.id) {
                        targets.push(machine.clone());
                    }
                }
            }
            FanoutSelector::One(target) => match target.resolve(visible) {
                NameMatches::One(machine) if seen.insert(machine.id) => {
                    targets.push(machine.clone());
                }
                NameMatches::One(_) => {}
                NameMatches::None => missing.push(target.clone()),
                matches @ NameMatches::Ambiguous { .. } => {
                    return Err(MachineSelectorError::Ambiguous {
                        selector: target.clone(),
                        matches: matches.iter().map(|machine| machine.id).collect(),
                    });
                }
            },
        }
    }
    if !missing.is_empty() {
        Err(MachineSelectorError::NotFound(missing))
    } else if targets.is_empty() {
        Err(MachineSelectorError::NoVisibleMachines)
    } else {
        Ok(targets)
    }
}

#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum MachineSelectorError {
    #[error("no Machine targets were requested")]
    NoTargets,
    #[error("no Machines are visible to this entry Machine")]
    NoVisibleMachines,
    #[error("No Server named {}", .0.iter().map(|target| format!("{:?}", target.as_str())).collect::<Vec<_>>().join(", "))]
    NotFound(Vec<MachineTarget>),
    #[error("{} matches more than one Server; name one by ID: {}", .selector.as_str().escape_debug(), .matches.iter().map(ToString::to_string).collect::<Vec<_>>().join(", "))]
    Ambiguous {
        selector: MachineTarget,
        matches: Vec<MachineId>,
    },
}

/// Membership Observation of advertised Machines from the responder's admin view.
///
/// The responder is Up. A peer absent from `states` is Down. Selected endpoint
/// and RTT start empty so the caller can overlay live values.
#[must_use]
pub fn synthesize_membership(
    machines: Vec<Machine>,
    responder_id: &MachineId,
    states: &BTreeMap<ManagementAddress, MembershipEvidence>,
) -> Vec<MachineObservation> {
    machines
        .into_iter()
        .map(|machine| {
            let evidence = (&machine.id != responder_id)
                .then(|| states.get(&machine.management_address()).cloned())
                .flatten();
            let membership = if &machine.id == responder_id {
                MembershipObservation::Up
            } else {
                evidence
                    .as_ref()
                    .map_or(MembershipObservation::Down, MembershipEvidence::membership)
            };
            MachineObservation {
                membership_evidence: evidence,
                ..MachineObservation::new(machine, membership)
            }
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RttStatistics {
    pub median_ns: u64,
    pub population_stddev_ns: u64,
}

/// One directed Corrosion RTT observation, retaining the peer's native identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RttObservation {
    pub peer_id: String,
    pub address: SocketAddr,
    #[serde(default)]
    pub machine: Option<MachineIdentity>,
    pub statistics: RttStatistics,
    /// The observing Machine's mesh path to this peer. Absent before it has probed the
    /// path, for a peer it can't name, and from daemons before 0.3.7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub path: Option<PathObservation>,
}

/// One Machine's observation of its WireGuard path to a peer, from the full-size probes
/// it sends that peer's management address.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PathObservation {
    /// Bytes in the largest inner packet this Machine sends the peer: the full
    /// WireGuard MTU, or the lower route MTU it set when larger packets were lost.
    pub mtu: u32,
    pub large_packets: LargePackets,
    /// Whether `mtu` is below this Machine's WireGuard MTU, because larger packets were lost.
    /// Absent from Machines before it was added, which read as not lowered.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub lowered: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LargePackets {
    /// Packets of `mtu` bytes cross the path both ways.
    Carried,
    /// Small packets cross, but even a 1280-byte packet, the smallest MTU IPv6 allows,
    /// is lost: no MTU this Machine can set carries a large reply.
    Dropped,
    /// A value introduced after this build.
    #[serde(other)]
    Unrecognized,
}

#[must_use]
pub fn rtt_statistics(samples_ms: &[f64]) -> Option<RttStatistics> {
    if samples_ms.is_empty() {
        return None;
    }
    let mut sorted = samples_ms.to_vec();
    sorted.sort_by(f64::total_cmp);
    let middle = sorted.len() / 2;
    let upper = *sorted
        .get(middle)
        .expect("a non-empty sample set has a median");
    let median = if sorted.len().is_multiple_of(2) {
        let lower = *sorted
            .get(middle - 1)
            .expect("an even non-empty sample set has a lower median");
        (lower + upper) / 2.0
    } else {
        upper
    };
    let mean = sorted.iter().sum::<f64>() / sorted.len() as f64;
    let variance = sorted
        .iter()
        .map(|sample| (sample - mean).powi(2))
        .sum::<f64>()
        / sorted.len() as f64;
    Some(RttStatistics {
        median_ns: (median * 1_000_000.0) as u64,
        population_stddev_ns: (variance.sqrt() * 1_000_000.0) as u64,
    })
}

crate::value::open_string_enum!(LocalMachinePhase, Unrecognized {
    Uninitialized => "uninitialized",
    Joining => "joining",
    Participating => "participating",
    Resetting => "resetting",
});

crate::value::open_string_enum!(MembershipObservation, Unrecognized {
    Unknown => "unknown",
    Up => "up",
    Suspect => "suspect",
    Down => "down",
});

/// Membership evidence from the Entry Machine's admin decoder. The tag preserves
/// unrecognized raw values even when they equal a known Membership Observation wire value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MembershipEvidence {
    Up,
    Suspect,
    Down,
    Unrecognized { raw: String },
}

impl MembershipEvidence {
    /// The legacy observation value, without discarding the separate decoder evidence.
    #[must_use]
    pub fn membership(&self) -> MembershipObservation {
        match self {
            Self::Up => MembershipObservation::Up,
            Self::Suspect => MembershipObservation::Suspect,
            Self::Down => MembershipObservation::Down,
            Self::Unrecognized { raw } => MembershipObservation::Unrecognized(raw.clone()),
        }
    }

    /// Only recognized Up and Suspect evidence invites a peer RPC.
    #[must_use]
    pub fn invites_rpc(&self) -> bool {
        matches!(self, Self::Up | Self::Suspect)
    }
}

impl MembershipObservation {
    /// Up and Suspect invite one peer RPC. Down, Unknown, and Unrecognized do not.
    #[must_use]
    pub fn invites_rpc(&self) -> bool {
        matches!(self, Self::Up | Self::Suspect)
    }
}

#[cfg(test)]
mod placement_tests {
    use crate::{MachineId, MachineName, MachineSubnet, Placement, WireGuardPublicKey};

    use super::{
        BuildConcurrency, BuildConcurrencyUpdate, Machine, MachineObservation, MachineUpdate,
        MembershipEvidence, MembershipObservation, machine_matches_placement,
    };

    fn machine(hex: char, name: &str) -> Machine {
        Machine {
            labels: Default::default(),
            accepts_builds: true,
            accepts_services: true,
            accepts_ingress: true,
            id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: MachineSubnet::parse("10.210.0.0/24").unwrap(),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
            build_concurrency: None,
        }
    }

    #[test]
    fn automatic_build_concurrency_follows_services_role_and_ram() {
        const GB: u64 = 1_000_000_000;
        let automatic =
            |services, memory| usize::from(BuildConcurrency::automatic(services, memory));
        assert_eq!(automatic(true, Some(64 * GB)), 1);
        assert_eq!(automatic(false, None), 1);
        assert_eq!(automatic(false, Some(2 * GB)), 1);
        assert_eq!(automatic(false, Some(8 * GB + 1)), 2);
        assert_eq!(automatic(false, Some(12 * GB)), 3);
        assert_eq!(automatic(false, Some(64 * GB)), 4);
        assert!(BuildConcurrency::parse("0").is_err());
        assert!(BuildConcurrency::parse("256").is_err());
        assert_eq!(usize::from(BuildConcurrency::parse("8").unwrap()), 8);
    }

    #[test]
    fn older_records_decode_as_automatic_and_updates_keep_clear_or_set() {
        let mut machine = machine('a', "builder");
        machine.accepts_services = false;
        machine.runtime.memory_total_bytes = Some(16_500_000_000);
        let mut wire = serde_json::to_value(&machine).unwrap();
        let object = wire.as_object_mut().unwrap();
        object.remove("build_concurrency");
        object
            .get_mut("runtime")
            .and_then(serde_json::Value::as_object_mut)
            .unwrap()
            .remove("memory_total_bytes");
        let older: Machine = serde_json::from_value(wire).unwrap();
        assert_eq!(older.build_concurrency, None);
        assert_eq!(older.runtime.memory_total_bytes, None);
        assert_eq!(usize::from(machine.effective_build_concurrency()), 4);

        let two = BuildConcurrency::parse("2").unwrap();
        let update = |machine: &Machine, build_concurrency| {
            super::apply_machine_update(
                machine,
                &[],
                MachineUpdate {
                    build_concurrency,
                    ..Default::default()
                },
            )
            .unwrap()
        };
        assert!(
            !MachineUpdate {
                build_concurrency: BuildConcurrencyUpdate::Automatic,
                ..Default::default()
            }
            .is_empty()
        );
        let set = update(&machine, BuildConcurrencyUpdate::Set(two));
        assert_eq!(set.effective_build_concurrency(), two);
        assert_eq!(update(&set, BuildConcurrencyUpdate::Keep), set);
        let cleared = update(&set, BuildConcurrencyUpdate::Automatic);
        assert_eq!(cleared.build_concurrency, None);
        assert_eq!(usize::from(cleared.effective_build_concurrency()), 4);
    }

    #[test]
    fn only_an_omitted_row_with_no_sign_of_running_is_unarrived() {
        let registered =
            MachineObservation::new(machine('a', "claimed"), MembershipObservation::Down);
        assert!(registered.unarrived());

        let mut recorded = registered.clone();
        recorded.membership_evidence = Some(MembershipEvidence::Down);
        assert!(!recorded.unarrived(), "the admin recorded it, even as Down");

        let mut published = registered.clone();
        published.machine.runtime.daemon_version = "0.3.0".into();
        assert!(!published.unarrived(), "the Machine published its own row");

        let mut answering = registered;
        answering.membership = MembershipObservation::Up;
        assert!(!answering.unarrived(), "it invites RPC");
    }

    #[test]
    fn empty_placement_matches_every_machine() {
        let first = machine('a', "first");
        assert!(machine_matches_placement(&first, &Placement::default()));
    }

    #[test]
    fn placement_constraints_follow_swarm_matching() {
        let mut first = machine('a', "first");
        first
            .labels
            .insert("Region".parse().unwrap(), "EU-West".parse().unwrap());
        for (expressions, expected) in [
            (vec!["node.labels.Region==eu-WEST"], true),
            (vec!["node.labels.region==eu-west"], false),
            (vec!["node.labels.missing!=anything"], true),
            (vec!["node.labels.missing==anything"], false),
            (vec!["node.labels.Region==eu-*"], false),
            (
                vec![
                    "node.labels.Region==eu-west",
                    "node.id!=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                ],
                false,
            ),
            (vec!["node.id==AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"], true),
        ] {
            let placement = Placement {
                constraints: expressions
                    .into_iter()
                    .map(|text| crate::PlacementConstraint::parse(text).unwrap())
                    .collect(),
            };
            assert_eq!(machine_matches_placement(&first, &placement), expected);
        }
    }
}

#[cfg(test)]
mod machine_runtime_capabilities_tests {
    use serde_json::json;

    use super::MachineRuntime;

    #[test]
    fn runtime_without_capabilities_decodes_to_an_empty_set() {
        let runtime: MachineRuntime =
            serde_json::from_value(json!({ "daemon_version": "0.2.4" })).unwrap();
        assert!(runtime.capabilities.is_empty());
        assert_eq!(runtime.store_filesystem, None);
    }

    #[test]
    fn runtime_store_filesystem_round_trips() {
        let runtime: MachineRuntime = serde_json::from_value(json!({
            "daemon_version": "0.3.0",
            "store_filesystem": "zfs",
        }))
        .unwrap();
        assert_eq!(runtime.store_filesystem.as_deref(), Some("zfs"));
        let encoded = serde_json::to_value(&runtime).unwrap();
        assert_eq!(
            serde_json::from_value::<MachineRuntime>(encoded).unwrap(),
            runtime
        );
    }

    /// A row written by a daemon that encodes the identity differently must still load
    /// as a Machine; an identity this daemon cannot read claims nothing, like none.
    #[test]
    fn runtime_with_an_unreadable_management_identity_decodes_to_none() {
        for unreadable in [
            json!("not-hex"),
            json!([]),
            json!({}),
            json!(7),
            json!(true),
        ] {
            let runtime: MachineRuntime = serde_json::from_value(json!({
                "daemon_version": "0.3.2",
                "management_identity": unreadable,
            }))
            .unwrap();
            assert_eq!(runtime.management_identity, None, "{unreadable}");
        }
    }

    #[test]
    fn runtime_with_capabilities_decodes_the_set() {
        let runtime: MachineRuntime = serde_json::from_value(json!({
            "daemon_version": "0.2.5",
            "capabilities": ["ployz.logs.tail.v1", "ployz.build.v1", "ployz.logs.tail.v1"],
        }))
        .unwrap();
        assert_eq!(
            runtime.capabilities.into_iter().collect::<Vec<_>>(),
            ["ployz.build.v1", "ployz.logs.tail.v1"]
        );
    }
}
