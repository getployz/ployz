#[cfg(target_os = "linux")]
use defguard_wireguard_rs::Kernel;
#[cfg(not(target_os = "linux"))]
use defguard_wireguard_rs::Userspace;
use defguard_wireguard_rs::{
    InterfaceConfiguration, WGApi, WireguardInterfaceApi, key::Key, net::IpAddrMask, peer::Peer,
};
#[cfg(target_os = "linux")]
use futures_util::TryStreamExt;
use ipnet::Ipv4Net;
use ployz_core::dataplane::{WireGuardPeer, WireGuardPublicKey};
#[cfg(target_os = "linux")]
use rtnetlink::{
    LinkUnspec, RouteMessageBuilder,
    packet_route::{
        link::LinkAttribute,
        route::{RouteAttribute, RouteMetric},
    },
};
use std::io::Write;
use std::net::Ipv4Addr;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;

#[cfg(target_os = "linux")]
pub(super) type HostWireGuardApi = Kernel;
#[cfg(not(target_os = "linux"))]
pub(super) type HostWireGuardApi = Userspace;

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

pub(super) async fn detect_wireguard_mtu(wg_ifname: &str) -> Result<WireGuardMtu, String> {
    detect_wireguard_mtu_platform(wg_ifname).await
}

#[cfg(target_os = "linux")]
async fn detect_wireguard_mtu_platform(wg_ifname: &str) -> Result<WireGuardMtu, String> {
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
    let route_mtu = route.attributes.iter().find_map(|attribute| {
        if let RouteAttribute::Metrics(metrics) = attribute {
            return metrics.iter().find_map(|metric| {
                if let RouteMetric::Mtu(mtu) = metric {
                    Some(*mtu)
                } else {
                    None
                }
            });
        }
        None
    });
    let ifindex = route
        .attributes
        .iter()
        .find_map(|attribute| {
            if let RouteAttribute::Oif(ifindex) = attribute {
                Some(*ifindex)
            } else {
                None
            }
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
        if let LinkAttribute::IfName(value) = attribute {
            link_name = Some(value.as_str());
        } else if let LinkAttribute::Mtu(value) = attribute {
            link_mtu = Some(*value);
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

#[cfg(not(target_os = "linux"))]
async fn detect_wireguard_mtu_platform(wg_ifname: &str) -> Result<WireGuardMtu, String> {
    let local_addr = getifs::best_local_ipv4_addrs()
        .map_err(|source| source.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "no default IPv4 route".to_owned())?;
    let interface = getifs::interface_by_index(local_addr.index())
        .map_err(|source| source.to_string())?
        .ok_or_else(|| format!("output interface {} was not found", local_addr.index()))?;
    if interface.name().as_str() == wg_ifname {
        return Err(format!(
            "egress interface is the WireGuard interface {wg_ifname}"
        ));
    }
    Ok(wireguard_mtu_from_egress(interface.mtu()))
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
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|source| {
            format!(
                "create WireGuard key directory {}: {source}",
                parent.display()
            )
        })?;
    let private_key = Key::generate().to_string();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| format!("create WireGuard private key {}: {source}", path.display()))?;
    file.write_all(format!("{private_key}\n").as_bytes())
        .map_err(|source| format!("write WireGuard private key {}: {source}", path.display()))?;
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
    let mut api = WGApi::<HostWireGuardApi>::new(wg_ifname).map_err(|source| source.to_string())?;
    if !interface_exists(wg_ifname)? {
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
    #[cfg(target_os = "linux")]
    set_link_up(wg_ifname).await?;
    Ok(())
}

pub(super) fn read_latest_handshakes(
    wg_ifname: &str,
) -> Result<std::collections::BTreeMap<String, u64>, String> {
    let api = WGApi::<HostWireGuardApi>::new(wg_ifname).map_err(|source| source.to_string())?;
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
    let api = WGApi::<HostWireGuardApi>::new(wg_ifname).map_err(|source| source.to_string())?;
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

fn interface_exists(ifname: &str) -> Result<bool, String> {
    getifs::interfaces()
        .map(|interfaces| {
            interfaces
                .iter()
                .any(|interface| interface.name().as_str() == ifname)
        })
        .map_err(|source| source.to_string())
}

#[cfg(target_os = "linux")]
async fn set_link_up(ifname: &str) -> Result<(), String> {
    let (connection, handle, _) =
        rtnetlink::new_connection().map_err(|source| source.to_string())?;
    tokio::spawn(connection);
    handle
        .link()
        .set(LinkUnspec::new_with_name(ifname).up().build())
        .execute()
        .await
        .map_err(|source| source.to_string())
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

pub(super) fn wireguard_host_ipv4(endpoint_subnet: Ipv4Net) -> Result<Ipv4Addr, String> {
    if endpoint_subnet.prefix_len() != 24 {
        return Err(format!(
            "endpoint subnet must be an IPv4 /24 for host WireGuard addressing: {endpoint_subnet}"
        ));
    }
    if endpoint_subnet.network() != endpoint_subnet.addr() {
        return Err(format!(
            "endpoint subnet must start at the network address: {endpoint_subnet}"
        ));
    }
    let mut octets = endpoint_subnet.network().octets();
    octets[3] = 254;
    Ok(Ipv4Addr::from(octets))
}

pub(super) fn wireguard_host_cidr(endpoint_subnet: &str) -> Result<String, String> {
    let subnet = endpoint_subnet.parse::<Ipv4Net>().map_err(|source| {
        format!("endpoint subnet is not IPv4 CIDR: {endpoint_subnet}: {source}")
    })?;
    Ok(format!("{}/32", wireguard_host_ipv4(subnet)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

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
    fn ensure_private_key_creates_private_file_and_directory_modes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key_dir = dir.path().join("wireguard");
        let key_path = key_dir.join("private.key");

        let private_key = ensure_private_key(&key_path).expect("private key is created");

        assert!(!private_key.is_empty());
        assert_eq!(
            std::fs::metadata(&key_dir)
                .expect("key dir metadata")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&key_path)
                .expect("key file metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
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
