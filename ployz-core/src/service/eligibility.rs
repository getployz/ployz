//! Shared placement and storage eligibility policy for Services on Machines.

use crate::{
    Machine, MachineStorageObservation, Placement, RequestedServiceSpec, ResolvedServiceSpec,
    ServiceVolumeGraph, machine_matches_placement,
};

/// Whether one Machine can host a Service under current placement and storage evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServicePlacementEligibility {
    /// Placement matches and every mounted storage requirement is known to be supported.
    Eligible,
    /// The Machine is known not to satisfy one Service requirement.
    Ineligible(ServicePlacementIneligibleReason),
    /// Placement matches, but required storage capability could not be observed.
    Unknown(ServicePlacementUnknownReason),
}

/// Why a Machine is known not to be eligible for a Service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServicePlacementIneligibleReason {
    /// The Machine does not match the Service's placement selectors.
    PlacementMismatch,
    /// The Machine is known not to support mounted Provisioned Volumes.
    ProvisionedStorageUnsupported,
}

/// Why a Machine's eligibility for a Service cannot be determined.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ServicePlacementUnknownReason {
    /// Mounted Provisioned Volumes require storage evidence that is unavailable.
    MissingStorageEvidence,
}

impl RequestedServiceSpec {
    /// Assess this complete Service specification against one Machine.
    ///
    /// Membership Observation is intentionally a consumer concern. Provisioned
    /// maxima are enforced ceilings and are not compared with current free bytes.
    #[must_use]
    pub fn placement_eligibility(
        &self,
        machine: &Machine,
        storage: Option<&MachineStorageObservation>,
    ) -> ServicePlacementEligibility {
        placement_eligibility(&self.placement, self.volume_graph(), machine, storage)
    }
}

impl ResolvedServiceSpec {
    /// Assess this complete Service specification against one Machine.
    ///
    /// Membership Observation is intentionally a consumer concern. Provisioned
    /// maxima are enforced ceilings and are not compared with current free bytes.
    #[must_use]
    pub fn placement_eligibility(
        &self,
        machine: &Machine,
        storage: Option<&MachineStorageObservation>,
    ) -> ServicePlacementEligibility {
        placement_eligibility(&self.placement, self.volume_graph(), machine, storage)
    }
}

