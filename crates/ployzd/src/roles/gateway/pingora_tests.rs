use std::collections::BTreeSet;
use std::net::{Ipv4Addr, SocketAddr};

use ployz_core::ids::{MachineName, RouteHostname};
use ployz_core::ingress::RouteBindingOrigin;
use ployz_core::operation::RouteTarget;

use super::{
    HttpRouteTargetError, PingoraRouteRegistry, PingoraRouteSelectionError,
    route_target_from_authority, selection_error_status,
};
use crate::roles::gateway::projection::{
    GatewayProjectedRoute, GatewayProjection, GatewayUpstream,
};

#[test]
fn registry_atomically_replaces_the_complete_route_snapshot() {
    let registry = PingoraRouteRegistry::new();
    registry.replace_projection(&projection("api.example.com", 8080));
    assert_eq!(selected(&registry, "api.example.com").port(), 8080);

    registry.replace_projection(&projection("admin.example.com", 9090));
    assert_eq!(selected(&registry, "admin.example.com").port(), 9090);
    assert_eq!(
        registry
            .select_backend(&target("api.example.com"), &BTreeSet::new())
            .expect_err("old snapshot route is absent"),
        PingoraRouteSelectionError::NoRoute {
            target: target("api.example.com")
        }
    );
}

#[test]
fn known_route_without_upstreams_is_distinct_from_unknown_route() {
    let registry = PingoraRouteRegistry::new();
    registry.replace_projection(&GatewayProjection {
        routes: vec![GatewayProjectedRoute {
            id: route_id(),
            origin: RouteBindingOrigin::Declared,
            target: target("api.example.com"),
            upstreams: Vec::new(),
        }],
    });

    assert!(matches!(
        registry.select_backend(&target("api.example.com"), &BTreeSet::new()),
        Err(PingoraRouteSelectionError::NoUpstream { .. })
    ));
    assert!(matches!(
        registry.select_backend(&target("other.example.com"), &BTreeSet::new()),
        Err(PingoraRouteSelectionError::NoRoute { .. })
    ));
    assert_eq!(
        selection_error_status(&PingoraRouteSelectionError::NoUpstream {
            target: target("api.example.com"),
        }),
        503
    );
    assert_eq!(
        selection_error_status(&PingoraRouteSelectionError::NoRoute {
            target: target("other.example.com"),
        }),
        404
    );
}

#[test]
fn request_time_retry_can_select_each_untried_upstream() {
    let registry = PingoraRouteRegistry::new();
    let mut projection = projection("api.example.com", 8080);
    let [route] = projection.routes.as_mut_slice() else {
        panic!("fixture must contain exactly one route");
    };
    route.upstreams.push(upstream(8081, "second"));
    registry.replace_projection(&projection);

    let first = registry
        .select_backend(&target("api.example.com"), &BTreeSet::new())
        .expect("first upstream");
    let second = registry
        .select_backend(&target("api.example.com"), &BTreeSet::from([first]))
        .expect("second upstream");

    assert_ne!(first, second);
    assert_eq!(
        BTreeSet::from([first.port(), second.port()]),
        BTreeSet::from([8080, 8081])
    );
}

#[test]
fn authority_parsing_is_case_normalized_and_ignores_the_listener_port() {
    assert_eq!(
        route_target_from_authority("API.Example.Com:1234").expect("authority"),
        target("api.example.com")
    );
    assert_eq!(
        route_target_from_authority("").expect_err("empty"),
        HttpRouteTargetError::EmptyHost
    );
    assert_eq!(
        route_target_from_authority("user@example.com").expect_err("userinfo"),
        HttpRouteTargetError::UserInfo
    );
}

fn projection(hostname: &str, port: u16) -> GatewayProjection {
    GatewayProjection {
        routes: vec![GatewayProjectedRoute {
            id: route_id(),
            origin: RouteBindingOrigin::Declared,
            target: target(hostname),
            upstreams: vec![upstream(port, "first")],
        }],
    }
}

fn upstream(port: u16, key: &str) -> GatewayUpstream {
    GatewayUpstream {
        endpoint_key: key.to_owned(),
        machine_id: MachineName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("machine"),
        address: SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
    }
}

fn route_id() -> RouteHostname {
    RouteHostname::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("route")
}

fn target(hostname: &str) -> RouteTarget {
    RouteTarget::new(RouteHostname::try_new(hostname).expect("hostname"))
}

fn selected(registry: &PingoraRouteRegistry, hostname: &str) -> SocketAddr {
    registry
        .select_backend(&target(hostname), &BTreeSet::new())
        .expect("selected upstream")
}
