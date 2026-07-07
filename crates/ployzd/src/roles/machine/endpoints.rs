//! Machine-local endpoint discovery.
//!
//! Interface enumeration is a syscall (`local-ip-address`); the public-IP echo is a
//! network probe (`public-ip-address`). They are split so a cold-cache `FactsGet` can
//! answer from interfaces alone, never blocking on the echo.

use ployz_core::dataplane::DEFAULT_WIREGUARD_LISTEN_PORT;
use ployz_core::ids::MachineId;
use ployz_core::reachability::{is_public, is_routable, mesh_sort_key};
use ployz_core::state::MachineEndpointObservation;
use std::collections::BTreeSet;
use std::net::{IpAddr, SocketAddr};

/// Full discovery for the observation tick: local interface addresses plus an
/// external public-IP probe, so a NAT'd host whose public address is not on its NIC
/// still advertises a reachable control endpoint.
pub async fn observe_machine_endpoints(
    machine_id: &MachineId,
) -> Option<MachineEndpointObservation> {
    observe(machine_id, PublicIpProbe::Echo).await
}

/// Cheap discovery for a cold-cache `FactsGet`: interface addresses only (a syscall,
/// no network), so mesh peer discovery never blocks on the echo services. The
/// observation task fills in the echoed public IP for NAT'd hosts.
pub async fn observe_interface_endpoints(
    machine_id: &MachineId,
) -> Option<MachineEndpointObservation> {
    observe(machine_id, PublicIpProbe::InterfacesOnly).await
}

enum PublicIpProbe {
    Echo,
    InterfacesOnly,
}

async fn observe(
    machine_id: &MachineId,
    probe: PublicIpProbe,
) -> Option<MachineEndpointObservation> {
    let machine_id = machine_id.clone();
    tokio::task::spawn_blocking(move || build_observation(&machine_id, probe))
        .await
        .ok()
        .flatten()
}

fn build_observation(
    machine_id: &MachineId,
    probe: PublicIpProbe,
) -> Option<MachineEndpointObservation> {
    let interface_ips = routable_interface_ips();

    let mut control = interface_ips
        .iter()
        .copied()
        .filter(|ip| is_public(*ip))
        .collect::<Vec<_>>();
    if let PublicIpProbe::Echo = probe {
        control.extend(discover_public_ip());
    }
    let control_endpoints = unique(control);

    let mut mesh = interface_ips;
    mesh.extend(control_endpoints.iter().copied());
    mesh.sort_by_key(|ip| mesh_sort_key(*ip));
    let mesh_endpoints = unique(mesh)
        .into_iter()
        .map(|ip| SocketAddr::new(ip, DEFAULT_WIREGUARD_LISTEN_PORT))
        .collect::<Vec<_>>();

    if control_endpoints.is_empty() && mesh_endpoints.is_empty() {
        return None;
    }
    Some(MachineEndpointObservation {
        machine_id: machine_id.clone(),
        control_endpoints,
        mesh_endpoints,
    })
}

/// The machine's own routable interface addresses (a syscall, no network), excluding
/// the WireGuard, Docker, and Tailscale interfaces we must not advertise as underlay.
fn routable_interface_ips() -> Vec<IpAddr> {
    let Ok(interfaces) = local_ip_address::list_afinet_netifas() else {
        return Vec::new();
    };
    interfaces
        .into_iter()
        .filter(|(name, _)| include_interface(name))
        .map(|(_, ip)| ip)
        .filter(|ip| is_routable(*ip))
        .collect()
}

fn include_interface(name: &str) -> bool {
    name != "ployz-wg0"
        && name != "lo"
        && name != "tailscale0"
        && !name.starts_with("docker")
        && !name.starts_with("br-")
        && !name.starts_with("veth")
}

/// The machine's public IP as seen by an external service — the fallback for a NAT'd
/// host whose NIC only holds a private address.
fn discover_public_ip() -> Option<IpAddr> {
    public_ip_address::perform_lookup(None)
        .ok()
        .map(|response| response.ip)
}

fn unique(ips: Vec<IpAddr>) -> Vec<IpAddr> {
    let mut seen = BTreeSet::new();
    ips.into_iter().filter(|ip| seen.insert(*ip)).collect()
}
