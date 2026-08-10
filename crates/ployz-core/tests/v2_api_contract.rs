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
use ployz_core::ids::{ClusterName, MachineName, PeerName, TokenName};
use ployz_core::network::{MachineEndpointSubnet, WireGuardPublicKey};
use ployz_core::{
    API_MAJOR, ApiFeature, ApiRefusal, ApiVersion, CorrosionLogsTailLines, DEPLOY_INSPECT_ROUTE,
    DEPLOY_PREPARE_ROUTE, DEPLOY_RETIRE_ROUTE, DEPLOY_ROUTE, DeployRefusal, DeployRequest,
    DeployServiceRequest, FOUNDING_ROUTE, HealthGatePolicy, KNOWN_API_FEATURES, KnownApiFeature,
    LENS_SNAPSHOT_EVENT, LENS_STATE_EVENT, LENS_TERMINAL_EVENT, LensCollection, LensSnapshot,
    LensWatchEvent, MachineStatusLensRow, MachineStatusLensRowIdentityError,
    NAMESPACE_CREATE_ROUTE, NAMESPACE_REMOVE_ROUTE, PinnedMachineNames, RequestedPlacement,
    SERVICE_LOGS_PROBE_ROUTE, ServiceLogLine, ServiceLogStream, ServiceLogsFollowEvent,
    ServiceLogsRefusal, ServiceLogsRequest, V2Method, V2Route, VERSION_ROUTE, lens_route,
    lens_watch_route, service_logs_follow_route, service_logs_tail_route,
};
use serde_json::json;

const MACHINE_A: &str = "edge-a";
const MACHINE_B: &str = "edge-b";
const PEER_A: &str = "operator-a";
const PEER_B: &str = "operator-b";

fn machine_id(value: &str) -> MachineName {
    MachineName::try_new(value).expect("fixture machine id")
}

fn namespace_name(value: &str) -> CorrosionNamespaceName {
    CorrosionNamespaceName::try_new(value).expect("fixture namespace name")
}

