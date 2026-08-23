use std::{
    collections::{BTreeSet, HashMap},
    io,
    net::{IpAddr, SocketAddr},
    process::Command,
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};

use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{Ipam, IpamConfig, NetworkCreateRequest, NetworkDisconnectRequest, NetworkInspect},
};
use defguard_wireguard_rs::{
    InterfaceConfiguration, Kernel, WGApi, WireguardInterfaceApi, host::Peer, key::Key,
    net::IpAddrMask,
};
use ipnet::IpNet;
use ployz_core::{
    DOCKER_NETWORK_CONFLICT_RECOVERY, LocalMachinePhase, Machine, MachineId, SelectedEndpoint,
    WireGuardDevice, WireGuardPeer, WireGuardPublicKey,
};
use tokio_util::sync::CancellationToken;

use super::{
    DOCKER_NETWORK_NAME, MACHINE_API_PORT, MeshPeer, NetworkError, WIREGUARD_INTERFACE_NAME,
    WIREGUARD_KEEPALIVE_SECONDS, WIREGUARD_PORT, WireGuardPrivateKey, attach_peer_selections,
    checked_command, firewall::remove_firewall_rules, management_address, peers_for,
};
use crate::{
    corrosion::{ReplicatedObservations, ReplicatedStore},
    machine::{LocalMachineBody, LocalMachineRecord, LocalMachineStore},
};

const NETWORK_MTU: u32 = 1420;
const DOCKER_NETWORK_MANAGED_LABEL: &str = "ployzd.managed";

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
    bootstrap_peers: Vec<MeshPeer>,
    peers: Vec<MeshPeer>,
}

