use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use ployz_core::corrosion::{
    AcceptedRosterPrincipal, CorrosionDocumentVersion, CorrosionNamespaceName, CorrosionTimestamp,
    MachineLoadBand, MachineStatusDocument, MachineTransport, OperationInitiator,
    OperatorWriteProvenance, PeerTransport, Principal, SourcePrincipalResolutionError,
    resolve_source_principal,
};
use ployz_core::deploy::{
    ContainerRuntimeSpec, EnvName, EnvValue, ImageReference, ServiceEnvironment,
};
use ployz_core::ids::{ClusterId, MachineRowId, OperationRowId, PeerId, ServiceRowId, TokenId};
use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};
use ployz_core::{
    API_MAJOR, ApiFeature, ApiRefusal, ApiVersion, CorrosionLogsTailLines, DEPLOY_ROUTE,
    DeployRefusal, DeployRequest, FOUNDING_ROUTE, HandshakeObservation,
    HandshakeObservationOutcome, HealthGatePolicy, KNOWN_API_FEATURES, KnownApiFeature,
    LENS_SNAPSHOT_EVENT, LENS_STATE_EVENT, LENS_TERMINAL_EVENT, LensCollection, LensSnapshot,
    LensWatchEvent, MachineStatusLensRow, MachineStatusLensRowIdentityError,
    NAMESPACE_CREATE_ROUTE, NAMESPACE_REMOVE_ROUTE, OperationEvidence, OperationEvidenceEvent,
    OperationEvidenceSequence, OperationWatchEvent, ServiceLogLine, ServiceLogStream,
    ServiceLogsFollowEvent, ServiceLogsRefusal, V2Method, V2Route, VERSION_ROUTE, lens_route,
    lens_watch_route, operation_route, operation_watch_route, service_logs_follow_route,
    service_logs_tail_route,
};
use serde_json::json;

const MACHINE_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const MACHINE_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const PEER_A: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const PEER_B: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";

fn machine_id(value: &str) -> MachineRowId {
    MachineRowId::try_new(value).expect("fixture machine id")
}

fn operation_id(value: &str) -> OperationRowId {
    OperationRowId::try_new(value).expect("fixture operation id")
}

fn service_id(value: &str) -> ServiceRowId {
    ServiceRowId::try_new(value).expect("fixture service id")
}

fn peer_id(value: &str) -> PeerId {
    PeerId::try_new(value).expect("fixture peer id")
}

fn wireguard_machine(addr_v6: Ipv6Addr) -> MachineTransport {
    MachineTransport::Wireguard {
        pubkey: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
            .expect("fixture public key"),
        addr_v6,
        endpoint: Some(SocketAddr::from_str("198.51.100.10:51820").expect("fixture endpoint")),
        subnet_v4: MachineEndpointSubnet::try_new("10.210.20.0/24").expect("fixture subnet"),
    }
}

fn wireguard_peer(addr_v6: Ipv6Addr) -> PeerTransport {
    PeerTransport::Wireguard {
        pubkey: WireGuardPublicKey::try_new("AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=")
            .expect("fixture public key"),
        addr_v6,
        endpoint: Some(SocketAddr::from_str("198.51.100.11:51820").expect("fixture endpoint")),
    }
}

fn tailscale_machine(ip: Ipv4Addr) -> MachineTransport {
    MachineTransport::Tailscale {
        ip,
        subnet_v4: MachineEndpointSubnet::try_new("10.210.30.0/24").expect("fixture subnet"),
    }
}

fn tailscale_peer(ip: Ipv4Addr) -> PeerTransport {
    PeerTransport::Tailscale { ip }
}

#[test]
fn principal_is_the_one_durable_provenance_wire_union() {
    let peer_id = peer_id(PEER_A);
    let principal = Principal::Peer {
        peer_id: peer_id.clone(),
    };
    let alias: OperationInitiator = principal.clone();
    assert_eq!(alias, principal);

    let provenance = OperatorWriteProvenance {
        written_by: alias,
        written_at: CorrosionTimestamp::try_new("2026-08-04T10:00:00Z").expect("fixture timestamp"),
    };
    let serialized = serde_json::to_value(&provenance).expect("provenance serializes");
    assert_eq!(
        serialized,
        json!({
            "written_by": { "kind": "peer", "peer_id": PEER_A },
            "written_at": "2026-08-04T10:00:00.000000000Z"
        })
    );

    let restored: OperatorWriteProvenance =
        serde_json::from_value(serialized).expect("durable provenance deserializes");
    assert_eq!(restored.written_by, principal);
}

