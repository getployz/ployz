use ployz_core::dataplane::{
    PloyzNativeMeshComponent, WireGuardEbpfEndpointRoute, WireGuardEbpfPrepareError,
};
use ployz_core::ids::MachineId;
use std::path::PathBuf;

use super::host_commands::{HostCommandPlan, ebpf_ctl_args, unavailable};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HostDataplaneRouteProgramming {
    pub(super) ebpf_ctl_program: String,
    pub(super) bridge_ifname: String,
    pub(super) wg_ifname: String,
    pub(super) ebpf_pin_path: Option<PathBuf>,
}

impl HostDataplaneRouteProgramming {
    pub(super) fn plans_for(
        &self,
        machine_id: &MachineId,
        endpoint_routes: &[WireGuardEbpfEndpointRoute],
    ) -> Result<Vec<HostCommandPlan>, WireGuardEbpfPrepareError> {
        let local_route = endpoint_routes
            .iter()
            .find(|route| route.machine_id == *machine_id)
            .ok_or_else(|| {
                unavailable(
                    machine_id,
                    PloyzNativeMeshComponent::WireGuard,
                    "local endpoint route is missing".to_owned(),
                )
            })?;
        let mut requirements = vec![
            HostCommandPlan::provisioning_sysctl(
                PloyzNativeMeshComponent::WireGuard,
                "net.ipv4.ip_forward",
                "1",
            ),
            HostCommandPlan::provisioning_sysctl(
                PloyzNativeMeshComponent::WireGuard,
                "net.ipv4.conf.all.rp_filter",
                "0",
            ),
            HostCommandPlan::provisioning_sysctl(
                PloyzNativeMeshComponent::WireGuard,
                "net.ipv4.conf.default.rp_filter",
                "0",
            ),
            HostCommandPlan::provisioning_sysctl(
                PloyzNativeMeshComponent::WireGuard,
                format!("net.ipv4.conf.{}.rp_filter", self.wg_ifname),
                "0",
            ),
            HostCommandPlan::provisioning_command(
                PloyzNativeMeshComponent::WireGuard,
                "sh",
                [
                    "-c".to_owned(),
                    "test ! -e \"/proc/sys/net/ipv4/conf/$1/rp_filter\" || sysctl -w \"net.ipv4.conf.$1.rp_filter=0\"".to_owned(),
                    "--".to_owned(),
                    self.bridge_ifname.clone(),
                ],
            ),
            HostCommandPlan::provisioning_command(
                PloyzNativeMeshComponent::WireGuard,
                "sh",
                [
                    "-c".to_owned(),
                    "iptables -t raw -C PREROUTING -i \"$1\" -d \"$2\" -j ACCEPT || iptables -t raw -I PREROUTING 1 -i \"$1\" -d \"$2\" -j ACCEPT".to_owned(),
                    "--".to_owned(),
                    self.wg_ifname.clone(),
                    local_route.endpoint_subnet.clone(),
                ],
            ),
            HostCommandPlan::provisioning_command(
                PloyzNativeMeshComponent::WireGuard,
                "sh",
                [
                    "-c".to_owned(),
                    "iptables -C FORWARD -i \"$1\" -o \"$2\" -j ACCEPT || iptables -I FORWARD 1 -i \"$1\" -o \"$2\" -j ACCEPT".to_owned(),
                    "--".to_owned(),
                    self.wg_ifname.clone(),
                    self.bridge_ifname.clone(),
                ],
            ),
            HostCommandPlan::provisioning_command(
                PloyzNativeMeshComponent::WireGuard,
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
                .filter(|route| route.machine_id != *machine_id)
                .map(|route| {
                    HostCommandPlan::provisioning_command(
                        PloyzNativeMeshComponent::EbpfForwarding,
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
                    )
                }),
        );
        Ok(requirements)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapters::host_dataplane::HostCommandAction;

    #[test]
    fn route_programming_adds_only_peer_endpoint_subnets() {
        let route_programming = HostDataplaneRouteProgramming {
            ebpf_ctl_program: "/usr/local/bin/ployz-ebpf-ctl".to_owned(),
            bridge_ifname: "br-ployz".to_owned(),
            wg_ifname: "ployz-wg0".to_owned(),
            ebpf_pin_path: None,
        };
        let requirements = route_programming
            .plans_for(
                &machine_id("machine_a"),
                &[
                    WireGuardEbpfEndpointRoute {
                        machine_id: machine_id("machine_a"),
                        endpoint_subnet: "10.42.1.0/24".to_owned(),
                    },
                    WireGuardEbpfEndpointRoute {
                        machine_id: machine_id("machine_b"),
                        endpoint_subnet: "10.42.2.0/24".to_owned(),
                    },
                ],
            )
            .expect("route requirements are generated");

        assert!(requirements.contains(&HostCommandPlan::provisioning_sysctl(
            PloyzNativeMeshComponent::WireGuard,
            "net.ipv4.ip_forward",
            "1"
        )));
        assert!(requirements.contains(&HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::WireGuard,
            "sh",
            [
                "-c",
                "iptables -t raw -C PREROUTING -i \"$1\" -d \"$2\" -j ACCEPT || iptables -t raw -I PREROUTING 1 -i \"$1\" -d \"$2\" -j ACCEPT",
                "--",
                "ployz-wg0",
                "10.42.1.0/24"
            ]
        )));
        assert!(requirements.contains(&HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::WireGuard,
            "sh",
            [
                "-c",
                "iptables -C FORWARD -i \"$1\" -o \"$2\" -j ACCEPT || iptables -I FORWARD 1 -i \"$1\" -o \"$2\" -j ACCEPT",
                "--",
                "ployz-wg0",
                "br-ployz"
            ]
        )));
        assert!(requirements.contains(&HostCommandPlan::provisioning_command(
            PloyzNativeMeshComponent::WireGuard,
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
            requirements.contains(&HostCommandPlan::provisioning_command(
                PloyzNativeMeshComponent::EbpfForwarding,
                "/usr/local/bin/ployz-ebpf-ctl",
                ["route", "add-ifname", "10.42.2.0/24", "ployz-wg0"]
            ))
        );
        assert!(!requirements.iter().any(|plan| {
            matches!(
                &plan.action,
                HostCommandAction::CommandSucceeds {
                    component: PloyzNativeMeshComponent::EbpfForwarding,
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
            .plans_for(
                &machine_id("machine_a"),
                &[WireGuardEbpfEndpointRoute {
                    machine_id: machine_id("machine_b"),
                    endpoint_subnet: "10.42.2.0/24".to_owned(),
                }],
            )
            .expect_err("missing local route fails");

        assert!(matches!(
            error,
            WireGuardEbpfPrepareError::Unavailable {
                machine_id,
                component: PloyzNativeMeshComponent::WireGuard,
                ..
            } if machine_id == self::machine_id("machine_a")
        ));
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }
}
