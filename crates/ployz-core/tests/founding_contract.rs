use std::net::{Ipv4Addr, Ipv6Addr};

use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp,
    MachineDocument, MachineStorageIneligibleReason, MachineStorageSelection,
    MachineStorageSelectionReason, MachineTransport, MeshProvider, OperationInitiator,
    OperatorWriteProvenance, PeerDocument, PeerTransport, PloyzDnsTargetState, StorageMode,
    derive_builtin_wireguard_member,
};
use ployz_core::founding::{
    FOUNDING_RESET_COMMAND, FoundingArrival, FoundingDriverEnrollment, FoundingRefusal,
    FoundingRepairCommand, FoundingRequest, FoundingResult, FoundingRow, FoundingValidationError,
    InitStorageChoice, InitStorageFacts, InitStorageSelectionError, MINIMUM_ZFS_MEMORY_BYTES,
    classify_founding_arrival, select_init_storage,
};
use ployz_core::ids::{ClusterId, MachineRowId, PeerId};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};
use serde_json::json;

const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const OTHER_CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const PEER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const MACHINE_KEY: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
const PEER_KEY: &str = "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=";

fn cluster_id(value: &str) -> ClusterId {
    ClusterId::try_new(value).expect("fixture cluster id")
}

fn machine_id() -> MachineRowId {
    MachineRowId::try_new(MACHINE).expect("fixture machine id")
}

fn peer_id() -> PeerId {
    PeerId::try_new(PEER).expect("fixture peer id")
}

fn provenance() -> OperatorWriteProvenance {
    OperatorWriteProvenance {
        written_by: OperationInitiator::Machine {
            machine_id: machine_id(),
        },
        written_at: CorrosionTimestamp::try_new("2026-08-04T12:00:00Z").expect("fixture timestamp"),
    }
}

fn peer_document(cluster_id: &ClusterId) -> PeerDocument {
    let pubkey = WireGuardPublicKey::try_new(PEER_KEY).expect("fixture peer key");
    PeerDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id.clone(),
        provenance: provenance(),
        name: "operator-laptop".to_owned(),
        transport: PeerTransport::Wireguard {
            addr_v6: derive_builtin_wireguard_member(cluster_id, &pubkey)
                .bind_address()
                .get(),
            pubkey,
            endpoint: None,
        },
    }
}

fn founding_request(driver: FoundingDriverEnrollment) -> FoundingRequest {
    let cluster_id = cluster_id(CLUSTER);
    let pubkey = WireGuardPublicKey::try_new(MACHINE_KEY).expect("fixture machine key");
    FoundingRequest {
        cluster_id: cluster_id.clone(),
        cluster: ClusterDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id.clone(),
            provenance: provenance(),
            name: "ares".to_owned(),
            storage_default: StorageMode::Plain,
            hostname_mode: AutomaticHostnameMode::Ployz,
            ployz_dns_target: PloyzDnsTargetState::Pending,
            prefix: MachineEndpointSupernet::default_v1(),
            provider: MeshProvider::BuiltinWireguard,
            acme_directory_url: "https://acme.example/directory".to_owned(),
            acme_contact: None,
        },
        machine_id: machine_id(),
        machine: MachineDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster_id.clone(),
            provenance: provenance(),
            name: MachineName::try_new("ares").expect("fixture machine name"),
            lifecycle: MachineLifecycle::Active,
            transport: MachineTransport::Wireguard {
                addr_v6: derive_builtin_wireguard_member(&cluster_id, &pubkey)
                    .bind_address()
                    .get(),
                pubkey,
                endpoint: None,
                subnet_v4: MachineEndpointSubnet::try_new("10.210.0.0/24")
                    .expect("fixture first subnet"),
            },
            storage: MachineStorageSelection {
                mode: StorageMode::Plain,
                reason: MachineStorageSelectionReason::Default,
            },
        },
        driver,
    }
}

