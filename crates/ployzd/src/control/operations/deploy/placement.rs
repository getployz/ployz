use std::collections::BTreeMap;

use ployz_core::ids::MachineId;
use ployz_core::machine::MachineUsabilityReason;
use ployz_core::machine::{
    StorageCompatibility, StorageTestimony, classify_storage_compatibility,
    validate_placement_machine_peers,
};
use ployz_core::network::{DataplaneProjection, MachineDataplaneStatus};
use ployz_core::operation::{FailureMessage, UnusableMachine};

use crate::control::operations::local_execution_admission::{
    classify_local_execution_admission, dataplane_admission_failure,
};
use crate::control::role_client::machine::MachinePlacementFacts;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProvisionedStorageRequirement {
    None,
    Ready {
        expected_pools: Vec<ployz_core::deploy::ZfsPoolName>,
    },
}

pub(super) fn classify_storage_usability(
    candidates: &[MachineId],
    testimony: &BTreeMap<MachineId, Option<ployz_core::machine::StorageCapability>>,
    requirement: &ProvisionedStorageRequirement,
) -> (Vec<MachineId>, Vec<UnusableMachine>) {
    let ProvisionedStorageRequirement::Ready { expected_pools } = requirement else {
        return (candidates.to_vec(), Vec::new());
    };
    let mut eligible = Vec::new();
    let mut unusable = Vec::new();
    for machine_id in candidates {
        let storage = testimony
            .get(machine_id)
            .map_or(StorageTestimony::NoAnswer, |storage| {
                StorageTestimony::Answered(storage.as_ref())
            });
        let compatibility = if expected_pools.is_empty() {
            classify_storage_compatibility(storage, None)
        } else {
            expected_pools
                .iter()
                .map(|expected| classify_storage_compatibility(storage, Some(expected)))
                .find(|compatibility| *compatibility != StorageCompatibility::Compatible)
                .unwrap_or(StorageCompatibility::Compatible)
        };
        let reason = match compatibility {
            StorageCompatibility::Compatible => None,
            StorageCompatibility::MachineSilent => Some(MachineUsabilityReason::FactsUnavailable),
            StorageCompatibility::TestimonyNotReported => {
                Some(MachineUsabilityReason::StorageTestimonyNotReported)
            }
            StorageCompatibility::Unprepared => Some(MachineUsabilityReason::StorageUnprepared),
            StorageCompatibility::Unavailable { reason } => {
                Some(MachineUsabilityReason::StorageUnavailable { reason })
            }
            StorageCompatibility::PoolMismatch { expected, reported } => {
                Some(MachineUsabilityReason::StoragePoolMismatch { expected, reported })
            }
        };
        match reason {
            None => eligible.push(machine_id.clone()),
            Some(reason) => unusable.push(UnusableMachine {
                machine_id: machine_id.clone(),
                reason,
            }),
        }
    }
    (eligible, unusable)
}

