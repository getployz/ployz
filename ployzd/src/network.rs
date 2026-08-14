use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    process::{Command, Output},
    time::{Duration, SystemTime},
};

use ipnet::{IpNet, Ipv4Net};
use ployz_core::{
    AdvertisedEndpoint, Machine, MachineGateway, MachineId, MachineSubnet, ManagementAddress,
    SelectedEndpoint, WireGuardPublicKey,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

mod endpoints;
mod firewall;
mod linux;

pub use endpoints::discover_endpoints;
pub(crate) use endpoints::interface_addresses;
pub use firewall::apply_firewall_rules;
pub use linux::NetworkPlane;

pub const DOCKER_NETWORK_NAME: &str = "ployz";
pub const WIREGUARD_INTERFACE_NAME: &str = "ployz-wg";
pub const WIREGUARD_PORT: u16 = 51820;
pub const WIREGUARD_KEEPALIVE_SECONDS: u16 = 25;
pub const MACHINE_API_PORT: u16 = 51000;
pub const CORROSION_GOSSIP_PORT: u16 = 51001;
pub const UNREGISTRY_PORT: u16 = 51500;
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
    #[error("Machine Subnet must be an IPv4 /24")]
    InvalidMachineSubnet,
    #[error("cluster IPv4 pool must contain /24 subnets")]
    InvalidClusterNetwork,
    #[error("cluster IPv4 pool has no free /24 in this observation")]
    NoFreeSubnet,
    #[error("configured Machine has no WireGuard private key")]
    MissingPrivateKey,
    #[error("WireGuard private key does not match the Machine public key")]
    KeyMismatch,
    #[error("Management Address is not derived from the Machine public key")]
    ManagementAddressMismatch,
    #[error("existing Docker network `ployz` does not match the Machine network")]
    DockerNetworkConflict,
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

pub fn machine_gateway(subnet: MachineSubnet) -> Result<MachineGateway, NetworkError> {
    if subnet.0.prefix_len() != 24 {
        return Err(NetworkError::InvalidMachineSubnet);
    }
    Ok(MachineGateway(Ipv4Addr::from(
        u32::from(subnet.0.network()) + 1,
    )))
}

#[must_use]
pub fn management_address(public_key: WireGuardPublicKey) -> ManagementAddress {
    let mut address = [0_u8; 16];
    address[..2].copy_from_slice(&[0xfd, 0xcc]);
    address[2..].copy_from_slice(&public_key.0[..14]);
    ManagementAddress(Ipv6Addr::from(address))
}

pub fn allocate_machine_subnet(
    cluster_network: Ipv4Net,
    claimed: impl IntoIterator<Item = MachineSubnet>,
) -> Result<MachineSubnet, NetworkError> {
    let claimed = claimed.into_iter().collect::<Vec<_>>();
    let candidates = cluster_network
        .subnets(24)
        .map_err(|_| NetworkError::InvalidClusterNetwork)?;
    candidates
        .map(MachineSubnet)
        .find(|candidate| {
            claimed.iter().all(|claimed| {
                !candidate.0.contains(&claimed.0.network())
                    && !claimed.0.contains(&candidate.0.network())
            })
        })
        .ok_or(NetworkError::NoFreeSubnet)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshPeer {
    pub machine_id: MachineId,
    pub public_key: WireGuardPublicKey,
    pub allowed_ips: [IpNet; 2],
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
}

#[must_use]
pub fn peers_for(observer_id: &MachineId, machines: &[Machine]) -> Vec<MeshPeer> {
    machines
        .iter()
        .filter(|machine| machine.id != *observer_id)
        .map(|machine| MeshPeer {
            machine_id: machine.id.clone(),
            public_key: machine.public_key,
            allowed_ips: [
                IpNet::new(IpAddr::V6(machine.management_address.0), 128)
                    .expect("IPv6 /128 is valid"),
                IpNet::V4(machine.subnet.0),
            ],
            advertised_endpoints: machine.advertised_endpoints.clone(),
        })
        .collect()
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
