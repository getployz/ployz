use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployzd::gateway::{
    GatewayNodeObservation, GatewayObservationFreshness, GatewayProjectedRoute, GatewayProjection,
    GatewayProjectionError, GatewayProjectionInput, GatewayProjectionState,
    GatewayProjectionUpdate, GatewayRoute, GatewayUpstream, apply_gateway_update, project_gateway,
};

#[test]
fn gateway_filters_stale_and_non_running_route_upstreams() {
    let projection = project_gateway(GatewayProjectionInput {
        routes: vec![
            gateway_route("WWW.example.com", "svc_web", "rev_1"),
            gateway_route("api.example.com", "svc_api", "rev_2"),
        ],
        observed_nodes: vec![
            fresh_node(
                "node_1",
                vec![
                    service_container("node_1", "api_good", "svc_api", "rev_2", running()),
                    service_container("node_1", "web_good", "svc_web", "rev_1", running()),
                    service_container("node_1", "api_exited", "svc_api", "rev_2", exited()),
                    service_container("node_1", "api_old", "svc_api", "rev_1", running()),
                    managed_container(
                        "node_1",
                        "api_job",
                        "svc_api",
                        "rev_2",
                        ManagedContainerKind::Job,
                        running(),
                    ),
                ],
            ),
            stale_node(
                "node_2",
                vec![service_container(
                    "node_2",
                    "api_stale",
                    "svc_api",
                    "rev_2",
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
                        node_id: node_id("node_1"),
                        container_id: container_id("api_good"),
                    }],
                },
                GatewayProjectedRoute {
                    target: route_target("www.example.com", 443),
                    upstreams: vec![GatewayUpstream {
                        node_id: node_id("node_1"),
                        container_id: container_id("web_good"),
                    }],
                },
            ],
        }
    );
}

#[test]
fn gateway_keeps_last_good_projection_when_source_is_unavailable() {
    let error = source_unavailable();
    let last_good = GatewayProjection {
        routes: vec![GatewayProjectedRoute {
            target: route_target("api.example.com", 443),
            upstreams: vec![GatewayUpstream {
                node_id: node_id("node_1"),
                container_id: container_id("api_good"),
            }],
        }],
    };

    assert_eq!(
        apply_gateway_update(
            GatewayProjectionState::Current(last_good),
            GatewayProjectionUpdate::SourceUnavailable(error.clone()),
        ),
        GatewayProjectionState::ProjectionFailedRetained {
            retained: GatewayProjection {
                routes: vec![GatewayProjectedRoute {
                    target: route_target("api.example.com", 443),
                    upstreams: vec![GatewayUpstream {
                        node_id: node_id("node_1"),
                        container_id: container_id("api_good"),
                    }],
                }],
            },
            error,
        }
    );
    assert_eq!(
        apply_gateway_update(
            GatewayProjectionState::Unavailable,
            GatewayProjectionUpdate::SourceUnavailable(source_unavailable()),
        ),
        GatewayProjectionState::ProjectionFailedUnavailable {
            error: source_unavailable(),
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
                    target: route_target("API.example.com", 443),
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_1"),
                },
                GatewayRoute {
                    target: target.clone(),
                    service_id: service_id("svc_api"),
                    revision_id: revision_id("rev_2"),
                },
            ],
            observed_nodes: vec![],
        }),
        Err(GatewayProjectionError::DuplicateRouteTarget { target })
    );
}

#[test]
fn gateway_retains_last_good_projection_when_source_is_invalid() {
    let target = route_target("api.example.com", 443);
    let last_good = GatewayProjection {
        routes: vec![GatewayProjectedRoute {
            target: target.clone(),
            upstreams: vec![GatewayUpstream {
                node_id: node_id("node_1"),
                container_id: container_id("api_good"),
            }],
        }],
    };
    let update = GatewayProjectionUpdate::SourceAvailable(GatewayProjectionInput {
        routes: vec![
            GatewayRoute {
                target: target.clone(),
                service_id: service_id("svc_api"),
                revision_id: revision_id("rev_1"),
            },
            GatewayRoute {
                target: target.clone(),
                service_id: service_id("svc_api"),
                revision_id: revision_id("rev_2"),
            },
        ],
        observed_nodes: vec![],
    });

    assert_eq!(
        apply_gateway_update(GatewayProjectionState::Current(last_good), update),
        GatewayProjectionState::ProjectionFailedRetained {
            retained: GatewayProjection {
                routes: vec![GatewayProjectedRoute {
                    target: target.clone(),
                    upstreams: vec![GatewayUpstream {
                        node_id: node_id("node_1"),
                        container_id: container_id("api_good"),
                    }],
                }],
            },
            error: GatewayProjectionError::DuplicateRouteTarget { target },
        }
    );
}

