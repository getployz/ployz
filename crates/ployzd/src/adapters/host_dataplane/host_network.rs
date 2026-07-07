use defguard_wireguard_rs::{
    InterfaceConfiguration, Kernel, WGApi, WireguardInterfaceApi, key::Key, net::IpAddrMask,
    peer::Peer,
};
use futures_util::TryStreamExt;
use ipnet::Ipv4Net;
use ployz_core::dataplane::{WireGuardPeer, WireGuardPublicKey};
use rtnetlink::{
    RouteMessageBuilder,
    packet_route::{
        link::LinkAttribute,
        route::{RouteAttribute, RouteMetric},
    },
};
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub(super) const MIN_WIREGUARD_MTU: u32 = 1280;
pub(super) const MAX_WIREGUARD_MTU: u32 = 1500 - WIREGUARD_ENCAP_OVERHEAD;
const WIREGUARD_ENCAP_OVERHEAD: u32 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireGuardMtu(u32);

impl WireGuardMtu {
    pub(crate) fn new(value: u32) -> Result<Self, String> {
        if !(MIN_WIREGUARD_MTU..=MAX_WIREGUARD_MTU).contains(&value) {
            return Err(format!(
                "wireguard MTU {value} is outside {MIN_WIREGUARD_MTU}..={MAX_WIREGUARD_MTU}"
            ));
        }
        Ok(Self(value))
    }

    pub(crate) const fn fallback() -> Self {
        Self(MAX_WIREGUARD_MTU)
    }

    pub(crate) const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireGuardMtuPolicy {
    Auto,
    Fixed(u32),
}

impl WireGuardMtuPolicy {
    pub(crate) fn from_config(value: Option<u32>) -> Result<Self, String> {
        value.map_or(Ok(Self::Auto), |mtu| {
            WireGuardMtu::new(mtu).map(|validated| Self::Fixed(validated.get()))
        })
    }
}

pub(crate) async fn resolve_wireguard_mtu(policy: WireGuardMtuPolicy, wg_ifname: &str) -> u32 {
    match policy {
        WireGuardMtuPolicy::Fixed(mtu) => mtu,
        WireGuardMtuPolicy::Auto => detect_wireguard_mtu(wg_ifname)
            .await
            .unwrap_or_else(|_| WireGuardMtu::fallback())
            .get(),
    }
}

async fn detect_wireguard_mtu(wg_ifname: &str) -> Result<WireGuardMtu, String> {
    let (connection, handle, _) =
        rtnetlink::new_connection().map_err(|source| source.to_string())?;
    tokio::spawn(connection);

    let route = RouteMessageBuilder::<Ipv4Addr>::new()
        .destination_prefix(Ipv4Addr::new(1, 1, 1, 1), 32)
        .build();
    let mut routes = handle.route().get(route).execute();
    let route = routes
        .try_next()
        .await
        .map_err(|source| source.to_string())?
        .ok_or_else(|| "no route to public address".to_owned())?;
    let route_mtu = route
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            RouteAttribute::Metrics(metrics) => metrics.iter().find_map(|metric| match metric {
                RouteMetric::Mtu(mtu) => Some(*mtu),
                _ => None,
            }),
            _ => None,
        });
    let ifindex = route
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            RouteAttribute::Oif(ifindex) => Some(*ifindex),
            _ => None,
        })
        .ok_or_else(|| "route to public address had no output interface".to_owned())?;
    let link = handle
        .link()
        .get()
        .match_index(ifindex)
        .execute()
        .try_next()
        .await
        .map_err(|source| source.to_string())?
        .ok_or_else(|| format!("output interface {ifindex} was not found"))?;
    let mut link_name = None;
    let mut link_mtu = None;
    for attribute in &link.attributes {
        match attribute {
            LinkAttribute::IfName(value) => link_name = Some(value.as_str()),
            LinkAttribute::Mtu(value) => link_mtu = Some(*value),
            _ => {}
        }
    }
    if link_name == Some(wg_ifname) {
        return Err(format!(
            "egress interface is the WireGuard interface {wg_ifname}"
        ));
    }
    let egress_mtu = route_mtu
        .or(link_mtu)
        .ok_or_else(|| "egress interface MTU was not reported".to_owned())?;
    Ok(wireguard_mtu_from_egress(egress_mtu))
}

