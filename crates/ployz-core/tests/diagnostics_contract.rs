use std::collections::BTreeMap;
use std::net::Ipv6Addr;

use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionHealthResponse,
    CorrosionTable, CorrosionTimestamp, MachineDocument, MachineStatusDocument,
    MachineStorageSelection, MachineStorageSelectionReason, MachineTransport, MeshProvider,
    OperatorWriteProvenance, PloyzDnsTargetState, Principal, StorageMode, StoredRow,
    WireGuardHandshakeEvidence,
};
use ployz_core::ids::{ClusterId, CorrosionUlid, MachineRowId, PeerId, TokenId};
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::{
    DOCTOR_ROUTE, DoctorDocument, DoctorForeignAuthorship, DoctorMalformedRosterDocumentClass,
    DoctorProjectionInput, DoctorRawRows, DoctorRosterRowSkipReason, DoctorRosterTable,
    HandshakeFreshness, KnownApiFeature, MachineLensRow, STATUS_ROUTE, StatusAnsweringMachine,
    StatusBarrier, StatusCorrosionHealth, StatusDegradationReason, StatusDocument,
    StatusHandshakeEvidence, StatusHint, StatusProjectionInput, StatusSync, V2Method, V2Route,
    project_doctor, project_status,
};
use serde_json::json;

const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

fn machine_id(value: &str) -> MachineRowId {
    MachineRowId::try_new(value).expect("fixture machine id")
}

fn timestamp(value: &str) -> CorrosionTimestamp {
    CorrosionTimestamp::try_new(value).expect("fixture timestamp")
}

fn cluster() -> ClusterDocument {
    ClusterDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterId::try_new(CLUSTER).expect("fixture cluster id"),
        provenance: OperatorWriteProvenance {
            written_by: Principal::Peer {
                peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("fixture peer id"),
            },
            written_at: timestamp("2026-08-04T10:00:00Z"),
        },
        name: "acme-prod".to_owned(),
        storage_default: StorageMode::Plain,
        hostname_mode: AutomaticHostnameMode::Disabled,
        ployz_dns_target: PloyzDnsTargetState::Disabled,
        prefix: MachineEndpointSupernet::try_new("10.210.0.0/16").expect("fixture supernet"),
        provider: MeshProvider::BuiltinWireguard,
        acme_directory_url: "https://acme.example/directory".to_owned(),
        acme_contact: None,
    }
}

fn machine(value: &str, name: &str, addr_v6: Ipv6Addr) -> MachineLensRow {
    MachineLensRow {
        id: machine_id(value),
        document: MachineDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: ClusterId::try_new(CLUSTER).expect("fixture cluster id"),
            provenance: OperatorWriteProvenance {
                written_by: Principal::Peer {
                    peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX")
                        .expect("fixture peer id"),
                },
                written_at: timestamp("2026-08-04T10:00:00Z"),
            },
            name: MachineName::try_new(name).expect("fixture machine name"),
            lifecycle: MachineLifecycle::Active,
            transport: MachineTransport::Wireguard {
                pubkey: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                    .expect("fixture public key"),
                addr_v6,
                endpoint: None,
                subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24")
                    .expect("fixture subnet"),
            },
            storage: MachineStorageSelection {
                mode: StorageMode::Plain,
                reason: MachineStorageSelectionReason::Default,
            },
        },
    }
}

fn status_input(
    cluster: Option<ClusterDocument>,
    machines: Vec<MachineLensRow>,
    answering_machine_id: MachineRowId,
    health: StatusCorrosionHealth,
    wireguard_handshakes: Option<BTreeMap<MachineRowId, WireGuardHandshakeEvidence>>,
) -> StatusProjectionInput {
    StatusProjectionInput {
        cluster,
        machines,
        answering_machine_id,
        health,
        wireguard_handshakes,
        now_unix_seconds: 2_000,
    }
}

fn row(key: &str, document: serde_json::Value) -> StoredRow {
    StoredRow::new(
        key,
        serde_json::to_string(&document).expect("fixture document JSON"),
    )
}

