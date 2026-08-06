use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, SocketAddr};

use ipnet::{IpNet, Ipv4Net, Ipv6Net};
use ployz_core::corrosion::{DesiredBuiltinWireguardMesh, DesiredMachineContainerRoute};
use ployz_core::network::WireGuardPublicKey;

use super::BuiltinWireguardLatestHandshake;

pub(super) const PERSISTENT_KEEPALIVE_SECONDS: u16 = 25;

pub(super) fn render_desired_peers(desired: &DesiredBuiltinWireguardMesh) -> Vec<DesiredPeer> {
    let machines = desired.machine_peers.iter().map(|peer| {
        let mut allowed_ips = BTreeSet::from([IpNet::V6(peer.subnet_v6.network())]);
        if let DesiredMachineContainerRoute::Claimed { subnet } = &peer.container_route {
            allowed_ips.insert(subnet.ipnet());
        }
        DesiredPeer {
            public_key: peer.public_key.clone(),
            endpoint: peer.endpoint,
            allowed_ips,
        }
    });
    let roaming = desired.roaming_peers.iter().map(|peer| DesiredPeer {
        public_key: peer.public_key.clone(),
        endpoint: peer.endpoint,
        allowed_ips: BTreeSet::from([IpNet::V6(peer.subnet_v6.network())]),
    });
    machines.chain(roaming).collect()
}

pub(super) fn machine_gossip_sources(desired: &DesiredBuiltinWireguardMesh) -> BTreeSet<Ipv6Net> {
    let mut sources = BTreeSet::from([desired.local.subnet_v6.network()]);
    sources.extend(
        desired
            .machine_peers
            .iter()
            .map(|peer| peer.subnet_v6.network()),
    );
    sources
}

pub(super) fn render_allowed_ips(allowed_ips: &BTreeSet<IpNet>) -> String {
    allowed_ips
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn wireguard_peer_args(ifname: &str, peer: &DesiredPeer) -> Vec<String> {
    let mut args = vec![
        "set".to_owned(),
        ifname.to_owned(),
        "peer".to_owned(),
        peer.public_key.as_str().to_owned(),
        "allowed-ips".to_owned(),
        render_allowed_ips(&peer.allowed_ips),
    ];
    if let Some(endpoint) = peer.endpoint {
        args.extend([
            "endpoint".to_owned(),
            endpoint.to_string(),
            "persistent-keepalive".to_owned(),
            PERSISTENT_KEEPALIVE_SECONDS.to_string(),
        ]);
    }
    args
}

pub(super) fn route_family(route: IpNet) -> &'static str {
    match route {
        IpNet::V4(_) => "-4",
        IpNet::V6(_) => "-6",
    }
}

pub(super) fn owned_route_observation_args<'a>(family: &'a str, ifname: &'a str) -> [&'a str; 8] {
    [
        "-o", family, "route", "show", "dev", ifname, "proto", "boot",
    ]
}

pub(super) fn parse_wireguard_dump(
    dump: &str,
) -> Result<BTreeMap<WireGuardPublicKey, ObservedPeer>, String> {
    let mut lines = dump.lines();
    let Some(_interface) = lines.next() else {
        return Err("WireGuard dump had no interface row".to_owned());
    };
    lines
        .enumerate()
        .map(|(index, line)| {
            let fields = line.split('\t').collect::<Vec<_>>();
            let [
                public_key,
                _preshared_key,
                endpoint,
                allowed_ips,
                handshake,
                _rx,
                _tx,
                keepalive,
            ] = fields.as_slice()
            else {
                return Err(format!(
                    "WireGuard dump peer row {} had {} fields, expected 8",
                    index + 2,
                    fields.len()
                ));
            };
            let public_key = WireGuardPublicKey::try_new(*public_key)
                .map_err(|error| format!("parse observed WireGuard public key: {error}"))?;
            let endpoint = match *endpoint {
                "(none)" => None,
                value => Some(value.parse::<SocketAddr>().map_err(|error| {
                    format!("parse observed WireGuard endpoint {value:?}: {error}")
                })?),
            };
            let allowed_ips = match *allowed_ips {
                "(none)" => BTreeSet::new(),
                value => value
                    .split(',')
                    .map(|route| {
                        route.parse::<IpNet>().map_err(|error| {
                            format!("parse observed WireGuard allowed IP {route:?}: {error}")
                        })
                    })
                    .collect::<Result<_, _>>()?,
            };
            let persistent_keepalive = match *keepalive {
                "off" | "0" => None,
                value => Some(value.parse::<u16>().map_err(|error| {
                    format!("parse observed WireGuard keepalive {value:?}: {error}")
                })?),
            };
            let latest_handshake = BuiltinWireguardLatestHandshake::parse_dump_field(handshake)?;
            Ok((
                public_key,
                ObservedPeer {
                    endpoint,
                    allowed_ips,
                    persistent_keepalive,
                    latest_handshake,
                },
            ))
        })
        .collect()
}