#[test]
fn on_host_founding_has_no_peer_while_ssh_and_cloud_carry_public_peer_rows() {
    let on_host = founding_request(FoundingDriverEnrollment::OnHost);
    assert_eq!(on_host.driver.enrolled_peer(), None);
    assert!(on_host.clone().try_validate().is_ok());
    assert_eq!(
        serde_json::to_value(&on_host.driver).expect("on-host driver serializes"),
        json!({ "kind": "on_host" })
    );

    for driver in [
        FoundingDriverEnrollment::Ssh {
            peer_id: peer_id(),
            document: peer_document(&cluster_id(CLUSTER)),
        },
        FoundingDriverEnrollment::Cloud {
            peer_id: peer_id(),
            document: peer_document(&cluster_id(CLUSTER)),
        },
    ] {
        let request = founding_request(driver);
        let (enrolled_id, enrolled_document) = request
            .driver
            .enrolled_peer()
            .expect("assisted init enrolls its driver");
        assert_eq!(enrolled_id, &peer_id());
        assert_eq!(enrolled_document.name, "operator-laptop");
        assert!(request.clone().try_validate().is_ok());

        let wire = serde_json::to_string(&request).expect("founding request serializes");
        assert!(!wire.contains("callback"));
        assert!(!wire.contains("cloud_token"));
        assert!(!wire.contains("secret"));
        assert!(serde_json::from_str::<FoundingRequest>(&wire).is_ok());
    }

    let mut cloud_wire = serde_json::to_value(founding_request(FoundingDriverEnrollment::Cloud {
        peer_id: peer_id(),
        document: peer_document(&cluster_id(CLUSTER)),
    }))
    .expect("Cloud founding request serializes");
    let Some(driver) = cloud_wire
        .as_object_mut()
        .and_then(|request| request.get_mut("driver"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        panic!("founding request has a driver object")
    };
    driver.insert(
        "callback_url".to_owned(),
        json!("https://cloud.example/callback"),
    );
    assert!(serde_json::from_value::<FoundingRequest>(cloud_wire).is_err());
}

#[test]
fn founding_validation_rejects_divergent_row_ids_clusters_and_versions() {
    let mut wrong_key = founding_request(FoundingDriverEnrollment::OnHost);
    wrong_key.cluster_id = cluster_id(OTHER_CLUSTER);
    assert!(matches!(
        wrong_key.try_validate(),
        Err(FoundingValidationError::ClusterKeyMismatch { .. })
    ));

    let mut wrong_machine_key = founding_request(FoundingDriverEnrollment::OnHost);
    wrong_machine_key.machine_id =
        MachineRowId::try_new(OTHER_CLUSTER).expect("fixture other machine id");
    assert!(matches!(
        wrong_machine_key.try_validate(),
        Err(FoundingValidationError::InvalidProvenance {
            row: FoundingRow::Cluster,
            ..
        })
    ));

    let mut wrong_machine_cluster = founding_request(FoundingDriverEnrollment::OnHost);
    wrong_machine_cluster.machine.cluster_id = cluster_id(OTHER_CLUSTER);
    assert_eq!(
        wrong_machine_cluster.try_validate(),
        Err(FoundingValidationError::DocumentClusterMismatch {
            row: FoundingRow::Machine,
            expected: cluster_id(CLUSTER),
            found: cluster_id(OTHER_CLUSTER),
        })
    );

    let request = founding_request(FoundingDriverEnrollment::OnHost);
    let mut wire = serde_json::to_value(request).expect("founding request serializes");
    let Some(machine) = wire
        .as_object_mut()
        .and_then(|request| request.get_mut("machine"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        panic!("founding request has a machine object")
    };
    machine.insert("v".to_owned(), json!(2));
    let unsupported: FoundingRequest =
        serde_json::from_value(wire).expect("additive document version parses");
    assert_eq!(
        unsupported.try_validate(),
        Err(FoundingValidationError::UnsupportedDocumentVersion {
            row: FoundingRow::Machine,
            found: 2,
        })
    );
}

#[test]
fn founding_validation_rejects_provider_address_subnet_lifecycle_and_provenance() {
    let mut provider = founding_request(FoundingDriverEnrollment::OnHost);
    provider.cluster.provider = MeshProvider::Tailscale;
    assert_eq!(
        provider.try_validate(),
        Err(FoundingValidationError::UnsupportedProvider {
            found: MeshProvider::Tailscale,
        })
    );

    let mut address = founding_request(FoundingDriverEnrollment::OnHost);
    let MachineTransport::Wireguard { addr_v6, .. } = &mut address.machine.transport else {
        panic!("fixture is WireGuard");
    };
    *addr_v6 = Ipv6Addr::LOCALHOST;
    assert!(matches!(
        address.try_validate(),
        Err(FoundingValidationError::MachineAddressMismatch { .. })
    ));

    let mut subnet = founding_request(FoundingDriverEnrollment::OnHost);
    let MachineTransport::Wireguard { subnet_v4, .. } = &mut subnet.machine.transport else {
        panic!("fixture is WireGuard");
    };
    *subnet_v4 = MachineEndpointSubnet::try_new("10.210.1.0/24").expect("fixture subnet");
    assert!(matches!(
        subnet.try_validate(),
        Err(FoundingValidationError::MachineSubnetNotFirst { .. })
    ));

    let mut lifecycle = founding_request(FoundingDriverEnrollment::OnHost);
    lifecycle.machine.lifecycle = MachineLifecycle::Draining;
    assert_eq!(
        lifecycle.try_validate(),
        Err(FoundingValidationError::MachineLifecycleNotActive {
            found: MachineLifecycle::Draining,
        })
    );

    let mut storage_default = founding_request(FoundingDriverEnrollment::OnHost);
    storage_default.cluster.storage_default = StorageMode::Zfs;
    assert_eq!(
        storage_default.try_validate(),
        Err(FoundingValidationError::StorageDefaultMismatch {
            cluster_default: StorageMode::Zfs,
            machine_mode: StorageMode::Plain,
        })
    );

    let mut provenance = founding_request(FoundingDriverEnrollment::OnHost);
    provenance.cluster.provenance.written_by = OperationInitiator::Peer { peer_id: peer_id() };
    assert!(matches!(
        provenance.try_validate(),
        Err(FoundingValidationError::InvalidProvenance {
            row: FoundingRow::Cluster,
            ..
        })
    ));
}

#[test]
fn enrolled_peer_must_match_cluster_provider_derivation_and_machine_provenance() {
    let mut wrong_cluster = founding_request(FoundingDriverEnrollment::Ssh {
        peer_id: peer_id(),
        document: peer_document(&cluster_id(OTHER_CLUSTER)),
    });
    assert!(matches!(
        wrong_cluster.clone().try_validate(),
        Err(FoundingValidationError::DocumentClusterMismatch {
            row: FoundingRow::Peer,
            ..
        })
    ));

    let FoundingDriverEnrollment::Ssh { document, .. } = &mut wrong_cluster.driver else {
        panic!("fixture has SSH driver");
    };
    document.cluster_id = cluster_id(CLUSTER);
    document.transport = PeerTransport::Tailscale {
        ip: Ipv4Addr::new(100, 64, 0, 2),
    };
    assert_eq!(
        wrong_cluster.clone().try_validate(),
        Err(FoundingValidationError::PeerTransportProviderMismatch)
    );

    let mut wrong_address = founding_request(FoundingDriverEnrollment::Cloud {
        peer_id: peer_id(),
        document: peer_document(&cluster_id(CLUSTER)),
    });
    let FoundingDriverEnrollment::Cloud { document, .. } = &mut wrong_address.driver else {
        panic!("fixture has Cloud driver");
    };
    let PeerTransport::Wireguard { addr_v6, .. } = &mut document.transport else {
        panic!("fixture is WireGuard");
    };
    *addr_v6 = Ipv6Addr::LOCALHOST;
    assert!(matches!(
        wrong_address.try_validate(),
        Err(FoundingValidationError::PeerAddressMismatch { .. })
    ));

    let mut wrong_provenance = founding_request(FoundingDriverEnrollment::Cloud {
        peer_id: peer_id(),
        document: peer_document(&cluster_id(CLUSTER)),
    });
    let FoundingDriverEnrollment::Cloud { document, .. } = &mut wrong_provenance.driver else {
        panic!("fixture has Cloud driver");
    };
    document.provenance.written_by = OperationInitiator::Peer { peer_id: peer_id() };
    assert!(matches!(
        wrong_provenance.try_validate(),
        Err(FoundingValidationError::InvalidProvenance {
            row: FoundingRow::Peer,
            ..
        })
    ));
}

#[test]
fn init_storage_policy_records_flags_and_eligibility_evidence() {
    let abundant = InitStorageFacts {
        imported_zfs_pool: true,
        total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES,
    };
    assert_eq!(
        select_init_storage(InitStorageChoice::Automatic, abundant),
        Ok(MachineStorageSelection {
            mode: StorageMode::Zfs,
            reason: MachineStorageSelectionReason::Default,
        })
    );
    assert_eq!(
        select_init_storage(
            InitStorageChoice::Automatic,
            InitStorageFacts {
                imported_zfs_pool: true,
                total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES - 1,
            },
        ),
        Ok(MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Ineligible {
                reason: MachineStorageIneligibleReason::LowRam,
            },
        })
    );
    assert_eq!(
        select_init_storage(
            InitStorageChoice::Automatic,
            InitStorageFacts {
                imported_zfs_pool: false,
                total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES,
            },
        ),
        Ok(MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Default,
        })
    );
    assert_eq!(
        select_init_storage(
            InitStorageChoice::Flag {
                mode: StorageMode::Plain,
            },
            InitStorageFacts {
                imported_zfs_pool: true,
                total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES,
            },
        ),
        Ok(MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Flag,
        })
    );

    assert_eq!(
        select_init_storage(
            InitStorageChoice::Flag {
                mode: StorageMode::Zfs,
            },
            InitStorageFacts {
                imported_zfs_pool: false,
                total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES,
            },
        ),
        Err(InitStorageSelectionError::ZfsPoolNotImported)
    );
    assert_eq!(
        select_init_storage(
            InitStorageChoice::Flag {
                mode: StorageMode::Zfs,
            },
            InitStorageFacts {
                imported_zfs_pool: true,
                total_memory_bytes: MINIMUM_ZFS_MEMORY_BYTES - 1,
            },
        ),
        Err(InitStorageSelectionError::ZfsMemoryBelowMinimum)
    );
}