fn peer_id(value: &str) -> PeerName {
    PeerName::try_new(value).expect("fixture peer id")
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
fn v2_routes_have_exact_paths_methods_features_and_principals() {
    let namespace_name = namespace_name(MACHINE_B);
    let peer = Principal::Peer {
        peer_id: peer_id(PEER_A),
    };
    let machine = Principal::Machine {
        machine_id: machine_id(MACHINE_A),
    };
    let token = Principal::ApiToken {
        token_id: TokenName::try_new(PEER_B).expect("fixture token id"),
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
            V2Route::ServiceLogsTail(
                namespace_name.clone(),
                ployz_core::corrosion::CorrosionServiceName::try_new("api").expect("service"),
            ),
            service_logs_tail_route(
                &namespace_name,
                &ployz_core::corrosion::CorrosionServiceName::try_new("api").expect("service"),
            ),
            V2Method::Post,
            KnownApiFeature::Logs,
            true,
        ),
        (
            V2Route::ServiceLogsFollow(
                namespace_name.clone(),
                ployz_core::corrosion::CorrosionServiceName::try_new("api").expect("service"),
            ),
            service_logs_follow_route(
                &namespace_name,
                &ployz_core::corrosion::CorrosionServiceName::try_new("api").expect("service"),
            ),
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

    assert_eq!(V2Route::parse("/services/not-a-row-id/logs"), None);
}

#[test]
fn four_additive_operation_spine_features_are_advertised() {
    for feature in [
        KnownApiFeature::NamespacePrimitives,
        KnownApiFeature::Deploy,
        KnownApiFeature::OperationStatus,
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
        deploy_name: ployz_core::ids::DeployName::try_new("release-1").expect("deploy"),
        services: [(
            ployz_core::corrosion::CorrosionServiceName::try_new("api")
                .expect("fixture service name"),
            DeployServiceRequest {
                image: ImageReference::try_new("registry.example/api:latest")
                    .expect("fixture image reference"),
                credential: None,
                runtime,
                health_gate: HealthGatePolicy::Enforce,
                placement: None,
                machines: None,
            },
        )]
        .into_iter()
        .collect(),
    };

    assert!(!format!("{request:?}").contains(secret));
    let serialized = serde_json::to_value(&request).expect("authenticated request serializes");
    assert_eq!(
        serialized
            .pointer("/services/api/runtime/environment/TOKEN")
            .and_then(serde_json::Value::as_str),
        Some(secret)
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

    let remote_owner = ServiceLogsRefusal::RemoteOwner { machine_name };
    assert_eq!(
        serde_json::to_value(&remote_owner).expect("owner refusal serializes"),
        json!({ "kind": "remote_owner", "machine_name": "edge-a" })
    );
}

#[test]
fn machine_status_lens_row_requires_its_machine_owned_key() {
    let document = MachineStatusDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: ClusterName::try_new(MACHINE_A).expect("fixture cluster id"),
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
fn deploy_effect_routes_parse_build_and_authorize_only_machines() {
    let peer = Principal::Peer {
        peer_id: peer_id(PEER_A),
    };
    let machine = Principal::Machine {
        machine_id: machine_id(MACHINE_A),
    };
    let token = Principal::ApiToken {
        token_id: TokenName::try_new(PEER_B).expect("fixture token id"),
    };

    for (route, path) in [
        (V2Route::DeployInspect, DEPLOY_INSPECT_ROUTE),
        (V2Route::DeployPrepare, DEPLOY_PREPARE_ROUTE),
        (V2Route::DeployRetire, DEPLOY_RETIRE_ROUTE),
    ] {
        assert_eq!(route.path(), path);
        assert_eq!(V2Route::parse(path), Some(route.clone()));
        assert_eq!(route.method(), V2Method::Post);
        assert_eq!(route.feature(), KnownApiFeature::Deploy);
        assert!(route.accepts_principal(&machine));
        assert!(
            !route.accepts_principal(&peer),
            "target-host effects are machine authority"
        );
        assert!(!route.accepts_principal(&token));
        assert_eq!(V2Route::parse(&format!("{path}/extra")), None);
    }
}

#[test]
fn service_log_probe_is_machine_only() {
    let peer = Principal::Peer {
        peer_id: peer_id(PEER_A),
    };
    let machine = Principal::Machine {
        machine_id: machine_id(MACHINE_A),
    };
    let route = V2Route::ServiceLogsProbe;
    assert_eq!(route.path(), SERVICE_LOGS_PROBE_ROUTE);
    assert_eq!(
        V2Route::parse(SERVICE_LOGS_PROBE_ROUTE),
        Some(route.clone())
    );
    assert_eq!(route.method(), V2Method::Post);
    assert_eq!(route.feature(), KnownApiFeature::Logs);
    assert!(route.accepts_principal(&machine));
    assert!(!route.accepts_principal(&peer));
    assert_eq!(V2Route::parse("/services/logs/probe/extra"), None);
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
        serde_json::from_value::<RequestedPlacement>(json!({ "mode": "replicated" })).is_err(),
        "an explicit replicated placement names its count; omit placement for the default"
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
    assert_eq!(host_ports.len(), 1);

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
fn requested_pins_require_at_least_one_machine_name() {
    let pins: PinnedMachineNames =
        serde_json::from_value(json!(["edge-a", "edge-b"])).expect("named pins deserialize");
    assert_eq!(pins.iter().count(), 2);
    assert!(
        serde_json::from_value::<PinnedMachineNames>(json!([])).is_err(),
        "an empty pin set is expressed by omitting machines, never an empty list"
    );
}

#[test]
fn deploy_requests_may_omit_placement_and_pins_for_fixed_defaults() {
    let request: DeployRequest = serde_json::from_value(json!({
        "namespace_name": "payments",
        "deploy_name": "release-1",
        "services": {"api": {
            "image": "registry.example/api:latest",
            "runtime": serde_json::to_value(ContainerRuntimeSpec::image_defaults())
                .expect("runtime serializes"),
        }},
    }))
    .expect("request without placement deserializes");
    let service = request
        .services
        .get(&ployz_core::corrosion::CorrosionServiceName::try_new("api").expect("service"))
        .expect("one requested service");
    assert_eq!(service.placement, None);
    assert_eq!(service.machines, None);
    let serialized = serde_json::to_value(&request).expect("request serializes");
    let service = serialized
        .get("services")
        .and_then(|services| services.get("api"))
        .expect("one serialized service");
    assert_eq!(service.get("placement"), None);
    assert_eq!(service.get("machines"), None);
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
fn deploy_request_health_gate_defaults_to_enforce_and_skip_is_explicit() {
    let request: DeployRequest = serde_json::from_value(json!({
        "namespace_name": "payments",
        "deploy_name": "release-1",
        "services": {"api": {
            "image": "registry.example/api:latest",
            "runtime": serde_json::to_value(ContainerRuntimeSpec::image_defaults())
                .expect("runtime serializes"),
        }},
    }))
    .expect("request without health_gate deserializes");
    assert_eq!(
        request
            .services
            .get(&ployz_core::corrosion::CorrosionServiceName::try_new("api").expect("service"))
            .expect("one requested service")
            .health_gate,
        HealthGatePolicy::Enforce
    );

    assert_eq!(
        serde_json::to_value(HealthGatePolicy::Skip).expect("policy serializes"),
        json!("skip")
    );
}