pub(super) fn parse_owned_routes(output: &str) -> Result<BTreeMap<IpNet, ObservedRoute>, String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            let route_field = fields
                .first()
                .expect("nonempty route row has a first field");
            let route = route_field
                .parse::<IpNet>()
                .map_err(|error| format!("parse owned WireGuard route {route_field:?}: {error}"))?;
            let source_fields = fields
                .iter()
                .enumerate()
                .filter(|(_, field)| **field == "src")
                .collect::<Vec<_>>();
            let preferred_source = match (route, source_fields.as_slice()) {
                (IpNet::V4(_), []) => None,
                (IpNet::V4(_), [(index, _)]) => Some(
                    fields
                        .get(index + 1)
                        .ok_or_else(|| {
                            format!("owned WireGuard route had no value after src: {line:?}")
                        })?
                        .parse::<Ipv4Addr>()
                        .map_err(|error| {
                            format!("parse owned WireGuard route source in {line:?}: {error}")
                        })?,
                ),
                (IpNet::V4(_), [_, _, ..]) => {
                    return Err(format!(
                        "owned WireGuard route had more than one src field: {line:?}"
                    ));
                }
                (IpNet::V6(_), _) => None,
            };
            Ok((
                route,
                ObservedRoute {
                    destination: route,
                    preferred_source,
                },
            ))
        })
        .collect()
}

pub(super) fn parse_interface_ipv6_addresses(output: &str) -> Result<BTreeSet<Ipv6Net>, String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            let Some(address) = fields.windows(2).find_map(|pair| match pair {
                ["inet6", address] => Some(*address),
                [_, _] | [] | [_] | [_, _, ..] => None,
            }) else {
                return Err(format!(
                    "WireGuard IPv6 address row had no inet6 field: {line:?}"
                ));
            };
            address.parse::<Ipv6Net>().map_err(|error| {
                format!("parse WireGuard IPv6 interface address {address:?}: {error}")
            })
        })
        .collect()
}

