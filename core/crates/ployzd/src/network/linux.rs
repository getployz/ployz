use std::{
    collections::{BTreeSet, HashMap},
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    process::Command,
    time::{Duration, SystemTime},
};

use bollard::{
    Docker,
    models::{
        ContainerSummary, EndpointSettings, Ipam, IpamConfig, NetworkConnectRequest,
        NetworkCreateRequest, NetworkDisconnectRequest, NetworkInspect, RestartPolicyNameEnum,
    },
    query_parameters::{ListContainersOptionsBuilder, StopContainerOptionsBuilder},
};
use defguard_wireguard_rs::{
    InterfaceConfiguration, Kernel, WGApi, WireguardInterfaceApi,
    error::WireguardInterfaceError,
    host::{Host, Peer},
    key::Key,
    net::IpAddrMask,
};
use ipnet::IpNet;
use ployz_core::{
    DOCKER_NETWORK_CONFLICT_RECOVERY, LocalMachinePhase, Machine, MachineId, MachineSubnet,
    SelectedEndpoint, WireGuardPublicKey,
};
use serde::Deserialize;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::docker_image::is_not_found;

use super::{
    ChoiceRefused, DOCKER_NETWORK_NAME, EndpointChoice, MACHINE_API_PORT, MeshPeer, NetworkError,
    WIREGUARD_INTERFACE_NAME, WIREGUARD_KEEPALIVE_SECONDS, WIREGUARD_PORT, WireGuardPrivateKey,
    attach_peer_selections, checked_command, choose_endpoint,
    firewall::{docker_ipv6_firewall, fence_container_ipv6, remove_firewall_rules},
    peers_for, port_held_elsewhere,
};
use crate::{
    corrosion::{MachineView, MachinesSnapshot, ReplicatedObservations, ReplicatedStore},
    machine::{LocalMachineBody, LocalMachineRecord, RecordOwner},
};