fn named_document(table: &str, name: &str) -> serde_json::Value {
    let written_by = json!({"kind": "peer", "peer_id": "01ARZ3NDEKTSV4RRFFQ69G5FAX"});
    let written_at = "2026-08-04T10:00:00.000000000Z";
    match table {
        "machines" => json!({
            "v": 1, "cluster_id": CLUSTER,
            "written_by": written_by, "written_at": written_at,
            "name": name, "lifecycle": "active",
            "transport": {
                "kind": "wireguard",
                "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
                "addr_v6": "fd00::20", "endpoint": null, "subnet_v4": "10.210.20.0/24"
            },
            "storage": {"mode": "plain", "reason": {"kind": "default"}}
        }),
        "peers" => json!({
            "v": 1, "cluster_id": CLUSTER,
            "written_by": written_by, "written_at": written_at,
            "name": name,
            "transport": {"kind": "wireguard", "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", "addr_v6": "fd00::30", "endpoint": null}
        }),
        "namespaces" => json!({
            "v": 1, "cluster_id": CLUSTER,
            "written_by": written_by, "written_at": written_at,
            "name": name
        }),
        "services" => json!({
            "v": 1, "cluster_id": CLUSTER,
            "written_by": written_by, "written_at": written_at,
            "namespace_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "name": name,
            "image": "ghcr.io/acme/api:latest", "env_fingerprints": {},
            "mode": "global", "pinned_machines": [],
            "active_deploy": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "previous_image": null,
            "deployed_at": "2026-08-04T10:00:00.000000000Z",
            "operation_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV"
        }),
        "route_bindings" => json!({
            "v": 1, "cluster_id": CLUSTER,
            "written_by": written_by, "written_at": written_at,
            "hostname": name, "service_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            "namespace_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV", "endpoint_port": 8080,
            "origin": "declared", "ingress_mode": "direct"
        }),
        _ => panic!("unknown fixture table"),
    }
}

fn machine_status_row(key: &str, name_version: &str) -> StoredRow {
    row(
        key,
        json!({
            "v": 1, "cluster_id": CLUSTER, "machine_id": key,
            "ployz_version": name_version, "corrosion_version": "0.2.0-beta.0",
            "architecture": "x86_64", "free_disk_bytes": 1, "free_memory_bytes": 1,
            "load": "idle", "observed_at": "2026-08-04T10:00:00.000000000Z"
        }),
    )
}

#[test]
fn diagnostics_routes_and_corrosion_health_wire_shape_are_stable() {
    let machine = Principal::Machine {
        machine_id: machine_id(MACHINE),
    };
    let peer = Principal::Peer {
        peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("fixture peer id"),
    };
    let token = Principal::ApiToken {
        token_id: TokenId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("fixture token id"),
    };

    for (path, route) in [
        (STATUS_ROUTE, V2Route::Status),
        (DOCTOR_ROUTE, V2Route::Doctor),
    ] {
        assert_eq!(V2Route::parse(path), Some(route.clone()));
        assert_eq!(route.path(), path);
        assert_eq!(route.method(), V2Method::Get);
        assert_eq!(route.feature(), KnownApiFeature::Diagnostics);
        assert!(route.accepts_principal(&machine));
        assert!(route.accepts_principal(&peer));
        assert!(!route.accepts_principal(&token));
    }

    let response = CorrosionHealthResponse::Response {
        gaps: 0,
        members: 3,
        p99_lag: 0.48,
        queue_size: 0,
    };
    assert_eq!(
        serde_json::to_value(response).expect("health response serializes"),
        json!({
            "response": {
                "gaps": 0,
                "members": 3,
                "p99_lag": 0.48,
                "queue_size": 0
            }
        })
    );
    assert_eq!(
        serde_json::from_value::<CorrosionHealthResponse>(
            json!({"error": "no p99 lag information available"})
        )
        .expect("health error deserializes"),
        CorrosionHealthResponse::Error("no p99 lag information available".to_owned())
    );
}

#[test]
fn corrosion_table_catalog_covers_every_diagnostic_table_in_schema_order() {
    assert_eq!(
        CorrosionTable::ALL.map(CorrosionTable::as_str),
        [
            "cluster",
            "machines",
            "peers",
            "tokens",
            "namespaces",
            "services",
            "route_bindings",
            "controller",
            "containers",
            "machine_status",
            "gateway_observations",
            "operations",
            "cert_holdings",
            "acme_http01",
        ]
    );
}

