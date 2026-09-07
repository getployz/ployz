use std::{
    collections::BTreeMap,
    net::{IpAddr, SocketAddr},
    process::{Command, Output},
    time::{Duration, SystemTime},
};

use ipnet::{IpNet, Ipv4Net};
use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineSubnet, SelectedEndpoint, WireGuardPublicKey,
};
pub use ployz_core::{CORROSION_GOSSIP_PORT, MACHINE_API_PORT, UNREGISTRY_PORT};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod endpoints;
mod firewall;
mod linux;

pub(crate) use endpoints::interface_addresses;
pub use endpoints::{DiscoveredNetwork, discover_network};
pub use firewall::apply_firewall_rules;
pub use linux::{NetworkPlane, inspect_wireguard_device};

pub const DOCKER_NETWORK_NAME: &str = "ployz";
pub const WIREGUARD_INTERFACE_NAME: &str = "ployz-wg";
pub const WIREGUARD_PORT: u16 = 51820;
pub const WIREGUARD_KEEPALIVE_SECONDS: u16 = 25;
pub const ENDPOINT_CONNECTION_TIMEOUT: Duration = Duration::from_secs(15);
pub const PEER_DOWN_INTERVAL: Duration = Duration::from_secs(180 + 5 + 90);

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WireGuardPrivateKey([u8; 32]);

impl WireGuardPrivateKey {
    #[must_use]
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn generate() -> Self {
        Self(defguard_wireguard_rs::key::Key::generate().as_array())
    }

    #[must_use]
    pub fn public_key(&self) -> WireGuardPublicKey {
        WireGuardPublicKey(
            defguard_wireguard_rs::key::Key::new(self.0)
                .public_key()
                .as_array(),
        )
    }

    fn encoded(&self) -> String {
        defguard_wireguard_rs::key::Key::new(self.0).to_string()
    }
}

