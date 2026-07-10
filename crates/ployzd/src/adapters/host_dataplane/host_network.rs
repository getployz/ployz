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
use ployz_core::dataplane::{
    MAX_WIREGUARD_MTU, MIN_WIREGUARD_MTU, WireGuardPeer, WireGuardPublicKey,
};
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
use std::time::Duration;
#[cfg(target_os = "linux")]
use tokio::process::Command;

#[cfg(target_os = "linux")]
pub(super) type HostWireGuardApi = Kernel;
#[cfg(not(target_os = "linux"))]
pub(super) type HostWireGuardApi = Userspace;

const WIREGUARD_ENCAP_OVERHEAD: u32 = 80;
#[cfg(target_os = "linux")]
const IPV4_ICMP_OVERHEAD: u32 = 28;
#[cfg(target_os = "linux")]
const WIREGUARD_PING_TIMEOUT: Duration = Duration::from_secs(2);

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

pub(super) async fn probe_wireguard_path_mtu(
    wg_ifname: &str,
    peer_gateway: Ipv4Addr,
    configured_mtu: u32,
) -> Result<u32, String> {
    probe_path_mtu(configured_mtu, |mtu| {
        probe_wireguard_packet(wg_ifname, peer_gateway, mtu)
    })
    .await
}

#[cfg(target_os = "linux")]
pub(super) async fn probe_wireguard_rtt(
    wg_ifname: &str,
    peer_gateway: Ipv4Addr,
) -> Result<u64, String> {
    let output = run_wireguard_ping(wg_ifname, peer_gateway, None).await?;
    if output.status.success() {
        parse_ping_rtt_micros(&String::from_utf8_lossy(&output.stdout))
            .ok_or_else(|| "peer RTT probe returned no RTT".to_owned())
    } else {
        Err(format!(
            "peer RTT probe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(not(target_os = "linux"))]
pub(super) async fn probe_wireguard_rtt(
    _wg_ifname: &str,
    _peer_gateway: Ipv4Addr,
) -> Result<u64, String> {
    Err("WireGuard RTT probing is supported only on Linux".to_owned())
}

#[cfg(any(target_os = "linux", test))]
fn parse_ping_rtt_micros(output: &str) -> Option<u64> {
    let value = output.split("time=").nth(1)?.split_whitespace().next()?;
    let millis = value.trim_end_matches("ms").parse::<f64>().ok()?;
    Some((millis * 1_000.0).round() as u64)
}

async fn probe_path_mtu<F, Fut>(configured_mtu: u32, mut probe: F) -> Result<u32, String>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = Result<bool, String>>,
{
    let mut lower = MIN_WIREGUARD_MTU;
    let mut upper = configured_mtu.clamp(MIN_WIREGUARD_MTU, MAX_WIREGUARD_MTU);
    let mut measured = None;
    while lower <= upper {
        let candidate = lower + (upper - lower) / 2;
        if probe(candidate).await? {
            measured = Some(candidate);
            lower = candidate.saturating_add(1);
        } else {
            let Some(next_upper) = candidate.checked_sub(1) else {
                break;
            };
            upper = next_upper;
        }
    }
    measured.ok_or_else(|| {
        format!("peer did not answer a {MIN_WIREGUARD_MTU}-byte WireGuard path MTU probe")
    })
}

#[cfg(target_os = "linux")]
async fn probe_wireguard_packet(
    wg_ifname: &str,
    peer_gateway: Ipv4Addr,
    mtu: u32,
) -> Result<bool, String> {
    let payload_size = mtu.saturating_sub(IPV4_ICMP_OVERHEAD);
    let output = run_wireguard_ping(wg_ifname, peer_gateway, Some(payload_size)).await?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(format!(
            "path MTU probe failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
    }
}

#[cfg(target_os = "linux")]
async fn run_wireguard_ping(
    wg_ifname: &str,
    peer_gateway: Ipv4Addr,
    payload_size: Option<u32>,
) -> Result<std::process::Output, String> {
    let mut command = Command::new("ping");
    command.args(["-4", "-I", wg_ifname, "-n", "-c", "1", "-W", "1"]);
    if let Some(payload_size) = payload_size {
        command.args(["-M", "do", "-s", &payload_size.to_string()]);
    }
    command
        .arg(peer_gateway.to_string())
        .env("LC_ALL", "C")
        .kill_on_drop(true);
    tokio::time::timeout(WIREGUARD_PING_TIMEOUT, command.output())
        .await
        .map_err(|_| {
            format!(
                "WireGuard ping timed out after {}s",
                WIREGUARD_PING_TIMEOUT.as_secs()
            )
        })?
        .map_err(|source| format!("start WireGuard ping: {source}"))
}

#[cfg(not(target_os = "linux"))]
async fn probe_wireguard_packet(
    _wg_ifname: &str,
    _peer_gateway: Ipv4Addr,
    _mtu: u32,
) -> Result<bool, String> {
    Err("WireGuard path MTU probing is supported only on Linux".to_owned())
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

    #[tokio::test]
    async fn path_mtu_probe_returns_the_largest_answering_packet() {
        let mtu = probe_path_mtu(1420, |candidate| async move { Ok(candidate <= 1360) })
            .await
            .expect("probe succeeds");

        assert_eq!(mtu, 1360);
    }

    #[tokio::test]
    async fn path_mtu_probe_reports_when_the_minimum_does_not_answer() {
        let error = probe_path_mtu(1420, |_| async { Ok(false) })
            .await
            .expect_err("minimum MTU must answer");

        assert!(error.contains("1280-byte"));
    }

    #[test]
    fn ping_rtt_parser_returns_typed_microseconds() {
        assert_eq!(
            parse_ping_rtt_micros("64 bytes from 10.198.2.254: icmp_seq=1 ttl=64 time=1.234 ms"),
            Some(1_234)
        );
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
