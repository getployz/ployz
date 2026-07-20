use ployz_core::build::{
    BuildAdapter, BuildExecutorCapability, BuildPlatforms, ClusterBuildMachineCapability,
};
use ployz_core::ids::MachineId;
use ployz_core::image::OciPlatform;
use ployz_core::machine::MachineUsabilityReason;
use ployz_core::network::{DataplaneProjection, MachineDataplaneStatus};
use ployz_core::operation::{BuildOperationFailure, FailureMessage, UnusableMachine};

use crate::control::operations::local_execution_admission::classify_local_execution_admission;
use crate::control::role_client::machine::MachinePlacementFacts;
use crate::roles::machine::protocol::MachineBuildCapability;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClusterBuildExecutorAssignment {
    pub(crate) platform: OciPlatform,
    pub(crate) machine_id: MachineId,
}

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
) -> Result<Vec<ClusterBuildExecutorAssignment>, Box<BuildOperationFailure>> {
    let capabilities = classify_cluster_build_capabilities(facts, projection, dataplane_statuses);
    let mut assignments = Vec::new();
    for platform in platforms.iter() {
        let mut unusable = Vec::new();
        let mut eligible = Vec::new();
        for candidate in &capabilities {
            match candidate {
                ClusterBuildMachineCapability::Answered {
                    machine_id,
                    native_platform: _,
                    capability,
                } if !capability.supports(adapter) => unusable.push(UnusableMachine {
                    machine_id: machine_id.clone(),
                    reason: MachineUsabilityReason::BuildUnavailable,
                }),
                ClusterBuildMachineCapability::Answered {
                    machine_id,
                    native_platform,
                    capability: _,
                } if *native_platform == *platform => eligible.push(machine_id.clone()),
                ClusterBuildMachineCapability::Answered {
                    machine_id,
                    native_platform,
                    capability: _,
                } => unusable.push(UnusableMachine {
                    machine_id: machine_id.clone(),
                    reason: MachineUsabilityReason::PlatformMismatch {
                        supported: BuildPlatforms::try_new([platform.clone()])
                            .expect("one requested platform is non-empty"),
                        reported: native_platform.clone(),
                    },
                }),
                ClusterBuildMachineCapability::Unavailable { machine_id, reason } => {
                    unusable.push(UnusableMachine {
                        machine_id: machine_id.clone(),
                        reason: reason.clone(),
                    });
                }
            }
        }
        unusable.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
        let Some(machine_id) = eligible.into_iter().next() else {
            return Err(Box::new(BuildOperationFailure::NoEligibleMachine {
                platform: platform.clone(),
                unusable,
            }));
        };
        assignments.push(ClusterBuildExecutorAssignment {
            platform: platform.clone(),
            machine_id,
        });
    }
    Ok(assignments)
}

pub(crate) fn classify_cluster_build_capabilities(
    facts: &[MachinePlacementFacts],
    projection: &DataplaneProjection,
    dataplane_statuses: &[(MachineId, Result<MachineDataplaneStatus, FailureMessage>)],
) -> Vec<ClusterBuildMachineCapability> {
    let (admitted, unusable) =
        classify_local_execution_admission(facts, projection, dataplane_statuses);
    let admitted = admitted
        .into_iter()
        .map(|candidate| ClusterBuildMachineCapability::Answered {
            machine_id: candidate.machine_id.clone(),
            native_platform: candidate.platform.clone(),
            capability: build_executor_capability(candidate.build),
        });
    let unavailable =
        unusable
            .into_iter()
            .map(|candidate| ClusterBuildMachineCapability::Unavailable {
                machine_id: candidate.machine_id,
                reason: candidate.reason,
            });
    let mut capabilities = admitted.chain(unavailable).collect::<Vec<_>>();
    capabilities
        .sort_by(|left, right| capability_machine_id(left).cmp(capability_machine_id(right)));
    capabilities
}

fn capability_machine_id(capability: &ClusterBuildMachineCapability) -> &MachineId {
    match capability {
        ClusterBuildMachineCapability::Answered { machine_id, .. }
        | ClusterBuildMachineCapability::Unavailable { machine_id, .. } => machine_id,
    }
}