#[test]
fn source_resolution_matches_only_accepted_transport_addresses() {
    let machine_wireguard_id = machine_id(MACHINE_A);
    let peer_wireguard_id = peer_id(PEER_A);
    let machine_tailscale_id = machine_id(MACHINE_B);
    let peer_tailscale_id = peer_id(PEER_B);
    let roster = [
        AcceptedRosterPrincipal::machine(
            machine_wireguard_id.clone(),
            wireguard_machine(Ipv6Addr::from_str("fd00::20").expect("fixture address")),
        ),
        AcceptedRosterPrincipal::peer(
            peer_wireguard_id.clone(),
            wireguard_peer(Ipv6Addr::from_str("fd00::21").expect("fixture address")),
        ),
        AcceptedRosterPrincipal::machine(
            machine_tailscale_id.clone(),
            tailscale_machine(Ipv4Addr::new(100, 64, 0, 20)),
        ),
        AcceptedRosterPrincipal::peer(
            peer_tailscale_id.clone(),
            tailscale_peer(Ipv4Addr::new(100, 64, 0, 21)),
        ),
    ];

    for (source, expected) in [
        (
            IpAddr::V6(Ipv6Addr::from_str("fd00::20").expect("fixture address")),
            Principal::Machine {
                machine_id: machine_wireguard_id,
            },
        ),
        (
            IpAddr::V6(Ipv6Addr::from_str("fd00::21").expect("fixture address")),
            Principal::Peer {
                peer_id: peer_wireguard_id,
            },
        ),
        (
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 20)),
            Principal::Machine {
                machine_id: machine_tailscale_id,
            },
        ),
        (
            IpAddr::V4(Ipv4Addr::new(100, 64, 0, 21)),
            Principal::Peer {
                peer_id: peer_tailscale_id,
            },
        ),
    ] {
        assert_eq!(
            resolve_source_principal(source, &roster).expect("one accepted address match"),
            expected
        );
    }

    for source in [
        IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)),
        IpAddr::V4(Ipv4Addr::new(10, 210, 20, 2)),
        IpAddr::V4(Ipv4Addr::new(10, 210, 30, 2)),
    ] {
        assert_eq!(
            resolve_source_principal(source, &roster),
            Err(SourcePrincipalResolutionError::UnknownSource { source }),
            "endpoints and container subnets never authenticate a principal"
        );
    }
}

#[test]
fn duplicate_accepted_addresses_fail_closed_as_ambiguous() {
    let address = Ipv6Addr::from_str("fd00::99").expect("fixture address");
    let source = IpAddr::V6(address);
    let roster = [
        AcceptedRosterPrincipal::machine(machine_id(MACHINE_A), wireguard_machine(address)),
        AcceptedRosterPrincipal::peer(peer_id(PEER_A), wireguard_peer(address)),
    ];

    let error = resolve_source_principal(source, &roster).expect_err("duplicate address fails");
    assert_eq!(
        error,
        SourcePrincipalResolutionError::AmbiguousSource {
            source,
            candidate_count: 2,
        }
    );
    assert_eq!(
        ApiRefusal::from(error),
        ApiRefusal::AmbiguousSource {
            source,
            candidate_count: 2,
        }
    );
}

