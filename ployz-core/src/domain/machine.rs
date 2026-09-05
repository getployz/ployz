use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    net::{IpAddr, SocketAddr},
    num::NonZeroU64,
    str::FromStr,
};

use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, de};

use super::NameMatches;
use crate::{
    AdvertisedEndpoint, FanoutSelector, MachineId, MachineName, MachineSubnet, MachineTarget,
    ManagementAddress, PairingCredential, Placement, QualifiedService, SelectedEndpoint,
    ValueError, WireGuardPublicKey,
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

/// One Machine's durable advertised record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub id: MachineId,
    pub name: MachineName,
    pub subnet: MachineSubnet,
    pub management_address: ManagementAddress,
    pub public_key: WireGuardPublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<IpAddr>,
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub runtime: MachineRuntime,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineRuntime {
    pub daemon_version: String,
    pub docker_version: String,
    pub hostname: String,
    pub architecture: String,
    pub os_pretty_name: String,
    pub kernel_version: String,
}

/// Storage preparation requested while enrolling one Machine.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "address")]
pub enum PublicIpDiscovery {
    #[default]
    Auto,
    Disabled,
    Override(IpAddr),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineToken {
    pub public_key: WireGuardPublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_ip: Option<IpAddr>,
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub runtime: MachineRuntime,
    // This token crosses independently upgraded CLI/daemon processes. Current
    // daemons always serialize these keys; None means the daemon could not
    // observe that capacity. Keep defaults on every additive observation so a
    // newer CLI can still enroll through an older remote daemon. Removing one
    // turns a harmless missing fact into a hard --connect wire break.
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineIdentity {
    pub id: MachineId,
    pub name: MachineName,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "action", content = "value")]
pub enum PublicIpUpdate {
    #[default]
    Keep,
    Remove,
    Set(IpAddr),
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<MachineName>,
    #[serde(default)]
    pub public_ip: PublicIpUpdate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub advertised_endpoints: Option<Vec<AdvertisedEndpoint>>,
}

impl MachineUpdate {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.name.is_none()
            && self.public_ip == PublicIpUpdate::Keep
            && self.advertised_endpoints.is_none()
    }
}

#[derive(Clone, Debug, Eq, thiserror::Error, PartialEq)]
pub enum MachineUpdateError {
    #[error("Machine name is already visible on another Machine")]
    DuplicateName,
    #[error("at least one Advertised Endpoint is required")]
    MissingEndpoints,
}

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
    Ok(updated)
}

/// Current storage evidence observed from one Machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

/// The latest failed Global reconciliation attempt for one Service on a Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GlobalReconcileFailureObservation {
    /// Global Service whose local slot could not be reconciled.
    pub service: QualifiedService,
    /// Last error returned by this Machine's Global slot ensure or retirement path.
    pub last_error: String,
    /// RFC 3339 time of the failed reconciliation attempt.
    pub observed_at: String,
}

/// An observer-relative view layered over a Machine's advertised record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineObservation {
    pub machine: Machine,
    pub membership: MembershipObservation,
    /// Current storage evidence, absent when this observer could not obtain it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<MachineStorageObservation>,
    #[serde(default)]
    pub selected_endpoint: Option<SelectedEndpoint>,
    /// Entry-local RTT. `ListMachines` omits it; Runtime Watch may include it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt: Option<RttStatistics>,
    /// Current failed Machine-local Global reconciliations. Success removes an entry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub global_reconcile_failures: Vec<GlobalReconcileFailureObservation>,
}

impl MachineObservation {
    /// Start an observer-relative Machine view with optional observations absent.
    #[must_use]
    pub fn new(machine: Machine, membership: MembershipObservation) -> Self {
        Self {
            machine,
            membership,
            storage: None,
            selected_endpoint: None,
            rtt: None,
            global_reconcile_failures: Vec::new(),
        }
    }
}

