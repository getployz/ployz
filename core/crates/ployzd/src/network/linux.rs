use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    net::{IpAddr, SocketAddr},
    path::Path,
    time::{Duration, SystemTime},
};

use bollard::{Docker, models::NetworkInspect};
use defguard_wireguard_rs::{Kernel, WGApi, WireguardInterfaceApi, host::Host, net::IpAddrMask};
use ipnet::IpNet;
use ployz_core::{LocalMachinePhase, Machine, MachineId};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use super::{
    DOCKER_NETWORK_NAME, EndpointChoice, MACHINE_API_PORT, MeshPeer, NetworkError,
    WIREGUARD_INTERFACE_NAME, WireGuardPrivateKey, attach_peer_selections,
    firewall::Installed,
    path::{self, PathBook, ProbedPeer},
    peers_for,
};
use crate::{
    corrosion::{MachineView, ReplicatedStore},
    machine::{LocalMachineBody, LocalMachineRecord, RecordOwner},
};

const NETWORK_MTU: u32 = 1420;

pub struct NetworkPlane {
    machine: Machine,
    docker: Docker,
    wireguard: WGApi<Kernel>,
    mtu: u32,
    routes: BTreeSet<IpNet>,
    bootstrap_peers: Vec<MeshPeer>,
    endpoint_peers: Vec<MeshPeer>,
    /// Peers whose path loses packets of the full MTU.
    lowered: BTreeMap<MachineId, Lowered>,
    paths: PathBook,
    probed: watch::Sender<Vec<ProbedPeer>>,
    /// `None` until start installs it, and on a plane built only to clean up.
    firewall: Option<Installed>,
}

impl NetworkPlane {
    pub async fn start(
        record: &LocalMachineRecord,
        data_dir: &Path,
    ) -> Result<Option<Self>, NetworkError> {
        let bootstrap = record.bootstrap();
        let machine = match record.body() {
            LocalMachineBody::Joining { machine, .. }
            | LocalMachineBody::Participating { machine, .. } => machine.clone(),
            LocalMachineBody::Uninitialized { .. } | LocalMachineBody::Resetting { .. } => {
                return Ok(None);
            }
        };
        let private_key = record.private_key().clone();

        let docker = Docker::connect_with_socket_defaults()?;
        let mut wireguard = WGApi::<Kernel>::new(WIREGUARD_INTERFACE_NAME)?;
        let retained = read_retained_device(&wireguard, &private_key, &machine)?;
        let routes = match &retained {
            Some(host) => peer_routes(host.peers.values())?,
            None => BTreeSet::new(),
        };
        let wireguard_created = retained.is_none();
        let initialize = retained
            .as_ref()
            .is_none_or(|host| host.private_key.is_none());
        if wireguard_created {
            wireguard.create_interface()?;
        }
        let now = SystemTime::now();
        let (peers, _) = attach_peer_selections(
            peers_for(&machine.id, &bootstrap),
            Vec::new(),
            &record.selected_endpoints,
            now,
        );
        let mut plane = Self {
            machine,
            docker,
            wireguard,
            mtu: record.wireguard_mtu.unwrap_or(NETWORK_MTU),
            routes,
            bootstrap_peers: peers.clone(),
            endpoint_peers: if wireguard_created {
                peers.clone()
            } else {
                Vec::new()
            },
            lowered: BTreeMap::new(),
            paths: PathBook::default(),
            probed: watch::Sender::new(Vec::new()),
            firewall: None,
        };
        // Boxed: settling the bridge is a large future, and inline it overflows the
        // stack of a daemon start.
        let docker_created = match Box::pin(plane.ensure_docker_network(data_dir)).await {
            Ok(created) => created,
            Err(error) => {
                if wireguard_created {
                    let _ = plane.wireguard.remove_interface();
                }
                return Err(error);
            }
        };
        if let Err(error) = plane
            .configure_device(&private_key, initialize)
            .and_then(|()| plane.apply_peers(&peers, PeerSet::Bootstrap, record, None))
        {
            plane
                .rollback_start(docker_created, wireguard_created)
                .await;
            return Err(error);
        }
        match super::apply_firewall_rules(plane.machine.subnet, plane.machine.management_address())
        {
            Ok(installed) => plane.firewall = Some(installed),
            Err(error) => {
                plane
                    .rollback_start(docker_created, wireguard_created)
                    .await;
                return Err(error);
            }
        }
        Ok(Some(plane))
    }