#[test]
fn operation_evidence_routes_are_row_id_based_and_have_no_list_alias() {
    let operation_id = operation_id(MACHINE_A);
    let service_id = service_id(MACHINE_B);
    let peer = Principal::Peer {
        peer_id: peer_id(PEER_A),
    };
    let machine = Principal::Machine {
        machine_id: machine_id(MACHINE_A),
    };
    let token = Principal::ApiToken {
        token_id: TokenId::try_new(PEER_B).expect("fixture token id"),
    };

    let routes = [
        (
            V2Route::NamespaceCreate,
            NAMESPACE_CREATE_ROUTE.to_owned(),
            V2Method::Post,
            KnownApiFeature::NamespacePrimitives,
            false,
        ),
        (
            V2Route::NamespaceRemove,
            NAMESPACE_REMOVE_ROUTE.to_owned(),
            V2Method::Post,
            KnownApiFeature::NamespacePrimitives,
            false,
        ),
        (
            V2Route::Deploy,
            DEPLOY_ROUTE.to_owned(),
            V2Method::Post,
            KnownApiFeature::Deploy,
            false,
        ),
        (
            V2Route::Operation(operation_id.clone()),
            operation_route(&operation_id),
            V2Method::Get,
            KnownApiFeature::OperationEvidence,
            true,
        ),
        (
            V2Route::OperationWatch(operation_id.clone()),
            operation_watch_route(&operation_id),
            V2Method::Get,
            KnownApiFeature::OperationEvidence,
            true,
        ),
        (
            V2Route::ServiceLogsTail(service_id.clone()),
            service_logs_tail_route(&service_id),
            V2Method::Post,
            KnownApiFeature::Logs,
            true,
        ),
        (
            V2Route::ServiceLogsFollow(service_id.clone()),
            service_logs_follow_route(&service_id),
            V2Method::Post,
            KnownApiFeature::Logs,
            true,
        ),
    ];

    for (route, path, method, feature, accepts_machine) in routes {
        assert_eq!(route.path(), path);
        assert_eq!(V2Route::parse(&path), Some(route.clone()));
        assert_eq!(route.method(), method);
        assert_eq!(route.feature(), feature);
        assert!(route.accepts_principal(&peer));
        assert_eq!(route.accepts_principal(&machine), accepts_machine);
        assert!(!route.accepts_principal(&token));
    }

    assert_eq!(V2Route::parse("/operations"), None);
    assert_eq!(V2Route::parse("/operations/not-a-row-id"), None);
    assert_eq!(
        V2Route::parse(&format!("{}/again", operation_watch_route(&operation_id))),
        None
    );
    assert_eq!(V2Route::parse("/services/not-a-row-id/logs"), None);
}

#[test]
fn four_additive_operation_spine_features_are_advertised() {
    for feature in [
        KnownApiFeature::NamespacePrimitives,
        KnownApiFeature::Deploy,
        KnownApiFeature::OperationEvidence,
        KnownApiFeature::Logs,
    ] {
        assert!(KNOWN_API_FEATURES.contains(&feature));
    }
}

#[test]
fn missing_namespace_refusal_names_the_resolving_primitive() {
    let namespace_name =
        CorrosionNamespaceName::try_new("payments").expect("fixture namespace name");
    assert_eq!(
        serde_json::to_value(DeployRefusal::namespace_not_found(namespace_name))
            .expect("refusal serializes"),
        json!({
            "kind": "namespace_not_found",
            "namespace_name": "payments",
            "create_command": "ployz namespace create payments"
        })
    );
}

#[test]
fn first_deploy_runtime_debug_redacts_environment_values() {
    let secret = "sentinel-secret-never-in-evidence";
    let mut environment = BTreeMap::new();
    environment.insert(
        EnvName::try_new("TOKEN").expect("fixture env name"),
        EnvValue::try_new(secret).expect("fixture env value"),
    );
    let mut runtime = ContainerRuntimeSpec::image_defaults();
    runtime.environment = ServiceEnvironment::from(environment);
    let request = DeployRequest {
        namespace_name: CorrosionNamespaceName::try_new("payments")
            .expect("fixture namespace name"),
        service_name: ployz_core::corrosion::CorrosionServiceName::try_new("api")
            .expect("fixture service name"),
        image: ImageReference::try_new("registry.example/api:latest")
            .expect("fixture image reference"),
        runtime,
        health_gate: HealthGatePolicy::Enforce,
    };

    assert!(!format!("{request:?}").contains(secret));
    let serialized = serde_json::to_value(&request).expect("authenticated request serializes");
    assert_eq!(
        serialized
            .pointer("/runtime/environment/TOKEN")
            .and_then(serde_json::Value::as_str),
        Some(secret)
    );
    assert!(
        !serde_json::to_string(&OperationEvidence::PullingImage)
            .expect("redaction-safe evidence serializes")
            .contains(secret)
    );
}

#[test]
fn operation_events_require_positive_stable_sequences_and_fixed_timestamps() {
    assert!(OperationEvidenceSequence::try_new(0).is_err());
    assert_eq!(
        serde_json::to_value(OperationEvidence::RowsCommitted)
            .expect("rows-committed evidence serializes"),
        json!({ "kind": "rows_committed" })
    );
    let event = OperationEvidenceEvent {
        sequence: OperationEvidenceSequence::try_new(1).expect("positive sequence"),
        timestamp: CorrosionTimestamp::try_new("2026-08-05T12:34:56Z").expect("fixture timestamp"),
        evidence: OperationEvidence::Created,
    };
    let envelope = OperationWatchEvent::Evidence { event };
    assert_eq!(envelope.event_name(), "evidence");
    assert_eq!(
        serde_json::to_value(&envelope).expect("event serializes"),
        json!({
            "kind": "evidence",
            "event": {
                "sequence": 1,
                "timestamp": "2026-08-05T12:34:56.000000000Z",
                "evidence": { "kind": "created" }
            }
        })
    );
    let encoded = serde_json::to_vec(&envelope).expect("event encodes");
    assert_eq!(
        serde_json::from_slice::<OperationWatchEvent>(&encoded).expect("event round-trips"),
        envelope
    );
}

