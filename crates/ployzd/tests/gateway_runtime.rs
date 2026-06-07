use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployzd::gateway::{
    GatewayNodeObservation, GatewayObservationFreshness, GatewayProjectedRoute, GatewayProjection,
    GatewayProjectionError, GatewayProjectionInput, GatewayProjectionState,
    GatewayProjectionUpdate, GatewayRoute, GatewayUpstream,
};
use ployzd::gateway_runtime::GatewayRuntime;

#[test]
fn gateway_runtime_serves_new_projection_from_available_source() {
    let mut runtime = GatewayRuntime::new();
    let api = projected_route("api.example.com", "node_1", "ctr_1");

    let tick = runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "node_1",
        "ctr_1",
    )));

    assert_eq!(
        tick.state,
        GatewayProjectionState::Current(GatewayProjection {
            routes: vec![api.clone()],
        })
    );
    assert_eq!(runtime.route_table().routes(), &[api]);
}

#[test]
fn gateway_runtime_keeps_serving_last_good_routes_when_source_disappears() {
    let mut runtime = GatewayRuntime::new();
    let api = projected_route("api.example.com", "node_1", "ctr_1");
    runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "node_1",
        "ctr_1",
    )));

    let tick = runtime.apply_source_update(GatewayProjectionUpdate::SourceUnavailable(
        source_unavailable(),
    ));

    assert_eq!(
        tick.state,
        GatewayProjectionState::ProjectionFailedRetained {
            retained: GatewayProjection {
                routes: vec![api.clone()],
            },
            error: source_unavailable(),
        }
    );
    assert_eq!(runtime.route_table().routes(), &[api]);
}

#[test]
fn gateway_runtime_keeps_serving_last_good_routes_when_source_is_invalid() {
    let mut runtime = GatewayRuntime::new();
    let api = projected_route("api.example.com", "node_1", "ctr_1");
    runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "node_1",
        "ctr_1",
    )));

    let tick =
        runtime.apply_source_update(GatewayProjectionUpdate::SourceInvalid(invalid_source()));

    assert_eq!(
        tick.state,
        GatewayProjectionState::ProjectionFailedRetained {
            retained: GatewayProjection {
                routes: vec![api.clone()],
            },
            error: invalid_source(),
        }
    );
    assert_eq!(runtime.route_table().routes(), &[api]);
}

#[test]
fn gateway_runtime_applies_later_route_changes_after_outage() {
    let mut runtime = GatewayRuntime::new();
    let api_v2 = projected_route("api.example.com", "node_2", "ctr_2");
    runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "node_1",
        "ctr_1",
    )));
    runtime.apply_source_update(GatewayProjectionUpdate::SourceUnavailable(
        source_unavailable(),
    ));

    let tick = runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "node_2",
        "ctr_2",
    )));

    assert_eq!(
        tick.state,
        GatewayProjectionState::Current(GatewayProjection {
            routes: vec![api_v2.clone()],
        })
    );
    assert_eq!(runtime.route_table().routes(), &[api_v2]);
}

#[test]
fn gateway_runtime_has_no_served_routes_before_first_valid_source() {
    let mut runtime = GatewayRuntime::new();

    let tick = runtime.apply_source_update(GatewayProjectionUpdate::SourceUnavailable(
        source_unavailable(),
    ));

    assert_eq!(
        tick.state,
        GatewayProjectionState::ProjectionFailedUnavailable {
            error: source_unavailable(),
        }
    );
    assert!(runtime.route_table().routes().is_empty());
}

fn source_input(
    hostname: &str,
    node_id_value: &str,
    container_id_value: &str,
) -> GatewayProjectionInput {
    GatewayProjectionInput {
        routes: vec![GatewayRoute {
            target: route_target(hostname, 443),
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        }],
        observed_nodes: vec![GatewayNodeObservation {
            freshness: GatewayObservationFreshness::Fresh,
            snapshot: NodeContainerObservationSnapshot::try_new(
                node_id(node_id_value),
                [managed_container(node_id_value, container_id_value)],
            )
            .expect("matching node snapshot"),
        }],
    }
}

fn managed_container(node_id_value: &str, container_id_value: &str) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: node_id(node_id_value),
        container_id: container_id(container_id_value),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
        operation_id: operation_id("op_123"),
        step_id: step_id("step_1"),
        kind: ManagedContainerKind::Service,
        state: ContainerRuntimeState::Running,
    }
}

fn projected_route(
    hostname: &str,
    node_id_value: &str,
    container_id_value: &str,
) -> GatewayProjectedRoute {
    GatewayProjectedRoute {
        target: route_target(hostname, 443),
        upstreams: vec![GatewayUpstream {
            node_id: node_id(node_id_value),
            container_id: container_id(container_id_value),
        }],
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::try_new(route_hostname(hostname), route_port(port))
}

fn route_hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}

fn invalid_source() -> GatewayProjectionError {
    GatewayProjectionError::InvalidSource {
        message: "decode route state".to_owned(),
    }
}

fn source_unavailable() -> GatewayProjectionError {
    GatewayProjectionError::SourceUnavailable {
        message: "nats unavailable".to_owned(),
    }
}