#[test]
fn machine_status_handshake_testimony_is_additive_and_distinguishes_never_from_time() {
    let old_row = json!({
        "v": 1,
        "cluster_id": CLUSTER,
        "machine_id": MACHINE,
        "ployz_version": "0.1.0-alpha.7",
        "corrosion_version": "0.2.0-beta.0",
        "architecture": "x86_64",
        "free_disk_bytes": 80_000_000_000_u64,
        "free_memory_bytes": 4_000_000_000_u64,
        "load": "idle",
        "observed_at": "2026-08-04T10:06:00.000000000Z"
    });
    let old: MachineStatusDocument =
        serde_json::from_value(old_row).expect("field-absent old row remains readable");
    assert_eq!(old.wireguard_handshakes, None);

    let testimony = BTreeMap::from([
        (
            machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAX"),
            WireGuardHandshakeEvidence::Never,
        ),
        (
            machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAY"),
            WireGuardHandshakeEvidence::At {
                unix_seconds: 1_775_000_000,
            },
        ),
    ]);
    let mut current = old;
    current.wireguard_handshakes = Some(testimony);
    assert_eq!(
        serde_json::to_value(current)
            .expect("current machine status serializes")
            .get("wireguard_handshakes")
            .cloned(),
        Some(json!({
            "01ARZ3NDEKTSV4RRFFQ69G5FAX": {"state": "never"},
            "01ARZ3NDEKTSV4RRFFQ69G5FAY": {
                "state": "at",
                "unix_seconds": 1_775_000_000_u64
            }
        }))
    );
}

#[test]
fn healthy_status_is_sorted_and_the_275_second_boundary_is_not_stale() {
    let answering = machine_id(MACHINE);
    let remote = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAX");
    let document = project_status(status_input(
        Some(cluster()),
        vec![
            machine(remote.as_str(), "zeta", Ipv6Addr::LOCALHOST),
            machine(answering.as_str(), "alpha", Ipv6Addr::UNSPECIFIED),
        ],
        answering.clone(),
        StatusCorrosionHealth::Reply(CorrosionHealthResponse::Response {
            gaps: 0,
            members: 2,
            p99_lag: 0.48,
            queue_size: 0,
        }),
        Some(BTreeMap::from([(
            remote,
            WireGuardHandshakeEvidence::At {
                unix_seconds: 1_725,
            },
        )])),
    ));

    assert_eq!(document.cluster.expect("cluster").machine_count, 2);
    assert_eq!(document.barrier, StatusBarrier::Ready);
    assert_eq!(document.sync, StatusSync::CaughtUp { p99_lag: 0.48 });
    assert_eq!(
        document.answering_machine,
        StatusAnsweringMachine::Known {
            id: answering,
            name: MachineName::try_new("alpha").expect("fixture machine name"),
        }
    );
    assert_eq!(
        document
            .machines
            .iter()
            .map(|row| (row.name.as_str(), &row.handshake))
            .collect::<Vec<_>>(),
        vec![
            ("alpha", &StatusHandshakeEvidence::SelfMachine),
            (
                "zeta",
                &StatusHandshakeEvidence::Ago {
                    seconds: 275,
                    freshness: HandshakeFreshness::Healthy,
                },
            ),
        ]
    );
    assert!(document.hints.is_empty());
}

#[test]
fn cold_boot_status_keeps_the_durable_barrier_ready_and_reports_never() {
    let answering = machine_id(MACHINE);
    let remote = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAX");
    let without_testimony = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAY");
    let document = project_status(status_input(
        Some(cluster()),
        vec![
            machine(answering.as_str(), "alpha", Ipv6Addr::UNSPECIFIED),
            machine(remote.as_str(), "zeta", Ipv6Addr::LOCALHOST),
            machine(
                without_testimony.as_str(),
                "unknown",
                "fd00::2".parse().expect("fixture address"),
            ),
        ],
        answering,
        StatusCorrosionHealth::Reply(CorrosionHealthResponse::Error(
            "no p99 lag information available".to_owned(),
        )),
        Some(BTreeMap::from([(
            remote,
            WireGuardHandshakeEvidence::Never,
        )])),
    ));

    assert_eq!(document.barrier, StatusBarrier::Ready);
    assert_eq!(document.sync, StatusSync::NoLagSample);
    let [_, without_testimony, remote] = document.machines.as_slice() else {
        panic!("expected self and two remote machine rows");
    };
    assert_eq!(
        without_testimony.handshake,
        StatusHandshakeEvidence::NoTestimony
    );
    assert_eq!(remote.handshake, StatusHandshakeEvidence::Never);
    assert_eq!(document.hints, vec![StatusHint::AllPeerHandshakesStale]);
}

