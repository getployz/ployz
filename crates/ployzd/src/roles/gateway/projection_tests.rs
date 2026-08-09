use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, ContainerDocument, CorrosionDocumentVersion,
    CorrosionServiceName, CorrosionTimestamp, GatewayProjectionInputKind, GatewayRouteAvailability,
    GatewayRouteProjectionFailure, GatewayRouteProjectionOutcome, GatewayRouteUnavailableReason,
    IngressMode, MachineDocument, MachineStorageIneligibleReason, MachineStorageSelection,
    MachineStorageSelectionReason, MachineTransport, MeshProvider, OperationInitiator,
    OperatorWriteProvenance, RouteBindingDocument, ServiceDocument, ServicePlacement,
    ServiceReplicaCount, Sha256Hex, StorageMode, StoredRow, V2ManagedContainerIdentity,
    managed_container_key,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ClusterName, CorrosionNamespaceName, DeployName, MachineName, PeerName};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::machine::MachineLifecycle;
use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::operation::{RouteHostname, RoutePort};

use super::{GatewayProjectionInput, GatewayUpstream, project_gateway_rows};

const CLUSTER: &str = "main";
const NAMESPACE: &str = "production";
const SERVICE: &str = "production/api";
const DEPLOY_BLUE: &str = "release-blue";
const DEPLOY_GREEN: &str = "release-green";
const MACHINE: &str = "edge-a";
const ROUTE_LOW: &str = "api.example.com";
const ROUTE_HIGH: &str = "admin.example.com";

#[test]
fn direct_route_joins_only_exact_active_deploy_containers() {
    let projection = project_gateway_rows(input(
        vec![stored(SERVICE, &service(DEPLOY_GREEN))],
        vec![stored(ROUTE_LOW, &route(IngressMode::Direct, SERVICE))],
        vec![
            stored_container(&container(SERVICE, NAMESPACE, DEPLOY_BLUE, [10, 20, 0, 2])),
            stored_container(&container(SERVICE, NAMESPACE, DEPLOY_GREEN, [10, 20, 0, 3])),
            stored_container(&container(SERVICE, "staging", DEPLOY_GREEN, [10, 20, 0, 4])),
        ],
    ));

    let [route] = projection.projection.routes.as_slice() else {
        panic!("projection must contain exactly one route");
    };
    assert_eq!(
        route.upstreams,
        [GatewayUpstream {
            container_key: "production/api/release-green/edge-a/global".to_owned(),
            machine_id: machine_id(),
            address: SocketAddr::from(([10, 20, 0, 3], 8080)),
        }]
    );
}

#[test]
fn active_deploy_flip_replaces_the_whole_route_upstream_set() {
    let routes = vec![stored(ROUTE_LOW, &route(IngressMode::Direct, SERVICE))];
    let containers = vec![
        stored_container(&container(SERVICE, NAMESPACE, DEPLOY_BLUE, [10, 20, 0, 2])),
        stored_container(&container(SERVICE, NAMESPACE, DEPLOY_GREEN, [10, 20, 0, 3])),
    ];

    let blue = project_gateway_rows(input(
        vec![stored(SERVICE, &service(DEPLOY_BLUE))],
        routes.clone(),
        containers.clone(),
    ));
    let green = project_gateway_rows(input(
        vec![stored(SERVICE, &service(DEPLOY_GREEN))],
        routes,
        containers,
    ));

    let [blue_route] = blue.projection.routes.as_slice() else {
        panic!("blue projection must contain exactly one route");
    };
    let [blue_upstream] = blue_route.upstreams.as_slice() else {
        panic!("blue route must contain exactly one upstream");
    };
    let [green_route] = green.projection.routes.as_slice() else {
        panic!("green projection must contain exactly one route");
    };
    let [green_upstream] = green_route.upstreams.as_slice() else {
        panic!("green route must contain exactly one upstream");
    };
    assert_eq!(
        blue_upstream.container_key,
        "production/api/release-blue/edge-a/global"
    );
    assert_eq!(
        green_upstream.container_key,
        "production/api/release-green/edge-a/global"
    );
}

#[test]
fn every_valid_binding_has_an_outcome_and_unsupported_routes_are_not_served() {
    let mut higher = route_for_host(IngressMode::Direct, SERVICE, ROUTE_HIGH);
    higher.endpoint_port = RoutePort::try_new(9090).expect("port");
    let projection = project_gateway_rows(input(
        vec![stored(SERVICE, &service(DEPLOY_GREEN))],
        vec![
            stored(ROUTE_HIGH, &higher),
            stored(ROUTE_LOW, &route(IngressMode::Direct, SERVICE)),
            stored(
                "tunnel.example.com",
                &route_for_host(IngressMode::CloudflareTunnel, SERVICE, "tunnel.example.com"),
            ),
        ],
        vec![stored_container(&container(
            SERVICE,
            NAMESPACE,
            DEPLOY_GREEN,
            [10, 20, 0, 3],
        ))],
    ));

    assert_eq!(projection.projection.routes.len(), 2);
    let low = projection
        .projection
        .routes
        .iter()
        .find(|route| route.id.as_str() == ROUTE_LOW)
        .expect("api route");
    let high = projection
        .projection
        .routes
        .iter()
        .find(|route| route.id.as_str() == ROUTE_HIGH)
        .expect("admin route");
    assert_eq!(
        low.upstreams.first().expect("api upstream").address.port(),
        8080
    );
    assert_eq!(
        high.upstreams
            .first()
            .expect("admin upstream")
            .address
            .port(),
        9090
    );
    assert_eq!(projection.route_observations.len(), 3);
    assert!(
        projection
            .route_observations
            .iter()
            .any(|observation| matches!(
                observation.outcome,
                GatewayRouteProjectionOutcome::Failed {
                    failure: GatewayRouteProjectionFailure::UnsupportedIngressMode { .. }
                }
            ))
    );
}

