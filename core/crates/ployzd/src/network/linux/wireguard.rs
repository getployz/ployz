use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io,
    net::IpAddr,
    path::Path,
    process::Command,
    time::SystemTime,
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
    LargePackets, LocalMachinePhase, Machine, MachineId, SelectedEndpoint, WireGuardPublicKey,
};
use serde::Deserialize;
use tokio::sync::watch;

use crate::network::{
    ChoiceRefused, EndpointChoice, MeshPeer, NetworkError, WIREGUARD_INTERFACE_NAME,
    WIREGUARD_KEEPALIVE_SECONDS, WIREGUARD_PORT, WireGuardPrivateKey, attach_peer_selections,
    checked_command, choose_endpoint,
    path::{MIN_MTU, PathReport, ProbedPeer},
    peers_for, port_held_elsewhere,
};
use crate::{
    corrosion::{MachinesSnapshot, ReplicatedObservations, ReplicatedStore},
    machine::{LocalMachineRecord, RecordOwner},
};

use super::NetworkPlane;

/// A peer's routes and the MTU set on them, kept while those routes are installed:
/// a bootstrap apply keeps routes of peers it no longer plans.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Lowered {
    mtu: u32,
    routes: [IpNet; 2],
}

/// The peer each installed route leads to: the Machine whose WireGuard peer holds that
/// allowed IP. WireGuard gives an allowed IP to one peer only, so when two Machine Subnets
/// overlap the route leads wherever WireGuard sends it, whatever the plan's order.
pub(super) fn route_owners<'a>(
    installed: &BTreeSet<IpNet>,
    wireguard: impl IntoIterator<Item = &'a Peer>,
    machines: &[&MeshPeer],
) -> BTreeMap<IpNet, MachineId> {
    wireguard
        .into_iter()
        .filter_map(|device| {
            machines
                .iter()
                .find(|machine| Key::new(machine.public_key.0) == device.public_key)
                .map(|machine| (device, machine.machine_id))
        })
        .flat_map(|(device, owner)| {
            device
                .allowed_ips
                .iter()
                .filter_map(|address| IpNet::new(address.address, address.cidr).ok())
                .filter(|route| installed.contains(route))
                .map(move |route| (route, owner))
        })
        .collect()
}

