use ployz_core::machine_runtime::{
    ContainerEndpoint, ContainerRuntimeState, MachineContainerObservationSnapshot,
    ManagedContainerKind, ManagedContainerObservation,
};
use ployz_core::ops::RouteTarget;
use ployz_test_support::ids::{
    container_id, machine_id, operation_id, revision_id, route_hostname, route_port, service_id,
    step_id,
};
use ployzd::gateway::{
    GatewayMachineObservation, GatewayObservationFreshness, GatewayProjectedRoute,
    GatewayProjection, GatewayProjectionError, GatewayProjectionInput, GatewayProjectionState,
    GatewayProjectionUpdate, GatewayRoute, GatewayUpstream,
};
use ployzd::gateway_runtime::{GatewayRouteSelectionError, GatewayRouteTable, GatewayRuntime};

#[test]
fn gateway_runtime_serves_new_projection_from_available_source() {
    let mut runtime = GatewayRuntime::new();
    let api = projected_route("api.example.com", "machine_1", "ctr_1");

    let tick = runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "machine_1",
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
    let api = projected_route("api.example.com", "machine_1", "ctr_1");
    runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "machine_1",
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
    let api = projected_route("api.example.com", "machine_1", "ctr_1");
    runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "machine_1",
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
    let api_v2 = projected_route("api.example.com", "machine_2", "ctr_2");
    runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "machine_1",
        "ctr_1",
    )));
    runtime.apply_source_update(GatewayProjectionUpdate::SourceUnavailable(
        source_unavailable(),
    ));

    let tick = runtime.apply_source_update(GatewayProjectionUpdate::SourceAvailable(source_input(
        "api.example.com",
        "machine_2",
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

#[test]
fn route_table_selects_first_projected_upstream_for_target() {
    let table = route_table([projected_route_with_upstreams(
        "api.example.com",
        [
            upstream("machine_1", "ctr_1"),
            upstream("machine_2", "ctr_2"),
        ],
    )]);

    assert_eq!(
        table
            .select_upstream(&route_target("api.example.com", 443))
            .expect("target has upstream"),
        upstream("machine_1", "ctr_1")
    );
}

#[test]
fn route_table_reports_unavailable_projection() {
    let table = GatewayRouteTable::empty();

    assert_eq!(
        table
            .select_upstream(&route_target("api.example.com", 443))
            .expect_err("route table is unavailable"),
        GatewayRouteSelectionError::RouteTableUnavailable
    );
}

#[test]
fn route_table_reports_missing_route() {
    let table = route_table([projected_route("api.example.com", "machine_1", "ctr_1")]);

    assert_eq!(
        table
            .select_upstream(&route_target("admin.example.com", 443))
            .expect_err("target has no route"),
        GatewayRouteSelectionError::NoRoute {
            target: route_target("admin.example.com", 443),
        }
    );
}

#[test]
fn route_table_reports_route_without_upstreams() {
    let table = route_table([GatewayProjectedRoute {
        target: route_target("api.example.com", 443),
        upstreams: Vec::new(),
        unroutable_containers: vec![],
    }]);

    assert_eq!(
        table
            .select_upstream(&route_target("api.example.com", 443))
            .expect_err("target has no upstream"),
        GatewayRouteSelectionError::NoUpstream {
            target: route_target("api.example.com", 443),
        }
    );
}

fn source_input(
    hostname: &str,
    machine_id_value: &str,
    container_id_value: &str,
) -> GatewayProjectionInput {
    GatewayProjectionInput {
        routes: vec![GatewayRoute {
            target: route_target(hostname, 443),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        }],
        observed_machines: vec![GatewayMachineObservation {
            freshness: GatewayObservationFreshness::Fresh,
            snapshot: MachineContainerObservationSnapshot::try_new(
                machine_id(machine_id_value),
                [managed_container(machine_id_value, container_id_value)],
            )
            .expect("matching machine snapshot"),
        }],
    }
}

fn managed_container(
    machine_id_value: &str,
    container_id_value: &str,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        machine_id: machine_id(machine_id_value),
        container_id: container_id(container_id_value),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_1"),
        operation_id: operation_id("op_123"),
        step_id: step_id("step_1"),
        kind: ManagedContainerKind::Service,
        state: ContainerRuntimeState::running_at(endpoint("10.0.0.1", 8080)),
    }
}

fn projected_route(
    hostname: &str,
    machine_id_value: &str,
    container_id_value: &str,
) -> GatewayProjectedRoute {
    GatewayProjectedRoute {
        target: route_target(hostname, 443),
        upstreams: vec![GatewayUpstream {
            machine_id: machine_id(machine_id_value),
            container_id: container_id(container_id_value),
            endpoint: endpoint("10.0.0.1", 8080),
        }],
        unroutable_containers: vec![],
    }
}

fn projected_route_with_upstreams(
    hostname: &str,
    upstreams: impl IntoIterator<Item = GatewayUpstream>,
) -> GatewayProjectedRoute {
    GatewayProjectedRoute {
        target: route_target(hostname, 443),
        upstreams: upstreams.into_iter().collect(),
        unroutable_containers: vec![],
    }
}

fn route_table(routes: impl IntoIterator<Item = GatewayProjectedRoute>) -> GatewayRouteTable {
    GatewayRouteTable::from_projection(GatewayProjection {
        routes: routes.into_iter().collect(),
    })
}

fn upstream(machine_id_value: &str, container_id_value: &str) -> GatewayUpstream {
    GatewayUpstream {
        machine_id: machine_id(machine_id_value),
        container_id: container_id(container_id_value),
        endpoint: endpoint("10.0.0.1", 8080),
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
}

fn endpoint(ip: &str, port: u16) -> ContainerEndpoint {
    ContainerEndpoint {
        ip: ip.parse().expect("valid endpoint ip"),
        port: route_port(port),
    }
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
