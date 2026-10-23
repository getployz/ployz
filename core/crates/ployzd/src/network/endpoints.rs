use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use ployz_core::{AdvertisedEndpoint, PublicIpDiscovery};
use serde::Deserialize;

use super::{
    DOCKER_NETWORK_NAME, NetworkError, WIREGUARD_INTERFACE_NAME, WIREGUARD_PORT, checked_command,
};

#[derive(Clone)]
pub struct DiscoveredNetwork {
    pub public_ip: Option<IpAddr>,
    pub endpoints: Vec<AdvertisedEndpoint>,
    /// Whether this server reaches IPv4 itself; see [`has_ipv4_route`].
    pub ipv4_route: bool,
}

pub async fn discover_network(
    public_ip: PublicIpDiscovery,
) -> Result<DiscoveredNetwork, NetworkError> {
    let mut addresses = routable_interface_addresses()?;
    let ipv4_route = has_ipv4_route();
    let public_ip = match public_ip {
        PublicIpDiscovery::Auto => discover_public_ip(ipv4_route).await,
        PublicIpDiscovery::Disabled => None,
        PublicIpDiscovery::Override(address) => Some(address),
    };
    if let Some(address) = public_ip
        && !addresses.contains(&address)
    {
        addresses.push(address);
    }
    Ok(DiscoveredNetwork {
        public_ip,
        ipv4_route,
        endpoints: addresses
            .into_iter()
            .map(|address| AdvertisedEndpoint(SocketAddr::new(address, WIREGUARD_PORT)))
            .collect(),
    })
}

#[derive(Deserialize)]
struct Interface {
    ifname: String,
    #[serde(default)]
    flags: Vec<String>,
    #[serde(default)]
    addr_info: Vec<InterfaceAddress>,
}

#[derive(Deserialize)]
struct InterfaceAddress {
    local: IpAddr,
    scope: String,
}

fn routable_interface_addresses() -> Result<Vec<IpAddr>, NetworkError> {
    Ok(routable_addresses(read_interfaces(true)?))
}

pub(crate) fn interface_addresses() -> Result<Vec<IpAddr>, NetworkError> {
    Ok(read_interfaces(false)?
        .into_iter()
        .flat_map(|interface| interface.addr_info)
        .map(|address| address.local)
        .collect())
}

fn read_interfaces(up_only: bool) -> Result<Vec<Interface>, NetworkError> {
    let args = if up_only {
        &["-json", "address", "show", "up"][..]
    } else {
        &["-json", "address", "show"][..]
    };
    let output = checked_command("ip", args)?;
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn routable_addresses(interfaces: Vec<Interface>) -> Vec<IpAddr> {
    // Link-layer type is not a filter: UP/LOWER_UP flags decide eligibility, so
    // tunnels and bridges advertise the same way as Ethernet.
    interfaces
        .into_iter()
        .filter(|interface| {
            interface.flags.iter().any(|flag| flag == "UP")
                && interface.flags.iter().any(|flag| flag == "LOWER_UP")
                && !interface.flags.iter().any(|flag| flag == "LOOPBACK")
                && !skip_interface(&interface.ifname)
        })
        .flat_map(|interface| interface.addr_info)
        .filter(|address| address.scope == "global" && !address.local.is_unspecified())
        .map(|address| address.local)
        .collect()
}

fn skip_interface(name: &str) -> bool {
    name == DOCKER_NETWORK_NAME
        || name == WIREGUARD_INTERFACE_NAME
        || name == "tailscale0"
        || name.starts_with("docker")
        || name.strip_prefix("br-").is_some_and(|suffix| {
            suffix.len() == 12 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

/// Whether this server reaches IPv4 itself. One without that route reaches an IPv4
/// lookup only through a NAT64 gateway, which answers with the gateway's address.
fn has_ipv4_route() -> bool {
    // Ask the kernel, so policy tables and split routes count, not only a main-table default.
    ipv4_route(checked_command("ip", &["-4", "route", "get", "1.1.1.1"]))
}

/// Only the kernel refusing the route means there is none. Anything else counts as a
/// route, as before.
fn ipv4_route(lookup: Result<std::process::Output, NetworkError>) -> bool {
    match lookup {
        Err(NetworkError::Command { stderr, .. }) => {
            !(stderr.contains("Network is unreachable") || stderr.contains("No route to host"))
        }
        _ => true,
    }
}

fn public_ip_lookups(ipv4_route: bool) -> &'static [&'static str] {
    if ipv4_route {
        &[
            "https://api.ipify.org",
            "https://ipinfo.io/ip",
            "http://ip-api.com/line/?fields=query",
        ]
    } else {
        &["https://api6.ipify.org"]
    }
}

/// `found`, unless it is an IPv4 address this server can't own: without an IPv4 route
/// it is a NAT64 gateway's.
fn own_public_ip(found: IpAddr, ipv4_route: bool) -> Option<IpAddr> {
    (ipv4_route || found.is_ipv6()).then_some(found)
}

async fn discover_public_ip(ipv4_route: bool) -> Option<IpAddr> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    for url in public_ip_lookups(ipv4_route) {
        if let Ok(response) = client.get(*url).send().await
            && let Ok(response) = response.error_for_status()
            && let Ok(body) = response.text().await
            && let Ok(address) = body.trim().parse()
            && let Some(address) = own_public_ip(address, ipv4_route)
        {
            return Some(address);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eligible_interface_is_not_filtered_by_link_layer_type() {
        let interfaces = serde_json::from_str(
            r#"[{"ifname":"tun0","link_type":"none","flags":["UP","LOWER_UP"],"addr_info":[{"local":"192.0.2.8","scope":"global"}]}]"#,
        )
        .unwrap();
        assert_eq!(
            routable_addresses(interfaces),
            ["192.0.2.8".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn a_server_without_an_ipv4_route_asks_only_for_its_ipv6_address() {
        assert!(
            public_ip_lookups(false)
                .iter()
                .all(|url| url.contains("api6."))
        );
        assert!(public_ip_lookups(true).contains(&"https://api.ipify.org"));
    }

    #[test]
    fn only_a_refused_route_lookup_means_no_ipv4_route() {
        let refused = |stderr: &str| {
            Err(NetworkError::Command {
                program: "ip".into(),
                stderr: stderr.into(),
            })
        };
        assert!(!ipv4_route(refused(
            "RTNETLINK answers: Network is unreachable"
        )));
        assert!(!ipv4_route(refused("RTNETLINK answers: No route to host")));
        assert!(ipv4_route(refused("RTNETLINK answers: Invalid argument")));
        assert!(ipv4_route(Err(std::io::Error::other("no ip").into())));
        // A policy table or a 0.0.0.0/1 split resolves here, with no main-table default.
        assert!(ipv4_route(Ok(std::process::Output {
            status: std::process::ExitStatus::default(),
            stdout: b"1.1.1.1 via 192.0.2.1 dev wg0 table 51820 src 192.0.2.8\n".to_vec(),
            stderr: Vec::new(),
        })));
    }

    #[test]
    fn an_ipv4_answer_without_an_ipv4_route_is_a_nat64_gateways() {
        let gateway: IpAddr = "203.0.113.7".parse().unwrap();
        let own: IpAddr = "2001:db8::10".parse().unwrap();
        assert_eq!(own_public_ip(gateway, false), None);
        assert_eq!(own_public_ip(own, false), Some(own));
        assert_eq!(own_public_ip(gateway, true), Some(gateway));
    }
}