pub(super) fn wireguard_mtu_from_egress(egress_mtu: u32) -> WireGuardMtu {
    WireGuardMtu(
        egress_mtu
            .saturating_sub(WIREGUARD_ENCAP_OVERHEAD)
            .clamp(MIN_WIREGUARD_MTU, MAX_WIREGUARD_MTU),
    )
}

pub(super) fn ensure_private_key(path: &Path) -> Result<String, String> {
    if path.exists() {
        return std::fs::read_to_string(path)
            .map(|value| value.trim().to_owned())
            .map_err(|source| format!("read WireGuard private key {}: {source}", path.display()));
    }
    let Some(parent) = path.parent() else {
        return Err(format!(
            "WireGuard private key path has no parent: {}",
            path.display()
        ));
    };
    std::fs::create_dir_all(parent).map_err(|source| {
        format!(
            "create WireGuard key directory {}: {source}",
            parent.display()
        )
    })?;
    let private_key = Key::generate().to_string();
    std::fs::write(path, format!("{private_key}\n"))
        .map_err(|source| format!("write WireGuard private key {}: {source}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        format!(
            "set WireGuard private key permissions {}: {source}",
            path.display()
        )
    })?;
    Ok(private_key)
}

pub(super) fn public_key_from_private_key(private_key: &str) -> Result<WireGuardPublicKey, String> {
    let private_key = Key::try_from(private_key)
        .map_err(|source| format!("parse WireGuard private key: {source}"))?;
    WireGuardPublicKey::try_new(private_key.public_key().to_string())
        .map_err(|source| format!("derive WireGuard public key: {source}"))
}

pub(super) async fn ensure_wireguard_interface(
    wg_ifname: &str,
    private_key: &str,
    listen_port: u16,
    mtu: u32,
    endpoint_routes: &[ployz_core::dataplane::WireGuardEbpfEndpointRoute],
    peers: &[WireGuardPeer],
    local_machine_id: &ployz_core::ids::MachineId,
) -> Result<(), String> {
    let local_host_cidr = endpoint_routes
        .iter()
        .find(|route| route.machine_id == *local_machine_id)
        .ok_or_else(|| "local endpoint route is missing".to_owned())
        .and_then(|route| wireguard_host_cidr(&route.endpoint_subnet))?;
    let mut api = WGApi::<Kernel>::new(wg_ifname).map_err(|source| source.to_string())?;
    if link_index(wg_ifname).await?.is_none() {
        api.create_interface()
            .map_err(|source| source.to_string())?;
    }
    let wg_peers = peers
        .iter()
        .filter(|peer| peer.machine_id != *local_machine_id)
        .map(to_defguard_peer)
        .collect::<Result<Vec<_>, _>>()?;
    api.configure_interface(&InterfaceConfiguration {
        name: wg_ifname.to_owned(),
        prvkey: private_key.to_owned(),
        addresses: vec![local_host_cidr.parse::<IpAddrMask>().map_err(|source| {
            format!("parse local WireGuard host CIDR {local_host_cidr}: {source}")
        })?],
        port: listen_port,
        peers: wg_peers.clone(),
        mtu: Some(mtu),
        fwmark: None,
    })
    .map_err(|source| source.to_string())?;
    api.configure_peer_routing(&wg_peers)
        .map_err(|source| source.to_string())?;
    Ok(())
}

pub(super) fn read_latest_handshakes(
    wg_ifname: &str,
) -> Result<std::collections::BTreeMap<String, u64>, String> {
    let api = WGApi::<Kernel>::new(wg_ifname).map_err(|source| source.to_string())?;
    let host = api
        .read_interface_data()
        .map_err(|source| source.to_string())?;
    Ok(host
        .peers
        .into_iter()
        .map(|(key, peer)| {
            let timestamp = peer
                .last_handshake
                .and_then(|handshake| handshake.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_secs())
                .unwrap_or_default();
            (key.to_string(), timestamp)
        })
        .collect())
}

