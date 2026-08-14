use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

use ployz_core::{AdvertisedEndpoint, PublicIpDiscovery};
use serde::Deserialize;

use super::{DOCKER_NETWORK_NAME, NetworkError, WIREGUARD_INTERFACE_NAME, checked_command};

pub struct DiscoveredNetwork {
    pub public_ip: Option<IpAddr>,
    pub endpoints: Vec<AdvertisedEndpoint>,
}

pub async fn discover_network(
    port: u16,
    public_ip: PublicIpDiscovery,
) -> Result<DiscoveredNetwork, NetworkError> {
    let mut addresses = routable_interface_addresses()?;
    let public_ip = match public_ip {
        PublicIpDiscovery::Auto => discover_public_ip().await,
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
        endpoints: addresses
            .into_iter()
            .map(|address| AdvertisedEndpoint(SocketAddr::new(address, port)))
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
    // TODO(UT-108): check for link/ether ifaces?
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

async fn discover_public_ip() -> Option<IpAddr> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .ok()?;
    for url in [
        "https://api.ipify.org",
        "https://ipinfo.io/ip",
        "http://ip-api.com/line/?fields=query",
    ] {
        if let Ok(response) = client.get(url).send().await
            && let Ok(response) = response.error_for_status()
            && let Ok(body) = response.text().await
            && let Ok(address) = body.trim().parse()
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
}
