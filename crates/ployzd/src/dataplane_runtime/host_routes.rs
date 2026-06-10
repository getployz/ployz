use ployz_core::dataplane::{
    WireGuardEbpfComponent, WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError,
};
use ployz_core::ids::NodeId;
use std::net::Ipv4Addr;
use std::path::PathBuf;

use super::{HostDataplaneRequirement, ebpf_ctl_args, unavailable};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HostDataplaneRouteProgramming {
    pub(super) ebpf_ctl_program: String,
    pub(super) bridge_ifname: String,
    pub(super) wg_ifname: String,
    pub(super) ebpf_pin_path: Option<PathBuf>,
}

impl HostDataplaneRouteProgramming {
    pub(super) fn requirements_for(
        &self,
        node_id: &NodeId,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
    ) -> Result<Vec<HostDataplaneRequirement>, WireGuardEbpfPrepareError> {
        let local_route = endpoint_routes
            .iter()
            .find(|route| route.node_id == *node_id)
            .ok_or_else(|| {
                unavailable(
                    node_id,
                    WireGuardEbpfComponent::WireGuard,
                    "local endpoint route is missing".to_owned(),
                )
            })?;
        let local_host_cidr = wireguard_host_cidr(&local_route.endpoint_subnet)
            .map_err(|message| unavailable(node_id, WireGuardEbpfComponent::WireGuard, message))?;
        let local_host_ip = local_host_cidr
            .split_once('/')
            .expect("generated wireguard host CIDR includes prefix")
            .0
            .to_owned();
        let mut requirements = vec![
            HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "ip",
                [
                    "addr".to_owned(),
                    "replace".to_owned(),
                    local_host_cidr,
                    "dev".to_owned(),
                    self.wg_ifname.clone(),
                ],
            ),
            HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "sysctl",
                ["-w", "net.ipv4.ip_forward=1"],
            ),
            HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "sysctl",
                ["-w", "net.ipv4.conf.all.rp_filter=0"],
            ),
            HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "sysctl",
                ["-w", "net.ipv4.conf.default.rp_filter=0"],
            ),
            HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "sysctl",
                [
                    "-w".to_owned(),
                    format!("net.ipv4.conf.{}.rp_filter=0", self.wg_ifname),
                ],
            ),
            HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "sh",
                [
                    "-c".to_owned(),
                    "test ! -e \"/proc/sys/net/ipv4/conf/$1/rp_filter\" || sysctl -w \"net.ipv4.conf.$1.rp_filter=0\"".to_owned(),
                    "--".to_owned(),
                    self.bridge_ifname.clone(),
                ],
            ),
            HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "sh",
                [
                    "-c".to_owned(),
                    "iptables -t raw -C PREROUTING -i \"$1\" -d \"$2\" -j ACCEPT || iptables -t raw -I PREROUTING 1 -i \"$1\" -d \"$2\" -j ACCEPT".to_owned(),
                    "--".to_owned(),
                    self.wg_ifname.clone(),
                    local_route.endpoint_subnet.clone(),
                ],
            ),
            HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "sh",
                [
                    "-c".to_owned(),
                    "iptables -C FORWARD -i \"$1\" -o \"$2\" -j ACCEPT || iptables -I FORWARD 1 -i \"$1\" -o \"$2\" -j ACCEPT".to_owned(),
                    "--".to_owned(),
                    self.wg_ifname.clone(),
                    self.bridge_ifname.clone(),
                ],
            ),
            HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "sh",
                [
                    "-c".to_owned(),
                    "iptables -C FORWARD -i \"$1\" -o \"$2\" -j ACCEPT || iptables -I FORWARD 1 -i \"$1\" -o \"$2\" -j ACCEPT".to_owned(),
                    "--".to_owned(),
                    self.bridge_ifname.clone(),
                    self.wg_ifname.clone(),
                ],
            ),
        ];
        requirements.extend(
            endpoint_routes
                .iter()
                .filter(|route| route.node_id != *node_id)
                .flat_map(|route| {
                    [
                        HostDataplaneRequirement::command_succeeds(
                            WireGuardEbpfComponent::WireGuard,
                            "ip",
                            [
                                "route".to_owned(),
                                "replace".to_owned(),
                                route.endpoint_subnet.clone(),
                                "dev".to_owned(),
                                self.wg_ifname.clone(),
                                "src".to_owned(),
                                local_host_ip.clone(),
                            ],
                        ),
                        HostDataplaneRequirement::command_succeeds(
                            WireGuardEbpfComponent::EbpfForwarding,
                            self.ebpf_ctl_program.clone(),
                            ebpf_ctl_args(
                                &self.ebpf_pin_path,
                                [
                                    "route".to_owned(),
                                    "add-ifname".to_owned(),
                                    route.endpoint_subnet.clone(),
                                    self.wg_ifname.clone(),
                                ],
                            ),
                        ),
                    ]
                }),
        );
        Ok(requirements)
    }
}

