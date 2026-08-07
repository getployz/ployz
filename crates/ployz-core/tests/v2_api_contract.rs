use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::num::NonZeroU16;
use std::str::FromStr;

use ployz_core::corrosion::{
    AcceptedRosterPrincipal, CorrosionDocumentVersion, CorrosionNamespaceName, CorrosionTimestamp,
    HostPortBinding, HostPortBindings, HostPortProtocol, MachineLoadBand, MachineStatusDocument,
    MachineTransport, OperationInitiator, OperatorWriteProvenance, PeerTransport, Principal,
    ServicePlacement, ServiceReplicaCount, SourcePrincipalResolutionError,
    resolve_source_principal,
};
use ployz_core::deploy::{
    ContainerRuntimeSpec, EnvName, EnvValue, ImageReference, ServiceEnvironment,
};
use ployz_core::ids::{ClusterId, MachineRowId, OperationRowId, PeerId, ServiceRowId, TokenId};
use ployz_core::machine::MachineLifecycle;
use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};
use ployz_core::placement::{PlacementElimination, PlacementEliminationReason, PlacementShortfall};
use ployz_core::{
    API_MAJOR, ApiFeature, ApiRefusal, ApiVersion, CorrosionLogsTailLines, DEPLOY_EXECUTE_ROUTE,
    DEPLOY_ROUTE, DeployExecuteOutcome, DeployExecuteRequest, DeployRefusal, DeployRequest,
    DeployVerb, FOUNDING_ROUTE, HandshakeObservation, HandshakeObservationOutcome,
    HealthGatePolicy, KNOWN_API_FEATURES, KnownApiFeature, LENS_SNAPSHOT_EVENT, LENS_STATE_EVENT,
    LENS_TERMINAL_EVENT, LensCollection, LensSnapshot, LensWatchEvent, MachineStatusLensRow,
    MachineStatusLensRowIdentityError, NAMESPACE_CREATE_ROUTE, NAMESPACE_REMOVE_ROUTE,
    OperationEvidence, OperationEvidenceEvent, OperationEvidenceSequence, OperationWatchEvent,
    PLACEMENT_BID_ROUTE, PlacementBid, RequestedPins, RequestedPlacement,
    ServiceContainerObservation, ServiceLogLine, ServiceLogStream, ServiceLogsFollowEvent,
    ServiceLogsRefusal, ServiceLogsRequest, SilenceClassification, SilentMachine, V2Method,
    V2Route, VERSION_ROUTE, lens_route, lens_watch_route, operation_route, operation_watch_route,
    service_logs_follow_route, service_logs_tail_route,
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
fn redeploy_admission_refusals_use_stable_snake_case_wire_names() {
    let namespace_id = ployz_core::ids::NamespaceRowId::try_new("01J00000000000000000000013")
        .expect("fixture namespace id");
    let service_id = ployz_core::ids::ServiceRowId::try_new("01J00000000000000000000014")
        .expect("fixture service id");
    assert_eq!(
        serde_json::to_value(DeployRefusal::DifferentService {
            namespace_id: namespace_id.clone(),
            incumbent_service_name: ployz_core::corrosion::CorrosionServiceName::try_new("web")
                .expect("fixture service name"),
        })
        .expect("refusal serializes"),
        json!({
            "kind": "different_service",
            "namespace_id": "01J00000000000000000000013",
            "incumbent_service_name": "web"
        })
    );
    assert_eq!(
        serde_json::to_value(DeployRefusal::MultipleServices {
            namespace_id: namespace_id.clone(),
            service_ids: vec![service_id],
        })
        .expect("refusal serializes"),
        json!({
            "kind": "multiple_services",
            "namespace_id": "01J00000000000000000000013",
            "service_ids": ["01J00000000000000000000014"]
        })
    );
    assert_eq!(
        serde_json::to_value(DeployRefusal::RoutesWithoutServices { namespace_id })
            .expect("refusal serializes"),
        json!({
            "kind": "routes_without_services",
            "namespace_id": "01J00000000000000000000013"
        })
    );
}

#[test]
fn placement_refusals_name_the_blocking_machines_and_resolvers() {
    let machine_id = machine_id(MACHINE_A);
    let other = ployz_core::ids::MachineRowId::try_new(MACHINE_B).expect("fixture machine id");
    let volume = ployz_core::deploy::VolumeName::try_new("data").expect("fixture volume name");
    assert_eq!(
        serde_json::to_value(DeployRefusal::NoEligibleMachines {
            eliminations: vec![PlacementElimination {
                machine_id: machine_id.clone(),
                reason: PlacementEliminationReason::Draining,
            }],
        })
        .expect("refusal serializes"),
        json!({
            "kind": "no_eligible_machines",
            "eliminations": [{
                "machine_id": MACHINE_A,
                "reason": { "kind": "draining" }
            }]
        })
    );
    assert_eq!(
        serde_json::to_value(DeployRefusal::VolumeHolderConflict {
            volume: volume.clone(),
            holders: vec![machine_id.clone(), other.clone()],
        })
        .expect("refusal serializes"),
        json!({
            "kind": "volume_holder_conflict",
            "volume": "data",
            "holders": [MACHINE_A, MACHINE_B]
        })
    );
    assert_eq!(
        serde_json::to_value(DeployRefusal::DarkVolumeHolder {
            machines: vec![other],
        })
        .expect("refusal serializes"),
        json!({ "kind": "dark_volume_holder", "machines": [MACHINE_B] })
    );
    assert_eq!(
        serde_json::to_value(DeployRefusal::VolumeReplicaLimit {
            requested: ployz_core::corrosion::ServiceReplicaCount::try_new(3)
                .expect("fixture replica count"),
        })
        .expect("refusal serializes"),
        json!({ "kind": "volume_replica_limit", "requested": 3 })
    );
    assert_eq!(
        serde_json::to_value(DeployRefusal::UnknownPinnedMachine {
            machine_name: ployz_core::machine::MachineName::try_new("edge-a")
                .expect("fixture machine name"),
        })
        .expect("refusal serializes"),
        json!({ "kind": "unknown_pinned_machine", "machine_name": "edge-a" })
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
        placement: None,
        machines: None,
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
fn log_requests_carry_an_optional_machine_selector_and_replay_free_reconnects() {
    let machine_name = ployz_core::machine::MachineName::try_new("edge-a").expect("machine name");
    let selected = ServiceLogsRequest {
        tail_lines: Some(CorrosionLogsTailLines::try_new(100).expect("tail lines")),
        machine: Some(machine_name.clone()),
    };
    assert_eq!(
        serde_json::to_value(&selected).expect("request serializes"),
        json!({ "tail_lines": 100, "machine": "edge-a" })
    );
    let encoded = serde_json::to_vec(&selected).expect("request encodes");
    assert_eq!(
        serde_json::from_slice::<ServiceLogsRequest>(&encoded).expect("request round-trips"),
        selected
    );

    let reconnect = ServiceLogsRequest {
        tail_lines: None,
        machine: Some(machine_name.clone()),
    };
    assert_eq!(
        serde_json::to_value(&reconnect).expect("reconnect serializes"),
        json!({ "machine": "edge-a" })
    );
    let pre_selector = serde_json::from_value::<ServiceLogsRequest>(json!({ "tail_lines": 5 }))
        .expect("pre-selector request decodes");
    assert_eq!(pre_selector.machine, None);

    let selector_required = ServiceLogsRefusal::MachineSelectorRequired {
        machines: vec![machine_name.clone(), machine_name.clone()],
    };
    assert_eq!(
        serde_json::to_value(&selector_required).expect("refusal serializes"),
        json!({ "kind": "machine_selector_required", "machines": ["edge-a", "edge-a"] })
    );
    let encoded = serde_json::to_vec(&selector_required).expect("refusal encodes");
    assert_eq!(
        serde_json::from_slice::<ServiceLogsRefusal>(&encoded).expect("refusal round-trips"),
        selector_required
    );

    let remote_owner = ServiceLogsRefusal::RemoteOwner {
        machine_id: machine_id(MACHINE_A),
        machine_name: Some(machine_name),
    };
    assert_eq!(
        serde_json::to_value(&remote_owner).expect("owner refusal serializes"),
        json!({ "kind": "remote_owner", "machine_id": MACHINE_A, "machine_name": "edge-a" })
    );
    let unnamed = serde_json::from_value::<ServiceLogsRefusal>(
        json!({ "kind": "remote_owner", "machine_id": MACHINE_A }),
    )
    .expect("pre-name owner refusal decodes");
    assert_eq!(
        unnamed,
        ServiceLogsRefusal::RemoteOwner {
            machine_id: machine_id(MACHINE_A),
            machine_name: None,
        }
    );
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
fn placement_routes_parse_build_and_authorize_like_machine_surfaces() {
    let peer = Principal::Peer {
        peer_id: peer_id(PEER_A),
    };
    let machine = Principal::Machine {
        machine_id: machine_id(MACHINE_A),
    };
    let token = Principal::ApiToken {
        token_id: TokenId::try_new(PEER_B).expect("fixture token id"),
    };

    for (route, path) in [
        (V2Route::PlacementBid, PLACEMENT_BID_ROUTE),
        (V2Route::DeployExecute, DEPLOY_EXECUTE_ROUTE),
    ] {
        assert_eq!(route.path(), path);
        assert_eq!(V2Route::parse(path), Some(route.clone()));
        assert_eq!(route.method(), V2Method::Post);
        assert_eq!(route.feature(), KnownApiFeature::Placement);
        assert!(route.accepts_principal(&machine));
        assert!(route.accepts_principal(&peer));
        assert!(!route.accepts_principal(&token));
    }
    assert_eq!(PLACEMENT_BID_ROUTE, "/deploy/bid");
    assert_eq!(DEPLOY_EXECUTE_ROUTE, "/deploy/execute");
    assert_eq!(V2Route::parse("/deploy/bid/extra"), None);
    assert_eq!(V2Route::parse("/deploy/execute/extra"), None);
    assert!(KNOWN_API_FEATURES.contains(&KnownApiFeature::Placement));
    assert_eq!(
        serde_json::to_value(KnownApiFeature::Placement).expect("feature serializes"),
        json!("v2.placement")
    );
}

#[test]
fn requested_placement_makes_host_ports_unrepresentable_off_global() {
    let replicated: RequestedPlacement = serde_json::from_value(json!({
        "mode": "replicated",
        "replicas": 2
    }))
    .expect("replicated placement deserializes");
    assert_eq!(
        replicated,
        RequestedPlacement::Replicated {
            replicas: ServiceReplicaCount::try_new(2).expect("fixture replica count"),
        }
    );
    assert!(
        serde_json::from_value::<RequestedPlacement>(json!({
            "mode": "replicated",
            "replicas": 1,
            "host_ports": [{ "host_port": 53, "container_port": 53, "protocol": "udp" }]
        }))
        .is_err(),
        "a replicated placement can never carry host ports"
    );

    let global: RequestedPlacement = serde_json::from_value(json!({
        "mode": "global",
        "host_ports": [{ "host_port": 53, "container_port": 5353, "protocol": "udp" }]
    }))
    .expect("global placement deserializes");
    let RequestedPlacement::Global { host_ports } = &global else {
        panic!("global placement expected");
    };
    assert_eq!(host_ports.as_slice().len(), 1);

    let portless: RequestedPlacement =
        serde_json::from_value(json!({ "mode": "global" })).expect("portless global deserializes");
    assert_eq!(
        portless,
        RequestedPlacement::Global {
            host_ports: HostPortBindings::default(),
        }
    );
}

#[test]
fn host_port_sets_refuse_duplicates_per_protocol_and_allow_tcp_udp_reuse() {
    let port = |value: u16| NonZeroU16::new(value).expect("fixture port");
    let binding = |host: u16, protocol: HostPortProtocol| HostPortBinding {
        host_port: port(host),
        container_port: port(8080),
        protocol,
    };

    assert!(
        HostPortBindings::try_new([
            binding(53, HostPortProtocol::Tcp),
            binding(53, HostPortProtocol::Udp),
        ])
        .is_ok(),
        "one host port may forward both TCP and UDP"
    );
    assert!(
        serde_json::from_value::<HostPortBindings>(json!([
            { "host_port": 53, "container_port": 8080, "protocol": "tcp" },
            { "host_port": 53, "container_port": 9090, "protocol": "tcp" }
        ]))
        .is_err(),
        "the same host port and protocol can never bind twice"
    );
}

#[test]
fn requested_pins_require_at_least_one_machine_name_or_the_any_clearer() {
    let pins: RequestedPins = serde_json::from_value(json!({
        "kind": "machines",
        "names": ["edge-a", "edge-b"]
    }))
    .expect("named pins deserialize");
    let RequestedPins::Machines { names } = &pins else {
        panic!("named pins expected");
    };
    assert_eq!(names.iter().count(), 2);
    assert!(
        serde_json::from_value::<RequestedPins>(json!({ "kind": "machines", "names": [] }))
            .is_err(),
        "an empty pin set is expressed as `any`, never an empty list"
    );
    assert_eq!(
        serde_json::from_value::<RequestedPins>(json!({ "kind": "any" }))
            .expect("any deserializes"),
        RequestedPins::Any
    );
}

#[test]
fn deploy_requests_without_placement_or_pins_inherit_by_omission() {
    let request: DeployRequest = serde_json::from_value(json!({
        "namespace_name": "payments",
        "service_name": "api",
        "image": "registry.example/api:latest",
        "runtime": serde_json::to_value(ContainerRuntimeSpec::image_defaults())
            .expect("runtime serializes"),
    }))
    .expect("request without placement deserializes");
    assert_eq!(request.placement, None);
    assert_eq!(request.machines, None);
    let serialized = serde_json::to_value(&request).expect("request serializes");
    assert_eq!(serialized.get("placement"), None);
    assert_eq!(serialized.get("machines"), None);
}

#[test]
fn durable_global_placement_reads_rows_written_before_host_ports() {
    let placement: ServicePlacement =
        serde_json::from_value(json!({ "mode": "global" })).expect("portless row deserializes");
    assert_eq!(
        placement,
        ServicePlacement::Global {
            host_ports: HostPortBindings::default(),
        }
    );
    assert_eq!(
        serde_json::to_value(&placement).expect("placement serializes"),
        json!({ "mode": "global" }),
        "an empty port set keeps the pre-placement wire form"
    );
}

#[test]
fn machine_load_bands_order_idle_before_normal_before_hot() {
    assert!(MachineLoadBand::Idle < MachineLoadBand::Normal);
    assert!(MachineLoadBand::Normal < MachineLoadBand::Hot);
}

#[test]
fn unknown_evidence_kinds_deserialize_as_unrecognized_on_older_readers() {
    let unknown: OperationEvidence = serde_json::from_value(json!({
        "kind": "future_evidence_kind",
        "detail": { "anything": true }
    }))
    .expect("unknown evidence kind deserializes");
    assert_eq!(unknown, OperationEvidence::Unrecognized);
    assert_eq!(
        serde_json::to_value(OperationEvidence::Unrecognized).expect("catch-all serializes"),
        json!({ "kind": "unrecognized" })
    );
}

#[test]
fn per_container_evidence_written_before_placement_defaults_its_machine() {
    let evidence: OperationEvidence = serde_json::from_value(json!({
        "kind": "container_created",
        "container_id": "c0ffee"
    }))
    .expect("pre-placement evidence deserializes");
    let OperationEvidence::ContainerCreated {
        container_id,
        machine,
    } = &evidence
    else {
        panic!("container-created evidence expected");
    };
    assert_eq!(container_id.as_str(), "c0ffee");
    assert_eq!(machine, &None);

    let placed: OperationEvidence = serde_json::from_value(json!({
        "kind": "container_created",
        "container_id": "c0ffee",
        "machine": MACHINE_A
    }))
    .expect("placed evidence deserializes");
    assert_eq!(
        placed,
        OperationEvidence::ContainerCreated {
            container_id: ployz_core::ids::ContainerId::try_new("c0ffee")
                .expect("fixture container id"),
            machine: Some(machine_id(MACHINE_A)),
        }
    );
}

#[test]
fn placement_evidence_replays_the_gather_and_the_pick() {
    let bid = PlacementBid {
        machine_id: machine_id(MACHINE_A),
        machine_name: ployz_core::machine::MachineName::try_new("edge-a")
            .expect("fixture machine name"),
        architecture: "x86_64".to_owned(),
        lifecycle: MachineLifecycle::Active,
        free_disk_bytes: 10 * 1024 * 1024 * 1024,
        free_memory_bytes: 2 * 1024 * 1024 * 1024,
        load: MachineLoadBand::Idle,
        total_container_count: 3,
        service_containers: vec![ServiceContainerObservation {
            container_id: ployz_core::ids::ContainerId::try_new("c0ffee")
                .expect("fixture container id"),
            deploy: operation_id(MACHINE_B),
            running: true,
            named_volumes: std::collections::BTreeSet::new(),
        }],
        volumes_held: std::collections::BTreeSet::new(),
    };
    let gathered = OperationEvidence::PlacementGathered {
        bids: vec![bid],
        silent: vec![
            SilentMachine {
                machine_id: machine_id(MACHINE_B),
                classification: SilenceClassification::ExpectedSilent {
                    handshake_age_seconds: 2460,
                },
            },
            SilentMachine {
                machine_id: machine_id(MACHINE_A),
                classification: SilenceClassification::AnomalousSilent {
                    reason: "bid request timed out".to_owned(),
                },
            },
        ],
    };
    let encoded = serde_json::to_value(&gathered).expect("gathered evidence serializes");
    assert_eq!(
        serde_json::from_value::<OperationEvidence>(encoded).expect("evidence round-trips"),
        gathered
    );

    let picked = OperationEvidence::PlacementPicked {
        targets: vec![machine_id(MACHINE_A), machine_id(MACHINE_A)],
        eliminations: vec![PlacementElimination {
            machine_id: machine_id(MACHINE_B),
            reason: PlacementEliminationReason::FreeDiskBelowFloor {
                free_disk_bytes: 512,
            },
        }],
        shortfall: Some(PlacementShortfall {
            requested: ServiceReplicaCount::try_new(2).expect("fixture replica count"),
            placed: 1,
        }),
    };
    let encoded = serde_json::to_value(&picked).expect("picked evidence serializes");
    assert_eq!(
        serde_json::from_value::<OperationEvidence>(encoded).expect("evidence round-trips"),
        picked
    );
}

#[test]
fn deploy_execute_verbs_bind_their_operation_and_service_identity() {
    let request = DeployExecuteRequest {
        operation_id: operation_id(MACHINE_A),
        namespace_id: ployz_core::ids::NamespaceRowId::try_new("01J00000000000000000000013")
            .expect("fixture namespace id"),
        service_id: service_id(MACHINE_B),
        verb: DeployVerb::StartContainer {
            container_id: ployz_core::ids::ContainerId::try_new("c0ffee")
                .expect("fixture container id"),
        },
    };
    let encoded = serde_json::to_value(&request).expect("verb request serializes");
    assert_eq!(
        encoded,
        json!({
            "operation_id": MACHINE_A,
            "namespace_id": "01J00000000000000000000013",
            "service_id": MACHINE_B,
            "verb": { "kind": "start_container", "container_id": "c0ffee" }
        })
    );
    assert_eq!(
        serde_json::from_value::<DeployExecuteRequest>(encoded).expect("request round-trips"),
        request
    );

    assert_eq!(
        serde_json::to_value(DeployExecuteOutcome::ClaimNotYetVisible).expect("outcome serializes"),
        json!({ "kind": "claim_not_yet_visible" }),
        "replication lag is its own outcome, distinct from every refusal"
    );
    assert_eq!(
        serde_json::to_value(DeployExecuteOutcome::CallerNotDriver {
            driver: machine_id(MACHINE_A),
        })
        .expect("outcome serializes"),
        json!({ "kind": "caller_not_driver", "driver": MACHINE_A })
    );
}

#[test]
fn create_container_verb_host_ports_default_empty_and_round_trip() {
    let bare = json!({
        "kind": "create_container",
        "image": "registry.example/api@sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
        "runtime": serde_json::to_value(ContainerRuntimeSpec::image_defaults())
            .expect("runtime serializes"),
        "namespace_name": "payments",
    });
    let verb: DeployVerb =
        serde_json::from_value(bare).expect("verb without host_ports deserializes");
    let DeployVerb::CreateContainer { host_ports, .. } = &verb else {
        panic!("create verb expected");
    };
    assert!(host_ports.is_empty());

    let published = DeployVerb::CreateContainer {
        image: ImageReference::try_new("registry.example/api@sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae")
            .expect("fixture image reference"),
        runtime: Box::new(ContainerRuntimeSpec::image_defaults()),
        namespace_name: ployz_core::corrosion::CorrosionNamespaceName::try_new("payments")
            .expect("fixture namespace name"),
        host_ports: ployz_core::corrosion::HostPortBindings::try_new([
            ployz_core::corrosion::HostPortBinding {
                protocol: ployz_core::corrosion::HostPortProtocol::Tcp,
                host_port: std::num::NonZeroU16::new(8443).expect("port"),
                container_port: std::num::NonZeroU16::new(443).expect("port"),
            },
        ])
        .expect("fixture host ports"),
    };
    let encoded = serde_json::to_value(&published).expect("verb serializes");
    assert_eq!(
        encoded
            .get("host_ports")
            .expect("published ports serialize")
            .as_array()
            .expect("port list")
            .len(),
        1
    );
    assert_eq!(
        serde_json::from_value::<DeployVerb>(encoded).expect("verb round-trips"),
        published
    );
}

#[test]
fn deploy_verb_pull_requests_never_debug_print_registry_secrets() {
    let secret = "sentinel-registry-secret";
    let verb = DeployVerb::PullImage {
        image: ImageReference::try_new("registry.example/api@sha256:2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae")
            .expect("fixture image reference"),
        credential: Some(
            ployz_core::image::RegistryCredential::try_basic("robot", secret)
                .expect("fixture credential"),
        ),
    };
    assert!(!format!("{verb:?}").contains(secret));
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
                machine: None,
            },
            json!({ "kind": "debris_swept", "removed": ["c0ffee"] }),
        ),
        (
            OperationEvidence::IncumbentStopped {
                container_id: container_id.clone(),
                machine: None,
            },
            json!({ "kind": "incumbent_stopped", "container_id": "c0ffee" }),
        ),
        (
            OperationEvidence::IncumbentRestarted {
                container_id: container_id.clone(),
                machine: None,
            },
            json!({ "kind": "incumbent_restarted", "container_id": "c0ffee" }),
        ),
        (
            OperationEvidence::IncumbentRemoved {
                container_id,
                machine: None,
            },
            json!({ "kind": "incumbent_removed", "container_id": "c0ffee" }),
        ),
        (OperationEvidence::Drained, json!({ "kind": "drained" })),
        (
            OperationEvidence::HealthGateSkipped,
            json!({ "kind": "health_gate_skipped" }),
        ),
        (
            OperationEvidence::ServiceClaimWon,
            json!({ "kind": "service_claim_won" }),
        ),
        (
            OperationEvidence::ServiceClaimLost {
                winner: ployz_core::ids::ServiceRowId::try_new("01J00000000000000000000014")
                    .expect("service id"),
            },
            json!({ "kind": "service_claim_lost", "winner": "01J00000000000000000000014" }),
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
