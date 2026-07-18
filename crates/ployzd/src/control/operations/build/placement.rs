use std::collections::{BTreeMap, BTreeSet};

use ployz_core::build::{BuildAdapter, BuildPlatforms};
use ployz_core::ids::MachineId;
use ployz_core::image::OciPlatform;
use ployz_core::machine::MachineUsabilityReason;
use ployz_core::network::{DataplaneProjection, MachineDataplaneStatus};
use ployz_core::operation::{BuildOperationFailure, FailureMessage, UnusableMachine};

use crate::control::operations::local_execution_admission::classify_local_execution_admission;
use crate::control::role_client::machine::MachinePlacementFacts;
use crate::roles::machine::protocol::MachineBuildCapability;

/// Resolves every requested platform before any machine work starts.
///
/// Facts are a fresh gather over the intent-known set. Input and output are
/// ordered, making the lowest eligible machine id the stable winner for a
/// platform regardless of RPC completion order.
pub(crate) fn place_build_platforms(
    platforms: &BuildPlatforms,
    adapter: &BuildAdapter,
    facts: &[MachinePlacementFacts],
    projection: &DataplaneProjection,
    dataplane_statuses: &[(MachineId, Result<MachineDataplaneStatus, FailureMessage>)],
) -> Result<BTreeMap<OciPlatform, MachineId>, Box<BuildOperationFailure>> {
    let build_unavailable = facts
        .iter()
        .filter_map(|candidate| {
            let answer = candidate.answer.as_ref()?;
            (!build_capability_supports_adapter(&answer.build, adapter))
                .then_some(&candidate.machine_id)
        })
        .collect::<BTreeSet<_>>();
    let (admitted, shared_unusable) =
        classify_local_execution_admission(facts, projection, dataplane_statuses);
    let mut by_platform = BTreeMap::new();
    for platform in platforms.iter() {
        let mut unusable = shared_unusable.clone();
        let mut eligible = Vec::new();
        for candidate in &admitted {
            if build_unavailable.contains(candidate.machine_id) {
                unusable.push(UnusableMachine {
                    machine_id: candidate.machine_id.clone(),
                    reason: MachineUsabilityReason::BuildUnavailable,
                });
                continue;
            }
            if *candidate.platform == *platform {
                eligible.push(candidate.machine_id.clone());
            } else {
                unusable.push(UnusableMachine {
                    machine_id: candidate.machine_id.clone(),
                    reason: MachineUsabilityReason::PlatformMismatch {
                        supported: BuildPlatforms::try_new([platform.clone()])
                            .expect("one requested platform is non-empty"),
                        reported: candidate.platform.clone(),
                    },
                });
            }
        }
        unusable.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
        let Some(machine_id) = eligible.into_iter().next() else {
            return Err(Box::new(BuildOperationFailure::NoEligibleMachine {
                platform: platform.clone(),
                unusable,
            }));
        };
        by_platform.insert(platform.clone(), machine_id);
    }
    Ok(by_platform)
}

