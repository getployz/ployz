//! Machine-local endpoint discovery.

use ployz_core::dataplane::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_core::ids::MachineId;
use ployz_core::state::MachineEndpointObservation;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

const PUBLIC_IP_SERVICES: &[&str] = &[
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
    let control_endpoints = discover_public_ip().into_iter().collect::<Vec<_>>();
    let mesh_endpoints = discover_mesh_endpoints(&control_endpoints);
    let control_endpoints = unique_ips(control_endpoints);
    let mesh_endpoints = unique_socket_addrs(mesh_endpoints);
    if control_endpoints.is_empty() && mesh_endpoints.is_empty() {
        return None;
    }
    Some(MachineEndpointObservation {
        machine_id: machine_id.clone(),
        control_endpoints,
        mesh_endpoints,
    })
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
        if let Ok(ip) = body.trim().parse::<IpAddr>() {
            return Some(ip);
        }
    }
    None
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

fn mesh_sort_key(ip: IpAddr) -> (u8, String) {
    match ip {
        IpAddr::V4(ip) if ip.is_private() => (0, ip.to_string()),
        IpAddr::V6(ip) if is_unique_local(ip) => (1, ip.to_string()),
        IpAddr::V4(ip) => (2, ip.to_string()),
        IpAddr::V6(ip) => (3, ip.to_string()),
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