const NETWORK_MTU: u32 = 1420;
const DOCKER_NETWORK_MANAGED_LABEL: &str = "ployzd.managed";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PeerSet {
    Bootstrap,
    Snapshot,
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
        let docker_created = match plane.ensure_docker_network(data_dir).await {
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
    /// is polled every second: handshakes and endpoint roaming have no event. An
    /// operator's endpoint from `choices` goes to WireGuard at once.
    pub async fn run(
        &mut self,
        machines: Option<(ReplicatedStore, MachineView)>,
        local: RecordOwner,
        mut choices: mpsc::Receiver<EndpointChoice>,
        shutdown: CancellationToken,
    ) -> io::Result<()> {
        let Some((replicated, machines)) = machines else {
            drop(choices);
            shutdown.cancelled().await;
            return Ok(());
        };
        let mut machines = machines.watch();
        machines.mark_changed();
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

    /// Rebuild from the view's latest snapshot unless it is the last one settled.
    async fn rebuild_when_changed(
        &mut self,
        replicated: &ReplicatedStore,
        machines: &watch::Receiver<MachinesSnapshot>,
        previous: &mut MachinesSnapshot,
        local: &RecordOwner,
    ) -> io::Result<()> {
        let snapshot = machines.borrow().clone();
        if let Some(snapshot) = snapshot
            && previous.as_ref() != Some(&snapshot)
        {
            let settled = self
                .rebuild(replicated, &snapshot, local)
                .await
                .map_err(io::Error::other)?;
            // An unsettled rebuild may have added peers, so no snapshot counts as applied:
            // the same snapshot arriving again must still prune them.
            *previous = settled.then_some(snapshot);
        }
        Ok(())
    }

    /// Whether the snapshot is settled. `false` when the store couldn't be read, or when
    /// only adding kept peers the store can't yet vouch for, so a later tick reads again.
    async fn rebuild(
        &mut self,
        replicated: &ReplicatedStore,
        snapshot: &ReplicatedObservations<Machine, MachineId>,
        local: &RecordOwner,
    ) -> Result<bool, NetworkError> {
        let held_another_machine = match replicated.held_another_machine(&self.machine.id).await {
            Ok(held) => held,
            Err(error) => {
                eprintln!(
                    "failed to read whether this Corrosion store has held another Machine: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                return Ok(false);
            }
        };
        let record = local.record();
        let Some((planned, set)) = snapshot_peers(
            &self.machine.id,
            snapshot,
            &self.bootstrap_peers,
            record.phase() == LocalMachinePhase::Joining,
            held_another_machine,
        ) else {
            return Ok(true);
        };
        let selected = &record.selected_endpoints;
        let now = SystemTime::now();
        let mut previous = std::mem::take(&mut self.endpoint_peers);
        let host = self.wireguard.read_interface_data()?;
        seed_retained_selections(&planned, &mut previous, &host.peers, now);
        let (planned, _) = attach_peer_selections(planned, previous, selected, now);
        let settled = set == PeerSet::Snapshot || host.peers.is_empty() && planned.is_empty();
        self.apply_peers(&planned, set)?;
        self.endpoint_peers = planned;
        // Only an installed selection is worth remembering across a restart.
        for peer in &self.endpoint_peers {
            if let Some(endpoint) = peer.selected()
                && selected.get(&peer.machine_id).copied() != Some(endpoint)
            {
                persist_selection(local, peer.machine_id, endpoint).await;
            }
        }
        Ok(settled)
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
                })
                .map_err(|error| self.port_clash(error))?;
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

    /// Names another program holding the WireGuard port, unless this interface listens on it
    /// already and the setup failed after that.
    fn port_clash(&self, failure: WireguardInterfaceError) -> NetworkError {
        let listening = self
            .wireguard
            .read_interface_data()
            .is_ok_and(|host| host.listen_port == WIREGUARD_PORT);
        if listening {
            return failure.into();
        }
        port_held_elsewhere(WIREGUARD_PORT, failure)
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

    async fn apply_choice(&mut self, choice: EndpointChoice, local: &RecordOwner) {
        let EndpointChoice {
            peer,
            endpoint,
            applied,
        } = choice;
        let outcome =
            match choose_endpoint(&mut self.endpoint_peers, peer, endpoint, SystemTime::now()) {
                Ok(chosen) => self
                    .wireguard
                    .configure_peer(&wireguard_peer(chosen, Some(endpoint)))
                    .map_err(|error| {
                        eprintln!(
                            "failed to dial the operator's WireGuard endpoint: {error}",
                            error = ployz_core::error_chain::inline(&error),
                        );
                        ChoiceRefused::WireGuard
                    }),
                Err(refused) => Err(refused),
            };
        if outcome.is_ok() {
            persist_selection(local, peer, endpoint).await;
        }
        let _ = applied.send(outcome);
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

    async fn ensure_docker_network(&self, data_dir: &Path) -> Result<bool, NetworkError> {
        // The fence goes in before any container holds IPv6 that NAT66 could carry
        // into the mesh, including one an earlier start gave IPv6; without it the
        // bridge stays IPv4-only.
        let fenced = docker_ipv6_firewall()
            && fence_container_ipv6()
                .inspect_err(|error| {
                    eprintln!(
                        "the {DOCKER_NETWORK_NAME} network stays IPv4-only: the IPv6 mesh fence failed: {error}",
                        error = ployz_core::error_chain::inline(error),
                    );
                })
                .is_ok();
        let ipv6 =
            (fenced && bridge_ipv6_allowed()).then(|| docker_network_ipv6(self.machine.subnet));
        // Both passes may stop members, which start again only once a bridge stands.
        let _extend = crate::daemon::extend_systemd_start_timeout("settling the ployz network");
        let mut stopped = Stopped::load(data_dir);
        let created = self
            .ensure_docker_network_shape(ipv6.as_ref(), &mut stopped)
            .await?;
        self.pin_members(&mut stopped).await;
        stopped.start(&self.docker).await;
        Ok(created)
    }

    async fn ensure_docker_network_shape(
        &self,
        ipv6: Option<&IpamConfig>,
        stopped: &mut Stopped,
    ) -> Result<bool, NetworkError> {
        let subnet = self.machine.subnet.to_string();
        let gateway = self.machine.subnet.gateway().0.to_string();
        let required_options = required_docker_network_options(self.mtu);
        let request = docker_network_request(self.machine.subnet, &required_options, ipv6);
        let ipv4_only = ipv6
            .is_some()
            .then(|| docker_network_request(self.machine.subnet, &required_options, None));
        match self.docker.inspect_network(DOCKER_NETWORK_NAME, None).await {
            Ok(network) => {
                if docker_network_matches(&network, &subnet, &gateway, &required_options) {
                    if !docker_network_ipv6_matches(&network, ipv6) {
                        self.readdress_docker_network(&network, request, ipv4_only, stopped)
                            .await?;
                    }
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
        if let Some(refused) = self.create_bridge(request, ipv4_only).await? {
            report_readdress(true, &refused);
        }
        Ok(true)
    }

    /// Docker can't change a network's addressing in place, so the bridge is replaced
    /// under its members, stopped ones included, since a stopped container keeps the
    /// old network's ID and could not start. Running containers keep running. Each
    /// member rejoins at the address it holds or is pinned to, so routes to it stay
    /// valid; one with neither takes a free one. Docker won't detach or attach a
    /// container between restarts, Ployz's or not, so such a member is stopped first
    /// and noted in `stopped` to start again. The bridge is replaced only once every
    /// member is detached, so a failure leaves a bridge serving, with every member on
    /// it; the next start tries again.
    ///
    /// # Errors
    ///
    /// Returns an error only when the Machine is left with no bridge at all.
    async fn readdress_docker_network(
        &self,
        network: &NetworkInspect,
        request: NetworkCreateRequest,
        ipv4_only: Option<NetworkCreateRequest>,
        stopped: &mut Stopped,
    ) -> Result<(), NetworkError> {
        let take_ipv6 = ipv4_only.is_some();
        let mut members = match self.bridge_members().await {
            Ok(members) => members,
            Err(error) => {
                report_readdress(take_ipv6, &error);
                return Ok(());
            }
        };
        members.sort_by_key(|member| (member.ipv4.is_none(), member.ipv4));
        let mut detached = Vec::new();
        let mut disconnected = Ok(());
        for member in members.iter().filter(|member| member.attached) {
            match self.detach(&member.id, stopped).await {
                Ok(true) => detached.push(member),
                Ok(false) => {}
                Err(error) => {
                    disconnected = Err(error.into());
                    break;
                }
            }
        }
        let replaced = match disconnected {
            Ok(()) => {
                self.replace_docker_network(network, request, ipv4_only)
                    .await
            }
            Err(error) => Ok(Some(error)),
        };
        if let Ok(Some(error)) = &replaced {
            report_readdress(take_ipv6, error);
        }
        let taken = members.iter().filter_map(|member| member.ipv4).collect();
        for (member, ipv4) in addresses(detached, taken, self.machine.subnet) {
            self.rejoin(member, ipv4, stopped).await;
        }
        replaced.map(|_| ())
    }

    /// Returns why the bridge stays as it was or fell back to IPv4-only, or an error
    /// when no bridge could be created.
    async fn replace_docker_network(
        &self,
        network: &NetworkInspect,
        request: NetworkCreateRequest,
        ipv4_only: Option<NetworkCreateRequest>,
    ) -> Result<Option<NetworkError>, NetworkError> {
        let id = network.id.as_deref().unwrap_or(DOCKER_NETWORK_NAME);
        match self.docker.remove_network(id).await {
            Ok(()) => {}
            Err(error) if is_not_found(&error) => {}
            Err(error) => return Ok(Some(error.into())),
        }
        self.create_bridge(request, ipv4_only).await
    }

    /// Falls back to the IPv4-only bridge when Docker refuses the dual-stack one.
    /// Returns why it fell back, or an error when no bridge could be created.
    async fn create_bridge(
        &self,
        request: NetworkCreateRequest,
        ipv4_only: Option<NetworkCreateRequest>,
    ) -> Result<Option<NetworkError>, NetworkError> {
        let Err(refused) = self.docker.create_network(request).await else {
            return Ok(None);
        };
        let Some(ipv4_only) = ipv4_only else {
            return Err(refused.into());
        };
        match self.docker.create_network(ipv4_only).await {
            Ok(_) => Ok(Some(refused.into())),
            Err(error) => Err(NetworkError::Io(io::Error::other(format!(
                "Docker refused the {DOCKER_NETWORK_NAME} network both dual-stack ({refused}) and IPv4-only ({error})",
                refused = ployz_core::error_chain::inline(&refused),
                error = ployz_core::error_chain::inline(&error),
            )))),
        }
    }

    async fn bridge_members(&self) -> Result<Vec<BridgeMember>, NetworkError> {
        let containers = self.bridge_containers().await?;
        Ok(bridge_members(&containers))
    }

    async fn bridge_containers(&self) -> Result<Vec<ContainerSummary>, NetworkError> {
        Ok(self
            .docker
            .list_containers(Some(
                ListContainersOptionsBuilder::default().all(true).build(),
            ))
            .await?)
    }

    /// Every Container on the bridge holds a static address, so Docker can't hand a
    /// Container's address to another when it starts them all at boot. One without
    /// keeps the address it has, or takes a free one. A container created on the
    /// bridge that lost its endpoint, such as one a readdress detached, joins it again.
    /// One Ployz doesn't manage is pinned too, since a dynamic one could take a
    /// pinned address at boot, and it rejoins with its own aliases and options.
    async fn pin_members(&self, stopped: &mut Stopped) {
        let containers = match self.bridge_containers().await {
            Ok(containers) => containers,
            Err(error) => {
                eprintln!(
                    "could not list containers to pin their {DOCKER_NETWORK_NAME} network addresses: {error}",
                    error = ployz_core::error_chain::inline(&error),
                );
                return;
            }
        };
        let members = bridge_members(&containers);
        let taken = crate::docker::bridge_address::taken(&containers);
        let unpinned = members.iter().filter(|member| member.needs_pin());
        for (member, ipv4) in addresses(unpinned, taken, self.machine.subnet) {
            let mut endpoint = crate::docker::bridge_address::endpoint(member.ipv6, None);
            if member.attached {
                match self.docker.inspect_container(&member.id, None).await {
                    Ok(inspected) => {
                        if let Some(current) = inspected
                            .network_settings
                            .and_then(|settings| settings.networks)
                            .and_then(|mut networks| networks.remove(DOCKER_NETWORK_NAME))
                        {
                            endpoint = crate::docker::bridge_address::reconnect(current);
                        }
                    }
                    Err(error) => {
                        eprintln!(
                            "container {container} keeps a dynamic {DOCKER_NETWORK_NAME} network address: {error}",
                            container = member.id,
                            error = ployz_core::error_chain::inline(&error),
                        );
                        continue;
                    }
                }
                match self.detach(&member.id, stopped).await {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        eprintln!(
                            "container {container} keeps a dynamic {DOCKER_NETWORK_NAME} network address: {error}",
                            container = member.id,
                            error = ployz_core::error_chain::inline(&error),
                        );
                        continue;
                    }
                }
            }
            self.rejoin_with(member, endpoint, ipv4, stopped).await;
        }
    }

    /// Detaches `id` from the bridge, stopping it first when Docker holds it between
    /// restarts. Returns `false` when the container is gone.
    async fn detach(
        &self,
        id: &str,
        stopped: &mut Stopped,
    ) -> Result<bool, bollard::errors::Error> {
        let disconnect = || {
            self.docker.disconnect_network(
                DOCKER_NETWORK_NAME,
                NetworkDisconnectRequest {
                    container: id.to_owned(),
                    force: Some(true),
                },
            )
        };
        let mut result = disconnect().await;
        if result.as_ref().is_err_and(between_restarts) {
            self.stop(id, stopped).await;
            result = disconnect().await;
        }
        match result {
            Ok(()) => Ok(true),
            Err(error) if is_not_found(&error) => Ok(false),
            Err(error) => Err(error),
        }
    }

    /// Settles `id`, which Docker holds between restarts, so the operation Docker
    /// refused can be retried; see `settle`. A stop drops a restart Docker has
    /// pending and waits out its own, so Docker can't attach the member behind a
    /// detach. A member is noted in `stopped` before its stop, and left alone when
    /// that note can't be written.
    async fn stop(&self, id: &str, stopped: &mut Stopped) {
        let inspected = match self.docker.inspect_container(id, None).await {
            Ok(inspected) => inspected,
            Err(error) => {
                if !is_not_found(&error) {
                    report_stop(id, &error);
                }
                return;
            }
        };
        let state = inspected.state.unwrap_or_default();
        let policy = inspected
            .host_config
            .and_then(|host| host.restart_policy)
            .and_then(|policy| policy.name);
        match settle(
            state.running == Some(true),
            state.restarting == Some(true),
            policy,
        ) {
            Settle::Leave => return,
            Settle::Stop => {}
            Settle::StopAndNote => {
                if let Err(error) = stopped.note(id) {
                    report_stop(id, &error);
                    return;
                }
            }
        }
        let options = StopContainerOptionsBuilder::default().t(0).build();
        if let Err(error) = self.docker.stop_container(id, Some(options)).await
            && !is_not_found(&error)
        {
            report_stop(id, &error);
        }
    }

    async fn rejoin(&self, member: &BridgeMember, ipv4: Option<Ipv4Addr>, stopped: &mut Stopped) {
        let endpoint = crate::docker::bridge_address::endpoint(member.ipv6, None);
        self.rejoin_with(member, endpoint, ipv4, stopped).await;
    }

    /// Falls back to any free address when `ipv4` can't be had. Stops a container
    /// Docker holds between restarts first, as `detach` does.
    async fn rejoin_with(
        &self,
        member: &BridgeMember,
        endpoint: EndpointSettings,
        ipv4: Option<Ipv4Addr>,
        stopped: &mut Stopped,
    ) {
        let connect = |ipv4| NetworkConnectRequest {
            container: member.id.clone(),
            endpoint_config: Some(crate::docker::bridge_address::pin(endpoint.clone(), ipv4)),
        };
        let mut result = self
            .docker
            .connect_network(DOCKER_NETWORK_NAME, connect(ipv4))
            .await;
        if result.as_ref().is_err_and(between_restarts) {
            self.stop(&member.id, stopped).await;
            result = self
                .docker
                .connect_network(DOCKER_NETWORK_NAME, connect(ipv4))
                .await;
        }
        if result.is_err() && ipv4.is_some() {
            result = self
                .docker
                .connect_network(DOCKER_NETWORK_NAME, connect(None))
                .await;
        }
        if let Err(error) = result {
            eprintln!(
                "container {container} could not rejoin the {DOCKER_NETWORK_NAME} network: {error}",
                container = member.id,
                error = ployz_core::error_chain::inline(&error),
            );
        }
    }
}

/// The static address each member takes: its own, or the next free one.
fn addresses<'a>(
    members: impl IntoIterator<Item = &'a BridgeMember>,
    mut taken: BTreeSet<Ipv4Addr>,
    subnet: MachineSubnet,
) -> Vec<(&'a BridgeMember, Option<Ipv4Addr>)> {
    let mut last = None;
    members
        .into_iter()
        .map(|member| {
            let ipv4 = member.ipv4.or_else(|| {
                let next = crate::docker::bridge_address::next(subnet, &taken, last);
                last = next.or(last);
                next
            });
            taken.extend(ipv4);
            (member, ipv4)
        })
        .collect()
}

fn report_readdress(take_ipv6: bool, error: &NetworkError) {
    eprintln!(
        "{failure}: {error}",
        failure = readdress_failure(take_ipv6),
        error = ployz_core::error_chain::inline(error),
    );
}

fn readdress_failure(take_ipv6: bool) -> String {
    let change = if take_ipv6 { "take" } else { "drop" };
    format!("the {DOCKER_NETWORK_NAME} network could not {change} IPv6")
}

fn report_stop(id: &str, error: &(dyn std::error::Error + 'static)) {
    eprintln!(
        "container {id} could not be stopped to move it to the new {DOCKER_NETWORK_NAME} network: {error}",
        error = ployz_core::error_chain::inline(error),
    );
}

/// Docker still counts a container between restarts as running, but it holds no
/// sandbox, so Docker refuses to detach it from the bridge or attach it.
fn between_restarts(error: &bollard::errors::Error) -> bool {
    let bollard::errors::Error::DockerResponseServerError { message, .. } = error else {
        return false;
    };
    let not_connected = format!("is not connected to network {DOCKER_NETWORK_NAME}");
    message.ends_with(&not_connected)
        || (message.starts_with("network sandbox for container ")
            && message.ends_with(" not found"))
}

/// What to do with a member Docker refused as being between restarts.
#[derive(Debug, PartialEq, Eq)]
enum Settle {
    /// Docker would run it again, so Ployz starts it again once it has moved.
    StopAndNote,
    /// It is exiting for good, so it stays stopped, as Docker would leave it.
    Stop,
    /// Whether Docker runs it again can't be told yet, or it has exited already:
    /// the refused operation is only retried.
    Leave,
}

/// Docker restarts a container it is restarting, and one exiting under `always`
/// or `unless-stopped`. One exiting under `on-failure` restarts only on a failing
/// exit code, which Docker records once it has exited, so Ployz doesn't stop it.
fn settle(running: bool, restarting: bool, policy: Option<RestartPolicyNameEnum>) -> Settle {
    if restarting {
        return Settle::StopAndNote;
    }
    if !running {
        return Settle::Leave;
    }
    match policy {
        Some(RestartPolicyNameEnum::ALWAYS | RestartPolicyNameEnum::UNLESS_STOPPED) => {
            Settle::StopAndNote
        }
        Some(RestartPolicyNameEnum::ON_FAILURE) => Settle::Leave,
        None | Some(RestartPolicyNameEnum::NO | RestartPolicyNameEnum::EMPTY) => Settle::Stop,
    }
}

/// A container on the ployz bridge, or created for it.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct BridgeMember {
    id: String,
    ipv4: Option<Ipv4Addr>,
    attached: bool,
    pinned: bool,
    ipv6: bool,
}

impl BridgeMember {
    /// Detached, or still on a dynamic address.
    fn needs_pin(&self) -> bool {
        !self.attached || !self.pinned
    }
}

fn bridge_members(containers: &[ContainerSummary]) -> Vec<BridgeMember> {
    let mut members = containers
        .iter()
        .filter_map(|container| {
            let endpoint = container
                .network_settings
                .as_ref()
                .and_then(|settings| settings.networks.as_ref())
                .and_then(|networks| networks.get(DOCKER_NETWORK_NAME));
            let created_on = container
                .host_config
                .as_ref()
                .and_then(|host| host.network_mode.as_deref())
                == Some(DOCKER_NETWORK_NAME);
            (endpoint.is_some() || created_on).then(|| BridgeMember {
                id: container.id.clone().unwrap_or_default(),
                ipv4: endpoint
                    .and_then(|endpoint| endpoint.ip_address.as_deref())
                    .and_then(|address| address.parse().ok())
                    .or_else(|| endpoint.and_then(crate::docker::bridge_address::pinned)),
                attached: endpoint.is_some(),
                pinned: endpoint
                    .and_then(crate::docker::bridge_address::pinned)
                    .is_some(),
                ipv6: container
                    .labels
                    .as_ref()
                    .is_some_and(crate::docker::takes_bridge_ipv6),
            })
        })
        .filter(|member| !member.id.is_empty())
        .collect::<Vec<_>>();
    members.sort();
    members
}

/// Docker turns on IPv6 forwarding for a dual-stack bridge, and a forwarding
/// interface ignores Router Advertisements unless its `accept_ra` is 2. A host that
/// learns its IPv6 route or address from them would lose it when that expires, so
/// its bridge stays IPv4-only until the operator sets 2, which the next start sees.
fn bridge_ipv6_allowed() -> bool {
    if !host_has_ipv6() {
        return false;
    }
    if !docker_ipv6_firewall() {
        eprintln!(
            "the {DOCKER_NETWORK_NAME} network stays IPv4-only, so apps see IPv6 visitors as its gateway: \
             Docker's ip6tables is off or uses nftables, so Ployz can't fence container IPv6 out of the mesh."
        );
        return false;
    }
    let listing = |args: &[&str]| {
        checked_command("ip", args)
            .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
            .unwrap_or_default()
    };
    let exposed = router_advertised_interfaces(
        &listing(&["-6", "-o", "route", "show", "default"]),
        &listing(&["-6", "-o", "addr", "show", "scope", "global"]),
    )
    .into_iter()
    .filter(|interface| {
        std::fs::read_to_string(format!("/proc/sys/net/ipv6/conf/{interface}/accept_ra"))
            .is_ok_and(|accept_ra| accept_ra.trim() == "1")
    })
    .collect::<Vec<_>>();
    if exposed.is_empty() {
        return true;
    }
    let settings = exposed
        .iter()
        .map(|interface| format!("net.ipv6.conf.{}.accept_ra=2", interface.replace('.', "/")))
        .collect::<Vec<_>>()
        .join(" ");
    eprintln!(
        "the {DOCKER_NETWORK_NAME} network stays IPv4-only, so apps see IPv6 visitors as its gateway: \
         {interfaces} learn IPv6 from router advertisements, which IPv6 forwarding would stop. \
         Run `sysctl -w {settings}`, persist it in /etc/sysctl.d, then `systemctl restart ployz`.",
        interfaces = exposed.join(", "),
    );
    false
}

/// Interfaces with a default route or a global address from a router advertisement,
/// read from `ip -6 -o route show default` and `ip -6 -o addr show scope global`.
fn router_advertised_interfaces(routes: &str, addresses: &str) -> BTreeSet<String> {
    let routed = routes.lines().filter_map(|line| {
        let words = line.split_whitespace().collect::<Vec<_>>();
        let after = |key| {
            words
                .iter()
                .skip_while(|word| **word != key)
                .nth(1)
                .copied()
        };
        (after("proto") == Some("ra"))
            .then(|| after("dev").map(str::to_owned))
            .flatten()
    });
    let addressed = addresses.lines().filter_map(|line| {
        let mut words = line.split_whitespace();
        let interface = words.nth(1)?;
        words
            .any(|word| word == "dynamic")
            .then(|| interface.to_owned())
    });
    routed.chain(addressed).collect()
}

/// The host can carry IPv6 on a new bridge: the kernel has it and new interfaces
/// don't start with it disabled.
fn host_has_ipv6() -> bool {
    Path::new("/proc/net/if_inet6").exists()
        && std::fs::read_to_string("/proc/sys/net/ipv6/conf/default/disable_ipv6")
            .is_ok_and(|disabled| disabled.trim() == "0")
}

/// A ULA `/64` that embeds the Machine Subnet, so no two Machines' containers share
/// one. IPv6 visitors reach published ports by DNAT, with their own source address,
/// instead of through docker-proxy, which would replace it with the gateway (#2837).
fn docker_network_ipv6(subnet: MachineSubnet) -> IpamConfig {
    let [a, b, c, _] = subnet.gateway().0.octets();
    let prefix = std::net::Ipv6Addr::new(
        0xfdcd,
        0,
        u16::from_be_bytes([a, b]),
        u16::from_be_bytes([c, 0]),
        0,
        0,
        0,
        0,
    );
    let gateway = std::net::Ipv6Addr::from(u128::from(prefix) + 1);
    IpamConfig {
        subnet: Some(format!("{prefix}/64")),
        gateway: Some(gateway.to_string()),
        ..Default::default()
    }
}

fn docker_network_request(
    subnet: MachineSubnet,
    options: &HashMap<String, String>,
    ipv6: Option<&IpamConfig>,
) -> NetworkCreateRequest {
    let ipv4 = IpamConfig {
        subnet: Some(subnet.to_string()),
        gateway: Some(subnet.gateway().0.to_string()),
        ..Default::default()
    };
    NetworkCreateRequest {
        name: DOCKER_NETWORK_NAME.into(),
        driver: Some("bridge".into()),
        scope: Some("local".into()),
        enable_ipv6: Some(ipv6.is_some()),
        ipam: Some(Ipam {
            config: Some(std::iter::once(ipv4).chain(ipv6.cloned()).collect()),
            ..Default::default()
        }),
        options: Some(options.clone()),
        labels: Some(HashMap::from([(
            DOCKER_NETWORK_MANAGED_LABEL.into(),
            String::new(),
        )])),
        ..Default::default()
    }
}

fn docker_network_ipv6_matches(network: &NetworkInspect, wanted: Option<&IpamConfig>) -> bool {
    let observed = network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .into_iter()
        .flatten()
        .find(|config| {
            config
                .subnet
                .as_deref()
                .is_some_and(|subnet| subnet.contains(':'))
        });
    network.enable_ipv6.unwrap_or(false) == wanted.is_some()
        && observed.map(|config| (&config.subnet, &config.gateway))
            == wanted.map(|config| (&config.subnet, &config.gateway))
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

/// The peers a snapshot plans, and whether they replace the installed set. A store that
/// never held a row for another Machine, such as one recreated after its `store.db` was
/// lost, can't say who left: it only adds, so the peers that carry its gossip stay. A
/// removed Machine's row leaves its key behind, so a removal still prunes.
fn snapshot_peers(
    machine_id: &MachineId,
    snapshot: &ReplicatedObservations<Machine, MachineId>,
    bootstrap: &[MeshPeer],
    joining: bool,
    held_another_machine: bool,
) -> Option<(Vec<MeshPeer>, PeerSet)> {
    if !snapshot.incomplete_ids.is_empty() {
        return None;
    }
    let mut peers = peers_for(machine_id, &snapshot.observations);
    if joining || !held_another_machine {
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
    let set = if held_another_machine {
        PeerSet::Snapshot
    } else {
        PeerSet::Bootstrap
    };
    Some((peers, set))
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
        .and_then(|configs| {
            configs.iter().find(|config| {
                config
                    .subnet
                    .as_deref()
                    .is_some_and(|subnet| !subnet.contains(':'))
            })
        });
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

mod stopped;
use stopped::Stopped;