/// Evaluate placement constraints and mounted Provisioned Volume capability.
///
/// Membership Observation is intentionally a consumer concern. Provisioned
/// maxima are enforced ceilings and are not compared with current free bytes.
#[must_use]
fn placement_eligibility(
    placement: &Placement,
    volumes: &ServiceVolumeGraph,
    machine: &Machine,
    storage: Option<&MachineStorageObservation>,
) -> ServicePlacementEligibility {
    if !machine_matches_placement(machine, placement) {
        return ServicePlacementEligibility::Ineligible(
            ServicePlacementIneligibleReason::PlacementMismatch,
        );
    }
    if !volumes.has_mounted_provisioned_volume() {
        return ServicePlacementEligibility::Eligible;
    }
    match storage {
        Some(MachineStorageObservation::Ready | MachineStorageObservation::Pool { .. }) => {
            ServicePlacementEligibility::Eligible
        }
        Some(MachineStorageObservation::Stateless) => ServicePlacementEligibility::Ineligible(
            ServicePlacementIneligibleReason::ProvisionedStorageUnsupported,
        ),
        None => ServicePlacementEligibility::Unknown(
            ServicePlacementUnknownReason::MissingStorageEvidence,
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, num::NonZeroU64};

    use serde_json::json;

    use crate::{
        ContainerPath, DockerVolumeName, Machine, MachineId, MachineName,
        MachineStorageObservation, Placement, ProvisionedVolumeMaximumBytes, RequestedServiceSpec,
        ResolvedUpdateConfig, ServiceId, ServiceMode, ServiceMount, ServiceVolume,
        ServiceVolumeGraph, ServiceVolumeReference, VolumeSource, WireGuardPublicKey,
    };

    use super::{
        ServicePlacementEligibility, ServicePlacementIneligibleReason,
        ServicePlacementUnknownReason,
    };

    #[test]
    fn whole_specs_assess_placement_and_only_mounted_provisioned_storage() {
        let machine = machine("storage");
        let other = Placement {
            machines: vec![crate::MachineTarget::parse("other").unwrap()],
        };
        let provisioned = volume_graph(
            crate::RawVolumeSource::Provisioned {
                name: DockerVolumeName::parse("data").unwrap(),
                maximum_bytes: ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(100).unwrap()),
                labels: BTreeMap::new(),
            }
            .admit()
            .expect("valid volume declaration"),
            true,
        );
        let unused_provisioned = volume_graph(
            crate::RawVolumeSource::Provisioned {
                name: DockerVolumeName::parse("unused").unwrap(),
                maximum_bytes: ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(100).unwrap()),
                labels: BTreeMap::new(),
            }
            .admit()
            .expect("valid volume declaration"),
            false,
        );
        let external = volume_graph(
            crate::RawVolumeSource::External {
                name: DockerVolumeName::parse("external").unwrap(),
            }
            .admit()
            .expect("valid volume declaration"),
            true,
        );
        let pool = MachineStorageObservation::Pool {
            size_bytes: NonZeroU64::new(10).unwrap(),
            used_bytes: 9,
            free_bytes: 1,
        };
        let cases = [
            (
                requested(other, provisioned.clone()),
                Some(MachineStorageObservation::Ready),
                ServicePlacementEligibility::Ineligible(
                    ServicePlacementIneligibleReason::PlacementMismatch,
                ),
            ),
            (
                requested(Placement::default(), ServiceVolumeGraph::default()),
                None,
                ServicePlacementEligibility::Eligible,
            ),
            (
                requested(Placement::default(), provisioned.clone()),
                Some(MachineStorageObservation::Ready),
                ServicePlacementEligibility::Eligible,
            ),
            (
                requested(Placement::default(), provisioned.clone()),
                Some(pool),
                ServicePlacementEligibility::Eligible,
            ),
            (
                requested(Placement::default(), provisioned.clone()),
                Some(MachineStorageObservation::Stateless),
                ServicePlacementEligibility::Ineligible(
                    ServicePlacementIneligibleReason::ProvisionedStorageUnsupported,
                ),
            ),
            (
                requested(Placement::default(), provisioned),
                None,
                ServicePlacementEligibility::Unknown(
                    ServicePlacementUnknownReason::MissingStorageEvidence,
                ),
            ),
            (
                requested(Placement::default(), unused_provisioned),
                None,
                ServicePlacementEligibility::Eligible,
            ),
            (
                requested(Placement::default(), external),
                None,
                ServicePlacementEligibility::Eligible,
            ),
        ];

        for (mut requested, storage, expected) in cases {
            requested
                .set_volume_graph(
                    requested
                        .volume_graph()
                        .clone()
                        .scope_to_project(&crate::ProjectName::parse("shop").unwrap())
                        .unwrap(),
                )
                .unwrap();
            assert_eq!(
                requested.placement_eligibility(&machine, storage.as_ref()),
                expected
            );
            assert_eq!(
                requested
                    .to_resolved(ServiceId::random(), ResolvedUpdateConfig::default())
                    .expect("volume graph is scoped")
                    .placement_eligibility(&machine, storage.as_ref()),
                expected
            );
        }
    }

    fn machine(name: &str) -> Machine {
        Machine {
            id: MachineId::parse("1".repeat(32)).unwrap(),
            name: MachineName::parse(name).unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
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

    fn requested(placement: Placement, volume_graph: ServiceVolumeGraph) -> RequestedServiceSpec {
        let mode = serde_json::to_value(ServiceMode::Global).unwrap();
        let mut spec: RequestedServiceSpec = serde_json::from_value(json!({
            "name": "api",
            "mode": mode,
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
        }))
        .unwrap();
        spec.placement = placement;
        spec.set_volume_graph(volume_graph).unwrap();
        spec
    }
}