    /// Keep the mesh matching the Machine view and the WireGuard device.
    ///
    /// Peers are rebuilt when the view publishes a different snapshot. The device
    /// is polled every second: handshakes and endpoint roaming have no event. An
    /// operator's endpoint from `choices` goes to WireGuard at once. The firewall
    /// is applied again every [`FIREWALL_INTERVAL`], since ufw-docker, a
    /// firewalld reload or a security agent can rewrite iptables at any time.
    pub async fn run(
        &mut self,
        machines: Option<(ReplicatedStore, MachineView)>,
        local: RecordOwner,
        mut choices: mpsc::Receiver<EndpointChoice>,
        shutdown: CancellationToken,
    ) -> io::Result<()> {
        let reapplying = shutdown.child_token();
        let _stop_reapplying = reapplying.clone().drop_guard();
        if let Some(firewall) = self.firewall {
            tokio::spawn(keep_firewall(firewall, reapplying));
        }
        let Some((replicated, machines)) = machines else {
            drop(choices);
            shutdown.cancelled().await;
            return Ok(());
        };
        let mut machines = machines.watch();
        machines.mark_changed();
        let probing = shutdown.child_token();
        let _stop_probing = probing.clone().drop_guard();
        let (reports, mut path_reports) = mpsc::channel(16);
        tokio::spawn(path::probe_paths(
            self.probed.subscribe(),
            self.mtu,
            reports,
            probing,
        ));
        let mut previous = None;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        // A slow store read must not leave ticks queued ahead of operator choices.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => break,
                changed = machines.changed() => {
                    if changed.is_err() {
                        // The view stops only with the daemon; wait for shutdown.
                        shutdown.cancelled().await;
                        break;
                    }
                    machines.borrow_and_update();
                    self.rebuild_when_changed(&replicated, &machines, &mut previous, &local)
                        .await?;
                }
                Some(choice) = choices.recv() => self.apply_choice(choice, &local).await,
                Some(report) = path_reports.recv() => self.apply_path(report),
                _ = ticker.tick() => {
                    // A rebuild that couldn't read the store, or only added, tries again here.
                    self.rebuild_when_changed(&replicated, &machines, &mut previous, &local)
                        .await?;
                    self.poll_endpoints(&local).await;
                }
            }
        }
        if local.record().phase() == LocalMachinePhase::Resetting {
            self.cleanup().await.map_err(io::Error::other)?;
        }
        Ok(())
    }

    /// This Machine's path observation of each peer, kept current while [`Self::run`] runs.
    #[must_use]
    pub(crate) fn paths(&self) -> PathBook {
        self.paths.clone()
    }

    pub(crate) async fn cleanup_retained(record: &LocalMachineRecord) -> Result<(), NetworkError> {
        let Some(machine) = record.machine() else {
            return Ok(());
        };
        let wireguard = WGApi::<Kernel>::new(WIREGUARD_INTERFACE_NAME)?;
        Self {
            machine: machine.clone(),
            docker: Docker::connect_with_socket_defaults()?,
            wireguard,
            mtu: record.wireguard_mtu.unwrap_or(NETWORK_MTU),
            routes: BTreeSet::new(),
            bootstrap_peers: Vec::new(),
            endpoint_peers: Vec::new(),
            lowered: BTreeMap::new(),
            paths: PathBook::default(),
            probed: watch::Sender::new(Vec::new()),
            firewall: None,
        }
        .cleanup()
        .await
    }

    pub async fn cleanup(&mut self) -> Result<(), NetworkError> {
        let mut failures = Vec::new();
        if let Err(error) = remove_firewall(self.machine.subnet).await {
            failures.push(ployz_core::error_chain::inline(&error));
        }
        if let Err(error) = self.remove_docker_network().await {
            failures.push(ployz_core::error_chain::inline(&error));
        }
        if wireguard_device_exists()?
            && let Err(error) = self.wireguard.remove_interface()
        {
            failures.push(ployz_core::error_chain::inline(&error));
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(NetworkError::Io(io::Error::other(failures.join("; "))))
        }
    }

    /// Socket address the Machine API listens on: the Machine's management
    /// address, reachable only by other Machines over WireGuard.
    #[must_use]
    pub fn machine_api_address(&self) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V6(self.machine.management_address().0),
            MACHINE_API_PORT,
        )
    }

    async fn rollback_start(&mut self, docker_created: bool, wireguard_created: bool) {
        if wireguard_created {
            let _ = remove_firewall(self.machine.subnet).await;
            let _ = self.wireguard.remove_interface();
        }
        if docker_created {
            let _ = self.docker.remove_network(DOCKER_NETWORK_NAME).await;
        }
    }
}