#[test]
fn arrival_policy_is_found_resume_no_op_or_exact_repair_refusal() {
    let requested = cluster_id(CLUSTER);
    assert_eq!(
        classify_founding_arrival(&requested, FoundingArrival::Clean),
        Ok(FoundingResult::Found)
    );
    assert_eq!(
        classify_founding_arrival(
            &requested,
            FoundingArrival::Partial {
                persisted_cluster_id: requested.clone(),
            },
        ),
        Ok(FoundingResult::Resumed)
    );
    assert_eq!(
        classify_founding_arrival(
            &requested,
            FoundingArrival::Complete {
                persisted_cluster_id: requested.clone(),
            },
        ),
        Ok(FoundingResult::NoOp)
    );

    for arrival in [
        FoundingArrival::Partial {
            persisted_cluster_id: cluster_id(OTHER_CLUSTER),
        },
        FoundingArrival::Complete {
            persisted_cluster_id: cluster_id(OTHER_CLUSTER),
        },
        FoundingArrival::Joined {
            persisted_cluster_id: requested.clone(),
        },
    ] {
        let refusal = classify_founding_arrival(&requested, arrival).expect_err("foreign state");
        let FoundingRefusal::ForeignState { repair_command, .. } = refusal else {
            panic!("arrival produces foreign-state refusal");
        };
        assert_eq!(repair_command, FoundingRepairCommand::ResetMachine);
        assert_eq!(repair_command.as_str(), FOUNDING_RESET_COMMAND);
        assert_eq!(
            serde_json::to_value(repair_command).expect("repair serializes"),
            json!("ployz machine reset")
        );
    }
}

#[test]
fn design_default_is_the_10_210_supernet() {
    assert_eq!(
        MachineEndpointSupernet::default_v1().as_string(),
        "10.210.0.0/16"
    );
}