pub(super) fn classify_machine_usability(
    placement_facts: &[MachinePlacementFacts],
    projection: &DataplaneProjection,
    dataplane_statuses: &[(MachineId, Result<MachineDataplaneStatus, FailureMessage>)],
) -> (Vec<MachineId>, Vec<UnusableMachine>) {
    let (preliminary, unusable) =
        classify_local_execution_admission(placement_facts, projection, dataplane_statuses);
    let mut unusable = unusable
        .into_iter()
        .map(|machine| (machine.machine_id, machine.reason))
        .collect::<BTreeMap<_, _>>();

    let fixed_placement_set = preliminary
        .iter()
        .map(|candidate| candidate.member)
        .collect::<Vec<_>>();
    let mut eligible = Vec::new();
    for candidate in preliminary {
        match validate_placement_machine_peers(
            &fixed_placement_set,
            candidate.member,
            candidate.status,
        ) {
            None => eligible.push(candidate.machine_id.clone()),
            Some(failure) => {
                unusable.insert(
                    candidate.machine_id.clone(),
                    dataplane_admission_failure(failure),
                );
            }
        }
    }

    let unusable = unusable
        .into_iter()
        .map(|(machine_id, reason)| UnusableMachine { machine_id, reason })
        .collect();
    (eligible, unusable)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::machine::MachineLifecycle;
    use ployz_core::machine::runtime::MachineContainerObservationSnapshot;
    use ployz_core::machine::{
        DataplaneProjectionAdmissionFailure, DataplaneUnavailableReason, StorageCapability,
        StorageUnavailableReason,
    };
    use ployz_core::network::{
        DataplaneProjectionMember, DataplaneProjectionRevisions, DataplaneProjectionTestimony,
        EbpfAttachmentStatus, EndpointBridgeStatus, MachineEndpointSubnet,
        NativeDataplaneProjectionStatus, WireGuardConfiguredMtu, WireGuardDetectedMtu,
        WireGuardHandshakeStatus, WireGuardInterfaceMtu, WireGuardMtuProbe,
        WireGuardPeerEndpointSubnet, WireGuardPeerStatus, WireGuardPublicKey, WireGuardRttStatus,
        WireGuardStatus,
    };

    #[test]
    fn placement_preserves_dataplane_no_answer_message() {
        let machine = machine_id("machine_a");
        let projection = projection_for(&machine);
        let message =
            FailureMessage::try_new("dataplane status request timed out").expect("failure message");

        let (_, unusable) = classify_machine_usability(
            &[answering_facts(machine.clone())],
            &projection,
            &[(machine.clone(), Err(message.clone()))],
        );

        assert_eq!(
            unusable,
            vec![UnusableMachine {
                machine_id: machine,
                reason: dataplane_admission_failure(
                    DataplaneProjectionAdmissionFailure::NoAnswer { message }
                ),
            }]
        );
    }

    #[test]
    fn provisioned_storage_filters_only_when_required_and_preserves_typed_reasons() {
        let ready = machine_id("ready");
        let legacy = machine_id("legacy");
        let unprepared = machine_id("unprepared");
        let faulted = machine_id("faulted");
        let candidates = vec![
            ready.clone(),
            legacy.clone(),
            unprepared.clone(),
            faulted.clone(),
        ];
        let pool = ployz_core::deploy::ZfsPoolName::try_new("tank").expect("valid pool");
        let testimony = BTreeMap::from([
            (
                ready.clone(),
                Some(StorageCapability::Ready {
                    pool: pool.clone(),
                    capacity: ployz_core::machine::PoolCapacityFacts {
                        total_bytes: 1024,
                        provisioned_used_bytes: 0,
                        free_bytes: 1024,
                        child_quotas: Vec::new(),
                    },
                }),
            ),
            (legacy.clone(), None),
            (unprepared.clone(), Some(StorageCapability::Unprepared)),
            (
                faulted.clone(),
                Some(StorageCapability::Unavailable {
                    reason: StorageUnavailableReason::PoolFaulted { pool: pool.clone() },
                }),
            ),
        ]);

        assert_eq!(
            classify_storage_usability(
                &candidates,
                &testimony,
                &ProvisionedStorageRequirement::None,
            ),
            (candidates.clone(), Vec::new())
        );
        let (eligible, unusable) = classify_storage_usability(
            &candidates,
            &testimony,
            &ProvisionedStorageRequirement::Ready {
                expected_pools: vec![pool.clone()],
            },
        );
        assert_eq!(eligible, vec![ready]);
        assert!(matches!(
            unusable.as_slice(),
            [
                UnusableMachine {
                    reason: MachineUsabilityReason::StorageTestimonyNotReported,
                    ..
                },
                UnusableMachine {
                    reason: MachineUsabilityReason::StorageUnprepared,
                    ..
                },
                UnusableMachine {
                    reason: MachineUsabilityReason::StorageUnavailable { .. },
                    ..
                },
            ]
        ));

        let (_, mismatch) = classify_storage_usability(
            &[machine_id("ready")],
            &testimony,
            &ProvisionedStorageRequirement::Ready {
                expected_pools: vec![
                    ployz_core::deploy::ZfsPoolName::try_new("other").expect("valid pool"),
                ],
            },
        );
        assert!(matches!(
            mismatch.as_slice(),
            [UnusableMachine {
                reason: MachineUsabilityReason::StoragePoolMismatch { .. },
                ..
            }]
        ));
    }

    #[test]
    fn placement_distinguishes_projection_and_gather_shape_mismatches() {
        let machine = machine_id("machine_a");
        let empty_projection =
            DataplaneProjection::try_new(Vec::new(), None).expect("empty projection is valid");
        let (_, not_declared) =
            classify_machine_usability(&[answering_facts(machine.clone())], &empty_projection, &[]);
        assert!(matches!(
            not_declared.as_slice(),
            [UnusableMachine {
                reason: MachineUsabilityReason::DataplaneUnavailable {
                    reason: DataplaneUnavailableReason::NotDeclared,
                },
                ..
            }]
        ));

        let (_, missing_testimony) = classify_machine_usability(
            &[answering_facts(machine.clone())],
            &projection_for(&machine),
            &[],
        );
        assert!(matches!(
            missing_testimony.as_slice(),
            [UnusableMachine {
                reason: MachineUsabilityReason::DataplaneUnavailable {
                    reason: DataplaneUnavailableReason::TestimonyMissing,
                },
                ..
            }]
        ));
    }

    #[test]
    fn silent_declared_machine_does_not_poison_connected_placement_candidates() {
        let projection = three_machine_projection();
        let core = machine_id("core");
        let edge = machine_id("edge");
        let silent = machine_id("silent");
        let facts = vec![
            answering_facts(core.clone()),
            answering_facts(edge.clone()),
            silent_facts(silent.clone()),
        ];
        let statuses = vec![
            (
                core.clone(),
                Ok(ready_status(
                    &projection,
                    "core",
                    &[
                        ("edge", WireGuardHandshakeStatus::Ago { seconds: 1 }),
                        ("silent", WireGuardHandshakeStatus::Never),
                    ],
                )),
            ),
            (
                edge.clone(),
                Ok(ready_status(
                    &projection,
                    "edge",
                    &[
                        ("core", WireGuardHandshakeStatus::Ago { seconds: 1 }),
                        ("silent", WireGuardHandshakeStatus::Never),
                    ],
                )),
            ),
        ];

        let (eligible, unusable) = classify_machine_usability(&facts, &projection, &statuses);

        assert_eq!(eligible, vec![core, edge]);
        assert_eq!(
            unusable,
            vec![UnusableMachine {
                machine_id: silent,
                reason: MachineUsabilityReason::FactsUnavailable,
            }]
        );
    }

    #[test]
    fn placement_peer_validation_does_not_rescue_candidates_after_exclusion() {
        let projection = three_machine_projection();
        let core = machine_id("core");
        let edge = machine_id("edge");
        let isolated = machine_id("silent");
        let facts = vec![
            answering_facts(core.clone()),
            answering_facts(edge.clone()),
            answering_facts(isolated.clone()),
        ];
        let statuses = vec![
            (
                core.clone(),
                Ok(ready_status(
                    &projection,
                    "core",
                    &[("edge", WireGuardHandshakeStatus::Ago { seconds: 1 })],
                )),
            ),
            (
                edge.clone(),
                Ok(ready_status(
                    &projection,
                    "edge",
                    &[
                        ("core", WireGuardHandshakeStatus::Ago { seconds: 1 }),
                        ("silent", WireGuardHandshakeStatus::Ago { seconds: 1 }),
                    ],
                )),
            ),
            (
                isolated.clone(),
                Ok(ready_status(
                    &projection,
                    "silent",
                    &[("edge", WireGuardHandshakeStatus::Ago { seconds: 1 })],
                )),
            ),
        ];

        let (eligible, unusable) = classify_machine_usability(&facts, &projection, &statuses);

        assert_eq!(eligible, vec![edge]);
        let [core_unusable, isolated_unusable] = unusable.as_slice() else {
            panic!("expected exactly two unusable machines: {unusable:?}");
        };
        assert_eq!(core_unusable.machine_id, core);
        assert_eq!(isolated_unusable.machine_id, isolated);
        assert!(unusable.iter().all(|machine| matches!(
            machine.reason,
            MachineUsabilityReason::DataplaneUnavailable {
                reason: DataplaneUnavailableReason::Admission {
                    failure: DataplaneProjectionAdmissionFailure::PeerSetMismatch { .. },
                },
            }
        )));
    }

    fn answering_facts(machine_id: MachineId) -> MachinePlacementFacts {
        MachinePlacementFacts {
            answer: Some(
                crate::control::role_client::machine::MachinePlacementFactsAnswer {
                    containers: MachineContainerObservationSnapshot::try_new(
                        machine_id.clone(),
                        [],
                    )
                    .expect("empty observation snapshot"),
                    platform: ployz_core::image::OciPlatform::try_new("linux", "amd64")
                        .expect("platform"),
                    endpoints: None,
                    storage: None,
                    build: crate::roles::machine::protocol::MachineBuildCapability::Available,
                    clock: crate::control::role_client::machine::MachineClockTestimony {
                        control_request_started_at_unix_ms: 1,
                        machine_observed_at_unix_ms: 1,
                    },
                },
            ),
            machine_id,
            lifecycle: MachineLifecycle::Active,
        }
    }

    fn silent_facts(machine_id: MachineId) -> MachinePlacementFacts {
        MachinePlacementFacts {
            answer: None,
            machine_id,
            lifecycle: MachineLifecycle::Active,
        }
    }

    fn three_machine_projection() -> DataplaneProjection {
        DataplaneProjection::try_new(
            [("core", 1_u8), ("edge", 2), ("silent", 3)]
                .into_iter()
                .map(|(machine, octet)| DataplaneProjectionMember {
                    machine_id: machine_id(machine),
                    endpoint_subnet: MachineEndpointSubnet::try_new(format!("10.198.{octet}.0/24"))
                        .expect("endpoint subnet"),
                    mesh_endpoints: vec![
                        format!("192.0.2.{octet}:51820")
                            .parse()
                            .expect("mesh endpoint"),
                    ],
                    wireguard_public_key: WireGuardPublicKey::try_new(format!("public-{machine}"))
                        .expect("wireguard public key"),
                })
                .collect(),
            None,
        )
        .expect("projection")
    }

    fn ready_status(
        projection: &DataplaneProjection,
        local: &str,
        peers: &[(&str, WireGuardHandshakeStatus)],
    ) -> MachineDataplaneStatus {
        let local = projection_member(projection, local);
        MachineDataplaneStatus {
            projection: NativeDataplaneProjectionStatus {
                endpoint_bridge: EndpointBridgeStatus::Ready {
                    subnet: local.endpoint_subnet.clone(),
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
                peers: peers
                    .iter()
                    .map(|(peer, handshake)| {
                        let peer = projection_member(projection, peer);
                        WireGuardPeerStatus {
                            public_key: peer.wireguard_public_key.clone(),
                            endpoint_subnet: WireGuardPeerEndpointSubnet::Valid {
                                subnet: peer.endpoint_subnet.clone(),
                            },
                            endpoint: peer.mesh_endpoints.first().copied(),
                            handshake: *handshake,
                            rtt: WireGuardRttStatus::Unavailable {
                                message: "not measured".to_owned(),
                            },
                            rx_bytes: 0,
                            tx_bytes: 0,
                            mtu_probe: WireGuardMtuProbe::NotRequested,
                        }
                    })
                    .collect(),
            },
            ebpf_attachment: EbpfAttachmentStatus::Attached,
        }
    }

    fn projection_member<'a>(
        projection: &'a DataplaneProjection,
        machine: &str,
    ) -> &'a DataplaneProjectionMember {
        projection
            .declared_members()
            .iter()
            .find(|member| member.machine_id == machine_id(machine))
            .expect("projection member")
    }

    fn projection_for(machine_id: &MachineId) -> DataplaneProjection {
        DataplaneProjection::try_new(
            vec![DataplaneProjectionMember {
                machine_id: machine_id.clone(),
                endpoint_subnet: MachineEndpointSubnet::try_new("10.198.1.0/24")
                    .expect("endpoint subnet"),
                mesh_endpoints: vec!["192.0.2.1:51820".parse().expect("mesh endpoint")],
                wireguard_public_key: WireGuardPublicKey::try_new("public-machine-a")
                    .expect("wireguard public key"),
            }],
            None,
        )
        .expect("projection")
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("machine id")
    }
}