#[test]
fn corrosion_health_errors_other_than_the_pinned_no_sample_error_are_degraded() {
    let document = project_status(status_input(
        None,
        Vec::new(),
        machine_id(MACHINE),
        StatusCorrosionHealth::Reply(CorrosionHealthResponse::Error(
            "could not check health: database unavailable".to_owned(),
        )),
        None,
    ));
    assert_eq!(
        document.sync,
        StatusSync::Degraded {
            reason: StatusDegradationReason::CorrosionUnavailable,
        }
    );
    assert_eq!(
        serde_json::to_value(document.sync).expect("degraded sync serializes"),
        json!({
            "state": "degraded",
            "reason": "corrosion_unavailable"
        })
    );
}

#[test]
fn joining_and_no_roster_are_distinct_durable_barrier_states() {
    let answering = machine_id(MACHINE);
    let health = || {
        StatusCorrosionHealth::Reply(CorrosionHealthResponse::Response {
            gaps: 2,
            members: 1,
            p99_lag: 1.25,
            queue_size: 4,
        })
    };

    let joining = project_status(status_input(
        Some(cluster()),
        Vec::new(),
        answering.clone(),
        health(),
        Some(BTreeMap::new()),
    ));
    assert_eq!(joining.barrier, StatusBarrier::CatchingUp);
    assert_eq!(
        joining.sync,
        StatusSync::Syncing {
            gaps: 2,
            queue_size: 4,
            p99_lag: 1.25,
        }
    );

    let no_roster = project_status(status_input(
        None,
        Vec::new(),
        answering.clone(),
        health(),
        None,
    ));
    assert_eq!(no_roster.barrier, StatusBarrier::NoRoster);
    assert_eq!(
        no_roster.answering_machine,
        StatusAnsweringMachine::Unknown { id: answering }
    );
    let no_roster_json = serde_json::to_value(&no_roster).expect("no-roster status serializes");
    assert!(no_roster_json.get("cluster").is_none());
    let decoded: StatusDocument =
        serde_json::from_value(no_roster_json).expect("status accepts an omitted cluster");
    assert_eq!(decoded, no_roster);
}

#[test]
fn absent_handshake_map_is_no_testimony_and_suppresses_all_stale_hint() {
    let answering = machine_id(MACHINE);
    let remote = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAX");
    let document = project_status(status_input(
        Some(cluster()),
        vec![
            machine(answering.as_str(), "alpha", Ipv6Addr::UNSPECIFIED),
            machine(remote.as_str(), "zeta", Ipv6Addr::LOCALHOST),
        ],
        answering,
        StatusCorrosionHealth::Unavailable,
        None,
    ));

    assert_eq!(
        document.sync,
        StatusSync::Degraded {
            reason: StatusDegradationReason::CorrosionUnavailable,
        }
    );
    let [_, remote] = document.machines.as_slice() else {
        panic!("expected self and remote machine rows");
    };
    assert_eq!(remote.handshake, StatusHandshakeEvidence::NoTestimony);
    assert!(document.hints.is_empty());
}

#[test]
fn future_handshake_timestamps_saturate_to_zero_age_and_276_seconds_warns() {
    let answering = machine_id(MACHINE);
    let future = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAX");
    let stale = machine_id("01ARZ3NDEKTSV4RRFFQ69G5FAY");
    let document = project_status(status_input(
        Some(cluster()),
        vec![
            machine(answering.as_str(), "alpha", Ipv6Addr::UNSPECIFIED),
            machine(future.as_str(), "future", Ipv6Addr::LOCALHOST),
            machine(stale.as_str(), "stale", Ipv6Addr::LOCALHOST),
        ],
        answering,
        StatusCorrosionHealth::InvalidResponse,
        Some(BTreeMap::from([
            (
                future,
                WireGuardHandshakeEvidence::At {
                    unix_seconds: 2_001,
                },
            ),
            (
                stale,
                WireGuardHandshakeEvidence::At {
                    unix_seconds: 1_724,
                },
            ),
        ])),
    ));

    let [_, future, stale] = document.machines.as_slice() else {
        panic!("expected self and two remote machine rows");
    };
    assert_eq!(
        future.handshake,
        StatusHandshakeEvidence::Ago {
            seconds: 0,
            freshness: HandshakeFreshness::Healthy,
        }
    );
    assert_eq!(
        stale.handshake,
        StatusHandshakeEvidence::Ago {
            seconds: 276,
            freshness: HandshakeFreshness::Stale,
        }
    );
    assert_eq!(
        document.sync,
        StatusSync::Degraded {
            reason: StatusDegradationReason::InvalidCorrosionHealthResponse,
        }
    );
    assert!(document.hints.is_empty());
}

