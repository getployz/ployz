//! Machine-local endpoint discovery.

use ployz_core::dataplane::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_core::ids::MachineId;
use ployz_core::state::MachineEndpointObservation;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

const PUBLIC_IP_SERVICES: &[&str] = &[
    "https://www.cloudflare.com/cdn-cgi/trace",
    "https://api.ipify.org",
    "https://ipinfo.io/ip",
    "http://ip-api.com/line/?fields=query",
];

pub async fn observe_machine_endpoints(
    machine_id: &MachineId,
) -> Option<MachineEndpointObservation> {
    let machine_id = machine_id.clone();
    tokio::task::spawn_blocking(move || observe_machine_endpoints_blocking(&machine_id))
        .await
        .ok()
        .flatten()
}

fn observe_machine_endpoints_blocking(
    machine_id: &MachineId,
) -> Option<MachineEndpointObservation> {
    let control_endpoints = unique_ips(discover_control_endpoints());
    let mesh_endpoints = unique_socket_addrs(discover_mesh_endpoints(&control_endpoints));
    if control_endpoints.is_empty() && mesh_endpoints.is_empty() {
        return None;
    }
    Some(MachineEndpointObservation {
        machine_id: machine_id.clone(),
        control_endpoints,
        mesh_endpoints,
    })
}

/// The machine's own publicly-reachable control addresses. A cloud host carries its
/// public IP (v4 and v6) directly on its NIC, so its global-scope, non-private
/// interface addresses are the reachable control endpoints — this is what covers
/// IPv6, which the IPv4-only echo services below cannot. The external echo is a
/// fallback for a NAT'd host whose NIC only holds a private address.
fn discover_control_endpoints() -> Vec<IpAddr> {
    let mut endpoints: Vec<IpAddr> = routable_interface_ips()
        .into_iter()
        .filter(|ip| is_public(*ip))
        .collect();
    endpoints.extend(discover_public_ip());
    endpoints
}

/// A globally-routable address the fleet can dial: routable (see [`routable_ip`]) and
/// not in a private (RFC 1918) or unique-local (fc00::/7) range.
fn is_public(ip: IpAddr) -> bool {
    routable_ip(ip)
        && match ip {
            IpAddr::V4(ip) => !ip.is_private(),
            IpAddr::V6(ip) => !is_unique_local(ip),
        }
}

fn discover_public_ip() -> Option<IpAddr> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .timeout_connect(Some(Duration::from_secs(2)))
        .build()
        .into();
    for service in PUBLIC_IP_SERVICES {
        let Ok(mut response) = agent.get(*service).call() else {
            continue;
        };
        let Ok(body) = response.body_mut().read_to_string() else {
            continue;
        };
        if let Some(ip) = parse_public_ip_body(&body) {
            return Some(ip);
        }
    }
    None
}

/// Most services return the bare IP; Cloudflare's `/cdn-cgi/trace` returns
/// `key=value` lines, one of which is `ip=<addr>`.
fn parse_public_ip_body(body: &str) -> Option<IpAddr> {
    if let Ok(ip) = body.trim().parse::<IpAddr>() {
        return Some(ip);
    }
    body.lines()
        .find_map(|line| line.strip_prefix("ip="))
        .and_then(|value| value.trim().parse::<IpAddr>().ok())
}

fn discover_mesh_endpoints(control_endpoints: &[IpAddr]) -> Vec<SocketAddr> {
    let mut endpoints = routable_interface_ips()
        .into_iter()
        .map(|ip| SocketAddr::new(ip, DEFAULT_WIREGUARD_LISTEN_PORT))
        .collect::<Vec<_>>();
    endpoints.extend(
        control_endpoints
            .iter()
            .copied()
            .map(|ip| SocketAddr::new(ip, DEFAULT_WIREGUARD_LISTEN_PORT)),
    );
    endpoints.sort_by_key(|endpoint| mesh_sort_key(endpoint.ip()));
    endpoints
}

fn routable_interface_ips() -> Vec<IpAddr> {
    let Ok(output) = std::process::Command::new("ip")
        .args(["-j", "addr", "show", "up"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(interfaces) = serde_json::from_slice::<Vec<IpInterface>>(&output.stdout) else {
        return Vec::new();
    };
    interfaces
        .into_iter()
        .filter(|interface| include_interface(&interface.ifname))
        .flat_map(|interface| interface.addr_info)
        .filter(|addr| addr.scope.as_deref() == Some("global"))
        .filter_map(|addr| addr.local.parse::<IpAddr>().ok())
        .filter(|ip| routable_ip(*ip))
        .collect()
}

#[derive(Deserialize)]
struct IpInterface {
    ifname: String,
    #[serde(default)]
    addr_info: Vec<IpAddressInfo>,
}

#[derive(Deserialize)]
struct IpAddressInfo {
    local: String,
    scope: Option<String>,
}

fn include_interface(name: &str) -> bool {
    name != "ployz-wg0"
        && name != "lo"
        && !name.starts_with("docker")
        && !name.starts_with("br-")
        && !name.starts_with("veth")
}

fn routable_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            !ip.is_loopback() && !ip.is_unspecified() && !ip.is_multicast() && !ip.is_link_local()
        }
        IpAddr::V6(ip) => {
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && !ip.is_unicast_link_local()
        }
    }
}

fn mesh_sort_key(ip: IpAddr) -> (u8, IpAddr) {
    match ip {
        IpAddr::V4(ip) if ip.is_private() => (0, IpAddr::V4(ip)),
        IpAddr::V6(ip) if is_unique_local(ip) => (1, IpAddr::V6(ip)),
        IpAddr::V4(ip) => (2, IpAddr::V4(ip)),
        IpAddr::V6(ip) => (3, IpAddr::V6(ip)),
    }
}

fn is_unique_local(ip: std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn unique_ips(endpoints: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut seen = BTreeSet::new();
    endpoints
        .into_iter()
        .filter(|endpoint| seen.insert(*endpoint))
        .collect()
}

fn unique_socket_addrs(endpoints: Vec<SocketAddr>) -> Vec<SocketAddr> {
    let mut seen = BTreeSet::new();
    endpoints
        .into_iter()
        .filter(|endpoint| seen.insert(*endpoint))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{is_public, parse_public_ip_body};
    use std::net::IpAddr;

    fn ip(value: &str) -> IpAddr {
        value.parse().expect("valid ip")
    }

    #[test]
    fn public_ip_body_parses_plaintext_and_cloudflare_trace() {
        assert_eq!(
            parse_public_ip_body("203.0.113.5\n"),
            Some(ip("203.0.113.5"))
        );
        let trace = "fl=123abc\nh=www.cloudflare.com\nip=203.0.113.5\nts=1.0\n";
        assert_eq!(parse_public_ip_body(trace), Some(ip("203.0.113.5")));
        assert_eq!(parse_public_ip_body("not-an-ip"), None);
    }

    #[test]
    fn public_accepts_global_v4_and_v6_rejects_private_and_ula() {
        // A cloud host's global NIC addresses — the whole point of fix: IPv6 counts.
        assert!(is_public(ip("203.0.113.5")));
        assert!(is_public(ip("2a01:4ff:2f0:296a::1")));
        // Private / unique-local / loopback / link-local are not fleet-dialable.
        assert!(!is_public(ip("10.0.0.1")));
        assert!(!is_public(ip("192.168.1.1")));
        assert!(!is_public(ip("fc00::1")));
        assert!(!is_public(ip("fe80::1")));
        assert!(!is_public(ip("127.0.0.1")));
    }
}