/// Match a Machine by exact ID or observer-relative Name. `all` is identity text.
#[must_use]
pub fn machine_matches_target(machine: &Machine, target: &MachineTarget) -> bool {
    machine.id.as_str() == target.as_str() || machine.name.as_str() == target.as_str()
}

/// Empty Placement is every Machine; otherwise any Machine Target matches.
#[must_use]
pub(crate) fn machine_matches_placement(machine: &Machine, placement: &Placement) -> bool {
    placement.machines.is_empty()
        || placement
            .machines
            .iter()
            .any(|target| machine_matches_target(machine, target))
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
    #[error("Machine selectors were not found: {0:?}")]
    NotFound(Vec<MachineTarget>),
    #[error("Machine selector {selector:?} is ambiguous across IDs {matches:?}")]
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
    states: &BTreeMap<ManagementAddress, MembershipObservation>,
) -> Vec<MachineObservation> {
    machines
        .into_iter()
        .map(|machine| {
            let membership = if &machine.id == responder_id {
                MembershipObservation::Up
            } else {
                states
                    .get(&machine.management_address)
                    .cloned()
                    .unwrap_or(MembershipObservation::Down)
            };
            MachineObservation::new(machine, membership)
        })
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RttStatistics {
    pub median_ns: u64,
    pub population_stddev_ns: u64,
}

/// One directed Corrosion RTT observation, retaining the peer's native identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RttObservation {
    pub peer_id: String,
    pub address: SocketAddr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineIdentity>,
    pub statistics: RttStatistics,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WireGuardDevice {
    pub interface_name: String,
    pub public_key: WireGuardPublicKey,
    pub listen_port: u16,
    #[serde(default)]
    pub peers: Vec<WireGuardPeer>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WireGuardPeer {
    pub public_key: WireGuardPublicKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<SocketAddr>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_handshake_unix_seconds: Option<u64>,
    pub received_bytes: u64,
    pub sent_bytes: u64,
    #[serde(default)]
    pub allowed_ips: Vec<IpNet>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<MachineIdentity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtt: Option<RttStatistics>,
}

#[must_use]
pub fn associate_wireguard_peers(
    mut device: WireGuardDevice,
    machines: &[Machine],
    rtts: &BTreeMap<MachineId, RttStatistics>,
) -> WireGuardDevice {
    for peer in &mut device.peers {
        let mut matches = machines
            .iter()
            .filter(|machine| machine.public_key == peer.public_key);
        let Some(machine) = matches.next() else {
            continue;
        };
        if matches.next().is_some() {
            continue;
        }
        peer.machine = Some(MachineIdentity {
            id: machine.id,
            name: machine.name.clone(),
        });
        peer.rtt = rtts.get(&machine.id).cloned();
    }
    device
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

impl MembershipObservation {
    /// Up and Suspect invite one peer RPC. Down, Unknown, and Unrecognized do not.
    #[must_use]
    pub fn invites_rpc(&self) -> bool {
        matches!(self, Self::Up | Self::Suspect)
    }
}

/// Rejection when a Cloud response tries to hand a Machine a Dial Credential.
pub const DIAL_IN_PAIRING: &str = "Cloud Pairing must not carry a Dial Credential";

/// Cluster-scoped grant of a Cloud Relay endpoint and Pairing Credential.
///
/// Absence means no Machine dials Relay. The Dial Credential is not a field
/// here and is never stored on a Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudPairing {
    relay_url: String,
    secret: PairingCredential,
}

/// Cloud deploys ahead of installed CLIs, so unknown fields are ignored. A
/// `dial` field is still refused by name: a Machine never holds Dial.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CloudPairingWire {
    relay_url: String,
    secret: PairingCredential,
    #[serde(default)]
    dial: Option<de::IgnoredAny>,
}

impl CloudPairing {
    /// Build Cloud Pairing from a Relay endpoint and Pairing Credential.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when `relay_url` is empty.
    pub fn parse(
        relay_url: impl Into<String>,
        secret: PairingCredential,
    ) -> Result<Self, ValueError> {
        let relay_url = relay_url.into();
        if relay_url.is_empty() {
            return Err(ValueError::new(
                "Cloud Pairing relay URL",
                relay_url,
                "a non-empty URL",
            ));
        }
        Ok(Self { relay_url, secret })
    }

    /// Cloud Relay endpoint this Machine should dial. Not `--cloud-url`.
    #[must_use]
    pub fn relay_url(&self) -> &str {
        &self.relay_url
    }

    /// Pairing Credential used to authenticate Register.
    #[must_use]
    pub fn secret(&self) -> &PairingCredential {
        &self.secret
    }
}

impl<'de> Deserialize<'de> for CloudPairing {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = CloudPairingWire::deserialize(deserializer)?;
        if wire.dial.is_some() {
            return Err(de::Error::custom(DIAL_IN_PAIRING));
        }
        Self::parse(wire.relay_url, wire.secret).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod cloud_pairing_tests {
    use super::*;
    use serde_json::json;

    fn pairing() -> CloudPairing {
        CloudPairing::parse(
            "https://relay.example.invalid",
            PairingCredential::parse("pairing-secret").unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn cloud_pairing_wire_shape_is_relay_url_and_secret() {
        let value = serde_json::to_value(pairing()).unwrap();
        assert_eq!(
            value,
            json!({
                "relayUrl": "https://relay.example.invalid",
                "secret": "pairing-secret",
            })
        );
        assert_eq!(
            serde_json::from_value::<CloudPairing>(value).unwrap(),
            pairing()
        );
    }

    #[test]
    fn cloud_pairing_rejects_a_dial_credential_field() {
        let error = serde_json::from_value::<CloudPairing>(json!({
            "relayUrl": "https://relay.example.invalid",
            "secret": "pairing-secret",
            "dial": "dial-credential",
        }))
        .unwrap_err();
        assert!(error.to_string().contains(DIAL_IN_PAIRING), "{error}");
    }

    #[test]
    fn cloud_pairing_ignores_fields_the_cloud_adds_later() {
        let parsed = serde_json::from_value::<CloudPairing>(json!({
            "relayUrl": "https://relay.example.invalid",
            "secret": "pairing-secret",
            "privateRelayUrl": "http://relay.railway.internal",
        }))
        .unwrap();
        assert_eq!(parsed, pairing());
    }

    #[test]
    fn pairing_credential_and_relay_url_must_be_non_empty() {
        assert!(PairingCredential::parse("").is_err());
        assert!(
            CloudPairing::parse("", PairingCredential::parse("pairing-secret").unwrap()).is_err()
        );
    }

    #[test]
    fn pairing_credential_debug_redacts_the_bearer() {
        let secret = PairingCredential::parse("pairing-secret").unwrap();
        assert_eq!(format!("{secret:?}"), "PairingCredential(..)");
        assert!(!format!("{:?}", pairing()).contains("pairing-secret"));
    }
}

#[cfg(test)]
mod placement_tests {
    use std::net::Ipv6Addr;

    use crate::{
        MachineId, MachineName, MachineSubnet, MachineTarget, ManagementAddress, Placement,
        WireGuardPublicKey,
    };

    use super::{Machine, machine_matches_placement};

    fn machine(hex: char, name: &str) -> Machine {
        Machine {
            id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: MachineSubnet::parse("10.210.0.0/24").unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
        }
    }

    #[test]
    fn empty_placement_matches_every_machine() {
        let first = machine('a', "first");
        assert!(machine_matches_placement(&first, &Placement::default()));
    }

    #[test]
    fn placement_targets_match_by_name_or_id() {
        let first = machine('a', "first");
        let by_name = Placement {
            machines: vec![MachineTarget::parse("first").unwrap()],
        };
        let by_id = Placement {
            machines: vec![MachineTarget::parse(first.id.as_str()).unwrap()],
        };
        let other = Placement {
            machines: vec![MachineTarget::parse("other").unwrap()],
        };
        assert!(machine_matches_placement(&first, &by_name));
        assert!(machine_matches_placement(&first, &by_id));
        assert!(!machine_matches_placement(&first, &other));
    }
}