impl std::fmt::Debug for WireGuardPrivateKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("WireGuardPrivateKey(REDACTED)")
    }
}

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("cluster IPv4 pool must contain /24 subnets")]
    InvalidClusterNetwork,
    #[error("cluster IPv4 pool has no free /24 in this observation")]
    NoFreeSubnet,
    #[error("configured Machine has no WireGuard private key")]
    MissingPrivateKey,
    #[error(
        "refusing to replace the existing Docker network: {reason}; expected: {expected}; observed: {observed}; safe recovery: {recovery}"
    )]
    DockerNetworkConflict {
        reason: String,
        expected: String,
        observed: String,
        recovery: &'static str,
    },
    #[error("network I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("Docker network operation failed: {0}")]
    Docker(#[from] bollard::errors::Error),
    #[error("WireGuard operation failed: {0}")]
    WireGuard(#[from] defguard_wireguard_rs::error::WireguardInterfaceError),
    #[error("endpoint discovery request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("endpoint discovery output is invalid: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{program} failed: {stderr}")]
    Command { program: String, stderr: String },
}

#[must_use]
pub fn default_cluster_network() -> Ipv4Net {
    "10.210.0.0/16".parse().expect("static network is valid")
}

pub use ployz_core::management_address;

pub fn allocate_machine_subnet(
    cluster_network: Ipv4Net,
    claimed: impl IntoIterator<Item = MachineSubnet>,
) -> Result<MachineSubnet, NetworkError> {
    let claimed = claimed.into_iter().collect::<Vec<_>>();
    let candidates = cluster_network
        .subnets(24)
        .map_err(|_| NetworkError::InvalidClusterNetwork)?;
    candidates
        .map(|candidate| {
            MachineSubnet::try_from(candidate)
                .expect("cluster /24 candidates are valid Machine Subnets")
        })
        .find(|candidate| !claimed.contains(candidate))
        .ok_or(NetworkError::NoFreeSubnet)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeer {
    pub machine_id: MachineId,
    pub public_key: WireGuardPublicKey,
    pub allowed_ips: [IpNet; 2],
    selection: EndpointSelection,
}

impl MeshPeer {
    #[must_use]
    pub fn selected(&self) -> Option<SelectedEndpoint> {
        self.selection.selected()
    }

    pub fn poll(
        &mut self,
        now: SystemTime,
        last_handshake: Option<SystemTime>,
        device_endpoint: Option<SocketAddr>,
    ) -> Option<SelectedEndpoint> {
        self.selection.poll(now, last_handshake, device_endpoint)
    }
}

#[must_use]
pub fn peers_for(observer_id: &MachineId, machines: &[Machine]) -> Vec<MeshPeer> {
    machines
        .iter()
        .filter(|machine| machine.id != *observer_id)
        .map(|machine| MeshPeer {
            machine_id: machine.id,
            public_key: machine.public_key,
            allowed_ips: [
                IpNet::new(IpAddr::V6(machine.management_address().0), 128)
                    .expect("IPv6 /128 is valid"),
                machine.subnet.into(),
            ],
            selection: EndpointSelection::from_advertised(machine.advertised_endpoints.clone()),
        })
        .collect()
}

fn attach_peer_selections(
    planned: Vec<MeshPeer>,
    previous: Vec<MeshPeer>,
    persisted: &BTreeMap<MachineId, SelectedEndpoint>,
    now: SystemTime,
) -> (Vec<MeshPeer>, Vec<(MachineId, SelectedEndpoint)>) {
    let mut previous = previous
        .into_iter()
        .map(|peer| (peer.machine_id, peer.selection))
        .collect::<BTreeMap<_, _>>();
    let mut newly_selected = Vec::new();
    let peers = planned
        .into_iter()
        .map(|mut peer| {
            let persisted = persisted.get(&peer.machine_id).copied();
            if let Some(mut selection) = previous.remove(&peer.machine_id) {
                if let Some(endpoint) =
                    selection.replace_candidates(&peer.selection.candidates, persisted, now)
                {
                    newly_selected.push((peer.machine_id, endpoint));
                }
                peer.selection = selection;
            } else {
                peer.selection.bind(persisted, now);
                if persisted.is_none()
                    && let Some(endpoint) = peer.selection.selected()
                {
                    newly_selected.push((peer.machine_id, endpoint));
                }
            }
            peer
        })
        .collect();
    (peers, newly_selected)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PeerStatus {
    Unknown,
    Up,
    Down,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EndpointSelection {
    candidates: Vec<AdvertisedEndpoint>,
    state: EndpointState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EndpointState {
    Unselected,
    Testing {
        endpoint: SelectedEndpoint,
        since: SystemTime,
    },
    Established(SelectedEndpoint),
}

impl EndpointSelection {
    fn from_advertised(candidates: Vec<AdvertisedEndpoint>) -> Self {
        Self {
            candidates,
            state: EndpointState::Unselected,
        }
    }

    fn bind(&mut self, selected: Option<SelectedEndpoint>, now: SystemTime) {
        self.state = initial_endpoint_state(&self.candidates, selected, now);
    }

    #[must_use]
    pub fn new(
        candidates: &[AdvertisedEndpoint],
        selected: Option<SelectedEndpoint>,
        now: SystemTime,
    ) -> Self {
        Self {
            candidates: candidates.to_vec(),
            state: initial_endpoint_state(candidates, selected, now),
        }
    }

    #[must_use]
    pub fn selected(&self) -> Option<SelectedEndpoint> {
        match self.state {
            EndpointState::Unselected => None,
            EndpointState::Testing { endpoint, .. } | EndpointState::Established(endpoint) => {
                Some(endpoint)
            }
        }
    }

    pub fn replace_candidates(
        &mut self,
        candidates: &[AdvertisedEndpoint],
        fallback: Option<SelectedEndpoint>,
        now: SystemTime,
    ) -> Option<SelectedEndpoint> {
        self.candidates = candidates.to_vec();
        if self.state == EndpointState::Unselected {
            self.state = initial_endpoint_state(candidates, fallback, now);
            return self.selected();
        }
        None
    }

    #[must_use]
    pub fn status(&self, now: SystemTime, last_handshake: Option<SystemTime>) -> PeerStatus {
        let since_handshake = elapsed(now, last_handshake);
        let since = match self.state {
            EndpointState::Unselected => return PeerStatus::Unknown,
            EndpointState::Established(_) => {
                return if since_handshake < PEER_DOWN_INTERVAL {
                    PeerStatus::Up
                } else {
                    PeerStatus::Down
                };
            }
            EndpointState::Testing { since, .. } => since,
        };
        let since_change = now.duration_since(since).unwrap_or_default();
        let handshook_after_change = last_handshake.is_some_and(|time| time > since);
        if since_change > PEER_DOWN_INTERVAL {
            if since_handshake < PEER_DOWN_INTERVAL {
                PeerStatus::Up
            } else {
                PeerStatus::Down
            }
        } else if since_change < ENDPOINT_CONNECTION_TIMEOUT {
            if handshook_after_change {
                PeerStatus::Up
            } else {
                PeerStatus::Unknown
            }
        } else if handshook_after_change {
            PeerStatus::Up
        } else {
            PeerStatus::Down
        }
    }

    pub fn poll(
        &mut self,
        now: SystemTime,
        last_handshake: Option<SystemTime>,
        device_endpoint: Option<std::net::SocketAddr>,
    ) -> Option<SelectedEndpoint> {
        if let Some(endpoint) = device_endpoint
            && self.selected().map(|selected| selected.0) != Some(endpoint)
        {
            let endpoint = SelectedEndpoint(endpoint);
            self.state = EndpointState::Established(endpoint);
            return Some(endpoint);
        }

        let status = self.status(now, last_handshake);
        let changed = (status == PeerStatus::Down)
            .then(|| self.next_endpoint())
            .flatten();
        if let Some(endpoint) = changed {
            self.state = EndpointState::Testing {
                endpoint,
                since: now,
            };
        }
        changed
    }

    fn next_endpoint(&self) -> Option<SelectedEndpoint> {
        let selected = self.selected()?;
        let next = self
            .candidates
            .iter()
            .position(|endpoint| endpoint.0 == selected.0)
            .and_then(|index| {
                (self.candidates.len() > 1).then_some((index + 1) % self.candidates.len())
            })
            .unwrap_or(0);
        self.candidates
            .get(next)
            .map(|endpoint| SelectedEndpoint(endpoint.0))
    }
}

fn initial_endpoint_state(
    candidates: &[AdvertisedEndpoint],
    selected: Option<SelectedEndpoint>,
    now: SystemTime,
) -> EndpointState {
    selected.map_or_else(
        || {
            candidates
                .first()
                .map_or(EndpointState::Unselected, |endpoint| {
                    EndpointState::Testing {
                        endpoint: SelectedEndpoint(endpoint.0),
                        since: now,
                    }
                })
        },
        EndpointState::Established,
    )
}

fn elapsed(now: SystemTime, earlier: Option<SystemTime>) -> Duration {
    earlier
        .and_then(|earlier| now.duration_since(earlier).ok())
        .unwrap_or(Duration::MAX)
}

fn checked_command(program: &str, args: &[&str]) -> Result<Output, NetworkError> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(NetworkError::Command {
            program: program.into(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, net::SocketAddr};

    use ployz_core::{MachineName, MachineRuntime};

    use super::*;

    fn endpoint(seed: u8) -> AdvertisedEndpoint {
        AdvertisedEndpoint(SocketAddr::from(([192, 0, 2, seed], 51820)))
    }

    fn machine(seed: u8, advertised: Vec<AdvertisedEndpoint>) -> Machine {
        Machine {
            id: MachineId::parse(format!("{seed:032x}")).unwrap(),
            name: MachineName::parse(format!("machine-{seed}")).unwrap(),
            subnet: format!("10.210.{seed}.0/24").parse().unwrap(),
            public_key: WireGuardPublicKey([seed; 32]),
            public_ip: None,
            advertised_endpoints: advertised,
            runtime: MachineRuntime::default(),
        }
    }

    #[test]
    fn start_and_rebuild_keep_unselected_peers() {
        let now = SystemTime::UNIX_EPOCH;
        let observer = machine(1, vec![endpoint(1)]);
        let silent = machine(2, vec![]);
        let machines = [observer.clone(), silent.clone()];
        let planned = peers_for(&observer.id, &machines);

        let (started, start_persist) =
            attach_peer_selections(planned, Vec::new(), &BTreeMap::new(), now);
        let started_peer = started.first().expect("start keeps the Unselected peer");
        assert_eq!(started_peer.machine_id, silent.id);
        assert_eq!(started_peer.selected(), None);
        assert!(start_persist.is_empty());

        let planned = peers_for(&observer.id, &machines);
        let (rebuilt, rebuild_persist) =
            attach_peer_selections(planned, started, &BTreeMap::new(), now);
        let rebuilt_peer = rebuilt.first().expect("rebuild keeps the Unselected peer");
        assert_eq!(rebuilt_peer.machine_id, silent.id);
        assert_eq!(rebuilt_peer.selected(), None);
        assert!(rebuild_persist.is_empty());
    }

    #[test]
    fn rebuild_selects_when_an_unselected_peer_gains_advertised_endpoints() {
        let now = SystemTime::UNIX_EPOCH;
        let observer = machine(1, vec![endpoint(1)]);
        let silent = machine(2, vec![]);
        let planned = peers_for(&observer.id, &[observer.clone(), silent]);
        let (started, _) = attach_peer_selections(planned, Vec::new(), &BTreeMap::new(), now);
        let speaking = machine(2, vec![endpoint(2)]);
        let speaking_id = speaking.id;
        let planned = peers_for(&observer.id, &[observer.clone(), speaking]);
        let (rebuilt, persist) = attach_peer_selections(planned, started, &BTreeMap::new(), now);
        assert_eq!(
            rebuilt
                .first()
                .expect("peer remains after advertised endpoints appear")
                .selected(),
            Some(SelectedEndpoint(endpoint(2).0))
        );
        assert_eq!(
            persist,
            vec![(speaking_id, SelectedEndpoint(endpoint(2).0))]
        );
    }
}