fn wireguard_host_cidr(endpoint_subnet: &str) -> Result<String, String> {
    let Some((network, prefix)) = endpoint_subnet.split_once('/') else {
        return Err(format!(
            "endpoint subnet has no CIDR prefix: {endpoint_subnet}"
        ));
    };
    if prefix != "24" {
        return Err(format!(
            "endpoint subnet must be an IPv4 /24 for host WireGuard addressing: {endpoint_subnet}"
        ));
    }
    let network = network.parse::<Ipv4Addr>().map_err(|source| {
        format!("endpoint subnet network is not IPv4: {endpoint_subnet}: {source}")
    })?;
    let mut octets = network.octets();
    if octets[3] != 0 {
        return Err(format!(
            "endpoint subnet must start at the network address: {endpoint_subnet}"
        ));
    }
    octets[3] = 254;
    Ok(format!("{}/32", Ipv4Addr::from(octets)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_programming_adds_only_peer_endpoint_subnets() {
        let route_programming = HostDataplaneRouteProgramming {
            ebpf_ctl_program: "/usr/local/bin/ployz-ebpf-ctl".to_owned(),
            bridge_ifname: "br-ployz".to_owned(),
            wg_ifname: "ployz-wg0".to_owned(),
            ebpf_pin_path: None,
        };
        let requirements = route_programming
            .requirements_for(
                &node_id("node_a"),
                &[
                    WireGuardEbpfEndpointRoute {
                        node_id: node_id("node_a"),
                        endpoint_subnet: "10.42.1.0/24".to_owned(),
                    },
                    WireGuardEbpfEndpointRoute {
                        node_id: node_id("node_b"),
                        endpoint_subnet: "10.42.2.0/24".to_owned(),
                    },
                ],
            )
            .expect("route requirements are generated");

        assert!(
            requirements.contains(&HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "ip",
                ["addr", "replace", "10.42.1.254/32", "dev", "ployz-wg0"]
            ))
        );
        assert!(
            requirements.contains(&HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::WireGuard,
                "ip",
                [
                    "route",
                    "replace",
                    "10.42.2.0/24",
                    "dev",
                    "ployz-wg0",
                    "src",
                    "10.42.1.254"
                ]
            ))
        );
        assert!(requirements.contains(&HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c",
                "iptables -t raw -C PREROUTING -i \"$1\" -d \"$2\" -j ACCEPT || iptables -t raw -I PREROUTING 1 -i \"$1\" -d \"$2\" -j ACCEPT",
                "--",
                "ployz-wg0",
                "10.42.1.0/24"
            ]
        )));
        assert!(requirements.contains(&HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c",
                "iptables -C FORWARD -i \"$1\" -o \"$2\" -j ACCEPT || iptables -I FORWARD 1 -i \"$1\" -o \"$2\" -j ACCEPT",
                "--",
                "ployz-wg0",
                "br-ployz"
            ]
        )));
        assert!(requirements.contains(&HostDataplaneRequirement::command_succeeds(
            WireGuardEbpfComponent::WireGuard,
            "sh",
            [
                "-c",
                "iptables -C FORWARD -i \"$1\" -o \"$2\" -j ACCEPT || iptables -I FORWARD 1 -i \"$1\" -o \"$2\" -j ACCEPT",
                "--",
                "br-ployz",
                "ployz-wg0"
            ]
        )));
        assert!(
            requirements.contains(&HostDataplaneRequirement::command_succeeds(
                WireGuardEbpfComponent::EbpfForwarding,
                "/usr/local/bin/ployz-ebpf-ctl",
                ["route", "add-ifname", "10.42.2.0/24", "ployz-wg0"]
            ))
        );
        assert!(!requirements.iter().any(|requirement| {
            matches!(
                requirement,
                HostDataplaneRequirement::CommandSucceeds {
                    component: WireGuardEbpfComponent::EbpfForwarding,
                    args,
                    ..
                } if args == &["route", "add-ifname", "10.42.1.0/24", "ployz-wg0"]
            )
        }));
    }

    #[test]
    fn route_programming_rejects_missing_local_endpoint_route() {
        let route_programming = HostDataplaneRouteProgramming {
            ebpf_ctl_program: "/usr/local/bin/ployz-ebpf-ctl".to_owned(),
            bridge_ifname: "br-ployz".to_owned(),
            wg_ifname: "ployz-wg0".to_owned(),
            ebpf_pin_path: None,
        };

        let error = route_programming
            .requirements_for(
                &node_id("node_a"),
                &[WireGuardEbpfEndpointRoute {
                    node_id: node_id("node_b"),
                    endpoint_subnet: "10.42.2.0/24".to_owned(),
                }],
            )
            .expect_err("missing local route fails");

        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                node_id,
                component: WireGuardEbpfComponent::WireGuard,
                ..
            } if node_id == self::node_id("node_a")
        ));
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

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }
}