pub(super) fn configure_peer_endpoint(
    wg_ifname: &str,
    public_key: &str,
    endpoint: std::net::SocketAddr,
    endpoint_subnet: &str,
) -> Result<(), String> {
    let api = WGApi::<Kernel>::new(wg_ifname).map_err(|source| source.to_string())?;
    let mut peer = Peer::new(
        Key::try_from(public_key)
            .map_err(|source| format!("parse WireGuard peer public key: {source}"))?,
    );
    peer.endpoint = Some(endpoint);
    peer.persistent_keepalive_interval = Some(25);
    peer.allowed_ips = vec![
        endpoint_subnet
            .parse::<IpAddrMask>()
            .map_err(|source| format!("parse peer endpoint subnet {endpoint_subnet}: {source}"))?,
    ];
    api.configure_peer(&peer)
        .map_err(|source| source.to_string())
}

async fn link_index(ifname: &str) -> Result<Option<u32>, String> {
    let (connection, handle, _) =
        rtnetlink::new_connection().map_err(|source| source.to_string())?;
    tokio::spawn(connection);
    let link = handle
        .link()
        .get()
        .match_name(ifname.to_owned())
        .execute()
        .try_next()
        .await
        .map_err(|source| source.to_string())?;
    Ok(link.map(|message| message.header.index))
}

fn to_defguard_peer(peer: &WireGuardPeer) -> Result<Peer, String> {
    let mut wg_peer = Peer::new(
        Key::try_from(peer.public_key.as_str())
            .map_err(|source| format!("parse WireGuard peer public key: {source}"))?,
    );
    wg_peer.endpoint = Some(peer.active_endpoint);
    wg_peer.persistent_keepalive_interval = Some(25);
    wg_peer.allowed_ips = vec![
        peer.endpoint_subnet
            .parse::<IpAddrMask>()
            .map_err(|source| {
                format!(
                    "parse peer endpoint subnet {}: {source}",
                    peer.endpoint_subnet
                )
            })?,
    ];
    Ok(wg_peer)
}

pub(super) fn wireguard_host_cidr(endpoint_subnet: &str) -> Result<String, String> {
    let subnet = endpoint_subnet.parse::<Ipv4Net>().map_err(|source| {
        format!("endpoint subnet is not IPv4 CIDR: {endpoint_subnet}: {source}")
    })?;
    if subnet.prefix_len() != 24 {
        return Err(format!(
            "endpoint subnet must be an IPv4 /24 for host WireGuard addressing: {endpoint_subnet}"
        ));
    }
    if subnet.network() != subnet.addr() {
        return Err(format!(
            "endpoint subnet must start at the network address: {endpoint_subnet}"
        ));
    }
    let mut octets = subnet.network().octets();
    octets[3] = 254;
    Ok(format!("{}/32", Ipv4Addr::from(octets)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wireguard_mtu_from_egress_subtracts_overhead_and_clamps() {
        assert_eq!(wireguard_mtu_from_egress(1500).get(), 1420);
        assert_eq!(wireguard_mtu_from_egress(9000).get(), 1420);
        assert_eq!(wireguard_mtu_from_egress(1300).get(), 1280);
    }

    #[test]
    fn fixed_wireguard_mtu_policy_validates_range() {
        assert_eq!(
            WireGuardMtuPolicy::from_config(Some(1420)).expect("valid mtu"),
            WireGuardMtuPolicy::Fixed(1420)
        );
        assert!(WireGuardMtuPolicy::from_config(Some(1279)).is_err());
        assert!(WireGuardMtuPolicy::from_config(Some(1421)).is_err());
    }

    #[test]
    fn wireguard_host_cidr_uses_last_host_in_endpoint_subnet() {
        assert_eq!(
            wireguard_host_cidr("10.42.7.0/24").expect("host CIDR derives"),
            "10.42.7.254/32"
        );
        assert!(wireguard_host_cidr("10.42.7.0/25").is_err());
        assert!(wireguard_host_cidr("10.42.7.12/24").is_err());
    }
}