#[test]
fn doctor_shadow_claims_alone_identify_each_repair_primitive() {
    let winner = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    let loser = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    let mut rows = DoctorRawRows::empty();
    rows.machines = vec![
        row(winner, named_document("machines", "edge")),
        row(loser, named_document("machines", "edge")),
    ];
    rows.peers = vec![
        row(winner, named_document("peers", "laptop")),
        row(loser, named_document("peers", "laptop")),
    ];
    rows.namespaces = vec![
        row(winner, named_document("namespaces", "production")),
        row(loser, named_document("namespaces", "production")),
    ];
    rows.services = vec![
        row(winner, named_document("services", "api")),
        row(loser, named_document("services", "api")),
    ];
    rows.route_bindings = vec![
        row(winner, named_document("route_bindings", "api.example.com")),
        row(loser, named_document("route_bindings", "api.example.com")),
    ];

    let doctor = project_doctor(DoctorProjectionInput {
        cluster: cluster(),
        rows,
    });
    assert_eq!(
        doctor
            .shadows
            .iter()
            .map(|shadow| {
                serde_json::to_value(&shadow.claim)
                    .expect("name claim serializes")
                    .get("table")
                    .expect("name claim table field")
                    .as_str()
                    .expect("name claim table")
                    .to_owned()
            })
            .collect::<Vec<_>>(),
        vec!["machine", "peer", "namespace", "service", "route_binding",]
    );
    assert!(doctor.shadows.iter().all(|shadow| {
        shadow.winner_id == CorrosionUlid::try_new(winner).expect("winner id")
            && shadow.loser_id == CorrosionUlid::try_new(loser).expect("loser id")
    }));
}

