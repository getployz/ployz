use std::{
    collections::{BTreeSet, HashMap},
    io,
    net::{IpAddr, SocketAddr},
    path::Path,
    process::Command,
    time::{Duration, SystemTime},
};

use bollard::{
    Docker,
    models::{Ipam, IpamConfig, NetworkCreateRequest, NetworkInspect},
};
use defguard_wireguard_rs::{
    InterfaceConfiguration, Kernel, WGApi, WireguardInterfaceApi,
    host::{Host, Peer},
    key::Key,
    net::IpAddrMask,
};
use ipnet::IpNet;
use ployz_core::{
    DOCKER_NETWORK_CONFLICT_RECOVERY, LocalMachinePhase, Machine, MachineId, SelectedEndpoint,
    WireGuardDevice, WireGuardPeer, WireGuardPublicKey,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::docker_image::is_not_found;

use super::{
    DOCKER_NETWORK_NAME, MACHINE_API_PORT, MeshPeer, NetworkError, WIREGUARD_INTERFACE_NAME,
    WIREGUARD_KEEPALIVE_SECONDS, WIREGUARD_PORT, WireGuardPrivateKey, attach_peer_selections,
    checked_command, firewall::remove_firewall_rules, peers_for,
};
use crate::{
    corrosion::{MachineView, ReplicatedObservations},
    machine::{LocalMachineBody, LocalMachineRecord, RecordOwner},
};

const NETWORK_MTU: u32 = 1420;
const DOCKER_NETWORK_MANAGED_LABEL: &str = "ployzd.managed";

#[derive(Clone, Copy, Eq, PartialEq)]
enum PeerSet {
    Bootstrap,
    Snapshot,
}

pub fn inspect_wireguard_device() -> Result<WireGuardDevice, NetworkError> {
    let wireguard = WGApi::<Kernel>::new(WIREGUARD_INTERFACE_NAME)?;
    let host = wireguard.read_interface_data()?;
    let public_key = host
        .private_key
        .as_ref()
        .map(|key| WireGuardPublicKey(key.public_key().as_array()))
        .ok_or(NetworkError::MissingPrivateKey)?;
    let peers = host
        .peers
        .into_values()
        .map(|peer| {
            let allowed_ips = peer
                .allowed_ips
                .into_iter()
                .map(|address| IpNet::new(address.address, address.cidr))
                .collect::<Result<_, _>>()
                .map_err(|error| NetworkError::Io(io::Error::other(error)))?;
            Ok(WireGuardPeer {
                public_key: WireGuardPublicKey(peer.public_key.as_array()),
                endpoint: peer.endpoint,
                last_handshake_unix_seconds: peer.last_handshake.and_then(|time| {
                    time.duration_since(SystemTime::UNIX_EPOCH)
                        .ok()
                        .map(|duration| duration.as_secs())
                }),
                received_bytes: peer.rx_bytes,
                sent_bytes: peer.tx_bytes,
                allowed_ips,
                machine: None,
                rtt: None,
            })
        })
        .collect::<Result<Vec<_>, NetworkError>>()?;
    Ok(WireGuardDevice {
        interface_name: WIREGUARD_INTERFACE_NAME.into(),
        public_key,
        listen_port: host.listen_port,
        peers,
    })
}

pub struct NetworkPlane {
    machine: Machine,
    docker: Docker,
    wireguard: WGApi<Kernel>,
    mtu: u32,
    routes: BTreeSet<IpNet>,
    bootstrap_peers: Vec<MeshPeer>,
    endpoint_peers: Vec<MeshPeer>,
}

impl NetworkPlane {
    pub async fn start(record: &LocalMachineRecord) -> Result<Option<Self>, NetworkError> {
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
            peers_for(&machine.id, bootstrap),
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
        };
        let docker_created = match plane.ensure_docker_network().await {
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
            .and_then(|()| plane.apply_peers(&peers, PeerSet::Bootstrap))
        {
            plane
                .rollback_start(docker_created, wireguard_created)
                .await;
            return Err(error);
        }
        if let Err(error) =
            super::apply_firewall_rules(plane.machine.subnet, plane.machine.management_address())
        {
            plane
                .rollback_start(docker_created, wireguard_created)
                .await;
            return Err(error);
        }
        Ok(Some(plane))
    }

    /// Keep the mesh matching the Machine view and the WireGuard device.
    ///
    /// Peers are rebuilt when the view publishes a different snapshot. The device
    /// is polled every second: handshakes and endpoint roaming have no event.
    pub async fn run(
        &mut self,
        machines: Option<MachineView>,
        local: RecordOwner,
        shutdown: CancellationToken,
    ) -> io::Result<()> {
        let Some(machines) = machines else {
            shutdown.cancelled().await;
            return Ok(());
        };
        let mut machines = machines.watch();
        machines.mark_changed();
        let mut previous = None;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
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
                    let snapshot = machines.borrow_and_update().clone();
                    if let Some(snapshot) = snapshot
                        && previous.as_ref() != Some(&snapshot)
                    {
                        if let Err(error) = self.rebuild(&snapshot, &local).await {
                            return Err(io::Error::other(error));
                        }
                        previous = Some(snapshot);
                    }
                }
                _ = ticker.tick() => self.poll_endpoints(&local).await,
            }
        }
        if local.record().phase() == LocalMachinePhase::Resetting {
            self.cleanup().await.map_err(io::Error::other)?;
        }
        Ok(())
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
        }
        .cleanup()
        .await
    }

    pub async fn cleanup(&mut self) -> Result<(), NetworkError> {
        let mut failures = Vec::new();
        if let Err(error) = remove_firewall_rules(self.machine.subnet) {
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

    async fn remove_docker_network(&self) -> Result<(), NetworkError> {
        let network = match self.docker.inspect_network(DOCKER_NETWORK_NAME, None).await {
            Ok(network) => network,
            Err(error) if is_not_found(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        let subnet = self.machine.subnet.to_string();
        let gateway = self.machine.subnet.gateway().0.to_string();
        let options = required_docker_network_options(self.mtu);
        let id = reset_network_id(&network, &subnet, &gateway, &options)?;
        match self.docker.remove_network(id).await {
            Ok(()) => Ok(()),
            Err(error) if is_not_found(&error) => Ok(()),
            Err(error) => Err(error.into()),
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

    async fn rebuild(
        &mut self,
        snapshot: &ReplicatedObservations<Machine, MachineId>,
        local: &RecordOwner,
    ) -> Result<(), NetworkError> {
        let record = local.record();
        let Some(planned) = snapshot_peers(
            &self.machine.id,
            snapshot,
            &self.bootstrap_peers,
            record.phase() == LocalMachinePhase::Joining,
        ) else {
            return Ok(());
        };
        let selected = &record.selected_endpoints;
        let now = SystemTime::now();
        let mut previous = std::mem::take(&mut self.endpoint_peers);
        let host = self.wireguard.read_interface_data()?;
        seed_retained_selections(&planned, &mut previous, &host.peers, now);
        let (planned, _) = attach_peer_selections(planned, previous, selected, now);
        self.apply_peers(&planned, PeerSet::Snapshot)?;
        self.endpoint_peers = planned;
        // Only an installed selection is worth remembering across a restart.
        for peer in &self.endpoint_peers {
            if let Some(endpoint) = peer.selected()
                && selected.get(&peer.machine_id).copied() != Some(endpoint)
            {
                persist_selection(local, peer.machine_id, endpoint).await;
            }
        }
        Ok(())
    }

    fn configure_device(
        &self,
        private_key: &WireGuardPrivateKey,
        initialize: bool,
    ) -> Result<(), NetworkError> {
        let address = IpAddrMask::host(IpAddr::V6(self.machine.management_address().0));
        if initialize {
            self.wireguard
                .configure_interface(&InterfaceConfiguration {
                    name: WIREGUARD_INTERFACE_NAME.into(),
                    prvkey: private_key.encoded(),
                    addresses: vec![address],
                    port: WIREGUARD_PORT,
                    peers: Vec::new(),
                    mtu: Some(self.mtu),
                })?;
        } else {
            self.wireguard.assign_address(&address)?;
            checked_command(
                "ip",
                &[
                    "link",
                    "set",
                    "dev",
                    WIREGUARD_INTERFACE_NAME,
                    "mtu",
                    &self.mtu.to_string(),
                ],
            )?;
        }
        Ok(())
    }

    fn apply_peers(&mut self, peers: &[MeshPeer], set: PeerSet) -> Result<(), NetworkError> {
        let wg_peers = peers
            .iter()
            .map(|peer| wireguard_peer(peer, peer.selected()))
            .collect::<Vec<_>>();
        let host = self.wireguard.read_interface_data()?;
        let changes = peer_changes(&host.peers, &wg_peers, set)?;
        for peer in changes.updates {
            self.wireguard.configure_peer(peer)?;
        }
        let gateway = self.machine.subnet.gateway().0.to_string();
        for route in &changes.routes {
            replace_route(*route, route.addr().is_ipv4().then_some(&gateway))?;
        }
        for key in changes.removals {
            self.wireguard.remove_peer(key)?;
        }
        for route in self.routes.difference(&changes.routes) {
            delete_route(route);
        }
        self.routes = changes.routes;
        Ok(())
    }

    async fn poll_endpoints(&mut self, local: &RecordOwner) {
        let host = match self.wireguard.read_interface_data() {
            Ok(host) => host,
            Err(error) => {
                eprintln!(
                    "failed to poll WireGuard device: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                return;
            }
        };
        let now = SystemTime::now();
        for peer in &mut self.endpoint_peers {
            let key = Key::new(peer.public_key.0);
            let device = host.peers.get(&key);
            let Some(endpoint) = peer.poll(
                now,
                device.and_then(|peer| peer.last_handshake),
                device.and_then(|peer| peer.endpoint),
            ) else {
                continue;
            };
            if let Err(error) = self
                .wireguard
                .configure_peer(&wireguard_peer(peer, Some(endpoint)))
            {
                eprintln!(
                    "failed to update WireGuard peer endpoint: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                continue;
            }
            persist_selection(local, peer.machine_id, endpoint).await;
        }
    }

    async fn rollback_start(&mut self, docker_created: bool, wireguard_created: bool) {
        if wireguard_created {
            let _ = remove_firewall_rules(self.machine.subnet);
            let _ = self.wireguard.remove_interface();
        }
        if docker_created {
            let _ = self.docker.remove_network(DOCKER_NETWORK_NAME).await;
        }
    }

    async fn ensure_docker_network(&self) -> Result<bool, NetworkError> {
        let subnet = self.machine.subnet.to_string();
        let gateway = self.machine.subnet.gateway().0.to_string();
        let required_options = required_docker_network_options(self.mtu);
        match self.docker.inspect_network(DOCKER_NETWORK_NAME, None).await {
            Ok(network) => {
                if docker_network_matches(&network, &subnet, &gateway, &required_options) {
                    return Ok(false);
                }
                match stale_network_replacement_allowed(&network) {
                    Ok(()) => {
                        let Some(network_id) = network.id.as_deref() else {
                            return Err(docker_network_conflict(
                                &network,
                                &subnet,
                                &gateway,
                                &required_options,
                                "the inspected network has no stable Docker ID",
                            ));
                        };
                        match self.docker.remove_network(network_id).await {
                            Ok(()) => {}
                            Err(error) if is_not_found(&error) => {}
                            Err(error) => {
                                return Err(docker_network_conflict(
                                    &network,
                                    &subnet,
                                    &gateway,
                                    &required_options,
                                    format!(
                                        "Docker refused to remove the network after inspection: {error}",
                                        error = ployz_core::error_chain::inline(&error),
                                    ),
                                ));
                            }
                        }
                    }
                    Err(reason) => {
                        return Err(docker_network_conflict(
                            &network,
                            &subnet,
                            &gateway,
                            &required_options,
                            reason,
                        ));
                    }
                }
            }
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Err(error.into()),
        }
        self.docker
            .create_network(NetworkCreateRequest {
                name: DOCKER_NETWORK_NAME.into(),
                driver: Some("bridge".into()),
                scope: Some("local".into()),
                ipam: Some(Ipam {
                    config: Some(vec![IpamConfig {
                        subnet: Some(subnet),
                        gateway: Some(gateway),
                        ..Default::default()
                    }]),
                    ..Default::default()
                }),
                options: Some(required_options),
                labels: Some(HashMap::from([(
                    DOCKER_NETWORK_MANAGED_LABEL.into(),
                    String::new(),
                )])),
                ..Default::default()
            })
            .await?;
        // TODO: check if this works when firewalld used instead of raw iptables. The Docker daemon has a different
        // code path for firewalld.
        Ok(true)
    }
}

fn read_retained_device(
    wireguard: &WGApi<Kernel>,
    private_key: &WireGuardPrivateKey,
    machine: &Machine,
) -> Result<Option<Host>, NetworkError> {
    validate_machine_identity(private_key, machine.public_key)?;
    if !wireguard_device_exists()? {
        return Ok(None);
    }
    let host = wireguard.read_interface_data()?;
    let addresses = if host.private_key.is_none() {
        wireguard_addresses()?
    } else {
        Vec::new()
    };
    validate_retained_device(&host, private_key, machine, &addresses)?;
    Ok(Some(host))
}

fn wireguard_device_exists() -> io::Result<bool> {
    Path::new("/sys/class/net")
        .join(WIREGUARD_INTERFACE_NAME)
        .try_exists()
}

const RETAINED_DEVICE_RECOVERY: &str = "run `systemctl stop ployz.socket ployz`; run `ip link delete ployz-wg`; run `systemctl reset-failed ployz.socket ployz`; run `systemctl start ployz`";

fn validate_machine_identity(
    private_key: &WireGuardPrivateKey,
    machine_key: WireGuardPublicKey,
) -> Result<(), NetworkError> {
    if private_key.public_key() != machine_key {
        return Err(NetworkError::WireGuardConflict {
            reason: "the private key identity differs from the Machine record",
            recovery: "rerun the Ployz install command with `--reset` at the end; this removes every container Ployz runs on the server",
        });
    }
    Ok(())
}

fn validate_retained_device(
    host: &Host,
    private_key: &WireGuardPrivateKey,
    machine: &Machine,
    addresses: &[IpAddrMask],
) -> Result<(), NetworkError> {
    validate_machine_identity(private_key, machine.public_key)?;
    let Some(key) = &host.private_key else {
        let management = IpAddrMask::host(IpAddr::V6(machine.management_address().0));
        return if host.peers.is_empty() && addresses.iter().all(|address| *address == management) {
            Ok(())
        } else {
            Err(NetworkError::WireGuardConflict {
                reason: "the keyless device has peers or unrelated addresses",
                recovery: RETAINED_DEVICE_RECOVERY,
            })
        };
    };
    if WireGuardPublicKey(key.public_key().as_array()) != machine.public_key {
        return Err(NetworkError::WireGuardConflict {
            reason: "the existing device identity differs from the Machine record",
            recovery: RETAINED_DEVICE_RECOVERY,
        });
    }
    if host.listen_port != WIREGUARD_PORT {
        return Err(NetworkError::WireGuardConflict {
            reason: "the existing device listen port differs from the mesh port",
            recovery: RETAINED_DEVICE_RECOVERY,
        });
    }
    Ok(())
}

fn wireguard_addresses() -> Result<Vec<IpAddrMask>, NetworkError> {
    #[derive(Deserialize)]
    struct Interface {
        addr_info: Vec<Address>,
    }
    #[derive(Deserialize)]
    struct Address {
        local: IpAddr,
        prefixlen: u8,
    }
    let output = checked_command(
        "ip",
        &["-json", "address", "show", "dev", WIREGUARD_INTERFACE_NAME],
    )?;
    let interfaces: Vec<Interface> = serde_json::from_slice(&output.stdout)?;
    Ok(interfaces
        .into_iter()
        .flat_map(|interface| interface.addr_info)
        .map(|address| IpAddrMask::new(address.local, address.prefixlen))
        .collect())
}

fn seed_retained_selections(
    planned: &[MeshPeer],
    previous: &mut Vec<MeshPeer>,
    installed: &HashMap<Key, Peer>,
    now: SystemTime,
) {
    for peer in planned {
        if previous
            .iter()
            .any(|previous| previous.machine_id == peer.machine_id)
        {
            continue;
        }
        if let Some(endpoint) = installed
            .get(&Key::new(peer.public_key.0))
            .and_then(|peer| peer.endpoint)
        {
            let mut peer = peer.clone();
            peer.selection.bind(Some(SelectedEndpoint(endpoint)), now);
            previous.push(peer);
        }
    }
}

fn snapshot_peers(
    machine_id: &MachineId,
    snapshot: &ReplicatedObservations<Machine, MachineId>,
    bootstrap: &[MeshPeer],
    joining: bool,
) -> Option<Vec<MeshPeer>> {
    if !snapshot.incomplete_ids.is_empty() {
        return None;
    }
    let mut peers = peers_for(machine_id, &snapshot.observations);
    if joining {
        let known = peers
            .iter()
            .map(|peer| peer.machine_id)
            .collect::<BTreeSet<_>>();
        peers.extend(
            bootstrap
                .iter()
                .filter(|peer| !known.contains(&peer.machine_id))
                .cloned(),
        );
    }
    Some(peers)
}

struct PeerChanges<'peers> {
    updates: Vec<&'peers Peer>,
    removals: Vec<&'peers Key>,
    routes: BTreeSet<IpNet>,
}

fn peer_changes<'peers>(
    installed: &'peers HashMap<Key, Peer>,
    planned: &'peers [Peer],
    set: PeerSet,
) -> Result<PeerChanges<'peers>, NetworkError> {
    let updates = planned
        .iter()
        .filter(|peer| {
            let Some(current) = installed.get(&peer.public_key) else {
                return true;
            };
            set == PeerSet::Snapshot
                && (current.allowed_ips.len() != peer.allowed_ips.len()
                    || !peer
                        .allowed_ips
                        .iter()
                        .all(|ip| current.allowed_ips.contains(ip))
                    || peer
                        .endpoint
                        .is_some_and(|endpoint| current.endpoint != Some(endpoint))
                    || current.persistent_keepalive_interval != peer.persistent_keepalive_interval)
        })
        .collect::<Vec<_>>();
    let removals = if set == PeerSet::Snapshot {
        installed
            .keys()
            .filter(|key| !planned.iter().any(|peer| &peer.public_key == *key))
            .collect()
    } else {
        Vec::new()
    };
    let routes = match set {
        PeerSet::Bootstrap => peer_routes(installed.values().chain(updates.iter().copied()))?,
        PeerSet::Snapshot => peer_routes(planned.iter())?,
    };
    Ok(PeerChanges {
        updates,
        removals,
        routes,
    })
}

fn peer_routes<'peers>(
    peers: impl Iterator<Item = &'peers Peer>,
) -> Result<BTreeSet<IpNet>, NetworkError> {
    peers
        .flat_map(|peer| &peer.allowed_ips)
        .map(|address| {
            IpNet::new(address.address, address.cidr)
                .map_err(|error| NetworkError::Io(io::Error::other(error)))
        })
        .collect()
}

fn required_docker_network_options(mtu: u32) -> HashMap<String, String> {
    HashMap::from([
        (
            "com.docker.network.bridge.name".into(),
            DOCKER_NETWORK_NAME.into(),
        ),
        (
            "com.docker.network.bridge.trusted_host_interfaces".into(),
            WIREGUARD_INTERFACE_NAME.into(),
        ),
        ("com.docker.network.driver.mtu".into(), mtu.to_string()),
    ])
}

fn docker_network_matches(
    network: &NetworkInspect,
    subnet: &str,
    gateway: &str,
    required_options: &HashMap<String, String>,
) -> bool {
    let ipam = network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .and_then(|configs| configs.first());
    docker_network_owned(network)
        && network.driver.as_deref() == Some("bridge")
        && network.scope.as_deref() == Some("local")
        && ipam.and_then(|config| config.subnet.as_deref()) == Some(subnet)
        && ipam.and_then(|config| config.gateway.as_deref()) == Some(gateway)
        && required_options.iter().all(|(key, value)| {
            network
                .options
                .as_ref()
                .and_then(|options| options.get(key))
                == Some(value)
        })
}

fn docker_network_owned(network: &NetworkInspect) -> bool {
    network.name.as_deref() == Some(DOCKER_NETWORK_NAME)
        && network
            .labels
            .as_ref()
            .and_then(|labels| labels.get(DOCKER_NETWORK_MANAGED_LABEL))
            .is_some_and(String::is_empty)
}

fn reset_network_id<'network>(
    network: &'network NetworkInspect,
    subnet: &str,
    gateway: &str,
    options: &HashMap<String, String>,
) -> Result<&'network str, NetworkError> {
    if docker_network_matches(network, subnet, gateway, options)
        && let Some(id) = network.id.as_deref()
    {
        return Ok(id);
    }
    Err(docker_network_conflict(
        network,
        subnet,
        gateway,
        options,
        "the network does not match the resetting Machine or has no stable Docker ID",
    ))
}

fn stale_network_replacement_allowed(network: &NetworkInspect) -> Result<(), &'static str> {
    if !docker_network_owned(network) {
        Err(
            "ownership is unproven because the exact name and `ployzd.managed` label do not both match",
        )
    } else if network
        .containers
        .as_ref()
        .is_some_and(|containers| !containers.is_empty())
    {
        Err("containers are attached")
    } else {
        Ok(())
    }
}

