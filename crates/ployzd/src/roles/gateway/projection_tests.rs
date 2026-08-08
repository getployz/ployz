use std::collections::{BTreeMap, BTreeSet};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::str::FromStr;

use ployz_core::corrosion::{
    AutomaticHostnameMode, ClusterDocument, ContainerDocument, CorrosionDocumentVersion,
    CorrosionServiceName, CorrosionTimestamp, GatewayProjectionInputKind, GatewayRouteAvailability,
    GatewayRouteProjectionFailure, GatewayRouteProjectionOutcome, GatewayRouteUnavailableReason,
    IngressMode, MachineDocument, MachineStorageIneligibleReason, MachineStorageSelection,
    MachineStorageSelectionReason, MachineTransport, MeshProvider, OperationInitiator,
    OperatorWriteProvenance, PloyzDnsTargetState, RouteBindingDocument, ServiceDocument,
    ServicePlacement, ServiceReplicaCount, Sha256Hex, StorageMode, StoredRow,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{
    ClusterId, MachineRowId, NamespaceRowId, OperationRowId, PeerId, ServiceRowId,
};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::machine::{MachineLifecycle, MachineName};
use ployz_core::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};
use ployz_core::operation::{RouteHostname, RoutePort};

use super::{GatewayProjectionInput, GatewayUpstream, project_gateway_rows};

const CLUSTER: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const NAMESPACE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAW";
const SERVICE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAX";
const DEPLOY_BLUE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAY";
const DEPLOY_GREEN: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAZ";
const MACHINE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB0";
const ROUTE_LOW: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB1";
const ROUTE_HIGH: &str = "01ARZ3NDEKTSV4RRFFQ69G5FB2";

#[test]
fn direct_route_joins_only_exact_active_deploy_containers() {
    let projection = project_gateway_rows(input(
        vec![stored(SERVICE, &service(DEPLOY_GREEN))],
        vec![stored(ROUTE_LOW, &route(IngressMode::Direct, SERVICE))],
        vec![
            stored(
                "blue",
                &container(SERVICE, NAMESPACE, DEPLOY_BLUE, [10, 20, 0, 2]),
            ),
            stored(
                "green",
                &container(SERVICE, NAMESPACE, DEPLOY_GREEN, [10, 20, 0, 3]),
            ),
            stored(
                "wrong-namespace",
                &container(
                    SERVICE,
                    "01ARZ3NDEKTSV4RRFFQ69G5FB3",
                    DEPLOY_GREEN,
                    [10, 20, 0, 4],
                ),
            ),
        ],
    ));

    let [route] = projection.projection.routes.as_slice() else {
        panic!("projection must contain exactly one route");
    };
    assert_eq!(
        route.upstreams,
        [GatewayUpstream {
            container_key: "green".to_owned(),
            machine_id: machine_id(),
            address: SocketAddr::from(([10, 20, 0, 3], 8080)),
        }]
    );
}

