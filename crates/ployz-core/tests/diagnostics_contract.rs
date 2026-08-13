use std::collections::BTreeMap;
use std::net::Ipv6Addr;

use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, CorrosionDocumentVersion, CorrosionHealthResponse,
    CorrosionTable, CorrosionTimestamp, MachineDocument, MachineStatusDocument,
    MachineStorageSelection, MachineStorageSelectionReason, MachineTransport, MeshProvider,
    OperatorWriteProvenance, Principal, StorageMode, StoredRow, WireGuardHandshakeEvidence,
};
use ployz_core::ids::{ClusterName, MachineName, PeerName, TokenName};
use ployz_core::machine::MachineLifecycle;
use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::{
    DOCTOR_ROUTE, DoctorDocument, DoctorForeignAuthorship, DoctorMalformedRosterDocumentClass,
    DoctorNoncanonicalRow, DoctorProjectionInput, DoctorRawRows, DoctorRosterRowSkipReason,
    DoctorRosterTable, HandshakeFreshness, KnownApiFeature, STATUS_ROUTE, StatusAnsweringMachine,
    StatusBarrier, StatusCorrosionHealth, StatusDegradationReason, StatusDocument,
    StatusHandshakeEvidence, StatusHint, StatusProjectionInput, StatusSync, V2Method, V2Route,
    project_doctor, project_status,
};
use serde_json::json;

const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";

fn machine_id(value: &str) -> MachineName {
    MachineName::try_new(value).expect("fixture machine id")
}

fn timestamp(value: &str) -> CorrosionTimestamp {
    CorrosionTimestamp::try_new(value).expect("fixture timestamp")
}

fn cluster() -> ClusterDocument {
    ClusterDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterName::try_new(CLUSTER).expect("fixture cluster id"),
        provenance: OperatorWriteProvenance {
            written_by: Principal::Peer {
                peer_id: PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("fixture peer id"),
            },
            written_at: timestamp("2026-08-04T10:00:00Z"),
        },
        name: "acme-prod".to_owned(),
        storage_default: StorageMode::Plain,
        hostname_mode: AutomaticHostnameMode::Disabled,
        prefix: MachineEndpointSupernet::try_new("10.210.0.0/16").expect("fixture supernet"),
        provider: MeshProvider::BuiltinWireguard,
        acme_directory_url: "https://acme.example/directory".to_owned(),
        acme_contact: None,
    }
}

fn machine(_value: &str, name: &str, addr_v6: Ipv6Addr) -> MachineDocument {
    MachineDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterName::try_new(CLUSTER).expect("fixture cluster id"),
        provenance: OperatorWriteProvenance {
            written_by: Principal::Peer {
                peer_id: PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("fixture peer id"),
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
            subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24").expect("fixture subnet"),
        },
        storage: MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Default,
        },
    }
}

fn status_input(
    cluster: Option<ClusterDocument>,
    machines: Vec<MachineDocument>,
    answering_machine_id: MachineName,
    health: StatusCorrosionHealth,
    wireguard_handshakes: Option<BTreeMap<MachineName, WireGuardHandshakeEvidence>>,
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
        peer_id: PeerName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAX").expect("fixture peer id"),
    };
    let token = Principal::ApiToken {
        token_id: TokenName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAY").expect("fixture token id"),
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
            "route_bindings",
            "controller",
            "machine_endpoints",
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
    let answering = machine_id("alpha");
    let remote = machine_id("zeta");
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
        StatusAnsweringMachine::Known { name: answering }
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
    let answering = machine_id("alpha");
    let remote = machine_id("zeta");
    let without_testimony = machine_id("unknown");
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
        StatusAnsweringMachine::Unknown { name: answering }
    );
    let no_roster_json = serde_json::to_value(&no_roster).expect("no-roster status serializes");
    assert!(no_roster_json.get("cluster").is_none());
    let decoded: StatusDocument =
        serde_json::from_value(no_roster_json).expect("status accepts an omitted cluster");
    assert_eq!(decoded, no_roster);
}

