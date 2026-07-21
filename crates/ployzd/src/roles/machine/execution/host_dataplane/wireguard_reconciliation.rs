use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use defguard_wireguard_rs::{
    InterfaceConfiguration, WGApi, WireguardInterfaceApi, host::Host, key::Key, net::IpAddrMask,
    peer::Peer,
};
use futures_util::TryStreamExt;
use ipnet::Ipv4Net;
use ployz_core::ids::MachineId;
use ployz_core::network::{WireGuardEbpfEndpointRoute, WireGuardPeer};
use rtnetlink::{
    AddressMessageBuilder, LinkUnspec, RouteMessageBuilder,
    packet_route::{
        AddressFamily,
        route::{RouteAddress, RouteAttribute, RouteMessage, RouteProtocol, RouteScope, RouteType},
    },
};

use super::host_commands::{HostCommandOutcome, run_host_command};
use super::host_network::{HostWireGuardApi, wireguard_host_cidr};

pub(super) struct WireGuardReconciliation<'a> {
    pub(super) ifname: &'a str,
    pub(super) private_key: &'a str,
    pub(super) private_key_path: &'a Path,
    pub(super) listen_port: u16,
    pub(super) mtu: u32,
    pub(super) endpoint_routes: &'a [WireGuardEbpfEndpointRoute],
    pub(super) peers: &'a [WireGuardPeer],
    pub(super) local_machine_id: &'a MachineId,
    pub(super) command_timeout: Duration,
}

pub(super) async fn ensure_wireguard_interface(
    desired: WireGuardReconciliation<'_>,
) -> Result<(), String> {
    let WireGuardReconciliation {
        ifname: wg_ifname,
        private_key,
        private_key_path,
        listen_port,
        mtu,
        endpoint_routes,
        peers,
        local_machine_id,
        command_timeout,
    } = desired;
    let local_host_cidr = endpoint_routes
        .iter()
        .find(|route| route.machine_id == *local_machine_id)
        .ok_or_else(|| "local endpoint route is missing".to_owned())
        .and_then(|route| wireguard_host_cidr(&route.endpoint_subnet))?;
    let desired_peers = peers
        .iter()
        .filter(|peer| peer.machine_id != *local_machine_id)
        .map(RenderedPeer::try_from)
        .collect::<Result<Vec<_>, _>>()?;
    let wg_peers = desired_peers
        .iter()
        .map(|peer| peer.configuration.clone())
        .collect::<Vec<_>>();
    let mut api = WGApi::<HostWireGuardApi>::new(wg_ifname).map_err(|source| source.to_string())?;

    if interface_exists(wg_ifname)? {
        let observed = api
            .read_interface_data()
            .map_err(|source| format!("read WireGuard interface {wg_ifname}: {source}"))?;
        reconcile_wireguard_host(
            wg_ifname,
            &observed,
            private_key,
            private_key_path,
            listen_port,
            command_timeout,
        )
        .await?;
        reconcile_wireguard_link(wg_ifname, &local_host_cidr, mtu).await?;
        apply_peer_reconciliation(&api, peer_reconciliation(&observed, desired_peers)?)?;
    } else {
        api.create_interface()
            .map_err(|source| format!("create WireGuard interface {wg_ifname}: {source}"))?;
        let configuration = InterfaceConfiguration {
            name: wg_ifname.to_owned(),
            prvkey: private_key.to_owned(),
            addresses: vec![local_host_cidr.parse::<IpAddrMask>().map_err(|source| {
                format!("parse local WireGuard host CIDR {local_host_cidr}: {source}")
            })?],
            port: listen_port,
            peers: wg_peers.clone(),
            mtu: Some(mtu),
            fwmark: None,
        };
        api.configure_interface(&configuration)
            .map_err(|source| format!("configure new WireGuard interface {wg_ifname}: {source}"))?;
    }

    let desired_routes = desired_route_set(&wg_peers);
    remove_conflicting_desired_routes(wg_ifname, &desired_routes).await?;
    api.configure_peer_routing(&wg_peers)
        .map_err(|source| format!("add desired WireGuard peer routes: {source}"))?;
    remove_stale_peer_routes(wg_ifname, &desired_routes).await?;
    set_link_up(wg_ifname).await
}

