//! ployzd hands out every address on the ployz bridge itself, as a static one. Docker
//! gives dynamic addresses out anew each time it starts Containers, so after a restart
//! a route to one Container would reach whichever came back at its address. With every
//! address static, no Container takes another's at boot.

use std::{collections::BTreeSet, net::Ipv4Addr};

use bollard::models::{ContainerSummary, EndpointIpamConfig, EndpointSettings};
use ployz_core::MachineSubnet;

use crate::network::DOCKER_NETWORK_NAME;

/// The ployz endpoint of a Container, with `ipv4` pinned. Only a Container that
/// [`super::takes_bridge_ipv6`] takes IPv6.
pub(crate) fn endpoint(ipv6: bool, ipv4: Option<Ipv4Addr>) -> EndpointSettings {
    pin(super::bridge_endpoint(ipv6).unwrap_or_default(), ipv4)
}

/// `endpoint` with `ipv4` pinned, keeping any other address it asks for.
pub(crate) fn pin(mut endpoint: EndpointSettings, ipv4: Option<Ipv4Addr>) -> EndpointSettings {
    let Some(address) = ipv4 else {
        return endpoint;
    };
    endpoint
        .ipam_config
        .get_or_insert_with(EndpointIpamConfig::default)
        .ipv4_address = Some(address.to_string());
    endpoint
}

/// What a Container's current endpoint asks Docker for, so it rejoins with the same
/// aliases, links, options and MAC address. The addresses Docker assigned are
/// dropped.
pub(crate) fn reconnect(current: EndpointSettings) -> EndpointSettings {
    EndpointSettings {
        ipam_config: current.ipam_config,
        links: current.links,
        aliases: current.aliases,
        driver_opts: current.driver_opts,
        gw_priority: current.gw_priority,
        mac_address: current.mac_address.filter(|mac| !mac.is_empty()),
        ..Default::default()
    }
}

/// The address pinned on a ployz endpoint.
pub(crate) fn pinned(endpoint: &EndpointSettings) -> Option<Ipv4Addr> {
    endpoint
        .ipam_config
        .as_ref()?
        .ipv4_address
        .as_deref()?
        .parse()
        .ok()
}

/// Addresses on the bridge that a Container holds or will take when it starts.
pub(crate) fn taken(containers: &[ContainerSummary]) -> BTreeSet<Ipv4Addr> {
    containers
        .iter()
        .filter_map(|container| {
            container
                .network_settings
                .as_ref()?
                .networks
                .as_ref()?
                .get(DOCKER_NETWORK_NAME)
        })
        .flat_map(|endpoint| {
            let current = endpoint
                .ip_address
                .as_deref()
                .and_then(|address| address.parse().ok());
            [pinned(endpoint), current]
        })
        .flatten()
        .collect()
}

/// The first free host address after `after`, wrapping, so a freed address is reused
/// last, once routes to the Container that held it are long gone. Without `after` the
/// search starts past the highest taken address.
pub(crate) fn next(
    subnet: MachineSubnet,
    taken: &BTreeSet<Ipv4Addr>,
    after: Option<Ipv4Addr>,
) -> Option<Ipv4Addr> {
    // A Machine Subnet is a /24 whose first host is the gateway.
    let gateway = u32::from(subnet.gateway().0);
    let hosts = (gateway + 1..gateway + 254)
        .map(Ipv4Addr::from)
        .collect::<Vec<_>>();
    let after = after.or_else(|| taken.last().copied());
    let start = after.map_or(0, |after| hosts.partition_point(|host| *host <= after));
    hosts
        .iter()
        .cycle()
        .skip(start)
        .take(hosts.len())
        .find(|address| !taken.contains(address))
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subnet() -> MachineSubnet {
        MachineSubnet::parse("10.210.3.0/24").unwrap()
    }

    fn address(last: u8) -> Ipv4Addr {
        Ipv4Addr::new(10, 210, 3, last)
    }

    fn summary(networks: serde_json::Value) -> ContainerSummary {
        serde_json::from_value(serde_json::json!({
            "NetworkSettings": { "Networks": networks },
        }))
        .unwrap()
    }

    #[test]
    fn a_new_container_takes_the_address_after_the_last_one_handed_out() {
        let taken = BTreeSet::from([address(2), address(3), address(5)]);
        assert_eq!(next(subnet(), &taken, Some(address(3))), Some(address(4)));
        assert_eq!(next(subnet(), &taken, Some(address(5))), Some(address(6)));
    }

    #[test]
    fn a_freed_address_is_reused_only_after_the_rest_of_the_subnet() {
        // .2 was just freed; the next Container takes .6, not .2.
        let taken = BTreeSet::from([address(3), address(4), address(5)]);
        assert_eq!(next(subnet(), &taken, None), Some(address(6)));
        assert_eq!(next(subnet(), &taken, Some(address(254))), Some(address(2)));
    }

    #[test]
    fn the_gateway_and_a_full_subnet_are_never_handed_out() {
        assert_eq!(next(subnet(), &BTreeSet::new(), None), Some(address(2)));
        let full = (2..=254).map(address).collect();
        assert_eq!(next(subnet(), &full, None), None);
    }

    #[test]
    fn a_container_rejoins_with_its_own_aliases_and_options_at_its_pin() {
        let current: EndpointSettings = serde_json::from_value(serde_json::json!({
            "Aliases": ["db"],
            "MacAddress": "02:42:ac:11:00:09",
            "DriverOpts": { "com.docker.network.endpoint.sysctls": "x" },
            "IPAMConfig": { "IPv6Address": "fdcd::9" },
            "IPAddress": "10.210.3.9",
            "EndpointID": "e",
            "NetworkID": "n",
        }))
        .unwrap();
        let rejoined = serde_json::to_value(pin(reconnect(current), Some(address(9)))).unwrap();
        let at = |pointer: &str| rejoined.pointer(pointer).cloned();
        assert_eq!(at("/Aliases"), Some(serde_json::json!(["db"])));
        assert_eq!(
            at("/MacAddress"),
            Some(serde_json::json!("02:42:ac:11:00:09"))
        );
        assert_eq!(
            at("/DriverOpts/com.docker.network.endpoint.sysctls"),
            Some(serde_json::json!("x"))
        );
        assert_eq!(
            at("/IPAMConfig/IPv4Address"),
            Some(serde_json::json!("10.210.3.9"))
        );
        assert_eq!(
            at("/IPAMConfig/IPv6Address"),
            Some(serde_json::json!("fdcd::9"))
        );
        assert!(at("/EndpointID").is_none_or(|id| id.is_null()));
    }

    #[test]
    fn a_stopped_containers_pinned_address_stays_taken() {
        let containers = [
            summary(serde_json::json!({ "ployz": {
                "IPAMConfig": { "IPv4Address": "10.210.3.7" },
                "IPAddress": "",
            } })),
            summary(serde_json::json!({ "ployz": { "IPAddress": "10.210.3.9" } })),
            summary(serde_json::json!({ "bridge": { "IPAddress": "10.210.3.11" } })),
        ];
        assert_eq!(taken(&containers), BTreeSet::from([address(7), address(9)]));
    }
}