/// What an earlier enrollment left on this Server that a network plane for a new
/// identity refuses to change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Leftover {
    /// `ployz-wg` exists and does not fit the new identity, for this reason.
    WireGuardDevice(&'static str),
    /// The `ployz` network is Ployz's, shaped for another Machine, and holds these
    /// containers, by name.
    AttachedNetwork(Vec<String>),
    /// The `ployz` network is shaped for another Machine and lacks Ployz's label.
    UnownedNetwork,
}

impl NetworkPlane {
    /// What would stop a network plane for `machine` from starting on this Server,
    /// read without changing anything. A part this cannot read, such as Docker when
    /// `docker` is `None` or unreachable, reports nothing.
    pub(crate) async fn leftovers(
        docker: Option<&Docker>,
        private_key: &WireGuardPrivateKey,
        machine: &Machine,
        wireguard_mtu: Option<u32>,
    ) -> Vec<Leftover> {
        let device = WGApi::<Kernel>::new(WIREGUARD_INTERFACE_NAME)
            .ok()
            .and_then(|wireguard| read_device(&wireguard).ok().flatten());
        let network = match docker {
            Some(docker) => docker.inspect_network(DOCKER_NETWORK_NAME, None).await.ok(),
            None => None,
        };
        leftovers(
            device
                .as_ref()
                .map(|(host, addresses)| (host, &addresses[..])),
            network.as_ref(),
            private_key,
            machine,
            wireguard_mtu.unwrap_or(NETWORK_MTU),
        )
    }
}

/// The leftovers among a retained `device` and its addresses, and the `ployz`
/// `network`, judged as a start of `machine` would judge them.
fn leftovers(
    device: Option<(&Host, &[IpAddrMask])>,
    network: Option<&NetworkInspect>,
    private_key: &WireGuardPrivateKey,
    machine: &Machine,
    mtu: u32,
) -> Vec<Leftover> {
    let mut leftovers = Vec::new();
    if let Some((host, addresses)) = device
        && let Err(NetworkError::WireGuardConflict { reason, .. }) =
            validate_retained_device(host, private_key, machine, addresses)
    {
        leftovers.push(Leftover::WireGuardDevice(reason));
    }
    if let Some(network) = network {
        let subnet = machine.subnet.to_string();
        let gateway = machine.subnet.gateway().0.to_string();
        let options = required_docker_network_options(mtu);
        match found_network(network, &subnet, &gateway, &options) {
            FoundNetwork::Fits | FoundNetwork::Stale => {}
            FoundNetwork::Attached => {
                let mut names = network
                    .containers
                    .iter()
                    .flatten()
                    .map(|(id, endpoint)| endpoint.name.clone().unwrap_or_else(|| id.clone()))
                    .collect::<Vec<_>>();
                names.sort_unstable();
                leftovers.push(Leftover::AttachedNetwork(names));
            }
            FoundNetwork::Unowned => leftovers.push(Leftover::UnownedNetwork),
        }
    }
    leftovers
}

mod docker_network;
mod firewall_keeper;
mod pins;
mod stopped;
#[cfg(test)]
mod tests;
mod wireguard;

use docker_network::{FoundNetwork, found_network, required_docker_network_options};
use firewall_keeper::{keep_firewall, remove_firewall};
use wireguard::{
    Lowered, PeerSet, peer_routes, read_device, read_retained_device, validate_retained_device,
    wireguard_device_exists,
};