fn build_capability_supports_adapter(
    capability: &MachineBuildCapability,
    adapter: &BuildAdapter,
) -> bool {
    match capability {
        MachineBuildCapability::Available => true,
        MachineBuildCapability::Unavailable => false,
        MachineBuildCapability::RailpackUnavailable => match adapter {
            BuildAdapter::Dockerfile {
                dockerfile: _,
                target: _,
            } => true,
            BuildAdapter::Railpack { cache_scope: _ } => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::{BuildCacheScope, BuildContextPath};
    use ployz_core::machine::MachineLifecycle;
    use ployz_core::network::{
        DataplaneProjectionMember, DataplaneProjectionRevisions, DataplaneProjectionTestimony,
        EbpfAttachmentStatus, EndpointBridgeStatus, MachineEndpointSubnet,
        NativeDataplaneProjectionStatus, WireGuardConfiguredMtu, WireGuardDetectedMtu,
        WireGuardInterfaceMtu, WireGuardPublicKey, WireGuardStatus,
    };

    use crate::control::role_client::machine::{
        MachinePlacementFacts, MachinePlacementFactsAnswer,
    };

    type DataplaneStatuses = Vec<(MachineId, Result<MachineDataplaneStatus, FailureMessage>)>;

    #[test]
    fn placement_is_deterministic_and_requires_fresh_native_platform_facts() {
        let amd64 = platform("linux", "amd64");
        let arm64 = platform("linux", "arm64");
        let platforms = BuildPlatforms::try_new([arm64.clone(), amd64.clone()])
            .expect("distinct build platforms");
        let facts = vec![
            silent("silent"),
            answering("z-amd64", amd64.clone()),
            answering("a-amd64", amd64.clone()),
            answering("arm64", arm64.clone()),
        ];

        let projection = DataplaneProjection::try_new(Vec::new(), None).expect("projection");
        let failure =
            place_build_platforms(&platforms, &railpack_adapter(), &facts, &projection, &[])
                .expect_err("machines without local dataplane are rejected");
        assert!(matches!(
            *failure,
            BuildOperationFailure::NoEligibleMachine { unusable, .. }
                if unusable.iter().any(|machine| matches!(
                    machine.reason,
                    MachineUsabilityReason::DataplaneUnavailable { .. }
                ))
        ));

        let (projection, statuses) = ready_dataplane(&facts);
        let placement = place_build_platforms(
            &platforms,
            &railpack_adapter(),
            &facts,
            &projection,
            &statuses,
        )
        .expect("complete placement");
        assert_eq!(
            placement
                .iter()
                .map(|(platform, machine)| (platform.clone(), machine.as_str()))
                .collect::<Vec<_>>(),
            vec![(amd64, "a-amd64"), (arm64, "arm64")]
        );
    }

    #[test]
    fn placement_fails_before_fanout_when_any_platform_has_no_candidate() {
        let amd64 = platform("linux", "amd64");
        let arm64 = platform("linux", "arm64");
        let platforms = BuildPlatforms::try_new([amd64.clone(), arm64.clone()])
            .expect("distinct build platforms");

        let facts = vec![answering("amd64", amd64), silent("silent")];
        let (projection, statuses) = ready_dataplane(&facts);
        let failure = place_build_platforms(
            &platforms,
            &railpack_adapter(),
            &facts,
            &projection,
            &statuses,
        )
        .expect_err("arm64 has no fresh matching testimony");

        assert!(matches!(
            *failure,
            BuildOperationFailure::NoEligibleMachine { platform, unusable }
                if platform == arm64
                    && unusable.iter().any(|machine| {
                        machine.machine_id.as_str() == "silent"
                            && machine.reason == MachineUsabilityReason::FactsUnavailable
                    })
        ));
    }

    #[test]
    fn placement_skips_an_unavailable_builder_for_an_available_builder() {
        let amd64 = platform("linux", "amd64");
        let platforms = BuildPlatforms::try_new([amd64.clone()]).expect("build platforms");
        let facts = vec![
            answering_with_build_capability(
                "a-unavailable",
                amd64.clone(),
                MachineBuildCapability::Unavailable,
            ),
            answering("b-available", amd64.clone()),
        ];
        let (projection, statuses) = ready_dataplane(&facts);

        let placement = place_build_platforms(
            &platforms,
            &railpack_adapter(),
            &facts,
            &projection,
            &statuses,
        )
        .expect("available builder is selected");

        assert_eq!(
            placement.get(&amd64).map(MachineId::as_str),
            Some("b-available")
        );
    }

    #[test]
    fn railpack_skips_a_builder_without_railpack_for_a_ready_builder() {
        let amd64 = platform("linux", "amd64");
        let platforms = BuildPlatforms::try_new([amd64.clone()]).expect("build platforms");
        let facts = vec![
            answering_with_build_capability(
                "a-no-railpack",
                amd64.clone(),
                MachineBuildCapability::RailpackUnavailable,
            ),
            answering("b-available", amd64.clone()),
        ];
        let (projection, statuses) = ready_dataplane(&facts);

        let placement = place_build_platforms(
            &platforms,
            &railpack_adapter(),
            &facts,
            &projection,
            &statuses,
        )
        .expect("Railpack-ready builder is selected");

        assert_eq!(
            placement.get(&amd64).map(MachineId::as_str),
            Some("b-available")
        );
    }

    #[test]
    fn dockerfile_may_select_the_lower_id_builder_without_railpack() {
        let amd64 = platform("linux", "amd64");
        let platforms = BuildPlatforms::try_new([amd64.clone()]).expect("build platforms");
        let facts = vec![
            answering_with_build_capability(
                "a-no-railpack",
                amd64.clone(),
                MachineBuildCapability::RailpackUnavailable,
            ),
            answering("b-available", amd64.clone()),
        ];
        let (projection, statuses) = ready_dataplane(&facts);

        let placement = place_build_platforms(
            &platforms,
            &dockerfile_adapter(),
            &facts,
            &projection,
            &statuses,
        )
        .expect("Dockerfile-capable builder is selected");

        assert_eq!(
            placement.get(&amd64).map(MachineId::as_str),
            Some("a-no-railpack")
        );
    }

    #[test]
    fn placement_reports_typed_build_unavailable_evidence_for_every_adapter() {
        let amd64 = platform("linux", "amd64");
        let platforms = BuildPlatforms::try_new([amd64.clone()]).expect("build platforms");
        let facts = vec![answering_with_build_capability(
            "builder",
            amd64.clone(),
            MachineBuildCapability::Unavailable,
        )];
        let (projection, statuses) = ready_dataplane(&facts);

        for adapter in [dockerfile_adapter(), railpack_adapter()] {
            let failure =
                place_build_platforms(&platforms, &adapter, &facts, &projection, &statuses)
                    .expect_err("unavailable build runtime is rejected");

            assert!(matches!(
                *failure,
                BuildOperationFailure::NoEligibleMachine { platform, unusable }
                    if platform == amd64
                        && unusable == vec![UnusableMachine {
                            machine_id: MachineId::try_new("builder").expect("machine id"),
                            reason: MachineUsabilityReason::BuildUnavailable,
                        }]
            ));
        }
    }

    fn answering(machine: &str, platform: OciPlatform) -> MachinePlacementFacts {
        answering_with_build_capability(machine, platform, MachineBuildCapability::Available)
    }

    fn answering_with_build_capability(
        machine: &str,
        platform: OciPlatform,
        build: MachineBuildCapability,
    ) -> MachinePlacementFacts {
        let machine_id = MachineId::try_new(machine).expect("machine id");
        MachinePlacementFacts {
            machine_id: machine_id.clone(),
            lifecycle: MachineLifecycle::Active,
            answer: Some(MachinePlacementFactsAnswer {
                containers:
                    ployz_core::machine::runtime::MachineContainerObservationSnapshot::try_new(
                        machine_id,
                        [],
                    )
                    .expect("empty snapshot"),
                platform,
                endpoints: None,
                storage: None,
                build,
                clock: crate::control::role_client::machine::MachineClockTestimony {
                    control_request_started_at_unix_ms: 1,
                    machine_observed_at_unix_ms: 1,
                },
            }),
        }
    }

    fn silent(machine: &str) -> MachinePlacementFacts {
        MachinePlacementFacts {
            machine_id: MachineId::try_new(machine).expect("machine id"),
            lifecycle: MachineLifecycle::Active,
            answer: None,
        }
    }

    fn dockerfile_adapter() -> BuildAdapter {
        BuildAdapter::Dockerfile {
            dockerfile: BuildContextPath::try_new("Dockerfile").expect("Dockerfile path"),
            target: None,
        }
    }

    fn railpack_adapter() -> BuildAdapter {
        BuildAdapter::Railpack {
            cache_scope: BuildCacheScope::try_new("placement-test").expect("cache scope"),
        }
    }

    fn ready_dataplane(
        facts: &[MachinePlacementFacts],
    ) -> (DataplaneProjection, DataplaneStatuses) {
        let members = facts
            .iter()
            .filter(|facts| facts.answer.is_some())
            .enumerate()
            .map(|(index, facts)| DataplaneProjectionMember {
                machine_id: facts.machine_id.clone(),
                endpoint_subnet: MachineEndpointSubnet::try_new(format!(
                    "10.198.{}.0/24",
                    index + 1
                ))
                .expect("endpoint subnet"),
                mesh_endpoints: vec![
                    format!("192.0.2.{}:51820", index + 1)
                        .parse()
                        .expect("mesh endpoint"),
                ],
                wireguard_public_key: WireGuardPublicKey::try_new(format!(
                    "public-{}",
                    facts.machine_id.as_str()
                ))
                .expect("wireguard public key"),
            })
            .collect::<Vec<_>>();
        let projection = DataplaneProjection::try_new(members, None).expect("projection");
        let statuses = projection
            .declared_members()
            .iter()
            .map(|member| {
                (
                    member.machine_id.clone(),
                    Ok(MachineDataplaneStatus {
                        projection: NativeDataplaneProjectionStatus {
                            endpoint_bridge: EndpointBridgeStatus::Ready {
                                subnet: member.endpoint_subnet.clone(),
                            },
                            testimony: DataplaneProjectionTestimony::Applied {
                                revisions: DataplaneProjectionRevisions {
                                    declared_revision: projection.declared_revision().clone(),
                                    target_revision: projection.target_revision().clone(),
                                },
                            },
                        },
                        wireguard: WireGuardStatus {
                            interface: "ployz-wg0".to_owned(),
                            configured_mtu: WireGuardConfiguredMtu::Auto,
                            detected_mtu: WireGuardDetectedMtu::Detected { mtu: 1420 },
                            interface_mtu: WireGuardInterfaceMtu::Detected { mtu: 1420 },
                            peers: Vec::new(),
                        },
                        ebpf_attachment: EbpfAttachmentStatus::Attached,
                    }),
                )
            })
            .collect();
        (projection, statuses)
    }

    fn platform(os: &str, architecture: &str) -> OciPlatform {
        OciPlatform::try_new(os, architecture).expect("OCI platform")
    }
}