fn build_executor_capability(capability: &MachineBuildCapability) -> BuildExecutorCapability {
    match capability {
        MachineBuildCapability::Available => BuildExecutorCapability::DockerfileAndRailpack,
        MachineBuildCapability::RailpackUnavailable => BuildExecutorCapability::DockerfileOnly,
        MachineBuildCapability::Unavailable => BuildExecutorCapability::RuntimeUnavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::build::{
        BuildCacheScope, BuildContextPath, BuildExecutorAssignment, BuildExecutorId,
        BuildPlatformExecutorAssignment, BuildPoolId, ExternalBuildExecutorCandidate,
        ExternalBuildPlacementError, place_external_build_platforms,
    };
    use ployz_core::machine::MachineLifecycle;
    use ployz_core::network::{
        DataplaneProjectionMember, DataplaneProjectionRevisions, DataplaneProjectionTestimony,
        EbpfAttachmentStatus, EndpointBridgeStatus, MachineEndpointSubnet,
        NativeDataplaneProjectionStatus, WireGuardConfiguredMtu, WireGuardDetectedMtu,
        WireGuardInterfaceMtu, WireGuardPublicKey, WireGuardStatus,
    };
    use std::collections::BTreeSet;

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
                .map(|assignment| (assignment.platform.clone(), assignment.machine_id.as_str(),))
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
            placement
                .first()
                .map(|assignment| assignment.machine_id.as_str()),
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
            placement
                .first()
                .map(|assignment| assignment.machine_id.as_str()),
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
            placement
                .first()
                .map(|assignment| assignment.machine_id.as_str()),
            Some("a-no-railpack")
        );
    }

    #[test]
    fn capability_testimony_preserves_adapter_platform_and_silence() {
        let amd64 = platform("linux", "amd64");
        let facts = vec![
            answering_with_build_capability(
                "dockerfile-only",
                amd64.clone(),
                MachineBuildCapability::RailpackUnavailable,
            ),
            silent("silent"),
        ];
        let (projection, statuses) = ready_dataplane(&facts);

        assert_eq!(
            classify_cluster_build_capabilities(&facts, &projection, &statuses),
            vec![
                ClusterBuildMachineCapability::Answered {
                    machine_id: MachineId::try_new("dockerfile-only").expect("machine id"),
                    native_platform: amd64,
                    capability: BuildExecutorCapability::DockerfileOnly,
                },
                ClusterBuildMachineCapability::Unavailable {
                    machine_id: MachineId::try_new("silent").expect("machine id"),
                    reason: MachineUsabilityReason::FactsUnavailable,
                },
            ]
        );
    }

    #[test]
    fn external_placement_requires_known_capability_then_reachable_seed() {
        let pool_id = BuildPoolId::try_new("pool-a").expect("pool id");
        let amd64 = platform("linux", "amd64");
        let platforms = BuildPlatforms::try_new([amd64.clone()]).expect("platforms");

        assert_eq!(
            place_external_build_platforms(&pool_id, &platforms, &[], &BTreeSet::new()),
            Err(ExternalBuildPlacementError::NoCapableExecutor {
                pool_id: pool_id.clone(),
                platform: amd64.clone(),
            })
        );

        let candidates = vec![ExternalBuildExecutorCandidate {
            pool_id: pool_id.clone(),
            executor_id: BuildExecutorId::try_new("executor-a").expect("executor id"),
            platform: amd64,
        }];
        assert_eq!(
            place_external_build_platforms(&pool_id, &platforms, &candidates, &BTreeSet::new()),
            Err(ExternalBuildPlacementError::NoReachableImageSeed {
                pool_id: pool_id.clone(),
            })
        );
    }

    #[test]
    fn external_placement_is_deterministic_and_excludes_other_pools() {
        let pool_id = BuildPoolId::try_new("pool-a").expect("pool id");
        let amd64 = platform("linux", "amd64");
        let platforms = BuildPlatforms::try_new([amd64.clone()]).expect("platforms");
        let candidates = vec![
            ExternalBuildExecutorCandidate {
                pool_id: BuildPoolId::try_new("other-pool").expect("pool id"),
                executor_id: BuildExecutorId::try_new("executor-0").expect("executor id"),
                platform: amd64.clone(),
            },
            ExternalBuildExecutorCandidate {
                pool_id: pool_id.clone(),
                executor_id: BuildExecutorId::try_new("executor-z").expect("executor id"),
                platform: amd64.clone(),
            },
            ExternalBuildExecutorCandidate {
                pool_id: pool_id.clone(),
                executor_id: BuildExecutorId::try_new("executor-a").expect("executor id"),
                platform: amd64.clone(),
            },
        ];
        let seeds = [
            MachineId::try_new("seed-z").expect("machine id"),
            MachineId::try_new("seed-a").expect("machine id"),
        ]
        .into_iter()
        .collect();

        assert_eq!(
            place_external_build_platforms(&pool_id, &platforms, &candidates, &seeds),
            Ok(vec![BuildPlatformExecutorAssignment {
                platform: amd64,
                executor: BuildExecutorAssignment::External {
                    pool_id,
                    executor_id: BuildExecutorId::try_new("executor-a").expect("executor id"),
                    image_seed: MachineId::try_new("seed-a").expect("machine id"),
                },
            }])
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
