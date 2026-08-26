//! Shared placement and storage eligibility policy for Services on Machines.

use crate::{
    Machine, MachineStorageObservation, Placement, ServiceVolumeGraph, machine_matches_placement,
};

/// Whether one Machine can host a Service under current placement and storage evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServicePlacementEligibility {
    /// Placement matches and every mounted storage requirement is known to be supported.
    Eligible,
    /// Placement does not match, or storage is known not to support the Service.
    Ineligible,
    /// Placement matches, but required storage capability could not be observed.
    Unknown,
}

/// Evaluate placement constraints and mounted Provisioned Volume capability.
///
/// Membership Observation is intentionally a consumer concern. Provisioned
/// maxima are enforced ceilings and are not compared with current free bytes.
#[must_use]
pub fn service_placement_eligibility(
    placement: &Placement,
    volumes: &ServiceVolumeGraph,
    machine: &Machine,
    storage: Option<&MachineStorageObservation>,
) -> ServicePlacementEligibility {
    if !machine_matches_placement(machine, placement) {
        return ServicePlacementEligibility::Ineligible;
    }
    if !volumes.has_mounted_provisioned_volume() {
        return ServicePlacementEligibility::Eligible;
    }
    match storage {
        Some(MachineStorageObservation::Ready | MachineStorageObservation::Pool { .. }) => {
            ServicePlacementEligibility::Eligible
        }
        Some(MachineStorageObservation::Stateless) => ServicePlacementEligibility::Ineligible,
        None => ServicePlacementEligibility::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, net::Ipv6Addr, num::NonZeroU64};

    use crate::{
        ContainerPath, DockerVolumeName, Machine, MachineId, MachineName,
        MachineStorageObservation, ManagementAddress, Placement, ProvisionedVolumeMaximumBytes,
        ServiceMount, ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference, VolumeSource,
        WireGuardPublicKey,
    };

    use super::{ServicePlacementEligibility, service_placement_eligibility};

    #[test]
    fn uses_only_mounted_provisioned_storage_capability() {
        let machine = machine("storage");
        let other = Placement {
            machines: vec![crate::MachineTarget::parse("other").unwrap()],
        };
        let provisioned = volume_graph(
            VolumeSource::Provisioned {
                name: DockerVolumeName::parse("data").unwrap(),
                maximum_bytes: ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(100).unwrap()),
                labels: BTreeMap::new(),
            },
            true,
        );
        let unused_provisioned = volume_graph(
            VolumeSource::Provisioned {
                name: DockerVolumeName::parse("unused").unwrap(),
                maximum_bytes: ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(100).unwrap()),
                labels: BTreeMap::new(),
            },
            false,
        );
        let external = volume_graph(
            VolumeSource::Named {
                name: DockerVolumeName::parse("external").unwrap(),
                external: true,
                driver: None,
                labels: BTreeMap::new(),
            },
            true,
        );
        let pool = MachineStorageObservation::Pool {
            size_bytes: NonZeroU64::new(10).unwrap(),
            used_bytes: 9,
            free_bytes: 1,
        };

        assert_eq!(
            [
                service_placement_eligibility(
                    &other,
                    &provisioned,
                    &machine,
                    Some(&MachineStorageObservation::Ready),
                ),
                service_placement_eligibility(
                    &Placement::default(),
                    &ServiceVolumeGraph::default(),
                    &machine,
                    None,
                ),
                service_placement_eligibility(
                    &Placement::default(),
                    &provisioned,
                    &machine,
                    Some(&MachineStorageObservation::Ready),
                ),
                service_placement_eligibility(
                    &Placement::default(),
                    &provisioned,
                    &machine,
                    Some(&pool),
                ),
                service_placement_eligibility(
                    &Placement::default(),
                    &provisioned,
                    &machine,
                    Some(&MachineStorageObservation::Stateless),
                ),
                service_placement_eligibility(&Placement::default(), &provisioned, &machine, None,),
                service_placement_eligibility(
                    &Placement::default(),
                    &unused_provisioned,
                    &machine,
                    None,
                ),
                service_placement_eligibility(&Placement::default(), &external, &machine, None,),
            ],
            [
                ServicePlacementEligibility::Ineligible,
                ServicePlacementEligibility::Eligible,
                ServicePlacementEligibility::Eligible,
                ServicePlacementEligibility::Eligible,
                ServicePlacementEligibility::Ineligible,
                ServicePlacementEligibility::Unknown,
                ServicePlacementEligibility::Eligible,
                ServicePlacementEligibility::Eligible,
            ]
        );
    }

    fn machine(name: &str) -> Machine {
        Machine {
            id: MachineId::parse("1".repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
            management_address: ManagementAddress(Ipv6Addr::LOCALHOST),
            public_key: WireGuardPublicKey([1; 32]),
            public_ip: None,
            advertised_endpoints: Vec::new(),
            runtime: Default::default(),
        }
    }

    fn volume_graph(source: VolumeSource, mounted: bool) -> ServiceVolumeGraph {
        let reference = ServiceVolumeReference::parse("volume").unwrap();
        ServiceVolumeGraph::parse(
            vec![ServiceVolume {
                reference: reference.clone(),
                source,
            }],
            mounted
                .then(|| ServiceMount {
                    volume: reference,
                    target: ContainerPath::parse("/data").unwrap(),
                    read_only: false,
                    no_copy: false,
                    subpath: None,
                })
                .into_iter()
                .collect(),
        )
        .unwrap()
    }
}