#[test]
fn dark_driver_refusal_carries_a_fixed_handshake_observation() {
    let refusal = ServiceLogsRefusal::DriverDark {
        machine_id: machine_id(MACHINE_A),
        observation: HandshakeObservationOutcome::Observed {
            observation: HandshakeObservation::Ago {
                observed_at: CorrosionTimestamp::try_new("2026-08-05T12:34:56Z")
                    .expect("fixture timestamp"),
                age_seconds: 17,
            },
        },
    };
    assert_eq!(
        serde_json::to_value(refusal).expect("refusal serializes"),
        json!({
            "kind": "driver_dark",
            "machine_id": MACHINE_A,
            "observation": {
                "kind": "observed",
                "observation": {
                    "status": "ago",
                    "observed_at": "2026-08-05T12:34:56.000000000Z",
                    "age_seconds": 17
                }
            }
        })
    );
}

#[test]
fn log_follow_gap_is_explicit_and_tail_counts_are_bounded() {
    let line = ServiceLogsFollowEvent::Line {
        log: ServiceLogLine {
            stream: ServiceLogStream::Stdout,
            line: "ready".to_owned(),
        },
    };
    assert_eq!(line.event_name(), "line");
    let gap = ServiceLogsFollowEvent::Gap;
    assert_eq!(gap.event_name(), "gap");
    assert_eq!(
        serde_json::to_value(gap).expect("gap serializes"),
        json!({ "kind": "gap" })
    );
    assert!(CorrosionLogsTailLines::try_new(0).is_err());
    assert!(CorrosionLogsTailLines::try_new(CorrosionLogsTailLines::MAX).is_ok());
    assert!(CorrosionLogsTailLines::try_new(CorrosionLogsTailLines::MAX + 1).is_err());
}

#[test]
fn machine_status_lens_row_requires_its_machine_owned_key() {
    let document = MachineStatusDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterId::try_new(MACHINE_A).expect("fixture cluster id"),
        machine_id: machine_id(MACHINE_A),
        ployz_version: "0.1.0-alpha.9".to_owned(),
        corrosion_version: "0.2.0".to_owned(),
        architecture: "x86_64".to_owned(),
        free_disk_bytes: 1,
        free_memory_bytes: 1,
        load: MachineLoadBand::Idle,
        observed_at: CorrosionTimestamp::try_new("2026-08-04T10:00:00Z")
            .expect("fixture timestamp"),
        mesh: None,
        container_isolation: None,
        wireguard_handshakes: None,
    };

    assert!(MachineStatusLensRow::try_new(machine_id(MACHINE_A), document.clone()).is_ok());
    assert_eq!(
        MachineStatusLensRow::try_new(machine_id(MACHINE_B), document),
        Err(MachineStatusLensRowIdentityError {
            id: machine_id(MACHINE_B),
            document_machine_id: machine_id(MACHINE_A),
        })
    );
}

