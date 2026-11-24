//! ployzd hands out every address on the ployz bridge itself, as a static one. Docker
//! gives dynamic addresses out anew each time it starts Containers, so after a restart
//! a route to one Container would reach whichever came back at its address. With every
//! address static, no Container takes another's at boot.
//!
//! Each new address lies in the [`AddressPart`] its Container belongs to, which the fence
//! reads back from the address alone. A Container keeps the address it holds or is pinned
//! to, whatever its part.

use std::{
    collections::{BTreeMap, BTreeSet},
    net::Ipv4Addr,
};

use bollard::models::{ContainerSummary, EndpointIpamConfig, EndpointSettings};
use ployz_core::{AddressPart, MachineSubnet};

use super::DOCKER_NETWORK_NAME;

/// Why a Container got no new address on the bridge.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum NoAddress {
    #[error("every {0} address on this Server's {DOCKER_NETWORK_NAME} network is taken")]
    Full(AddressPart),
    #[error("this Server's {DOCKER_NETWORK_NAME} network has no Machine Subnet Ployz can read")]
    UnknownSubnet,
    /// The daemon runs without its Container store, so it can't tell the part.
    #[error(
        "this Server keeps no Container records, so it can't tell which addresses a Container may take"
    )]
    NoRecords,
    #[error("this Server's Container records could not be read")]
    Unread,
}

/// The ployz endpoint of a Container, pinned at `ipv4`. Only a Container that
/// [`super::takes_ipv6`] takes IPv6.
pub(crate) fn endpoint(ipv6: bool, ipv4: Ipv4Addr) -> EndpointSettings {
    pin(super::base_endpoint(ipv6).unwrap_or_default(), ipv4)
}

/// `endpoint` pinned at `ipv4`, keeping any other address it asks for.
pub(crate) fn pin(mut endpoint: EndpointSettings, ipv4: Ipv4Addr) -> EndpointSettings {
    endpoint
        .ipam_config
        .get_or_insert_with(EndpointIpamConfig::default)
        .ipv4_address = Some(ipv4.to_string());
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

/// The last address handed out in each part, so a freed address is reused last, once
/// routes to the Container that held it are long gone.
#[derive(Debug, Default)]
pub(crate) struct Handed(BTreeMap<AddressPart, Ipv4Addr>);

impl Handed {
    /// A free address in `part`, noted as handed out: the first after the last one handed
    /// out there, wrapping. With none handed out yet the search starts past the highest
    /// address taken in the part.
    pub(crate) fn take(
        &mut self,
        subnet: MachineSubnet,
        part: AddressPart,
        taken: &BTreeSet<Ipv4Addr>,
    ) -> Result<Ipv4Addr, NoAddress> {
        let address =
            next(subnet, part, taken, self.0.get(&part).copied()).ok_or(NoAddress::Full(part))?;
        self.0.insert(part, address);
        Ok(address)
    }
}

fn next(
    subnet: MachineSubnet,
    part: AddressPart,
    taken: &BTreeSet<Ipv4Addr>,
    after: Option<Ipv4Addr>,
) -> Option<Ipv4Addr> {
    // A Machine Subnet is a /24 whose first host is the gateway.
    let network = u32::from(subnet.gateway().0) - 1;
    let hosts = part
        .hosts()
        .map(|last| Ipv4Addr::from(network + u32::from(last)))
        .collect::<Vec<_>>();
    let after = after.or_else(|| {
        taken
            .iter()
            .rev()
            .find(|address| hosts.binary_search(address).is_ok())
            .copied()
    });
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

    fn open(taken: &BTreeSet<Ipv4Addr>, after: Option<Ipv4Addr>) -> Option<Ipv4Addr> {
        next(subnet(), AddressPart::Open, taken, after)
    }

    #[test]
    fn a_new_container_takes_the_address_after_the_last_one_handed_out() {
        let taken = BTreeSet::from([address(2), address(3), address(5)]);
        assert_eq!(open(&taken, Some(address(3))), Some(address(4)));
        assert_eq!(open(&taken, Some(address(5))), Some(address(6)));
    }

    #[test]
    fn a_freed_address_is_reused_only_after_the_rest_of_its_part() {
        // .2 was just freed; the next Container takes .6, not .2.
        let taken = BTreeSet::from([address(3), address(4), address(5), address(200)]);
        assert_eq!(open(&taken, None), Some(address(6)));
        assert_eq!(open(&taken, Some(address(159))), Some(address(2)));
    }

    #[test]
    fn a_new_address_lies_in_its_part_and_a_full_part_is_refused() {
        for part in AddressPart::ALL {
            let mut handed = Handed::default();
            let mut taken = BTreeSet::from([address(1)]);
            let room = part.hosts().count();
            for _ in 0..room {
                let ipv4 = handed.take(subnet(), part, &taken).unwrap();
                assert!(part.hosts().contains(&ipv4.octets()[3]), "{part}: {ipv4}");
                taken.insert(ipv4);
            }
            assert_eq!(
                handed.take(subnet(), part, &taken),
                Err(NoAddress::Full(part))
            );
        }
        let full_free = AddressPart::Free.hosts().map(address).collect();
        assert_eq!(
            Handed::default().take(subnet(), AddressPart::Open, &full_free),
            Ok(address(2))
        );
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
        let rejoined = serde_json::to_value(pin(reconnect(current), address(9))).unwrap();
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