impl NetworkPlane {
    pub async fn start(record: &LocalMachineRecord) -> Result<Option<Self>, NetworkError> {
        let (machine, bootstrap) = match &record.body {
            LocalMachineBody::Joining {
                machine, bootstrap, ..
            }
            | LocalMachineBody::Participating {
                machine, bootstrap, ..
            } => (machine.clone(), bootstrap.as_slice()),
            LocalMachineBody::Uninitialized { .. } | LocalMachineBody::Resetting { .. } => {
                return Ok(None);
            }
        };
        let private_key = record.wireguard_private_key.clone();
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
        let (peers, _) = attach_peer_selections(
            peers_for(&machine.id, bootstrap),
            Vec::new(),
            &record.selected_endpoints,
            now,
        );
        let mut plane = Self {
            machine,
            private_key,
            docker,
            wireguard,
            mtu: record.wireguard_mtu.unwrap_or(NETWORK_MTU),
            routes: BTreeSet::new(),
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
                        .phase()
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
        if let Err(error) = self.remove_docker_network().await {
            failures.push(error.to_string());
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

    // Docker refuses to delete a network that still has endpoints. Unregistry
    // stays attached until after this plane returns, so detach first.
    async fn remove_docker_network(&self) -> Result<(), NetworkError> {
        let containers = match self.docker.inspect_network(DOCKER_NETWORK_NAME, None).await {
            Ok(network) => attached_container_ids(network.containers.as_ref()),
            Err(error) if docker_not_found(&error) => return Ok(()),
            Err(error) => return Err(error.into()),
        };
        for container in containers {
            match self
                .docker
                .disconnect_network(
                    DOCKER_NETWORK_NAME,
                    NetworkDisconnectRequest {
                        container,
                        force: Some(true),
                    },
                )
                .await
            {
                Ok(()) => {}
                Err(error) if docker_not_found(&error) => {}
                Err(error) => return Err(error.into()),
            }
        }
        match self.docker.remove_network(DOCKER_NETWORK_NAME).await {
            Ok(()) => Ok(()),
            Err(error) if docker_not_found(&error) => Ok(()),
            Err(error) => Err(error.into()),
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
        snapshot: &ReplicatedObservations<Machine, MachineId>,
        local: &Arc<Mutex<LocalMachineStore>>,
    ) -> Result<(), NetworkError> {
        let (selected, joining) = {
            let local = local
                .lock()
                .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
            (
                local.record().selected_endpoints.clone(),
                local.record().phase() == LocalMachinePhase::Joining,
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
        let previous = std::mem::take(&mut self.peers);
        let (planned, newly_selected) = attach_peer_selections(planned, previous, &selected, now);
        for (machine_id, endpoint) in newly_selected {
            persist_selection(local, machine_id, endpoint);
        }
        self.apply_peers(&planned)?;
        self.peers = planned;
        Ok(())
    }

    fn apply_peers(&mut self, peers: &[MeshPeer]) -> Result<(), NetworkError> {
        let wg_peers = peers
            .iter()
            .map(|peer| wireguard_peer(peer, peer.selected()))
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
                            Err(error) if docker_not_found(&error) => {}
                            Err(error) => {
                                return Err(docker_network_conflict(
                                    &network,
                                    &subnet,
                                    &gateway,
                                    &required_options,
                                    format!(
                                        "Docker refused to remove the network after inspection: {error}"
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
            Err(error) if docker_not_found(&error) => {}
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
        // TODO(UT-113): check if this works when firewalld used instead of raw iptables. The Docker daemon has a different
        // code path for firewalld.
        Ok(true)
    }
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
    NetworkError::DockerNetworkConflict {
        reason: reason.into(),
        expected: format!(
            "name={DOCKER_NETWORK_NAME}, driver=bridge, scope=local, label={DOCKER_NETWORK_MANAGED_LABEL}=\"\", subnet={subnet}, gateway={gateway}, options={required_options:?}"
        ),
        observed: format!("{network:?}"),
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

fn attached_container_ids<T>(containers: Option<&HashMap<String, T>>) -> Vec<String> {
    containers
        .map(|containers| containers.keys().cloned().collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stale_network(name: &str, managed_label: Option<&str>, attached: bool) -> NetworkInspect {
        NetworkInspect {
            name: Some(name.into()),
            labels: managed_label
                .map(|value| HashMap::from([(DOCKER_NETWORK_MANAGED_LABEL.into(), value.into())])),
            containers: attached
                .then(|| HashMap::from([("7074faa8a368".into(), Default::default())])),
            ..Default::default()
        }
    }

    #[test]
    fn stale_network_deletion_requires_exact_name_label_and_no_attachments() {
        assert_eq!(
            stale_network_replacement_allowed(&stale_network("ployz", Some(""), false)),
            Ok(())
        );

        for network in [
            stale_network("ployz-old", Some(""), false),
            stale_network("ployz", None, false),
            stale_network("ployz", Some("other-owner"), false),
            stale_network("ployz", Some(""), true),
        ] {
            assert!(stale_network_replacement_allowed(&network).is_err());
        }
    }

    #[test]
    fn matching_network_is_reused_with_attached_containers() {
        let mut network = stale_network("ployz", Some(""), true);
        network.driver = Some("bridge".into());
        network.scope = Some("local".into());
        network.ipam = Some(Ipam {
            config: Some(vec![IpamConfig {
                subnet: Some("10.210.1.0/24".into()),
                gateway: Some("10.210.1.1".into()),
                ..Default::default()
            }]),
            ..Default::default()
        });
        let expected_options = required_docker_network_options(1420);
        network.options = Some(expected_options.clone());

        assert!(docker_network_matches(
            &network,
            "10.210.1.0/24",
            "10.210.1.1",
            &expected_options,
        ));
    }

    #[test]
    fn stale_network_refusal_is_actionable() {
        let network = stale_network("ployz", Some(""), true);
        let expected_options = required_docker_network_options(1420);
        let error = docker_network_conflict(
            &network,
            "10.210.1.0/24",
            "10.210.1.1",
            &expected_options,
            "containers are attached",
        )
        .to_string();

        assert!(error.contains("expected: name=ployz"));
        assert!(error.contains("subnet=10.210.1.0/24"));
        assert!(error.contains("observed: NetworkInspect"));
        assert!(error.contains("7074faa8a368"));
        assert!(error.contains("systemctl stop ployz"));
        assert!(error.contains("docker network inspect ployz"));
        assert!(error.contains("docker network rm ployz"));
        assert!(error.contains("systemctl start ployz"));
    }

    #[test]
    fn reset_cleanup_lists_attached_containers_before_removing_the_network() {
        let network: bollard::models::NetworkInspect = serde_json::from_str(
            r#"{
                "Name": "ployz",
                "Containers": {
                    "7074faa8a368": { "Name": "ployz-unregistry" },
                    "779285f57926": { "Name": "leftover" }
                }
            }"#,
        )
        .unwrap();
        let mut ids = attached_container_ids(network.containers.as_ref());
        ids.sort();
        assert_eq!(ids, ["7074faa8a368".to_owned(), "779285f57926".to_owned()]);
        assert!(attached_container_ids::<bollard::models::EndpointResource>(None).is_empty());
    }
}