#[test]
fn doctor_projects_same_cluster_roster_skips_without_duplicating_other_evidence() {
    let foreign_cluster = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    let mut machine_provider_mismatch = named_document("machines", "machine-provider");
    *machine_provider_mismatch
        .get_mut("transport")
        .expect("machine fixture has transport") =
        json!({"kind": "tailscale", "ip": "100.64.0.20", "subnet_v4": "10.210.20.0/24"});
    let mut peer_provider_mismatch = named_document("peers", "peer-provider");
    *peer_provider_mismatch
        .get_mut("transport")
        .expect("peer fixture has transport") = json!({"kind": "tailscale", "ip": "100.64.0.30"});
    let mut malformed_machine = named_document("machines", "machine-malformed");
    *malformed_machine
        .get_mut("transport")
        .expect("machine fixture has transport") = json!({"kind": "unknown"});
    let mut malformed_peer = named_document("peers", "peer-malformed");
    *malformed_peer
        .get_mut("transport")
        .expect("peer fixture has transport") = json!({"kind": "unknown"});

    let mut rows = DoctorRawRows::empty();
    rows.machines = vec![
        row(MACHINE, machine_provider_mismatch),
        row("01ARZ3NDEKTSV4RRFFQ69G5FAX", malformed_machine),
        row(
            "invalid-machine-id",
            named_document("machines", "machine-id"),
        ),
        row(
            "01ARZ3NDEKTSV4RRFFQ69G5FAY",
            json!({"v": 2, "cluster_id": CLUSTER}),
        ),
        row(
            "01ARZ3NDEKTSV4RRFFQ69G5FAT",
            json!({"v": 1, "cluster_id": foreign_cluster}),
        ),
    ];
    rows.peers = vec![
        row(MACHINE, peer_provider_mismatch),
        row("01ARZ3NDEKTSV4RRFFQ69G5FAX", malformed_peer),
        row("invalid-peer-id", named_document("peers", "peer-id")),
        row(
            "01ARZ3NDEKTSV4RRFFQ69G5FAY",
            json!({"v": 2, "cluster_id": CLUSTER}),
        ),
        row(
            "01ARZ3NDEKTSV4RRFFQ69G5FAT",
            json!({"v": 1, "cluster_id": foreign_cluster}),
        ),
    ];

    let doctor = project_doctor(DoctorProjectionInput {
        cluster: cluster(),
        rows,
    });

    assert_eq!(
        doctor.skipped_roster_rows,
        vec![
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Machines,
                key: MACHINE.to_owned(),
                reason: DoctorRosterRowSkipReason::MeshProviderMismatch {
                    expected: MeshProvider::BuiltinWireguard,
                    found: MeshProvider::Tailscale,
                },
            },
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Machines,
                key: "01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned(),
                reason: DoctorRosterRowSkipReason::MalformedDocument {
                    class: DoctorMalformedRosterDocumentClass::InvalidPayload,
                },
            },
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Machines,
                key: "invalid-machine-id".to_owned(),
                reason: DoctorRosterRowSkipReason::InvalidRowId,
            },
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Peers,
                key: MACHINE.to_owned(),
                reason: DoctorRosterRowSkipReason::MeshProviderMismatch {
                    expected: MeshProvider::BuiltinWireguard,
                    found: MeshProvider::Tailscale,
                },
            },
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Peers,
                key: "01ARZ3NDEKTSV4RRFFQ69G5FAX".to_owned(),
                reason: DoctorRosterRowSkipReason::MalformedDocument {
                    class: DoctorMalformedRosterDocumentClass::InvalidPayload,
                },
            },
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Peers,
                key: "invalid-peer-id".to_owned(),
                reason: DoctorRosterRowSkipReason::InvalidRowId,
            },
        ]
    );
    assert_eq!(doctor.skipped_newer_versions.len(), 2);
    assert!(doctor.skipped_newer_versions.iter().all(|row| {
        row.key == "01ARZ3NDEKTSV4RRFFQ69G5FAY"
            && matches!(row.table, CorrosionTable::Machines | CorrosionTable::Peers)
    }));
    let [foreign] = doctor.foreign_clusters.as_slice() else {
        panic!("expected one foreign-cluster group");
    };
    assert_eq!(foreign.cluster_id, foreign_cluster);
    assert_eq!(foreign.rows.len(), 2);
    assert!(doctor.skipped_roster_rows.iter().all(|row| {
        row.key != "01ARZ3NDEKTSV4RRFFQ69G5FAY" && row.key != "01ARZ3NDEKTSV4RRFFQ69G5FAT"
    }));
    let Some(malformed_machine_reason) = doctor
        .skipped_roster_rows
        .iter()
        .find(|row| {
            row.table == DoctorRosterTable::Machines && row.key == "01ARZ3NDEKTSV4RRFFQ69G5FAX"
        })
        .map(|row| &row.reason)
    else {
        panic!("expected malformed machine evidence");
    };
    assert_eq!(
        serde_json::to_value(malformed_machine_reason).expect("roster skip reason serializes"),
        json!({
            "kind": "malformed_document",
            "class": {"kind": "invalid_payload"}
        })
    );
}

