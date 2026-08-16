use std::{
    collections::{BTreeMap, BTreeSet},
    net::{IpAddr, SocketAddr},
};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use super::NameMatches;
use crate::{
    AdvertisedEndpoint, FanoutSelector, MachineId, MachineName, MachineSubnet, MachineTarget,
    ManagementAddress, SelectedEndpoint, WireGuardPublicKey,
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

/// An observer-relative view layered over a Machine's advertised record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineObservation {
    pub machine: Machine,
    pub membership: MembershipObservation,
    #[serde(default)]
    pub selected_endpoint: Option<SelectedEndpoint>,
}

/// Match a Machine by exact ID or observer-relative Name. `all` is identity text.
#[must_use]
pub fn machine_matches_target(machine: &Machine, target: &MachineTarget) -> bool {
    machine.id.as_str() == target.as_str() || machine.name.as_str() == target.as_str()
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
                NameMatches::Ambiguous(matches) => {
                    return Err(MachineSelectorError::Ambiguous {
                        selector: target.clone(),
                        matches: matches.into_iter().map(|machine| machine.id).collect(),
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

#[must_use]
pub fn synthesize_membership(
    machines: Vec<Machine>,
    responder_id: &MachineId,
    states: &BTreeMap<ManagementAddress, MembershipObservation>,
) -> Vec<MachineObservation> {
    machines
        .into_iter()
        .map(|machine| MachineObservation {
            membership: if &machine.id == responder_id {
                MembershipObservation::Up
            } else {
                states
                    .get(&machine.management_address)
                    .cloned()
                    .unwrap_or(MembershipObservation::Down)
            },
            selected_endpoint: None,
            machine,
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