#[test]
fn v2_routes_version_and_watch_envelopes_are_stable() {
    assert_eq!(API_MAJOR, 1);
    assert_eq!(VERSION_ROUTE, "/version");
    assert_eq!(FOUNDING_ROUTE, "/founding");
    assert_eq!(V2Route::parse(FOUNDING_ROUTE), Some(V2Route::Founding));
    assert_eq!(V2Route::Founding.path(), FOUNDING_ROUTE);
    assert_eq!(V2Route::Founding.method(), V2Method::Post);
    assert_eq!(V2Route::Version.method(), V2Method::Get);
    assert_eq!(
        V2Route::Lens(LensCollection::Machines).method(),
        V2Method::Get
    );
    assert_eq!(
        V2Route::LensWatch(LensCollection::Machines).method(),
        V2Method::Get
    );
    assert_eq!(
        lens_route(LensCollection::MachineStatus),
        "/lenses/machine_status"
    );
    assert_eq!(
        lens_watch_route(LensCollection::Operations),
        "/lenses/operations/watch"
    );
    assert_eq!(
        V2Route::parse("/lenses/services/watch"),
        Some(V2Route::LensWatch(LensCollection::Services))
    );
    assert_eq!(V2Route::parse("/lenses/services/watch/again"), None);

    let version = ApiVersion::new(
        "0.1.0-alpha.9+abc",
        [
            ApiFeature::from(KnownApiFeature::Founding),
            ApiFeature::from(KnownApiFeature::Lenses),
            ApiFeature::other("future.example"),
        ],
    );
    assert_eq!(
        serde_json::to_value(version).expect("version serializes"),
        json!({
            "major": 1,
            "build": "0.1.0-alpha.9+abc",
            "features": ["v2.founding", "v2.lenses", "future.example"]
        })
    );

    let snapshot = LensSnapshot::Services { rows: Vec::new() };
    let first = LensWatchEvent::snapshot(snapshot.clone());
    assert_eq!(first.event_name(), LENS_SNAPSHOT_EVENT);
    assert_eq!(
        serde_json::to_value(&first).expect("snapshot event serializes"),
        json!({
            "kind": "snapshot",
            "snapshot": { "collection": "services", "rows": [] }
        })
    );
    assert_eq!(
        LensWatchEvent::state(snapshot).event_name(),
        LENS_STATE_EVENT
    );
    assert_eq!(
        LensWatchEvent::terminal(ApiRefusal::MissingCluster).event_name(),
        LENS_TERMINAL_EVENT
    );
}

#[test]
fn deploy_route_and_feature_use_the_generalized_wire_names() {
    assert_eq!(DEPLOY_ROUTE, "/deploy");
    assert_eq!(V2Route::parse(DEPLOY_ROUTE), Some(V2Route::Deploy));
    assert_eq!(V2Route::Deploy.feature(), KnownApiFeature::Deploy);
    assert_eq!(
        serde_json::to_value(KnownApiFeature::Deploy).expect("feature serializes"),
        json!("v2.deploy")
    );
}

#[test]
fn deploy_request_health_gate_defaults_to_enforce_and_skip_is_explicit() {
    let request: DeployRequest = serde_json::from_value(json!({
        "namespace_name": "payments",
        "service_name": "api",
        "image": "registry.example/api:latest",
        "runtime": serde_json::to_value(ContainerRuntimeSpec::image_defaults())
            .expect("runtime serializes"),
    }))
    .expect("request without health_gate deserializes");
    assert_eq!(request.health_gate, HealthGatePolicy::Enforce);

    assert_eq!(
        serde_json::to_value(HealthGatePolicy::Skip).expect("policy serializes"),
        json!("skip")
    );
}

#[test]
fn blue_green_evidence_variants_have_closed_wire_shapes() {
    let container_id = ployz_core::ids::ContainerId::try_new("c0ffee").expect("container id");
    let winner = operation_id(MACHINE_B);
    for (evidence, expected) in [
        (
            OperationEvidence::OpClaimWon,
            json!({ "kind": "op_claim_won" }),
        ),
        (
            OperationEvidence::OpClaimLost {
                winner: winner.clone(),
            },
            json!({ "kind": "op_claim_lost", "winner": MACHINE_B }),
        ),
        (
            OperationEvidence::DebrisSwept {
                removed: vec![container_id.clone()],
            },
            json!({ "kind": "debris_swept", "removed": ["c0ffee"] }),
        ),
        (
            OperationEvidence::IncumbentStopped {
                container_id: container_id.clone(),
            },
            json!({ "kind": "incumbent_stopped", "container_id": "c0ffee" }),
        ),
        (
            OperationEvidence::IncumbentRestarted {
                container_id: container_id.clone(),
            },
            json!({ "kind": "incumbent_restarted", "container_id": "c0ffee" }),
        ),
        (
            OperationEvidence::IncumbentRemoved { container_id },
            json!({ "kind": "incumbent_removed", "container_id": "c0ffee" }),
        ),
        (OperationEvidence::Drained, json!({ "kind": "drained" })),
        (
            OperationEvidence::HealthGateSkipped,
            json!({ "kind": "health_gate_skipped" }),
        ),
    ] {
        let serialized = serde_json::to_value(&evidence).expect("evidence serializes");
        assert_eq!(serialized, expected);
        assert_eq!(
            serde_json::from_value::<OperationEvidence>(serialized).expect("evidence round-trips"),
            evidence
        );
    }
}