#[derive(Debug)]
struct RenderedPeer {
    configuration: Peer,
    candidate_endpoints: Vec<SocketAddr>,
}

impl TryFrom<&WireGuardPeer> for RenderedPeer {
    type Error = String;

    fn try_from(peer: &WireGuardPeer) -> Result<Self, Self::Error> {
        let mut configuration = Peer::new(
            Key::try_from(peer.public_key.as_str())
                .map_err(|source| format!("parse WireGuard peer public key: {source}"))?,
        );
        configuration.endpoint = Some(peer.active_endpoint);
        configuration.persistent_keepalive_interval = Some(25);
        configuration.allowed_ips =
            vec![
                peer.endpoint_subnet
                    .parse::<IpAddrMask>()
                    .map_err(|source| {
                        format!(
                            "parse peer endpoint subnet {}: {source}",
                            peer.endpoint_subnet
                        )
                    })?,
            ];
        Ok(Self {
            configuration,
            candidate_endpoints: peer.candidate_endpoints.clone(),
        })
    }
}

async fn reconcile_wireguard_host(
    ifname: &str,
    observed: &Host,
    desired_private_key: &str,
    private_key_path: &Path,
    desired_port: u16,
    command_timeout: Duration,
) -> Result<(), String> {
    let desired_private_key = Key::try_from(desired_private_key)
        .map_err(|source| format!("parse WireGuard private key: {source}"))?;
    let args = host_update_args(
        ifname,
        !wireguard_private_key_matches(observed.private_key.as_ref(), &desired_private_key),
        private_key_path,
        observed.listen_port != desired_port,
        desired_port,
    );
    if args.len() == 2 {
        return Ok(());
    }
    match run_host_command("wg", &args, command_timeout).await {
        HostCommandOutcome::Success => Ok(()),
        HostCommandOutcome::TimedOut => Err(format!(
            "update WireGuard interface {ifname} timed out after {}s",
            command_timeout.as_secs()
        )),
        HostCommandOutcome::CouldNotStart(source) => Err(format!(
            "start WireGuard interface update for {ifname}: {source}"
        )),
        HostCommandOutcome::Failed(output) => Err(format!(
            "update WireGuard interface {ifname}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

fn host_update_args(
    ifname: &str,
    private_key_drifted: bool,
    private_key_path: &Path,
    listen_port_drifted: bool,
    listen_port: u16,
) -> Vec<String> {
    let mut args = vec!["set".to_owned(), ifname.to_owned()];
    if private_key_drifted {
        args.extend([
            "private-key".to_owned(),
            private_key_path.display().to_string(),
        ]);
    }
    if listen_port_drifted {
        args.extend(["listen-port".to_owned(), listen_port.to_string()]);
    }
    args
}

fn wireguard_private_key_matches(observed: Option<&Key>, desired: &Key) -> bool {
    observed.is_some_and(|observed| observed.public_key() == desired.public_key())
}

#[derive(Debug, PartialEq)]
struct PeerReconciliation {
    upserts: Vec<Peer>,
    removals: Vec<Key>,
}

fn peer_reconciliation(
    observed: &Host,
    desired: Vec<RenderedPeer>,
) -> Result<PeerReconciliation, String> {
    let mut desired_keys = HashSet::new();
    let mut upserts = Vec::new();
    for RenderedPeer {
        mut configuration,
        candidate_endpoints,
    } in desired
    {
        desired_keys.insert(configuration.public_key.clone());
        let Some(observed_peer) = observed.peers.get(&configuration.public_key) else {
            upserts.push(configuration);
            continue;
        };
        if let Some(endpoint) = observed_peer
            .endpoint
            .filter(|endpoint| candidate_endpoints.contains(endpoint))
        {
            configuration.endpoint = Some(endpoint);
        }
        if peer_configuration_matches(observed_peer, &configuration, &candidate_endpoints) {
            continue;
        }
        if configured_preshared_key(observed_peer).is_some()
            && configured_preshared_key(&configuration).is_none()
        {
            configuration.preshared_key = Some(Key::default());
        }
        upserts.push(configuration);
    }
    let removals = observed
        .peers
        .keys()
        .filter(|key| !desired_keys.contains(*key))
        .cloned()
        .collect();
    Ok(PeerReconciliation { upserts, removals })
}

fn peer_configuration_matches(
    observed: &Peer,
    desired: &Peer,
    candidate_endpoints: &[SocketAddr],
) -> bool {
    configured_preshared_key(observed) == configured_preshared_key(desired)
        && observed.persistent_keepalive_interval == desired.persistent_keepalive_interval
        && observed.allowed_ips.len() == desired.allowed_ips.len()
        && observed
            .allowed_ips
            .iter()
            .all(|allowed_ip| desired.allowed_ips.contains(allowed_ip))
        && observed.endpoint.is_some_and(|endpoint| {
            desired.endpoint == Some(endpoint) || candidate_endpoints.contains(&endpoint)
        })
}

fn configured_preshared_key(peer: &Peer) -> Option<&Key> {
    peer.preshared_key
        .as_ref()
        .filter(|key| key.as_slice().iter().any(|byte| *byte != 0))
}

fn apply_peer_reconciliation<A: WireguardInterfaceApi>(
    api: &A,
    PeerReconciliation { upserts, removals }: PeerReconciliation,
) -> Result<(), String> {
    for peer in &upserts {
        api.configure_peer(peer)
            .map_err(|source| format!("configure WireGuard peer {}: {source}", peer.public_key))?;
    }
    for public_key in &removals {
        api.remove_peer(public_key)
            .map_err(|source| format!("remove WireGuard peer {public_key}: {source}"))?;
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct LinkReconciliation {
    index: u32,
    mutations: Vec<LinkMutation>,
}

#[derive(Debug, PartialEq, Eq)]
enum LinkMutation {
    AddAddress(Ipv4Net),
    RemoveAddress(Ipv4Net),
    SetMtu(u32),
    BringUp,
}

fn link_reconciliation(
    index: u32,
    observed_addresses: &[Ipv4Net],
    observed_mtu: u32,
    observed_up: bool,
    desired_address: Ipv4Net,
    desired_mtu: u32,
) -> LinkReconciliation {
    let mut mutations = Vec::new();
    if !observed_addresses.contains(&desired_address) {
        mutations.push(LinkMutation::AddAddress(desired_address));
    }
    mutations.extend(
        observed_addresses
            .iter()
            .copied()
            .filter(|address| *address != desired_address)
            .map(LinkMutation::RemoveAddress),
    );
    if observed_mtu != desired_mtu {
        mutations.push(LinkMutation::SetMtu(desired_mtu));
    }
    if !observed_up {
        mutations.push(LinkMutation::BringUp);
    }
    LinkReconciliation { index, mutations }
}

async fn reconcile_wireguard_link(
    ifname: &str,
    local_host_cidr: &str,
    mtu: u32,
) -> Result<(), String> {
    let desired_address = local_host_cidr
        .parse::<Ipv4Net>()
        .map_err(|source| format!("parse WireGuard address {local_host_cidr}: {source}"))?;
    let interface = getifs::interfaces()
        .map_err(|source| source.to_string())?
        .into_iter()
        .find(|interface| interface.name().as_str() == ifname)
        .ok_or_else(|| format!("WireGuard interface {ifname} disappeared during reconciliation"))?;
    let addresses = interface
        .ipv4_addrs()
        .map_err(|source| format!("read WireGuard interface {ifname} addresses: {source}"))?
        .into_iter()
        .map(|address| {
            Ipv4Net::new(address.addr(), address.prefix_len()).map_err(|source| {
                format!("read WireGuard interface {ifname} address {address}: {source}")
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    apply_link_reconciliation(link_reconciliation(
        interface.index(),
        &addresses,
        interface.mtu(),
        interface.flags().contains(getifs::Flags::UP),
        desired_address,
        mtu,
    ))
    .await
}

async fn apply_link_reconciliation(
    LinkReconciliation { index, mutations }: LinkReconciliation,
) -> Result<(), String> {
    let (connection, handle, _) =
        rtnetlink::new_connection().map_err(|source| source.to_string())?;
    tokio::spawn(connection);
    for mutation in mutations {
        match mutation {
            LinkMutation::AddAddress(address) => handle
                .address()
                .add(index, IpAddr::V4(address.addr()), address.prefix_len())
                .execute()
                .await
                .map_err(|source| format!("add WireGuard address {address}: {source}"))?,
            LinkMutation::RemoveAddress(address) => handle
                .address()
                .del(
                    AddressMessageBuilder::<Ipv4Addr>::new()
                        .index(index)
                        .address(address.addr(), address.prefix_len())
                        .build(),
                )
                .execute()
                .await
                .map_err(|source| format!("remove stale WireGuard address {address}: {source}"))?,
            LinkMutation::SetMtu(mtu) => handle
                .link()
                .set(LinkUnspec::new_with_index(index).mtu(mtu).build())
                .execute()
                .await
                .map_err(|source| format!("set WireGuard link {index} MTU {mtu}: {source}"))?,
            LinkMutation::BringUp => handle
                .link()
                .set(LinkUnspec::new_with_index(index).up().build())
                .execute()
                .await
                .map_err(|source| format!("bring WireGuard link {index} up: {source}"))?,
        }
    }
    Ok(())
}

fn desired_route_set(peers: &[Peer]) -> HashSet<Ipv4Net> {
    peers
        .iter()
        .flat_map(|peer| &peer.allowed_ips)
        .filter_map(|route| match route.address {
            IpAddr::V4(address) => Ipv4Net::new(address, route.cidr).ok(),
            IpAddr::V6(_) => None,
        })
        .collect()
}

async fn remove_stale_peer_routes(
    ifname: &str,
    desired_routes: &HashSet<Ipv4Net>,
) -> Result<(), String> {
    let ifindex = wireguard_ifindex(ifname)?;
    let (connection, handle, _) =
        rtnetlink::new_connection().map_err(|source| source.to_string())?;
    tokio::spawn(connection);
    let mut stale = Vec::new();
    for message in read_ipv4_routes(&handle, ifname).await? {
        if let Some(subnet) = owned_wireguard_route(&message, ifindex)
            && !desired_routes.contains(&subnet)
        {
            stale.push((subnet, message));
        }
    }
    remove_observed_routes(&handle, stale, "stale WireGuard").await
}

async fn remove_conflicting_desired_routes(
    ifname: &str,
    desired_routes: &HashSet<Ipv4Net>,
) -> Result<(), String> {
    if desired_routes.is_empty() {
        return Ok(());
    }
    let ifindex = wireguard_ifindex(ifname)?;
    let (connection, handle, _) =
        rtnetlink::new_connection().map_err(|source| source.to_string())?;
    tokio::spawn(connection);
    let conflicts = read_ipv4_routes(&handle, ifname)
        .await?
        .into_iter()
        .filter_map(|message| {
            conflicting_desired_route(&message, ifindex, desired_routes)
                .map(|subnet| (subnet, message))
        })
        .collect();
    remove_observed_routes(&handle, conflicts, "conflicting desired WireGuard").await
}

async fn read_ipv4_routes(
    handle: &rtnetlink::Handle,
    ifname: &str,
) -> Result<Vec<RouteMessage>, String> {
    handle
        .route()
        .get(RouteMessageBuilder::<Ipv4Addr>::new().build())
        .execute()
        .try_collect()
        .await
        .map_err(|source| format!("read WireGuard routes for {ifname}: {source}"))
}

async fn remove_observed_routes(
    handle: &rtnetlink::Handle,
    routes: Vec<(Ipv4Net, RouteMessage)>,
    description: &str,
) -> Result<(), String> {
    for (subnet, message) in routes {
        match handle.route().del(message).execute().await {
            Ok(()) => {}
            Err(rtnetlink::Error::NetlinkError(error))
                if error.code.is_some_and(|code| code.get() == -3) => {}
            Err(source) => {
                return Err(format!("remove {description} route {subnet}: {source}"));
            }
        }
    }
    Ok(())
}

fn conflicting_desired_route(
    message: &RouteMessage,
    ifindex: u32,
    desired_routes: &HashSet<Ipv4Net>,
) -> Option<Ipv4Net> {
    if effective_route_table(message) != 254 {
        return None;
    }
    let subnet = ipv4_route_subnet(message)?;
    if !desired_routes.contains(&subnet) || owned_wireguard_route(message, ifindex) == Some(subnet)
    {
        return None;
    }
    Some(subnet)
}

fn owned_wireguard_route(message: &RouteMessage, ifindex: u32) -> Option<Ipv4Net> {
    let header = &message.header;
    if header.address_family != AddressFamily::Inet
        || header.protocol != RouteProtocol::Boot
        || header.scope != RouteScope::Link
        || header.kind != RouteType::Unicast
    {
        return None;
    }
    let mut output_interface = None;
    for attribute in &message.attributes {
        if matches!(
            attribute,
            RouteAttribute::Gateway(_) | RouteAttribute::Via(_) | RouteAttribute::MultiPath(_)
        ) {
            return None;
        }
        if let RouteAttribute::Oif(index) = attribute {
            output_interface = Some(*index);
        }
    }
    if effective_route_table(message) != 254 || output_interface != Some(ifindex) {
        return None;
    }
    ipv4_route_subnet(message)
}

fn effective_route_table(message: &RouteMessage) -> u32 {
    let Some(table) = message
        .attributes
        .iter()
        .filter_map(|attribute| {
            if let RouteAttribute::Table(table) = attribute {
                Some(*table)
            } else {
                None
            }
        })
        .next_back()
    else {
        return u32::from(message.header.table);
    };
    table
}

fn ipv4_route_subnet(message: &RouteMessage) -> Option<Ipv4Net> {
    let destination = message.attributes.iter().find_map(|attribute| {
        if let RouteAttribute::Destination(RouteAddress::Inet(address)) = attribute {
            Some(*address)
        } else {
            None
        }
    })?;
    Ipv4Net::new(destination, message.header.destination_prefix_length).ok()
}

fn wireguard_ifindex(ifname: &str) -> Result<u32, String> {
    getifs::interfaces()
        .map_err(|source| source.to_string())?
        .into_iter()
        .find(|interface| interface.name().as_str() == ifname)
        .map(|interface| interface.index())
        .ok_or_else(|| format!("WireGuard interface {ifname} disappeared during route cleanup"))
}

fn interface_exists(ifname: &str) -> Result<bool, String> {
    getifs::interfaces()
        .map(|interfaces| {
            interfaces
                .iter()
                .any(|interface| interface.name().as_str() == ifname)
        })
        .map_err(|source| source.to_string())
}

async fn set_link_up(ifname: &str) -> Result<(), String> {
    let (connection, handle, _) =
        rtnetlink::new_connection().map_err(|source| source.to_string())?;
    tokio::spawn(connection);
    handle
        .link()
        .set(LinkUnspec::new_with_name(ifname).up().build())
        .execute()
        .await
        .map_err(|source| format!("bring WireGuard interface {ifname} up: {source}"))
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::time::{Duration, SystemTime};

    use defguard_wireguard_rs::error::WireguardInterfaceError;

    use super::*;
    use ployz_core::network::WireGuardPublicKey;

    #[test]
    fn unchanged_peer_runtime_evidence_requires_no_mutation() {
        let desired = desired_peer("10.42.2.0/24", 51820);
        let mut observed_peer = rendered(&desired);
        observed_peer.last_handshake = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(42));
        observed_peer.rx_bytes = 100;
        observed_peer.tx_bytes = 200;
        observed_peer.protocol_version = Some(1);
        observed_peer.preshared_key = Some(Key::default());
        let observed = host_with_peer(observed_peer);

        let plan = peer_reconciliation(
            &observed,
            vec![RenderedPeer::try_from(&desired).expect("peer renders")],
        )
        .expect("plan succeeds");

        assert_eq!(
            plan,
            PeerReconciliation {
                upserts: Vec::new(),
                removals: Vec::new(),
            }
        );
    }

    #[test]
    fn psk_clear_is_an_in_place_upsert() {
        let desired = desired_peer("10.42.2.0/24", 51820);
        let mut observed_peer = rendered(&desired);
        observed_peer.preshared_key = Some(Key::generate());
        let plan = peer_reconciliation(
            &host_with_peer(observed_peer),
            vec![RenderedPeer::try_from(&desired).expect("peer renders")],
        )
        .expect("plan succeeds");
        let api = RecordingWireGuardApi::default();

        apply_peer_reconciliation(&api, plan).expect("execution succeeds");

        assert_eq!(api.actions.into_inner(), vec![PeerAction::ClearPsk]);
    }

    #[test]
    fn desired_peers_are_applied_before_rogue_peers_are_removed() {
        let desired = desired_peer("10.42.2.0/24", 51820);
        let rogue_key = Key::generate();
        let plan = peer_reconciliation(
            &host_with_peer(Peer::new(rogue_key)),
            vec![RenderedPeer::try_from(&desired).expect("peer renders")],
        )
        .expect("plan succeeds");
        let api = RecordingWireGuardApi::default();

        apply_peer_reconciliation(&api, plan).expect("execution succeeds");

        assert_eq!(
            api.actions.into_inner(),
            vec![PeerAction::Configure, PeerAction::Remove]
        );
    }

    #[test]
    fn host_update_args_touch_only_drifted_fields() {
        let path = Path::new("/etc/ployz/wireguard.key");
        assert_eq!(
            host_update_args("wg0", true, path, false, 51820),
            ["set", "wg0", "private-key", "/etc/ployz/wireguard.key"]
        );
        assert_eq!(
            host_update_args("wg0", false, path, true, 51820),
            ["set", "wg0", "listen-port", "51820"]
        );
        assert_eq!(
            host_update_args("wg0", true, path, true, 51820),
            [
                "set",
                "wg0",
                "private-key",
                "/etc/ployz/wireguard.key",
                "listen-port",
                "51820"
            ]
        );
    }

    #[test]
    fn kernel_normalized_private_key_keeps_the_same_effective_identity() {
        let mut desired_bytes = Key::generate().as_array();
        let [first, middle @ .., last] = &mut desired_bytes;
        let _ = middle;
        *first |= 7;
        *last |= 128;
        *last &= !64;
        let desired = Key::new(desired_bytes);
        let mut normalized_bytes = desired.as_array();
        let [first, middle @ .., last] = &mut normalized_bytes;
        let _ = middle;
        *first &= 248;
        *last &= 127;
        *last |= 64;
        let normalized = Key::new(normalized_bytes);

        assert_ne!(normalized, desired);
        assert!(wireguard_private_key_matches(Some(&normalized), &desired));
    }

    #[test]
    fn link_planner_uses_self_contained_mutations() {
        let desired = "10.42.1.1/24".parse::<Ipv4Net>().expect("desired address");
        let stale = "10.42.9.1/24".parse::<Ipv4Net>().expect("stale address");

        let plan = link_reconciliation(7, &[stale], 1280, false, desired, 1420);

        assert_eq!(
            plan,
            LinkReconciliation {
                index: 7,
                mutations: vec![
                    LinkMutation::AddAddress(desired),
                    LinkMutation::RemoveAddress(stale),
                    LinkMutation::SetMtu(1420),
                    LinkMutation::BringUp,
                ],
            }
        );
    }

    #[test]
    fn route_cleanup_is_independent_of_peer_drift_and_retries_from_observation() {
        let desired = HashSet::from(["10.42.2.0/24".parse::<Ipv4Net>().expect("desired")]);
        let observed = [
            "10.42.2.0/24".parse::<Ipv4Net>().expect("desired"),
            "192.0.2.255/32".parse::<Ipv4Net>().expect("rogue"),
        ];
        let stale = || {
            observed
                .iter()
                .copied()
                .filter(|route| !desired.contains(route))
                .collect::<Vec<_>>()
        };

        assert_eq!(stale(), ["192.0.2.255/32".parse().expect("rogue")]);
        assert_eq!(stale(), ["192.0.2.255/32".parse().expect("retry")]);
    }

    #[test]
    fn desired_route_conflicts_are_removed_without_touching_correct_or_isolated_routes() {
        let desired_subnet = "10.42.2.0/24".parse::<Ipv4Net>().expect("desired");
        let desired = HashSet::from([desired_subnet]);
        let correct = owned_route_message(7, desired_subnet);
        assert_eq!(conflicting_desired_route(&correct, 7, &desired), None);

        let mut wrong_interface = correct.clone();
        wrong_interface
            .attributes
            .retain(|attribute| !matches!(attribute, RouteAttribute::Oif(_)));
        wrong_interface.attributes.push(RouteAttribute::Oif(8));
        assert_eq!(
            conflicting_desired_route(&wrong_interface, 7, &desired),
            Some(desired_subnet)
        );

        let mut wrong_protocol = correct.clone();
        wrong_protocol.header.protocol = RouteProtocol::Static;
        assert_eq!(
            conflicting_desired_route(&wrong_protocol, 7, &desired),
            Some(desired_subnet)
        );

        let mut gateway = correct.clone();
        gateway
            .attributes
            .push(RouteAttribute::Gateway(RouteAddress::Inet(Ipv4Addr::new(
                192, 0, 2, 1,
            ))));
        assert_eq!(
            conflicting_desired_route(&gateway, 7, &desired),
            Some(desired_subnet)
        );

        let mut non_main_header = wrong_interface.clone();
        non_main_header.header.table = 253;
        assert_eq!(
            conflicting_desired_route(&non_main_header, 7, &desired),
            None
        );

        let mut non_main_attribute = wrong_interface;
        non_main_attribute
            .attributes
            .push(RouteAttribute::Table(10_001));
        assert_eq!(
            conflicting_desired_route(&non_main_attribute, 7, &desired),
            None
        );

        let unrelated = owned_route_message(8, "10.42.9.0/24".parse().expect("unrelated"));
        assert_eq!(conflicting_desired_route(&unrelated, 7, &desired), None);
    }

    #[test]
    fn failed_conflict_deletion_is_rediscovered_from_the_next_route_dump() {
        let subnet = "10.42.2.0/24".parse::<Ipv4Net>().expect("desired");
        let desired = HashSet::from([subnet]);
        let mut conflict = owned_route_message(8, subnet);
        conflict.header.protocol = RouteProtocol::Static;

        assert_eq!(
            conflicting_desired_route(&conflict, 7, &desired),
            Some(subnet)
        );
        assert_eq!(
            conflicting_desired_route(&conflict, 7, &desired),
            Some(subnet)
        );
    }

    #[test]
    fn owned_route_classifier_requires_exact_ployz_route_shape() {
        let route = owned_route_message(7, "10.42.2.0/24".parse().expect("route"));
        assert_eq!(
            owned_wireguard_route(&route, 7),
            Some("10.42.2.0/24".parse().expect("route"))
        );

        let mut wrong_interface = route.clone();
        wrong_interface
            .attributes
            .retain(|attribute| !matches!(attribute, RouteAttribute::Oif(_)));
        wrong_interface.attributes.push(RouteAttribute::Oif(8));
        assert_eq!(owned_wireguard_route(&wrong_interface, 7), None);

        let mut wrong_table = route.clone();
        wrong_table.header.table = 253;
        assert_eq!(owned_wireguard_route(&wrong_table, 7), None);

        let mut wrong_protocol = route.clone();
        wrong_protocol.header.protocol = RouteProtocol::Static;
        assert_eq!(owned_wireguard_route(&wrong_protocol, 7), None);

        let mut wrong_scope = route.clone();
        wrong_scope.header.scope = RouteScope::Universe;
        assert_eq!(owned_wireguard_route(&wrong_scope, 7), None);

        let mut wrong_kind = route.clone();
        wrong_kind.header.kind = RouteType::BlackHole;
        assert_eq!(owned_wireguard_route(&wrong_kind, 7), None);

        let mut gateway = route;
        gateway
            .attributes
            .push(RouteAttribute::Gateway(RouteAddress::Inet(Ipv4Addr::new(
                192, 0, 2, 1,
            ))));
        assert_eq!(owned_wireguard_route(&gateway, 7), None);
    }

    fn owned_route_message(ifindex: u32, subnet: Ipv4Net) -> RouteMessage {
        RouteMessageBuilder::<Ipv4Addr>::new()
            .destination_prefix(subnet.network(), subnet.prefix_len())
            .output_interface(ifindex)
            .protocol(RouteProtocol::Boot)
            .scope(RouteScope::Link)
            .build()
    }

    #[derive(Debug, PartialEq, Eq)]
    enum PeerAction {
        Configure,
        ClearPsk,
        Remove,
    }

    #[derive(Default)]
    struct RecordingWireGuardApi {
        actions: RefCell<Vec<PeerAction>>,
    }

    impl WireguardInterfaceApi for RecordingWireGuardApi {
        fn create_interface(&mut self) -> Result<(), WireguardInterfaceError> {
            Ok(())
        }
        fn assign_address(&self, _address: &IpAddrMask) -> Result<(), WireguardInterfaceError> {
            Ok(())
        }
        fn configure_peer_routing(&self, _peers: &[Peer]) -> Result<(), WireguardInterfaceError> {
            Ok(())
        }
        fn configure_interface(
            &self,
            _config: &InterfaceConfiguration,
        ) -> Result<(), WireguardInterfaceError> {
            Ok(())
        }
        fn remove_interface(&self) -> Result<(), WireguardInterfaceError> {
            Ok(())
        }
        fn configure_peer(&self, peer: &Peer) -> Result<(), WireguardInterfaceError> {
            let action = if peer
                .preshared_key
                .as_ref()
                .is_some_and(|key| key.as_slice().iter().all(|byte| *byte == 0))
            {
                PeerAction::ClearPsk
            } else {
                PeerAction::Configure
            };
            self.actions.borrow_mut().push(action);
            Ok(())
        }
        fn remove_peer(&self, _peer_pubkey: &Key) -> Result<(), WireguardInterfaceError> {
            self.actions.borrow_mut().push(PeerAction::Remove);
            Ok(())
        }
        fn read_interface_data(&self) -> Result<Host, WireguardInterfaceError> {
            Ok(Host::new(51820, Key::generate()))
        }
        fn configure_dns(
            &self,
            _dns: &[IpAddr],
            _search_domains: &[&str],
        ) -> Result<(), WireguardInterfaceError> {
            Ok(())
        }
    }

    fn desired_peer(endpoint_subnet: &str, port: u16) -> WireGuardPeer {
        let active_endpoint = format!("192.0.2.1:{port}").parse().expect("endpoint");
        WireGuardPeer {
            machine_id: MachineId::try_new("edge").expect("machine id is valid"),
            endpoint_subnet: endpoint_subnet.to_owned(),
            active_endpoint,
            candidate_endpoints: vec![active_endpoint],
            public_key: WireGuardPublicKey::try_new(Key::generate().to_string())
                .expect("public key is valid"),
        }
    }

    fn rendered(peer: &WireGuardPeer) -> Peer {
        RenderedPeer::try_from(peer)
            .expect("peer renders")
            .configuration
    }

    fn host_with_peer(peer: Peer) -> Host {
        let mut host = Host::new(
            ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT,
            Key::generate(),
        );
        host.peers.insert(peer.public_key.clone(), peer);
        host
    }
}
