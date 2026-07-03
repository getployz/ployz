use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerKind,
    ManagedContainerObservation,
};
use ployz_core::ops::RouteTarget;
use ployz_test_support::ids::{
    namespace_id,
    container_id, machine_id, namespace_revision_entry_id, operation_id, route_hostname,
    route_port, service_id, step_id,
};
use ployzd::gateway::{
    GatewayMachineObservation, GatewayObservationFreshness, GatewayProjectedRoute,
    GatewayProjection, GatewayProjectionError, GatewayProjectionInput, GatewayProjectionState,
    GatewayProjectionUpdate, GatewayRoute, GatewayServingEntry, GatewayUnroutableContainer,
    GatewayUpstream, apply_gateway_update, project_gateway,
};
use std::net::SocketAddr;

#[test]
fn gateway_filters_stale_and_non_running_route_upstreams() {
    let projection = project_gateway(GatewayProjectionInput {
        routes: vec![
            gateway_route("WWW.example.com", "svc_web"),
            gateway_route("api.example.com", "svc_api"),
        ],
        serving: vec![
            serving_entry("svc_web", "entry_1"),
            serving_entry("svc_api", "entry_2"),
        ],
        observed_machines: vec![
            fresh_machine(
                "machine_1",
                vec![
                    service_container("machine_1", "api_good", "svc_api", "entry_2", running()),
                    service_container("machine_1", "web_good", "svc_web", "entry_1", running()),
                    service_container("machine_1", "api_exited", "svc_api", "entry_2", exited()),
                    service_container("machine_1", "api_old", "svc_api", "entry_1", running()),
                    managed_container(
                        "machine_1",
                        "api_job",
                        "svc_api",
                        "entry_2",
                        ManagedContainerKind::Job,
                        running(),
                    ),
                ],
            ),
            stale_machine(
                "machine_2",
                vec![service_container(
                    "machine_2",
                    "api_stale",
                    "svc_api",
                    "entry_2",
                    running(),
                )],
            ),
        ],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            routes: vec![
                GatewayProjectedRoute {
                    target: route_target("api.example.com", 443),
                    upstreams: vec![GatewayUpstream {
                        machine_id: machine_id("machine_1"),
                        container_id: container_id("api_good"),
                        address: socket_addr("10.0.0.1", 8080),
                    }],
                    unroutable_containers: vec![],
                },
                GatewayProjectedRoute {
                    target: route_target("www.example.com", 443),
                    upstreams: vec![GatewayUpstream {
                        machine_id: machine_id("machine_1"),
                        container_id: container_id("web_good"),
                        address: socket_addr("10.0.0.1", 8080),
                    }],
                    unroutable_containers: vec![],
                },
            ],
        }
    );
}

#[test]
fn gateway_filters_running_containers_without_endpoint_evidence() {
    let projection = project_gateway(GatewayProjectionInput {
        routes: vec![gateway_route("api.example.com", "svc_api")],
        serving: vec![serving_entry("svc_api", "entry_2")],
        observed_machines: vec![fresh_machine(
            "machine_1",
            vec![service_container(
                "machine_1",
                "api_unroutable",
                "svc_api",
                "entry_2",
                ContainerRuntimeState::running_unroutable(),
            )],
        )],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            routes: vec![GatewayProjectedRoute {
                target: route_target("api.example.com", 443),
                upstreams: vec![],
                unroutable_containers: vec![GatewayUnroutableContainer {
                    machine_id: machine_id("machine_1"),
                    container_id: container_id("api_unroutable"),
                }],
            }],
        }
    );
}

#[test]
fn gateway_dials_matching_containers_on_the_route_endpoint_port() {
    // The container's own created port never participates in matching:
    // every serving-entry container is dialed on the route's endpoint port
    // (ADR 0023), so an endpoint reroute needs no container replacement.
    let projection = project_gateway(GatewayProjectionInput {
        routes: vec![gateway_route("api.example.com", "svc_api")],
        serving: vec![serving_entry("svc_api", "entry_2")],
        observed_machines: vec![fresh_machine(
            "machine_1",
            vec![
                service_container(
                    "machine_1",
                    "api_one",
                    "svc_api",
                    "entry_2",
                    ContainerRuntimeState::running_at(endpoint_ip("10.0.0.1")),
                ),
                service_container(
                    "machine_1",
                    "api_two",
                    "svc_api",
                    "entry_2",
                    ContainerRuntimeState::running_at(endpoint_ip("10.0.0.2")),
                ),
            ],
        )],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            routes: vec![GatewayProjectedRoute {
                target: route_target("api.example.com", 443),
                upstreams: vec![
                    GatewayUpstream {
                        machine_id: machine_id("machine_1"),
                        container_id: container_id("api_one"),
                        address: socket_addr("10.0.0.1", 8080),
                    },
                    GatewayUpstream {
                        machine_id: machine_id("machine_1"),
                        container_id: container_id("api_two"),
                        address: socket_addr("10.0.0.2", 8080),
                    },
                ],
                unroutable_containers: vec![],
            }],
        }
    );
}

