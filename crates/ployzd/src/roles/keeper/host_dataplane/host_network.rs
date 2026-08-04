//! Root-owned WireGuard interface and path-probing effects.

#[cfg(target_os = "linux")]
use defguard_wireguard_rs::Kernel;
#[cfg(not(target_os = "linux"))]
use defguard_wireguard_rs::Userspace;
use defguard_wireguard_rs::{WGApi, WireguardInterfaceApi, key::Key, net::IpAddrMask, peer::Peer};
use ipnet::Ipv4Net;
use ployz_core::network::{MAX_WIREGUARD_MTU, MIN_WIREGUARD_MTU, WireGuardPublicKey};
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

#[cfg(target_os = "linux")]
const IPV4_ICMP_OVERHEAD: u32 = 28;
#[cfg(target_os = "linux")]
const WIREGUARD_PING_TIMEOUT: Duration = Duration::from_secs(2);

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
