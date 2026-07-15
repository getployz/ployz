use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use ployz_core::ids::MachineId;
use ployz_core::intent::ActiveMachineState;
use ployz_core::roles::GatewayRole;

use crate::control::role_client::machine::MachinePlacementFacts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCertificateTarget {
    pub machine_id: MachineId,
    pub public_ips: Vec<IpAddr>,
}

pub(crate) fn gateway_certificate_targets(
    active_machines: &[ActiveMachineState],
    placement_facts: &[MachinePlacementFacts],
) -> Vec<GatewayCertificateTarget> {
    let fresh_public_ips = placement_facts
        .iter()
        .filter_map(|facts| {
            facts
                .answer
                .as_ref()
                .and_then(|answer| answer.endpoints.as_ref())
                .map(|endpoints| {
                    (
                        facts.machine_id.clone(),
                        endpoints
                            .control_endpoints
                            .iter()
                            .copied()
                            .collect::<BTreeSet<_>>()
                            .into_iter()
                            .collect::<Vec<_>>(),
                    )
                })
        })
        .collect::<BTreeMap<_, _>>();

    active_machines
        .iter()
        .filter(|machine| matches!(machine.roles.gateway, GatewayRole::Install))
        .map(|machine| GatewayCertificateTarget {
            machine_id: machine.machine_id.clone(),
            public_ips: fresh_public_ips
                .get(&machine.machine_id)
                .cloned()
                .unwrap_or_default(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use ployz_core::ids::MachineId;
    use ployz_core::intent::ActiveMachineState;
    use ployz_core::machine::MachineEndpointObservation;
    use ployz_core::machine::MachineLifecycle;
    use ployz_core::machine::MachineName;
    use ployz_core::machine::runtime::MachineContainerObservationSnapshot;
    use ployz_core::network::MachineEndpointSubnet;
    use ployz_core::roles::InstallRolePolicy;

    use ployz_test_support::ids::operation_id;

    use super::*;

    #[test]
    fn silent_intent_gateway_is_retained_and_foreign_responder_is_excluded() {
        let gateway = active_gateway("gateway_a");
        let foreign_id = MachineId::try_new("foreign_a").expect("valid machine id");
        let foreign_facts = MachinePlacementFacts {
            machine_id: foreign_id.clone(),
            lifecycle: MachineLifecycle::Active,
            answer: Some(
                crate::control::role_client::machine::MachinePlacementFactsAnswer {
                    containers: MachineContainerObservationSnapshot::try_new(
                        foreign_id.clone(),
                        [],
                    )
                    .expect("valid empty snapshot"),
                    platform: ployz_core::image::OciPlatform {
                        os: "linux".to_owned(),
                        architecture: "amd64".to_owned(),
                    },
                    endpoints: Some(MachineEndpointObservation {
                        machine_id: foreign_id,
                        control_endpoints: vec!["203.0.113.50".parse().expect("valid IP")],
                        mesh_endpoints: Vec::new(),
                    }),
                },
            ),
        };

        assert_eq!(
            gateway_certificate_targets(std::slice::from_ref(&gateway), &[foreign_facts]),
            [GatewayCertificateTarget {
                machine_id: gateway.machine_id,
                public_ips: Vec::new(),
            }]
        );
    }

    fn active_gateway(machine_id: &str) -> ActiveMachineState {
        ActiveMachineState {
            machine_id: MachineId::try_new(machine_id).expect("valid machine id"),
            name: MachineName::try_new(machine_id).expect("valid machine name"),
            activated_by: operation_id("op_machine_add"),
            lifecycle: MachineLifecycle::Active,
            roles: InstallRolePolicy::install_all(),
            control_endpoints: Vec::new(),
            mesh_endpoints: Vec::new(),
            endpoint_subnet: MachineEndpointSubnet::try_new("10.198.1.0/24")
                .expect("valid endpoint subnet"),
            wireguard_public_key: ployz_core::network::WireGuardPublicKey::try_new(format!(
                "public-{machine_id}"
            ))
            .expect("public key"),
        }
    }
}
