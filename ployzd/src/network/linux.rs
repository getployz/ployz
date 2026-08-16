use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io,
    net::{IpAddr, SocketAddr},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{Ipam, IpamConfig, NetworkCreateRequest},
};
use defguard_wireguard_rs::{
    InterfaceConfiguration, Kernel, WGApi, WireguardInterfaceApi, host::Peer, key::Key,
    net::IpAddrMask,
};
use ipnet::IpNet;
use ployz_core::{
    LocalMachinePhase, Machine, MachineId, SelectedEndpoint, WireGuardDevice, WireGuardPeer,
    WireGuardPublicKey,
};
use tokio_util::sync::CancellationToken;

use super::{
    DOCKER_NETWORK_NAME, EndpointSelection, MACHINE_API_PORT, MeshPeer, NetworkError,
    WIREGUARD_INTERFACE_NAME, WIREGUARD_KEEPALIVE_SECONDS, WIREGUARD_PORT, WireGuardPrivateKey,
    checked_command, firewall::remove_firewall_rules, management_address, peers_for,
};
use crate::{
    corrosion::{ReplicatedObservations, ReplicatedStore},
    machine::{LocalMachineRecord, LocalMachineStore},
};

const NETWORK_MTU: u32 = 1420;

pub fn inspect_wireguard_device() -> Result<WireGuardDevice, NetworkError> {
    let wireguard = WGApi::<Kernel>::new(WIREGUARD_INTERFACE_NAME.into())?;
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
                .map(|address| IpNet::new(address.ip, address.cidr))
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
    private_key: WireGuardPrivateKey,
    docker: Docker,
    wireguard: WGApi<Kernel>,
    mtu: u32,
    routes: BTreeSet<IpNet>,
    selections: BTreeMap<MachineId, EndpointSelection>,
    bootstrap_peers: Vec<MeshPeer>,
    peers: Vec<MeshPeer>,
}

impl NetworkPlane {
    pub async fn start(record: &LocalMachineRecord) -> Result<Option<Self>, NetworkError> {
        if !matches!(
            record.phase,
            LocalMachinePhase::Joining | LocalMachinePhase::Participating
        ) {
            return Ok(None);
        }
        let Some(machine) = record.machine.clone() else {
            return Ok(None);
        };
        let private_key = record
            .wireguard_private_key
            .clone()
            .ok_or(NetworkError::MissingPrivateKey)?;
        if private_key.public_key() != machine.public_key {
            return Err(NetworkError::KeyMismatch);
        }
        if management_address(machine.public_key) != machine.management_address {
            return Err(NetworkError::ManagementAddressMismatch);
        }

        let docker = Docker::connect_with_socket_defaults()?;
        let wireguard = WGApi::<Kernel>::new(WIREGUARD_INTERFACE_NAME.into())?;
        wireguard.create_interface()?;
        let now = SystemTime::now();
        let peers = peers_for(&machine.id, &record.bootstrap_machines);
        let selections = peers
            .iter()
            .filter_map(|peer| {
                let selection = EndpointSelection::new(
                    &peer.advertised_endpoints,
                    record.selected_endpoints.get(&peer.machine_id).copied(),
                    now,
                );
                selection.selected().map(|_| (peer.machine_id, selection))
            })
            .collect();
        let mut plane = Self {
            machine,
            private_key,
            docker,
            wireguard,
            mtu: record.wireguard_mtu.unwrap_or(NETWORK_MTU),
            routes: BTreeSet::new(),
            selections,
            bootstrap_peers: peers.clone(),
            peers: peers.clone(),
        };
        let docker_created = match plane.ensure_docker_network().await {
            Ok(created) => created,
            Err(error) => {
                let _ = plane.wireguard.remove_interface();
                return Err(error);
            }
        };
        if let Err(error) = plane.apply_peers(&peers) {
            plane.rollback_start(docker_created).await;
            return Err(error);
        }
        if let Err(error) = super::apply_firewall_rules(plane.machine.subnet) {
            plane.rollback_start(docker_created).await;
            return Err(error);
        }
        Ok(Some(plane))
    }