#[test]
fn accepted_route_without_an_exact_service_remains_known_but_empty() {
    let projection = project_gateway_rows(input(
        vec![stored(SERVICE, &service(DEPLOY_GREEN))],
        vec![stored(
            ROUTE_LOW,
            &route(IngressMode::Direct, "production/missing"),
        )],
        Vec::new(),
    ));

    let [route] = projection.projection.routes.as_slice() else {
        panic!("projection must contain exactly one route");
    };
    assert!(route.upstreams.is_empty());
    let [observation] = projection.route_observations.as_slice() else {
        panic!("known route must have exactly one observation");
    };
    assert!(matches!(
        observation.outcome,
        GatewayRouteProjectionOutcome::Applied {
            availability: GatewayRouteAvailability::Unavailable {
                reason: GatewayRouteUnavailableReason::ServiceMissing
            }
        }
    ));
}

#[test]
fn malformed_foreign_and_newer_rows_are_ignored_tolerantly() {
    let mut foreign = route(IngressMode::Direct, SERVICE);
    foreign.cluster_id = ClusterName::try_new("other").expect("cluster");
    let projection = project_gateway_rows(input(
        vec![StoredRow::new(SERVICE, "not json")],
        vec![
            StoredRow::new(ROUTE_LOW, "{}"),
            stored(ROUTE_HIGH, &foreign),
        ],
        vec![StoredRow::new("container", r#"{"v":2}"#)],
    ));

    assert!(projection.projection.routes.is_empty());
    assert!(projection.aggregate_failures.iter().any(|failure| {
        failure.input == GatewayProjectionInputKind::Services && failure.rejected_rows == 1
    }));
    assert!(projection.aggregate_failures.iter().any(|failure| {
        failure.input == GatewayProjectionInputKind::RouteBindings && failure.rejected_rows == 2
    }));
    assert!(projection.aggregate_failures.iter().any(|failure| {
        failure.input == GatewayProjectionInputKind::Containers && failure.rejected_rows == 1
    }));
}

#[test]
fn upstreams_require_current_roster_identity_but_draining_is_serveable() {
    let foreign_machine = "other-node";
    let mut draining = machine();
    draining.lifecycle = MachineLifecycle::Draining;
    let mut foreign_container = container(SERVICE, NAMESPACE, DEPLOY_GREEN, [10, 20, 0, 4]);
    foreign_container.machine_id = machine_id_for(foreign_machine);
    let mut input = input(
        vec![stored(SERVICE, &service(DEPLOY_GREEN))],
        vec![stored(ROUTE_LOW, &route(IngressMode::Direct, SERVICE))],
        vec![
            stored_container(&container(SERVICE, NAMESPACE, DEPLOY_GREEN, [10, 20, 0, 3])),
            stored_container(&foreign_container),
        ],
    );
    input.machines = vec![stored(MACHINE, &draining)];

    let projection = project_gateway_rows(input);
    let [route] = projection.projection.routes.as_slice() else {
        panic!("projection must contain exactly one route");
    };
    let [upstream] = route.upstreams.as_slice() else {
        panic!("only the draining current-roster machine must remain");
    };
    assert_eq!(
        upstream.container_key,
        "production/api/release-green/edge-a/global"
    );
}

#[test]
fn accepted_service_without_current_upstream_is_visibly_unavailable() {
    let projection = project_gateway_rows(input(
        vec![stored(SERVICE, &service(DEPLOY_GREEN))],
        vec![stored(ROUTE_LOW, &route(IngressMode::Direct, SERVICE))],
        Vec::new(),
    ));

    let [observation] = projection.route_observations.as_slice() else {
        panic!("route must have exactly one observation");
    };
    assert!(matches!(
        observation.outcome,
        GatewayRouteProjectionOutcome::Applied {
            availability: GatewayRouteAvailability::Unavailable {
                reason: GatewayRouteUnavailableReason::NoUpstream
            }
        }
    ));
}

fn input(
    services: Vec<StoredRow>,
    route_bindings: Vec<StoredRow>,
    containers: Vec<StoredRow>,
) -> GatewayProjectionInput {
    GatewayProjectionInput {
        cluster_id: cluster_id(),
        cluster: vec![stored(CLUSTER, &cluster())],
        machines: vec![stored(MACHINE, &machine())],
        services,
        route_bindings,
        containers,
    }
}

fn cluster() -> ClusterDocument {
    ClusterDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        provenance: provenance(),
        name: "test-cluster".to_owned(),
        storage_default: StorageMode::Plain,
        hostname_mode: AutomaticHostnameMode::Custom {
            suffix: RouteHostname::try_new("example.com").expect("hostname suffix"),
        },
        prefix: MachineEndpointSupernet::try_new("10.20.0.0/16").expect("supernet"),
        provider: MeshProvider::BuiltinWireguard,
        acme_directory_url: "https://acme.example/directory".to_owned(),
        acme_contact: None,
    }
}

fn machine() -> MachineDocument {
    MachineDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        provenance: provenance(),
        name: MachineName::try_new("edge-a").expect("machine name"),
        lifecycle: MachineLifecycle::Active,
        transport: MachineTransport::Wireguard {
            pubkey: WireGuardPublicKey::try_new("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=")
                .expect("public key"),
            addr_v6: Ipv6Addr::from_str("fd00::20").expect("IPv6"),
            endpoint: Some(SocketAddr::from(([192, 0, 2, 10], 51820))),
            subnet_v4: MachineEndpointSubnet::try_new("10.20.0.0/24").expect("subnet"),
        },
        storage: MachineStorageSelection {
            mode: StorageMode::Plain,
            reason: MachineStorageSelectionReason::Ineligible {
                reason: MachineStorageIneligibleReason::LowRam,
            },
        },
    }
}