#[test]
fn gateway_keeps_route_with_no_upstreams_when_service_is_not_serving() {
    // A binding whose service is absent from the serving target stays
    // attached and unavailable instead of becoming invalid state (ADR 0024).
    let projection = project_gateway(GatewayProjectionInput {
        routes: vec![gateway_route("api.example.com", "svc_api")],
        serving: vec![],
        observed_machines: vec![fresh_machine(
            "machine_1",
            vec![service_container(
                "machine_1",
                "api_orphan",
                "svc_api",
                "entry_2",
                running(),
            )],
        )],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            routes: vec![GatewayProjectedRoute {
                target: route_target("api.example.com", 443),
                upstreams: vec![],
                unroutable_containers: vec![],
            }],
        }
    );
}

#[test]
fn gateway_ignores_containers_with_a_different_entry_identity() {
    let projection = project_gateway(GatewayProjectionInput {
        routes: vec![gateway_route("api.example.com", "svc_api")],
        serving: vec![serving_entry("svc_api", "entry_2")],
        observed_machines: vec![fresh_machine(
            "machine_1",
            vec![service_container(
                "machine_1",
                "api_old",
                "svc_api",
                "entry_old",
                running(),
            )],
        )],
    })
    .expect("valid gateway projection");

    assert_eq!(
        projection,
        GatewayProjection {
            routes: vec![GatewayProjectedRoute {
                target: route_target("api.example.com", 443),
                upstreams: vec![],
                unroutable_containers: vec![],
            }],
        }
    );
}

#[test]
fn gateway_keeps_last_good_projection_when_source_is_unavailable() {
    let error = source_unavailable();
    let last_good = single_route_projection();

    assert_eq!(
        apply_gateway_update(
            current_state(last_good),
            GatewayProjectionUpdate::SourceUnavailable(error.clone()),
        ),
        failed_state(single_route_projection(), error)
    );
    assert_eq!(
        apply_gateway_update(
            GatewayProjectionState::unavailable(),
            GatewayProjectionUpdate::SourceUnavailable(source_unavailable()),
        ),
        GatewayProjectionState {
            last_good: None,
            last_error: Some(source_unavailable()),
        }
    );
}

#[test]
fn gateway_rejects_duplicate_route_targets() {
    let target = route_target("api.example.com", 443);
    assert_eq!(
        project_gateway(GatewayProjectionInput {
            routes: vec![
                GatewayRoute {
                    namespace_id: namespace_id("default"),
                    target: route_target("API.example.com", 443),
                    endpoint_port: route_port(8080),
                    service_id: service_id("svc_api"),
                },
                GatewayRoute {
                    namespace_id: namespace_id("default"),
                    target: target.clone(),
                    endpoint_port: route_port(8080),
                    service_id: service_id("svc_api"),
                },
            ],
            serving: vec![],
            observed_machines: vec![],
        }),
        Err(GatewayProjectionError::DuplicateRouteTarget { target })
    );
}

#[test]
fn gateway_retains_last_good_projection_when_source_is_invalid() {
    let target = route_target("api.example.com", 443);
    let last_good = single_route_projection();
    let update = GatewayProjectionUpdate::SourceAvailable(GatewayProjectionInput {
        routes: vec![
            GatewayRoute {
                namespace_id: namespace_id("default"),
                target: target.clone(),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
            },
            GatewayRoute {
                namespace_id: namespace_id("default"),
                target: target.clone(),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
            },
        ],
        serving: vec![],
        observed_machines: vec![],
    });

    assert_eq!(
        apply_gateway_update(current_state(last_good), update),
        failed_state(
            single_route_projection(),
            GatewayProjectionError::DuplicateRouteTarget { target },
        )
    );
}

#[test]
fn gateway_retains_last_good_projection_when_source_decode_fails() {
    let last_good = single_route_projection();
    let error = GatewayProjectionError::InvalidSource {
        message: "route decode failed".to_owned(),
    };

    assert_eq!(
        apply_gateway_update(
            current_state(last_good),
            GatewayProjectionUpdate::SourceInvalid(error.clone()),
        ),
        failed_state(single_route_projection(), error)
    );
}