/// Whether every one of `routes` leads to `machine_id`. A lowered MTU, or a probe report,
/// of a peer that lost a route never touches the replacement Machine that took its freed
/// IPv4 subnet; that Machine starts at the full MTU, and its own probe lowers it again.
pub(super) fn owns_routes(
    owners: &BTreeMap<IpNet, MachineId>,
    machine_id: MachineId,
    routes: &[IpNet; 2],
) -> bool {
    routes
        .iter()
        .all(|route| owners.get(route) == Some(&machine_id))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum PeerSet {
    Bootstrap,
    Snapshot,
}

pub(super) fn read_retained_device(
    wireguard: &WGApi<Kernel>,
    private_key: &WireGuardPrivateKey,
    machine: &Machine,
) -> Result<Option<Host>, NetworkError> {
    validate_machine_identity(private_key, machine.public_key)?;
    let Some((host, addresses)) = read_device(wireguard)? else {
        return Ok(None);
    };
    validate_retained_device(&host, private_key, machine, &addresses)?;
    Ok(Some(host))
}

/// The `ployz-wg` device and, when it holds no key, its addresses.
pub(super) fn read_device(
    wireguard: &WGApi<Kernel>,
) -> Result<Option<(Host, Vec<IpAddrMask>)>, NetworkError> {
    if !wireguard_device_exists()? {
        return Ok(None);
    }
    let host = wireguard.read_interface_data()?;
    let addresses = if host.private_key.is_none() {
        wireguard_addresses()?
    } else {
        Vec::new()
    };
    Ok(Some((host, addresses)))
}

pub(super) fn wireguard_device_exists() -> io::Result<bool> {
    Path::new("/sys/class/net")
        .join(WIREGUARD_INTERFACE_NAME)
        .try_exists()
}

const RETAINED_DEVICE_RECOVERY: &str = "run `systemctl stop ployz.socket ployz`; run `ip link delete ployz-wg`; run `systemctl reset-failed ployz.socket ployz`; run `systemctl start ployz`";

pub(super) fn validate_machine_identity(
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

pub(super) fn validate_retained_device(
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

pub(super) fn seed_retained_selections(
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
/// lost, can't say who left: it only adds, so the peers that carry its gossip stay, unless
/// Device enforcement refuses them. A removed Machine's row leaves its key behind, so a
/// removal still prunes.
pub(super) fn snapshot_peers(
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

pub(super) struct PeerChanges<'peers> {
    pub(super) updates: Vec<&'peers Peer>,
    pub(super) removals: Vec<&'peers Key>,
    pub(super) routes: BTreeSet<IpNet>,
    /// `false` while the device keeps a peer the plan doesn't list, or while a bootstrap
    /// plan has peers on either side, so the caller applies a later read again.
    pub(super) settled: bool,
}

pub(super) fn peer_changes<'peers>(
    installed: &'peers HashMap<Key, Peer>,
    planned: &'peers [Peer],
    set: PeerSet,
    keeps_unlisted: impl Fn(&Key) -> bool,
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
    let (kept, removals): (Vec<_>, Vec<_>) = installed
        .keys()
        .filter(|key| !planned.iter().any(|peer| &peer.public_key == *key))
        .partition(|key| keeps_unlisted(key));
    let settled =
        kept.is_empty() && (set == PeerSet::Snapshot || installed.is_empty() && planned.is_empty());
    let routes = peer_routes(
        installed
            .values()
            .filter(|peer| {
                !removals.contains(&&peer.public_key)
                    && !updates
                        .iter()
                        .any(|update| update.public_key == peer.public_key)
            })
            .chain(updates.iter().copied()),
    )?;
    Ok(PeerChanges {
        updates,
        removals,
        routes,
        settled,
    })
}

pub(super) fn peer_routes<'peers>(
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

pub(super) fn wireguard_peer(config: &MeshPeer, endpoint: Option<SelectedEndpoint>) -> Peer {
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

/// Route `route` over WireGuard. `mtu` caps the packets sent on it below the device MTU;
/// the kernel also derives the TCP MSS it advertises to that peer from it.
fn replace_route(route: IpNet, source: Option<&str>, mtu: Option<u32>) -> Result<(), NetworkError> {
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
    let mtu = mtu.map(|mtu| mtu.to_string());
    if let Some(mtu) = &mtu {
        args.extend(["mtu", mtu.as_str()]);
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

impl NetworkPlane {
    /// The peer each installed route leads to now, as the WireGuard device says. A device
    /// that can't be read owns nothing, so no report or kept MTU passes on a guess.
    fn route_owners(&self, applied: &[MeshPeer]) -> BTreeMap<IpNet, MachineId> {
        self.route_owners_of(&self.routes, applied)
    }

    fn route_owners_of(
        &self,
        installed: &BTreeSet<IpNet>,
        applied: &[MeshPeer],
    ) -> BTreeMap<IpNet, MachineId> {
        let Ok(device) = self.wireguard.read_interface_data() else {
            return BTreeMap::new();
        };
        let machines = applied
            .iter()
            .chain(&self.endpoint_peers)
            .chain(&self.bootstrap_peers)
            .collect::<Vec<_>>();
        route_owners(installed, device.peers.values(), &machines)
    }

    /// Set the MTU of a peer's routes to what its path carries, and record the path.
    pub(super) fn apply_path(&mut self, report: PathReport) {
        let Some(peer) = self
            .endpoint_peers
            .iter()
            .chain(&self.bootstrap_peers)
            .find(|peer| peer.machine_id == report.machine_id)
            .cloned()
        else {
            return;
        };
        // A report from a round that began before the peer lost its routes is stale. Read the
        // owners now: an endpoint write can move an overlapping subnet's allowed IP.
        if !owns_routes(&self.route_owners(&[]), peer.machine_id, &peer.allowed_ips) {
            return;
        }
        let name = &peer.name;
        let path = report.path;
        let before = self.paths.get(&peer.machine_id);
        let lowered = (path.mtu < self.mtu).then_some(path.mtu);
        if self
            .lowered
            .get(&peer.machine_id)
            .map(|lowered| lowered.mtu)
            != lowered
        {
            let gateway = self.machine.subnet.gateway().0.to_string();
            for route in peer
                .allowed_ips
                .iter()
                .filter(|route| self.routes.contains(route))
            {
                let source = route.addr().is_ipv4().then_some(gateway.as_str());
                if let Err(error) = replace_route(*route, source, lowered) {
                    eprintln!(
                        "failed to set the MTU of the route to {name} ({route}): {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
                    return;
                }
            }
            match lowered {
                Some(mtu) => self.lowered.insert(
                    peer.machine_id,
                    Lowered {
                        mtu,
                        routes: peer.allowed_ips,
                    },
                ),
                None => self.lowered.remove(&peer.machine_id),
            };
        }
        self.paths.set(peer.machine_id, path, report.rtt);
        // The prober reports every known path each round; log only a change.
        if before == Some(path) {
            return;
        }
        let full = self.mtu;
        match (path.large_packets, lowered) {
            (LargePackets::Dropped, _)
                if before.map(|path| path.large_packets) != Some(LargePackets::Dropped) =>
            {
                eprintln!(
                    "large packets to {name} are dropped: small packets cross the mesh path, but not even {MIN_MTU}-byte ones, so calls with large replies to {name} fail"
                )
            }
            (LargePackets::Carried, Some(mtu))
                if before.is_some_and(|path| path.large_packets == LargePackets::Dropped) =>
            {
                eprintln!(
                    "the mesh path to {name} carries {mtu}-byte packets again; its routes carry at most {mtu} bytes, below the WireGuard MTU of {full}"
                )
            }
            (LargePackets::Carried, Some(mtu)) => eprintln!(
                "large packets to {name} were lost on the mesh path; its routes now carry at most {mtu} bytes, below the WireGuard MTU of {full}"
            ),
            (LargePackets::Carried, None)
                if before.is_some_and(|path| {
                    path.mtu < full || path.large_packets != LargePackets::Carried
                }) =>
            {
                eprintln!(
                    "the mesh path to {name} carries {full}-byte packets again; its routes use the WireGuard MTU"
                )
            }
            _ => {}
        }
    }

    /// Rebuild from the view's latest snapshot unless it is the last one settled.
    pub(super) async fn rebuild_when_changed(
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
    /// the device keeps a peer the snapshot doesn't list, so a later tick reads again.
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
        let accepted: BTreeSet<MachineId> = record
            .bootstrap()
            .iter()
            .map(|machine| machine.id)
            .collect();
        self.bootstrap_peers
            .retain(|peer| accepted.contains(&peer.machine_id));
        let Some((planned, set)) = snapshot_peers(
            &self.machine.id,
            snapshot,
            &self.bootstrap_peers,
            record.phase() == LocalMachinePhase::Joining,
            held_another_machine,
        ) else {
            return Ok(true);
        };
        let host = self.wireguard.read_interface_data()?;
        let answered = match set {
            PeerSet::Bootstrap => None,
            PeerSet::Snapshot => {
                let mut answered = snapshot
                    .observations
                    .iter()
                    .map(|machine| machine.id)
                    .collect::<BTreeSet<_>>();
                let unlisted = record.unlisted_trusted_machines(
                    |key| host.peers.contains_key(&Key::new(key.0)),
                    &answered,
                );
                match replicated.removed_machines(&unlisted).await {
                    Ok(removed) => answered.extend(removed),
                    Err(error) => {
                        eprintln!(
                            "failed to read which Machines this Corrosion store removed: {error}",
                            error = ployz_core::error_chain::inline(&error),
                        );
                        return Ok(false);
                    }
                }
                Some(answered)
            }
        };
        let selected = &record.selected_endpoints;
        let now = SystemTime::now();
        let mut previous = std::mem::take(&mut self.endpoint_peers);
        seed_retained_selections(&planned, &mut previous, &host.peers, now);
        let (planned, _) = attach_peer_selections(planned, previous, selected, now);
        let settled = self.apply_peers(&planned, set, &record, answered.as_ref())?;
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

    pub(super) fn configure_device(
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

    /// Whether the plan settled, as [`PeerChanges::settled`] says.
    pub(super) fn apply_peers(
        &mut self,
        peers: &[MeshPeer],
        set: PeerSet,
        record: &LocalMachineRecord,
        answered: Option<&BTreeSet<MachineId>>,
    ) -> Result<bool, NetworkError> {
        let wg_peers = peers
            .iter()
            .map(|peer| wireguard_peer(peer, peer.selected()))
            .collect::<Vec<_>>();
        let host = self.wireguard.read_interface_data()?;
        let changes = peer_changes(&host.peers, &wg_peers, set, |key| {
            record.keeps_unlisted_peer(&WireGuardPublicKey(key.as_array()), answered)
        })?;
        for peer in changes.updates {
            self.wireguard.configure_peer(peer)?;
        }
        let gateway = self.machine.subnet.gateway().0.to_string();
        let route_owners = self.route_owners_of(&changes.routes, peers);
        self.lowered
            .retain(|machine_id, lowered| owns_routes(&route_owners, *machine_id, &lowered.routes));
        self.paths.retain(|machine_id| {
            self.lowered.contains_key(machine_id)
                || peers.iter().any(|peer| peer.machine_id == *machine_id)
        });
        for route in &changes.routes {
            let mtu = self
                .lowered
                .values()
                .find(|lowered| lowered.routes.contains(route))
                .map(|lowered| lowered.mtu);
            replace_route(*route, route.addr().is_ipv4().then_some(&gateway), mtu)?;
        }
        for key in changes.removals {
            self.wireguard.remove_peer(key)?;
        }
        for route in self.routes.difference(&changes.routes) {
            delete_route(route);
        }
        self.routes = changes.routes;
        self.probed.send_replace(
            peers
                .iter()
                .map(|peer| ProbedPeer {
                    machine_id: peer.machine_id,
                    name: peer.name.clone(),
                    address: ployz_core::management_address(peer.public_key).0,
                })
                .collect(),
        );
        Ok(changes.settled)
    }

    pub(super) async fn poll_endpoints(&mut self, local: &RecordOwner) {
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

    pub(super) async fn apply_choice(&mut self, choice: EndpointChoice, local: &RecordOwner) {
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
}