fn docker_network_conflict(
    network: &NetworkInspect,
    subnet: &str,
    gateway: &str,
    required_options: &HashMap<String, String>,
    reason: impl Into<String>,
) -> NetworkError {
    let quoted = |value: Option<&str>| {
        value.map_or_else(
            || "unknown".to_owned(),
            |value| format!("\"{}\"", value.escape_debug()),
        )
    };
    let options = |values: &HashMap<String, String>| {
        let mut pairs = values
            .iter()
            .map(|(key, value)| format!("{}={}", quoted(Some(key)), quoted(Some(value))))
            .collect::<Vec<_>>();
        pairs.sort();
        format!("{{{}}}", pairs.join(", "))
    };
    let ipam = network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .map(|configs| {
            let configs = configs
                .iter()
                .map(|config| {
                    format!(
                        "{{subnet={}, gateway={}}}",
                        quoted(config.subnet.as_deref()),
                        quoted(config.gateway.as_deref())
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("({configs})")
        });
    let containers = network.containers.as_ref().map(|containers| {
        let mut ids = containers
            .keys()
            .map(|id| quoted(Some(id)))
            .collect::<Vec<_>>();
        ids.sort_unstable();
        format!("({})", ids.join(", "))
    });
    NetworkError::DockerNetworkConflict {
        reason: reason.into(),
        expected: format!(
            "name={DOCKER_NETWORK_NAME}, driver=bridge, scope=local, label={DOCKER_NETWORK_MANAGED_LABEL}=\"\", subnet={subnet}, gateway={gateway}, options={}",
            options(required_options)
        ),
        observed: format!(
            "id={}, name={}, driver={}, scope={}, labels={}, IPAM={}, options={}, containers={}",
            quoted(network.id.as_deref()),
            quoted(network.name.as_deref()),
            quoted(network.driver.as_deref()),
            quoted(network.scope.as_deref()),
            network
                .labels
                .as_ref()
                .map(&options)
                .as_deref()
                .unwrap_or("unknown"),
            ipam.as_deref().unwrap_or("unknown"),
            network
                .options
                .as_ref()
                .map(options)
                .as_deref()
                .unwrap_or("unknown"),
            containers.as_deref().unwrap_or("unknown"),
        ),
        recovery: DOCKER_NETWORK_CONFLICT_RECOVERY,
    }
}

fn wireguard_peer(config: &MeshPeer, endpoint: Option<SelectedEndpoint>) -> Peer {
    let mut peer = Peer::new(Key::new(config.public_key.0));
    peer.allowed_ips = config
        .allowed_ips
        .iter()
        .map(|network| IpAddrMask::new(network.addr(), network.prefix_len()))
        .collect();
    peer.endpoint = endpoint.map(|endpoint| endpoint.0);
    peer.persistent_keepalive_interval = Some(WIREGUARD_KEEPALIVE_SECONDS);
    peer
}

async fn persist_selection(local: &RecordOwner, machine_id: MachineId, endpoint: SelectedEndpoint) {
    let result = local
        .mutate(move |store| store.persist_selected_endpoint(machine_id, endpoint))
        .await
        .map_err(io::Error::other)
        .and_then(|persisted| persisted.map_err(io::Error::other));
    if let Err(error) = result {
        eprintln!(
            "failed to persist observer-local Selected Endpoint: {error}",
            error = ployz_core::error_chain::inline(&error),
        );
    }
}

fn replace_route(route: IpNet, source: Option<&str>) -> Result<(), NetworkError> {
    let mut args = Vec::new();
    if route.addr().is_ipv6() {
        args.push("-6");
    }
    let route = route.to_string();
    args.extend([
        "route",
        "replace",
        route.as_str(),
        "dev",
        WIREGUARD_INTERFACE_NAME,
    ]);
    if let Some(source) = source {
        args.extend(["src", source]);
    }
    checked_command("ip", &args).map(|_| ())
}

fn delete_route(route: &IpNet) {
    let mut args = Vec::new();
    if route.addr().is_ipv6() {
        args.push("-6");
    }
    let route = route.to_string();
    args.extend([
        "route",
        "del",
        route.as_str(),
        "dev",
        WIREGUARD_INTERFACE_NAME,
    ]);
    let _ = Command::new("ip").args(args).output();
}

#[cfg(test)]
mod tests;
