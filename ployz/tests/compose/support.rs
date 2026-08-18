use std::net::Ipv6Addr;

use ployz::{
    compose::ComposeProject,
    deploy::{DeployIntent, DeployPlan, DeploySnapshot, PlanError, PlanOptions, plan_deploy},
};
use ployz_core::{
    AdvertisedEndpoint, Machine, MachineId, MachineName, MachineObservation, ManagementAddress,
    MembershipObservation, WireGuardPublicKey,
};

pub(super) fn plan_compose(
    project: &ComposeProject,
    snapshot: &DeploySnapshot,
) -> Result<DeployPlan, PlanError> {
    let mut resolved = project.clone();
    resolved.resolve_secrets().expect("resolve secrets");
    plan_deploy(
        &DeployIntent::from_named_specs(
            &resolved.services,
            &resolved.dependencies,
            resolved
                .services
                .values()
                .map(|spec| ployz::deploy::ServiceAttempt {
                    name: spec.name.clone(),
                })
                .collect(),
            PlanOptions::default(),
        ),
        snapshot,
    )
}

pub(super) fn machine(hex: char, name: &str) -> MachineObservation {
    MachineObservation {
        machine: Machine {
            id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                .parse()
                .unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([hex as u8; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        membership: MembershipObservation::Up,
        selected_endpoint: None,
        rtt: None,
    }
}

pub(super) fn service<'a>(
    project: &'a ComposeProject,
    name: &str,
) -> &'a ployz_core::RequestedServiceSpec {
    project.services.get(name).unwrap()
}