#[test]
fn gateway_keeps_failure_evidence_when_invalid_source_then_disappears() {
    let last_good = single_route_projection();
    let error = GatewayProjectionError::InvalidSource {
        message: "route decode failed".to_owned(),
    };

    let failed = apply_gateway_update(
        current_state(last_good),
        GatewayProjectionUpdate::SourceInvalid(error.clone()),
    );

    assert_eq!(
        apply_gateway_update(
            failed,
            GatewayProjectionUpdate::SourceUnavailable(source_unavailable()),
        ),
        failed_state(single_route_projection(), error)
    );
}

fn single_route_projection() -> GatewayProjection {
    GatewayProjection {
        routes: vec![GatewayProjectedRoute {
            target: route_target("api.example.com", 443),
            upstreams: vec![GatewayUpstream {
                machine_id: machine_id("machine_1"),
                container_id: container_id("api_good"),
                address: socket_addr("10.0.0.1", 8080),
            }],
            unroutable_containers: vec![],
        }],
    }
}

fn fresh_machine(
    machine_id_value: &str,
    containers: Vec<ManagedContainerObservation>,
) -> GatewayMachineObservation {
    observed_machine(
        machine_id_value,
        GatewayObservationFreshness::Fresh,
        containers,
    )
}

fn stale_machine(
    machine_id_value: &str,
    containers: Vec<ManagedContainerObservation>,
) -> GatewayMachineObservation {
    observed_machine(
        machine_id_value,
        GatewayObservationFreshness::Stale,
        containers,
    )
}

fn observed_machine(
    machine_id_value: &str,
    freshness: GatewayObservationFreshness,
    containers: Vec<ManagedContainerObservation>,
) -> GatewayMachineObservation {
    GatewayMachineObservation {
        freshness,
        snapshot: MachineContainerObservationSnapshot::try_new(
            machine_id(machine_id_value),
            containers,
        )
        .expect("valid machine snapshot"),
    }
}

fn gateway_route(hostname: &str, service_id_value: &str) -> GatewayRoute {
    GatewayRoute {
        namespace_id: namespace_id("default"),
        target: route_target(hostname, 443),
        endpoint_port: route_port(8080),
        service_id: service_id(service_id_value),
    }
}

fn serving_entry(
    service_id_value: &str,
    namespace_revision_entry_id_value: &str,
) -> GatewayServingEntry {
    GatewayServingEntry {
        namespace_id: namespace_id("default"),
        service_id: service_id(service_id_value),
        namespace_revision_entry_id: namespace_revision_entry_id(
            namespace_revision_entry_id_value,
        ),
    }
}

fn service_container(
    machine_id_value: &str,
    container_id_value: &str,
    service_id_value: &str,
    namespace_revision_entry_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    managed_container(
        machine_id_value,
        container_id_value,
        service_id_value,
        namespace_revision_entry_id_value,
        ManagedContainerKind::Service,
        state,
    )
}

fn managed_container(
    machine_id_value: &str,
    container_id_value: &str,
    service_id_value: &str,
    namespace_revision_entry_id_value: &str,
    kind: ManagedContainerKind,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        machine_id: machine_id(machine_id_value),
        container_id: container_id(container_id_value),
        namespace_id: namespace_id("default"),
        service_id: service_id(service_id_value),
        namespace_revision_entry_id: namespace_revision_entry_id(namespace_revision_entry_id_value),
        operation_id: operation_id("op_1"),
        step_id: step_id("step_1"),
        kind,
        state,
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
}

fn endpoint_ip(ip: &str) -> std::net::IpAddr {
    ip.parse().expect("valid endpoint ip")
}

fn socket_addr(ip: &str, port: u16) -> SocketAddr {
    SocketAddr::new(endpoint_ip(ip), port)
}

fn source_unavailable() -> GatewayProjectionError {
    GatewayProjectionError::SourceUnavailable {
        message: "kv read timed out".to_owned(),
    }
}

fn current_state(projection: GatewayProjection) -> GatewayProjectionState {
    GatewayProjectionState {
        last_good: Some(projection),
        last_error: None,
    }
}

fn failed_state(
    projection: GatewayProjection,
    error: GatewayProjectionError,
) -> GatewayProjectionState {
    GatewayProjectionState {
        last_good: Some(projection),
        last_error: Some(error),
    }
}

fn running() -> ContainerRuntimeState {
    ContainerRuntimeState::running_at(endpoint_ip("10.0.0.1"))
}

fn exited() -> ContainerRuntimeState {
    ContainerRuntimeState::Exited
}