#[test]
fn gateway_retains_last_good_projection_when_source_decode_fails() {
    let last_good = GatewayProjection {
        routes: vec![GatewayProjectedRoute {
            target: route_target("api.example.com", 443),
            upstreams: vec![GatewayUpstream {
                node_id: node_id("node_1"),
                container_id: container_id("api_good"),
            }],
        }],
    };
    let error = GatewayProjectionError::InvalidSource {
        message: "route decode failed".to_owned(),
    };

    assert_eq!(
        apply_gateway_update(
            GatewayProjectionState::Current(last_good),
            GatewayProjectionUpdate::SourceInvalid(error.clone()),
        ),
        GatewayProjectionState::ProjectionFailedRetained {
            retained: GatewayProjection {
                routes: vec![GatewayProjectedRoute {
                    target: route_target("api.example.com", 443),
                    upstreams: vec![GatewayUpstream {
                        node_id: node_id("node_1"),
                        container_id: container_id("api_good"),
                    }],
                }],
            },
            error,
        }
    );
}

#[test]
fn gateway_keeps_failure_evidence_when_invalid_source_then_disappears() {
    let last_good = GatewayProjection {
        routes: vec![GatewayProjectedRoute {
            target: route_target("api.example.com", 443),
            upstreams: vec![GatewayUpstream {
                node_id: node_id("node_1"),
                container_id: container_id("api_good"),
            }],
        }],
    };
    let error = GatewayProjectionError::InvalidSource {
        message: "route decode failed".to_owned(),
    };

    let failed = apply_gateway_update(
        GatewayProjectionState::Current(last_good),
        GatewayProjectionUpdate::SourceInvalid(error.clone()),
    );

    assert_eq!(
        apply_gateway_update(
            failed,
            GatewayProjectionUpdate::SourceUnavailable(source_unavailable()),
        ),
        GatewayProjectionState::ProjectionFailedRetained {
            retained: GatewayProjection {
                routes: vec![GatewayProjectedRoute {
                    target: route_target("api.example.com", 443),
                    upstreams: vec![GatewayUpstream {
                        node_id: node_id("node_1"),
                        container_id: container_id("api_good"),
                    }],
                }],
            },
            error,
        }
    );
}

fn fresh_node(
    node_id_value: &str,
    containers: Vec<ManagedContainerObservation>,
) -> GatewayNodeObservation {
    observed_node(
        node_id_value,
        GatewayObservationFreshness::Fresh,
        containers,
    )
}

fn stale_node(
    node_id_value: &str,
    containers: Vec<ManagedContainerObservation>,
) -> GatewayNodeObservation {
    observed_node(
        node_id_value,
        GatewayObservationFreshness::Stale,
        containers,
    )
}

fn observed_node(
    node_id_value: &str,
    freshness: GatewayObservationFreshness,
    containers: Vec<ManagedContainerObservation>,
) -> GatewayNodeObservation {
    GatewayNodeObservation {
        freshness,
        snapshot: NodeContainerObservationSnapshot::try_new(node_id(node_id_value), containers)
            .expect("valid node snapshot"),
    }
}

fn gateway_route(hostname: &str, service_id_value: &str, revision_id_value: &str) -> GatewayRoute {
    GatewayRoute {
        target: route_target(hostname, 443),
        service_id: service_id(service_id_value),
        revision_id: revision_id(revision_id_value),
    }
}

fn service_container(
    node_id_value: &str,
    container_id_value: &str,
    service_id_value: &str,
    revision_id_value: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    managed_container(
        node_id_value,
        container_id_value,
        service_id_value,
        revision_id_value,
        ManagedContainerKind::Service,
        state,
    )
}

fn managed_container(
    node_id_value: &str,
    container_id_value: &str,
    service_id_value: &str,
    revision_id_value: &str,
    kind: ManagedContainerKind,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: node_id(node_id_value),
        container_id: container_id(container_id_value),
        service_id: service_id(service_id_value),
        revision_id: revision_id(revision_id_value),
        operation_id: operation_id("op_1"),
        step_id: step_id("step_1"),
        kind,
        state,
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

fn source_unavailable() -> GatewayProjectionError {
    GatewayProjectionError::SourceUnavailable {
        message: "kv read timed out".to_owned(),
    }
}

fn running() -> ContainerRuntimeState {
    ContainerRuntimeState::Running
}

fn exited() -> ContainerRuntimeState {
    ContainerRuntimeState::Exited
}
