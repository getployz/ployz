use std::{collections::BTreeMap, net::Ipv6Addr};

use ployz::{
    compose::ComposeProject,
    deploy::{DeployPreview, DeploySnapshot, ObservedDockerVolume, PlanError},
};
use ployz_core::{
    AdvertisedEndpoint, DockerVolumeId, DockerVolumeName, MANAGED_LABEL, Machine, MachineId,
    MachineName, MachineObservation, ManagementAddress, MembershipObservation, PROJECT_NAME_LABEL,
    ProjectName, WireGuardPublicKey,
};

pub(super) fn plan_compose(
    project: &ComposeProject,
    snapshot: &DeploySnapshot,
) -> Result<DeployPreview, PlanError> {
    plan_compose_for(project, snapshot, "app")
}

pub(super) fn plan_compose_for(
    project: &ComposeProject,
    snapshot: &DeploySnapshot,
    project_name: &str,
) -> Result<DeployPreview, PlanError> {
    let mut resolved = project.clone();
    resolved.resolve_secrets().expect("resolve secrets");
    ployz::deploy::plan_compose(
        &resolved,
        snapshot,
        ProjectName::parse(project_name).unwrap(),
    )
}

pub(super) fn operations(preview: &DeployPreview) -> Vec<ployz::deploy::DeployOperation> {
    preview
        .operations
        .iter()
        .map(|row| row.operation.clone())
        .collect()
}

pub(super) fn app_volume(logical: &str) -> DockerVolumeName {
    ProjectName::parse("app")
        .unwrap()
        .volume_name(&DockerVolumeName::parse(logical).unwrap())
}

pub(super) fn snapshot_volume(machine_id: MachineId, name: &str) -> ObservedDockerVolume {
    ObservedDockerVolume {
        id: DockerVolumeId {
            machine_id,
            name: DockerVolumeName::parse(name).unwrap(),
        },
        driver: "local".into(),
        options: BTreeMap::new(),
        labels: BTreeMap::new(),
    }
}

pub(super) fn observed_volume(machine_id: MachineId, logical: &str) -> ObservedDockerVolume {
    ObservedDockerVolume {
        id: DockerVolumeId {
            machine_id,
            name: app_volume(logical),
        },
        driver: "local".into(),
        options: BTreeMap::new(),
        labels: BTreeMap::new(),
    }
}

pub(super) fn owned_volume(machine_id: MachineId, logical: &str) -> ObservedDockerVolume {
    ObservedDockerVolume {
        id: DockerVolumeId {
            machine_id,
            name: app_volume(logical),
        },
        driver: "local".into(),
        options: BTreeMap::new(),
        labels: BTreeMap::from([
            (MANAGED_LABEL.to_owned(), String::new()),
            (PROJECT_NAME_LABEL.to_owned(), "app".to_owned()),
        ]),
    }
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
        storage: None,
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