pub(super) fn parse_interface_ipv4_addresses(output: &str) -> Result<BTreeSet<Ipv4Net>, String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
            let Some(address) = fields.windows(2).find_map(|pair| match pair {
                ["inet", address] => Some(*address),
                [_, _] | [] | [_] | [_, _, ..] => None,
            }) else {
                return Err(format!(
                    "endpoint bridge IPv4 address row had no inet field: {line:?}"
                ));
            };
            address
                .parse::<Ipv4Net>()
                .map_err(|error| format!("parse endpoint bridge IPv4 address {address:?}: {error}"))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DesiredPeer {
    pub(super) public_key: WireGuardPublicKey,
    pub(super) endpoint: Option<SocketAddr>,
    pub(super) allowed_ips: BTreeSet<IpNet>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ObservedPeer {
    pub(super) endpoint: Option<SocketAddr>,
    pub(super) allowed_ips: BTreeSet<IpNet>,
    pub(super) persistent_keepalive: Option<u16>,
    pub(super) latest_handshake: BuiltinWireguardLatestHandshake,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DesiredRoute {
    pub(super) destination: IpNet,
    pub(super) preferred_source: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ObservedRoute {
    pub(super) destination: IpNet,
    pub(super) preferred_source: Option<Ipv4Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ConvergenceAction {
    UpsertPeer(DesiredPeer),
    ReplaceRoute(DesiredRoute),
    RemovePeer(WireGuardPublicKey),
    RemoveRoute(IpNet),
}

pub(super) fn convergence_plan(
    observed_peers: &BTreeMap<WireGuardPublicKey, ObservedPeer>,
    observed_routes: &BTreeMap<IpNet, ObservedRoute>,
    desired_peers: &[DesiredPeer],
    ipv4_preferred_source: Option<Ipv4Addr>,
) -> Vec<ConvergenceAction> {
    let desired_by_key = desired_peers
        .iter()
        .map(|peer| (peer.public_key.clone(), peer))
        .collect::<BTreeMap<_, _>>();
    let desired_routes = desired_peers
        .iter()
        .flat_map(|peer| peer.allowed_ips.iter().copied())
        .map(|destination| {
            let preferred_source = match destination {
                IpNet::V4(_) => ipv4_preferred_source,
                IpNet::V6(_) => None,
            };
            (
                destination,
                DesiredRoute {
                    destination,
                    preferred_source,
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut actions = Vec::new();
    actions.extend(desired_peers.iter().filter_map(|desired| {
        let matches = observed_peers
            .get(&desired.public_key)
            .is_some_and(|observed| peer_matches(observed, desired));
        (!matches).then(|| ConvergenceAction::UpsertPeer(desired.clone()))
    }));
    actions.extend(desired_routes.values().filter_map(|desired| {
        let current = observed_routes.get(&desired.destination);
        (current
            != Some(&ObservedRoute {
                destination: desired.destination,
                preferred_source: desired.preferred_source,
            }))
        .then_some(ConvergenceAction::ReplaceRoute(*desired))
    }));
    actions.extend(
        observed_peers
            .keys()
            .filter(|key| !desired_by_key.contains_key(*key))
            .cloned()
            .map(ConvergenceAction::RemovePeer),
    );
    actions.extend(
        observed_routes
            .keys()
            .filter(|destination| !desired_routes.contains_key(*destination))
            .copied()
            .map(ConvergenceAction::RemoveRoute),
    );
    actions
}

pub(super) fn peer_matches(observed: &ObservedPeer, desired: &DesiredPeer) -> bool {
    let endpoint_matches = match desired.endpoint {
        Some(endpoint) => {
            observed.endpoint == Some(endpoint)
                && observed.persistent_keepalive == Some(PERSISTENT_KEEPALIVE_SECONDS)
        }
        None => true,
    };
    endpoint_matches && observed.allowed_ips == desired.allowed_ips
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use ployz_core::corrosion::{
        BuiltinWireguardRosterEvidence, DesiredBuiltinWireguardLocal,
        DesiredBuiltinWireguardMachinePeer, DesiredBuiltinWireguardRoamingPeer,
        derive_builtin_wireguard_member,
    };
    use ployz_core::ids::{ClusterId, MachineRowId, PeerId};

    use super::*;

    fn key(byte: u8) -> WireGuardPublicKey {
        WireGuardPublicKey::try_new(base64::engine::general_purpose::STANDARD.encode([byte; 32]))
            .expect("key")
    }

    #[test]
    fn gossip_sources_include_local_and_remote_machines_but_not_roaming_peers() {
        let cluster = ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("cluster");
        let local_key = key(1);
        let machine_key = key(2);
        let roaming_key = key(3);
        let local = derive_builtin_wireguard_member(&cluster, &local_key);
        let machine = derive_builtin_wireguard_member(&cluster, &machine_key);
        let roaming = derive_builtin_wireguard_member(&cluster, &roaming_key);
        let desired = DesiredBuiltinWireguardMesh {
            local: DesiredBuiltinWireguardLocal {
                public_key: local_key,
                subnet_v6: local.subnet(),
                bind_address: local.bind_address(),
            },
            local_container_subnet: ployz_core::network::MachineEndpointSubnet::try_new(
                "10.210.1.0/24",
            )
            .expect("local subnet"),
            cluster_container_prefix: ployz_core::network::MachineEndpointSupernet::try_new(
                "10.210.0.0/16",
            )
            .expect("cluster prefix"),
            machine_peers: vec![DesiredBuiltinWireguardMachinePeer {
                machine_id: MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("machine"),
                public_key: machine_key,
                subnet_v6: machine.subnet(),
                endpoint: None,
                container_route: DesiredMachineContainerRoute::Claimed {
                    subnet: ployz_core::network::MachineEndpointSubnet::try_new("10.210.2.0/24")
                        .expect("subnet"),
                },
            }],
            roaming_peers: vec![DesiredBuiltinWireguardRoamingPeer {
                peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("peer"),
                public_key: roaming_key,
                subnet_v6: roaming.subnet(),
                endpoint: None,
            }],
            ebpf_routes: Vec::new(),
            evidence: BuiltinWireguardRosterEvidence {
                machine_skipped: Vec::new(),
                machine_shadows: Vec::new(),
                peer_skipped: Vec::new(),
                peer_shadows: Vec::new(),
                address_mismatches: Vec::new(),
                identity_conflicts: Vec::new(),
            },
        };

        assert_eq!(
            machine_gossip_sources(&desired),
            BTreeSet::from([local.subnet().network(), machine.subnet().network()])
        );
        assert!(!machine_gossip_sources(&desired).contains(&roaming.subnet().network()));
    }
}