fn stored<T: serde::Serialize>(key: &str, value: &T) -> StoredRow {
    StoredRow::new(
        key,
        serde_json::to_string(value).expect("serialize fixture"),
    )
}

fn service(active_deploy: &str) -> ServiceDocument {
    ServiceDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        provenance: provenance(),
        namespace_id: namespace_id(NAMESPACE),
        name: CorrosionServiceName::try_new("api").expect("service name"),
        image: ImageReference::try_new("ghcr.io/example/api:latest").expect("image"),
        env_fingerprints: BTreeMap::<String, Sha256Hex>::new(),
        placement: ServicePlacement::Replicated {
            replicas: ServiceReplicaCount::try_new(1).expect("replicas"),
        },
        pinned_machines: BTreeSet::new(),
        active_deploy: operation_id(active_deploy),
        previous_image: None,
        deployed_at: timestamp(),
    }
}

fn route(mode: IngressMode, service_key: &str) -> RouteBindingDocument {
    route_for_host(mode, service_key, "api.example.com")
}

fn route_for_host(mode: IngressMode, service_key: &str, hostname: &str) -> RouteBindingDocument {
    let service_name = service_key.rsplit('/').next().expect("service key");
    RouteBindingDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        provenance: provenance(),
        hostname: RouteHostname::try_new(hostname).expect("hostname"),
        namespace_id: namespace_id(NAMESPACE),
        service_name: CorrosionServiceName::try_new(service_name).expect("service"),
        endpoint_port: RoutePort::try_new(8080).expect("port"),
        origin: RouteBindingOrigin::Declared,
        ingress_mode: mode,
    }
}

fn container(
    _service_key: &str,
    namespace_id_value: &str,
    deploy: &str,
    ip: [u8; 4],
) -> ContainerDocument {
    ContainerDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        runtime_id: ployz_core::ids::ContainerId::try_new(format!("docker-{}", ip[3]))
            .expect("container id"),
        machine_id: machine_id(),
        namespace_id: namespace_id(namespace_id_value),
        service_name: CorrosionServiceName::try_new("api").expect("service"),
        replica_slot: ployz_core::deploy::ReplicaSlot::Global,
        ip: Ipv4Addr::from(ip),
        deploy: operation_id(deploy),
    }
}

fn stored_container(document: &ContainerDocument) -> StoredRow {
    let identity = V2ManagedContainerIdentity {
        namespace_id: document.namespace_id.clone(),
        service_name: document.service_name.clone(),
        operation_id: document.deploy.clone(),
        replica_slot: document.replica_slot,
    };
    stored(
        &managed_container_key(&identity, &document.machine_id),
        document,
    )
}

fn cluster_id() -> ClusterName {
    ClusterName::try_new(CLUSTER).expect("cluster")
}

fn namespace_id(value: &str) -> CorrosionNamespaceName {
    CorrosionNamespaceName::try_new(value).expect("namespace")
}

fn operation_id(value: &str) -> DeployName {
    DeployName::try_new(value).expect("operation")
}

fn machine_id() -> MachineName {
    machine_id_for(MACHINE)
}

fn machine_id_for(value: &str) -> MachineName {
    MachineName::try_new(value).expect("machine")
}

fn timestamp() -> CorrosionTimestamp {
    CorrosionTimestamp::try_new("2026-08-08T00:00:00Z").expect("timestamp")
}

fn provenance() -> OperatorWriteProvenance {
    OperatorWriteProvenance {
        written_by: OperationInitiator::Peer {
            peer_id: PeerName::try_new("operator").expect("peer"),
        },
        written_at: timestamp(),
    }
}