    pub async fn run(
        &mut self,
        replicated: Option<ReplicatedStore>,
        local: Arc<Mutex<LocalMachineStore>>,
        shutdown: CancellationToken,
    ) -> io::Result<()> {
        let Some(replicated) = replicated else {
            shutdown.cancelled().await;
            return Ok(());
        };
        let mut previous = None;
        let mut ticker = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match replicated.machines().await {
                        Ok(snapshot) => {
                            if previous.as_ref() != Some(&snapshot) {
                                if let Err(error) = self.rebuild(&snapshot, &local) {
                                    return Err(io::Error::other(error));
                                }
                                previous = Some(snapshot);
                            }
                            self.poll_endpoints(&local);
                        }
                        Err(error) => eprintln!("failed to read Machine table for network plane: {error}"),
                    }
                }
                () = shutdown.cancelled() => {
                    let resetting = local
                        .lock()
                        .map_err(|_| io::Error::other("local Machine record lock poisoned"))?
                        .record()
                        .phase
                        == LocalMachinePhase::Resetting;
                    if resetting {
                        self.cleanup().await.map_err(io::Error::other)?;
                    }
                    return Ok(());
                }
            }
        }
    }

    pub async fn cleanup(&mut self) -> Result<(), NetworkError> {
        let mut failures = Vec::new();
        if let Err(error) = remove_firewall_rules(self.machine.subnet) {
            failures.push(error.to_string());
        }
        match self.docker.remove_network(DOCKER_NETWORK_NAME).await {
            Ok(()) => {}
            Err(error) if docker_not_found(&error) => {}
            Err(error) => failures.push(error.to_string()),
        }
        if let Err(error) = self.wireguard.remove_interface() {
            failures.push(error.to_string());
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(NetworkError::Io(io::Error::other(failures.join("; "))))
        }
    }

    pub fn machine_api_addresses(&self) -> Result<[SocketAddr; 2], NetworkError> {
        Ok([
            SocketAddr::new(
                IpAddr::V6(self.machine.management_address.0),
                MACHINE_API_PORT,
            ),
            SocketAddr::new(
                IpAddr::V4(self.machine.subnet.gateway().0),
                MACHINE_API_PORT,
            ),
        ])
    }

    fn rebuild(
        &mut self,
        snapshot: &ReplicatedObservations<Machine>,
        local: &Arc<Mutex<LocalMachineStore>>,
    ) -> Result<(), NetworkError> {
        let (selected, joining) = {
            let local = local
                .lock()
                .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
            (
                local.record().selected_endpoints.clone(),
                local.record().phase == LocalMachinePhase::Joining,
            )
        };
        let now = SystemTime::now();
        let mut planned = peers_for(&self.machine.id, &snapshot.observations);
        if joining {
            let known = planned
                .iter()
                .map(|peer| peer.machine_id)
                .collect::<BTreeSet<_>>();
            planned.extend(
                self.bootstrap_peers
                    .iter()
                    .filter(|peer| !known.contains(&peer.machine_id))
                    .cloned(),
            );
        }
        let live_ids = planned
            .iter()
            .map(|peer| peer.machine_id)
            .collect::<BTreeSet<_>>();
        self.selections
            .retain(|machine_id, _| live_ids.contains(machine_id));
        for peer in &planned {
            if let Some(selection) = self.selections.get_mut(&peer.machine_id) {
                if let Some(endpoint) = selection.replace_candidates(
                    &peer.advertised_endpoints,
                    selected.get(&peer.machine_id).copied(),
                    now,
                ) {
                    persist_selection(local, peer.machine_id, endpoint);
                }
            } else {
                let persisted = selected.get(&peer.machine_id).copied();
                let selection = EndpointSelection::new(&peer.advertised_endpoints, persisted, now);
                if persisted.is_none()
                    && let Some(endpoint) = selection.selected()
                {
                    persist_selection(local, peer.machine_id, endpoint);
                }
                self.selections.insert(peer.machine_id, selection);
            }
        }
        self.apply_peers(&planned)?;
        self.peers = planned;
        Ok(())
    }

    fn apply_peers(&mut self, peers: &[MeshPeer]) -> Result<(), NetworkError> {
        let wg_peers = peers
            .iter()
            .map(|peer| {
                wireguard_peer(
                    peer,
                    self.selections
                        .get(&peer.machine_id)
                        .and_then(EndpointSelection::selected),
                )
            })
            .collect::<Vec<_>>();
        let desired_routes = peers
            .iter()
            .flat_map(|peer| peer.allowed_ips)
            .collect::<BTreeSet<_>>();
        self.wireguard
            .configure_interface(&InterfaceConfiguration {
                name: WIREGUARD_INTERFACE_NAME.into(),
                prvkey: self.private_key.encoded(),
                addresses: vec![IpAddrMask::host(IpAddr::V6(
                    self.machine.management_address.0,
                ))],
                port: u32::from(WIREGUARD_PORT),
                peers: wg_peers.clone(),
                mtu: Some(self.mtu),
            })?;
        self.wireguard.configure_peer_routing(&wg_peers)?;
        let gateway = self.machine.subnet.gateway().0.to_string();
        for peer in peers {
            for route in &peer.allowed_ips {
                replace_route(*route, route.addr().is_ipv4().then_some(&gateway))?;
            }
        }
        for route in self.routes.difference(&desired_routes) {
            delete_route(route);
        }
        self.routes = desired_routes;
        Ok(())
    }

    fn poll_endpoints(&mut self, local: &Arc<Mutex<LocalMachineStore>>) {
        let host = match self.wireguard.read_interface_data() {
            Ok(host) => host,
            Err(error) => {
                eprintln!("failed to poll WireGuard device: {error}");
                return;
            }
        };
        let now = SystemTime::now();
        for peer in &mut self.peers {
            let key = Key::new(peer.public_key.0);
            let device = host.peers.get(&key);
            let Some(selection) = self.selections.get_mut(&peer.machine_id) else {
                continue;
            };
            let Some(endpoint) = selection.poll(
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
                eprintln!("failed to update WireGuard peer endpoint: {error}");
                continue;
            }
            persist_selection(local, peer.machine_id, endpoint);
        }
    }

    async fn rollback_start(&mut self, docker_created: bool) {
        let _ = remove_firewall_rules(self.machine.subnet);
        if docker_created {
            let _ = self.docker.remove_network(DOCKER_NETWORK_NAME).await;
        }
        let _ = self.wireguard.remove_interface();
    }

    async fn ensure_docker_network(&self) -> Result<bool, NetworkError> {
        let subnet = self.machine.subnet.to_string();
        let gateway = self.machine.subnet.gateway().0.to_string();
        let required_options = HashMap::from([
            (
                "com.docker.network.bridge.name".into(),
                DOCKER_NETWORK_NAME.into(),
            ),
            (
                "com.docker.network.bridge.trusted_host_interfaces".into(),
                WIREGUARD_INTERFACE_NAME.into(),
            ),
            ("com.docker.network.driver.mtu".into(), self.mtu.to_string()),
        ]);
        let exists = match self.docker.inspect_network(DOCKER_NETWORK_NAME, None).await {
            Ok(network) => {
                let ipam = network
                    .ipam
                    .and_then(|ipam| ipam.config)
                    .and_then(|configs| configs.into_iter().next());
                let options = network.options.unwrap_or_default();
                let matches = ipam.as_ref().and_then(|config| config.subnet.as_deref())
                    == Some(&subnet)
                    && ipam.as_ref().and_then(|config| config.gateway.as_deref()) == Some(&gateway)
                    && required_options
                        .iter()
                        .all(|(key, value)| options.get(key) == Some(value));
                if !matches {
                    return Err(NetworkError::DockerNetworkConflict);
                }
                true
            }
            Err(error) if docker_not_found(&error) => false,
            Err(error) => return Err(error.into()),
        };
        if !exists {
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
                    labels: Some(HashMap::from([("ployzd.managed".into(), String::new())])),
                    ..Default::default()
                })
                .await?;
        }
        // TODO(UT-113): check if this works when firewalld used instead of raw iptables. The Docker daemon has a different
        // code path for firewalld.
        Ok(!exists)
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

fn persist_selection(
    local: &Arc<Mutex<LocalMachineStore>>,
    machine_id: MachineId,
    endpoint: SelectedEndpoint,
) {
    let result = local
        .lock()
        .map_err(|_| io::Error::other("local Machine record lock poisoned"))
        .and_then(|mut store| {
            store
                .persist_selected_endpoint(machine_id, endpoint)
                .map_err(io::Error::other)
        });
    if let Err(error) = result {
        eprintln!("failed to persist observer-local Selected Endpoint: {error}");
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

fn docker_not_found(error: &DockerError) -> bool {
    matches!(
        error,
        DockerError::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}