#[test]
fn absent_handshake_map_is_no_testimony_and_suppresses_all_stale_hint() {
    let answering = machine_id("alpha");
    let remote = machine_id("zeta");
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
    let answering = machine_id("alpha");
    let future = machine_id("future");
    let stale = machine_id("stale");
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
    let machine_wrong_key = named_document("machines", "machine-canonical");
    let peer_wrong_key = named_document("peers", "peer-canonical");
    let namespace_wrong_key = json!({
        "v": 1,
        "cluster_id": CLUSTER,
        "written_by": {"kind": "peer", "peer_id": "01ARZ3NDEKTSV4RRFFQ69G5FAX"},
        "written_at": "2026-08-04T10:00:00.000000000Z",
        "name": "production-canonical",
        "services": {}
    });

    let mut rows = DoctorRawRows::empty();
    rows.machines = vec![
        row("machine-wrong-key", machine_wrong_key),
        row("machine-provider", machine_provider_mismatch),
        row("machine-malformed", malformed_machine),
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
        row("peer-wrong-key", peer_wrong_key),
        row("peer-provider", peer_provider_mismatch),
        row("peer-malformed", malformed_peer),
        row(
            "01ARZ3NDEKTSV4RRFFQ69G5FAY",
            json!({"v": 2, "cluster_id": CLUSTER}),
        ),
        row(
            "01ARZ3NDEKTSV4RRFFQ69G5FAT",
            json!({"v": 1, "cluster_id": foreign_cluster}),
        ),
    ];
    rows.namespaces = vec![row("production-wrong", namespace_wrong_key)];

    let doctor = project_doctor(DoctorProjectionInput {
        cluster: cluster(),
        rows,
    });

    assert_eq!(
        doctor.skipped_roster_rows,
        vec![
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Machines,
                key: "machine-malformed".to_owned(),
                reason: DoctorRosterRowSkipReason::MalformedDocument {
                    class: DoctorMalformedRosterDocumentClass::InvalidPayload,
                },
            },
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Machines,
                key: "machine-provider".to_owned(),
                reason: DoctorRosterRowSkipReason::MeshProviderMismatch {
                    expected: MeshProvider::BuiltinWireguard,
                    found: MeshProvider::Tailscale,
                },
            },
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Peers,
                key: "peer-malformed".to_owned(),
                reason: DoctorRosterRowSkipReason::MalformedDocument {
                    class: DoctorMalformedRosterDocumentClass::InvalidPayload,
                },
            },
            ployz_core::DoctorSkippedRosterRow {
                table: DoctorRosterTable::Peers,
                key: "peer-provider".to_owned(),
                reason: DoctorRosterRowSkipReason::MeshProviderMismatch {
                    expected: MeshProvider::BuiltinWireguard,
                    found: MeshProvider::Tailscale,
                },
            },
        ]
    );
    assert_eq!(
        doctor.noncanonical_rows,
        vec![
            DoctorNoncanonicalRow {
                table: CorrosionTable::Machines,
                key: "machine-wrong-key".to_owned(),
                expected: "machine-canonical".to_owned(),
            },
            DoctorNoncanonicalRow {
                table: CorrosionTable::Peers,
                key: "peer-wrong-key".to_owned(),
                expected: "peer-canonical".to_owned(),
            },
            DoctorNoncanonicalRow {
                table: CorrosionTable::Namespaces,
                key: "production-wrong".to_owned(),
                expected: "production-canonical".to_owned(),
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
        .find(|row| row.table == DoctorRosterTable::Machines && row.key == "machine-malformed")
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
    let wrong_key = doctor
        .noncanonical_rows
        .iter()
        .find(|row| row.key == "machine-wrong-key")
        .expect("expected noncanonical machine evidence");
    assert_eq!(
        serde_json::to_value(wrong_key).expect("noncanonical row serializes"),
        json!({
            "table": "machines",
            "key": "machine-wrong-key",
            "expected": "machine-canonical"
        })
    );
}

#[test]
fn doctor_compares_valid_versions_semantically_and_retains_newer_row_evidence() {
    let alpha = "alpha";
    let zeta = "zeta";
    let invalid = "invalid";
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
    rows.namespaces = vec![row(
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
    assert_eq!(newest_machine.as_str(), "zeta");
    assert_eq!(behind.version, "1.2.0");
    assert_eq!(behind.machine.as_str(), "alpha");
    assert_eq!(invalid.version, "release-next");
    assert_eq!(invalid.machine.as_str(), "invalid");
    assert_eq!(newer.table.as_str(), "namespaces");
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
    let current = "edge-a";
    let non_current = "edge-b";
    let foreign_active = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
    let foreign_orphan = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
    let mut rows = DoctorRawRows::empty();
    rows.machines = vec![row(current, named_document("machines", current))];
    rows.namespaces = vec![
        row(
            "01ARZ3NDEKTSV4RRFFQ69G5FAT",
            json!({
                "v": 1, "cluster_id": foreign_active,
                "written_by": {"kind": "machine", "machine_id": current}
            }),
        ),
        row(
            "01ARZ3NDEKTSV4RRFFQ69G5FAR",
            json!({
                "v": 1, "cluster_id": foreign_orphan,
                "written_by": {"kind": "machine", "machine_id": non_current}
            }),
        ),
    ];
    rows.tokens = vec![row(
        "01ARZ3NDEKTSV4RRFFQ69G5FAS",
        json!({
            "v": 1, "cluster_id": foreign_active,
            "written_by": {"kind": "peer", "peer_id": "01ARZ3NDEKTSV4RRFFQ69G5FAV"}
        }),
    )];
    rows.machine_endpoints = vec![row(
        "machine-endpoints-raw-key",
        json!({"v": 1, "cluster_id": foreign_orphan, "machine_id": "not/a/name"}),
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
        DoctorForeignAuthorship::CurrentMachine { machine_name }
            if machine_name == &machine_id(current)
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