#[test]
fn doctor_compares_valid_versions_semantically_and_retains_newer_row_evidence() {
    let alpha = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    let zeta = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    let invalid = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    let mut rows = DoctorRawRows::empty();
    rows.machines = vec![
        row(alpha, named_document("machines", "alpha")),
        row(zeta, named_document("machines", "zeta")),
        row(invalid, named_document("machines", "invalid")),
    ];
    rows.machine_status = vec![
        machine_status_row(alpha, "1.2.0"),
        machine_status_row(zeta, "1.10.0"),
        machine_status_row(invalid, "release-next"),
    ];
    rows.services = vec![row(
        "01ARZ3NDEKTSV4RRFFQ69G5FAZ",
        json!({"v": 2, "cluster_id": CLUSTER}),
    )];

    let doctor = project_doctor(DoctorProjectionInput {
        cluster: cluster(),
        rows,
    });
    let newest = doctor.versions.newest.expect("newest valid version");
    let [newest_machine] = newest.machines.as_slice() else {
        panic!("expected one newest machine");
    };
    let [behind] = doctor.versions.behind.as_slice() else {
        panic!("expected one behind machine");
    };
    let [invalid] = doctor.versions.invalid.as_slice() else {
        panic!("expected one invalid machine version");
    };
    let [newer] = doctor.skipped_newer_versions.as_slice() else {
        panic!("expected one newer-version row");
    };
    assert_eq!(newest.version, "1.10.0");
    assert_eq!(newest_machine.name.as_str(), "zeta");
    assert_eq!(behind.version, "1.2.0");
    assert_eq!(behind.machine.name.as_str(), "alpha");
    assert_eq!(invalid.version, "release-next");
    assert_eq!(invalid.machine.name.as_str(), "invalid");
    assert_eq!(newer.table.as_str(), "services");
    assert_eq!(newer.found, 2);
    assert_eq!(newer.supported, 1);
}

#[test]
fn doctor_omits_newest_when_no_valid_machine_version_exists() {
    let doctor = project_doctor(DoctorProjectionInput {
        cluster: cluster(),
        rows: DoctorRawRows::empty(),
    });

    let doctor_json = serde_json::to_value(&doctor).expect("doctor serializes");
    let Some(versions_json) = doctor_json.get("versions") else {
        panic!("doctor contains version evidence");
    };
    assert!(versions_json.get("newest").is_none());
    let decoded: DoctorDocument =
        serde_json::from_value(doctor_json).expect("doctor accepts an omitted newest");
    assert_eq!(decoded, doctor);
}

#[test]
fn doctor_groups_foreign_rows_and_only_current_machine_authors_are_actionable() {
    let current = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
    let non_current = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
    let foreign_active = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    let foreign_orphan = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    let mut rows = DoctorRawRows::empty();
    rows.machines = vec![row(current, named_document("machines", "edge-a"))];
    rows.namespaces = vec![row(
        "01ARZ3NDEKTSV4RRFFQ69G5FAT",
        json!({
            "v": 1, "cluster_id": foreign_active,
            "written_by": {"kind": "machine", "machine_id": current}
        }),
    )];
    rows.tokens = vec![row(
        "01ARZ3NDEKTSV4RRFFQ69G5FAS",
        json!({
            "v": 1, "cluster_id": foreign_active,
            "written_by": {"kind": "peer", "peer_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV"}
        }),
    )];
    rows.services = vec![row(
        "01ARZ3NDEKTSV4RRFFQ69G5FAR",
        json!({
            "v": 1, "cluster_id": foreign_orphan,
            "written_by": {"kind": "machine", "machine_id": non_current}
        }),
    )];
    rows.containers = vec![row(
        "container-raw-key",
        json!({"v": 1, "cluster_id": foreign_orphan, "machine_id": "not-a-ulid"}),
    )];

    let doctor = project_doctor(DoctorProjectionInput {
        cluster: cluster(),
        rows,
    });
    assert_eq!(doctor.foreign_clusters.len(), 2);
    let active = doctor
        .foreign_clusters
        .iter()
        .find(|group| group.cluster_id == foreign_active)
        .expect("active foreign group");
    assert!(active.rows.iter().any(|evidence| matches!(
        &evidence.authorship,
        DoctorForeignAuthorship::CurrentMachine { machine }
            if machine.id == machine_id(current) && machine.name.as_str() == "edge-a"
    )));
    assert!(
        active
            .rows
            .iter()
            .any(|evidence| matches!(evidence.authorship, DoctorForeignAuthorship::Peer { .. }))
    );

    let orphan = doctor
        .foreign_clusters
        .iter()
        .find(|group| group.cluster_id == foreign_orphan)
        .expect("orphan foreign group");
    assert!(orphan.rows.iter().any(|evidence| matches!(
        evidence.authorship,
        DoctorForeignAuthorship::NonCurrentMachine { .. }
    )));
    assert!(
        orphan
            .rows
            .iter()
            .any(|evidence| matches!(evidence.authorship, DoctorForeignAuthorship::Unparseable))
    );
    assert!(!orphan.rows.iter().any(|evidence| matches!(
        evidence.authorship,
        DoctorForeignAuthorship::CurrentMachine { .. }
    )));
}