#[test]
fn active_deploy_flip_replaces_the_whole_route_upstream_set() {
    let routes = vec![stored(ROUTE_LOW, &route(IngressMode::Direct, SERVICE))];
    let containers = vec![
        stored(
            "blue",
            &container(SERVICE, NAMESPACE, DEPLOY_BLUE, [10, 20, 0, 2]),
        ),
        stored(
            "green",
            &container(SERVICE, NAMESPACE, DEPLOY_GREEN, [10, 20, 0, 3]),
        ),
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
    assert_eq!(blue_upstream.container_key, "blue");
    assert_eq!(green_upstream.container_key, "green");
}

#[test]
fn every_valid_binding_has_an_outcome_and_unsupported_routes_are_not_served() {
    let mut higher = route(IngressMode::Direct, SERVICE);
    higher.endpoint_port = RoutePort::try_new(9090).expect("port");
    let projection = project_gateway_rows(input(
        vec![stored(SERVICE, &service(DEPLOY_GREEN))],
        vec![
            stored(ROUTE_HIGH, &higher),
            stored(ROUTE_LOW, &route(IngressMode::Direct, SERVICE)),
            stored(
                "01ARZ3NDEKTSV4RRFFQ69G5FB4",
                &route_for_host(IngressMode::CloudflareTunnel, SERVICE, "tunnel.example.com"),
            ),
        ],
        vec![stored(
            "green",
            &container(SERVICE, NAMESPACE, DEPLOY_GREEN, [10, 20, 0, 3]),
        )],
    ));

    let [route] = projection.projection.routes.as_slice() else {
        panic!("projection must contain only the winning direct route");
    };
    let [upstream] = route.upstreams.as_slice() else {
        panic!("route must contain exactly one upstream");
    };
    assert_eq!(route.id.as_str(), ROUTE_LOW);
    assert_eq!(upstream.address.port(), 8080);
    assert_eq!(projection.route_observations.len(), 3);
    assert!(
        projection
            .route_observations
            .iter()
            .any(|observation| matches!(
                observation.outcome,
                GatewayRouteProjectionOutcome::Failed {
                    failure: GatewayRouteProjectionFailure::Shadowed { .. }
                }
            ))
    );
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
            &route(IngressMode::Direct, "01ARZ3NDEKTSV4RRFFQ69G5FB5"),
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
    foreign.cluster_id = ClusterId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FB6").expect("cluster");
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
    let foreign_machine = "01ARZ3NDEKTSV4RRFFQ69G5FB3";
    let mut draining = machine();
    draining.lifecycle = MachineLifecycle::Draining;
    let mut foreign_container = container(SERVICE, NAMESPACE, DEPLOY_GREEN, [10, 20, 0, 4]);
    foreign_container.machine_id = machine_id_for(foreign_machine);
    let mut input = input(
        vec![stored(SERVICE, &service(DEPLOY_GREEN))],
        vec![stored(ROUTE_LOW, &route(IngressMode::Direct, SERVICE))],
        vec![
            stored(
                "draining",
                &container(SERVICE, NAMESPACE, DEPLOY_GREEN, [10, 20, 0, 3]),
            ),
            stored("not-in-roster", &foreign_container),
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
    assert_eq!(upstream.container_key, "draining");
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
        ployz_dns_target: PloyzDnsTargetState::Disabled,
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
        operation_id: operation_id(active_deploy),
    }
}

fn route(mode: IngressMode, service_id: &str) -> RouteBindingDocument {
    route_for_host(mode, service_id, "api.example.com")
}

fn route_for_host(
    mode: IngressMode,
    service_id_value: &str,
    hostname: &str,
) -> RouteBindingDocument {
    RouteBindingDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        provenance: provenance(),
        hostname: RouteHostname::try_new(hostname).expect("hostname"),
        service_id: service_id(service_id_value),
        namespace_id: namespace_id(NAMESPACE),
        endpoint_port: RoutePort::try_new(8080).expect("port"),
        origin: RouteBindingOrigin::Declared,
        ingress_mode: mode,
    }
}

fn container(
    service_id_value: &str,
    namespace_id_value: &str,
    deploy: &str,
    ip: [u8; 4],
) -> ContainerDocument {
    ContainerDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: cluster_id(),
        machine_id: machine_id(),
        service_id: service_id(service_id_value),
        namespace_id: namespace_id(namespace_id_value),
        ip: Ipv4Addr::from(ip),
        deploy: operation_id(deploy),
    }
}

fn cluster_id() -> ClusterId {
    ClusterId::try_new(CLUSTER).expect("cluster")
}

fn namespace_id(value: &str) -> NamespaceRowId {
    NamespaceRowId::try_new(value).expect("namespace")
}

fn service_id(value: &str) -> ServiceRowId {
    ServiceRowId::try_new(value).expect("service")
}

fn operation_id(value: &str) -> OperationRowId {
    OperationRowId::try_new(value).expect("operation")
}

fn machine_id() -> MachineRowId {
    machine_id_for(MACHINE)
}

fn machine_id_for(value: &str) -> MachineRowId {
    MachineRowId::try_new(value).expect("machine")
}

fn timestamp() -> CorrosionTimestamp {
    CorrosionTimestamp::try_new("2026-08-08T00:00:00Z").expect("timestamp")
}

fn provenance() -> OperatorWriteProvenance {
    OperatorWriteProvenance {
        written_by: OperationInitiator::Peer {
            peer_id: PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FB7").expect("peer"),
        },
        written_at: timestamp(),
    }
}
