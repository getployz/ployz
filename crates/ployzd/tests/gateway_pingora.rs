use pingora::protocols::l4::socket::SocketAddr as PingoraSocketAddr;
use ployz_core::ops::{RouteHostnameError, RouteTarget};
use ployz_test_support::ids::{container_id, machine_id, route_hostname, route_port};
use ployzd::roles::gateway::pingora::{
    HttpRouteTargetError, PingoraRouteRegistry, PingoraRouteSelectionError,
    route_target_from_authority,
};
use ployzd::roles::gateway::projection::{
    GatewayProjectedRoute, GatewayProjection, GatewayUpstream,
};
use std::collections::BTreeSet;
use std::net::SocketAddr;
use tokio::net::TcpListener;

#[test]
fn pingora_registry_replaces_routes_from_projection() {
    let registry = PingoraRouteRegistry::new();

    registry
        .replace_projection(&projection([
            projected_route_to_endpoint("api.example.com", 80, "127.0.0.1", 8080),
            projected_route_to_endpoint("admin.example.com", 80, "127.0.0.1", 8081),
        ]))
        .expect("registry accepts projection");
    selected_addr(&registry, &route_target("api.example.com", 80));
    selected_addr(&registry, &route_target("admin.example.com", 80));

    registry
        .replace_projection(&projection([projected_route_to_endpoint(
            "api.example.com",
            80,
            "127.0.0.1",
            9090,
        )]))
        .expect("registry accepts replacement projection");

    selected_addr(&registry, &route_target("api.example.com", 80));
    assert_eq!(
        registry
            .select_backend(&route_target("admin.example.com", 80), &BTreeSet::new())
            .expect_err("removed route is not selectable"),
        PingoraRouteSelectionError::NoRoute {
            target: route_target("admin.example.com", 80)
        }
    );
}

#[test]
fn pingora_registry_load_balances_across_route_upstreams() {
    let registry = PingoraRouteRegistry::new();
    let target = route_target("api.example.com", 80);
    registry
        .replace_projection(&projection([GatewayProjectedRoute {
            target: target.clone(),
            upstreams: vec![
                upstream_to_endpoint("127.0.0.1", 8080),
                upstream_to_endpoint("127.0.0.1", 8081),
            ],
            unroutable_containers: vec![],
        }]))
        .expect("registry accepts projection");

    let first = selected_addr(&registry, &target);
    let second = selected_addr(&registry, &target);

    assert_ne!(first, second);
    assert_eq!(
        BTreeSet::from([first, second]),
        BTreeSet::from([socket_addr("127.0.0.1:8080"), socket_addr("127.0.0.1:8081")])
    );
}

#[tokio::test]
async fn pingora_registry_health_checks_hide_refused_upstreams() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind temporary port");
    let refused_addr = listener.local_addr().expect("temporary listener addr");
    drop(listener);

    let registry = PingoraRouteRegistry::new();
    let target = route_target("api.example.com", 80);
    registry
        .replace_projection(&projection([projected_route_to_endpoint(
            "api.example.com",
            80,
            "127.0.0.1",
            refused_addr.port(),
        )]))
        .expect("registry accepts projection");

    registry.run_health_checks().await;

    assert_eq!(
        registry
            .select_backend(&target, &BTreeSet::new())
            .expect_err("refused upstream becomes unavailable"),
        PingoraRouteSelectionError::NoHealthyUpstream { target }
    );
}

#[test]
fn http_authority_defaults_to_listener_port() {
    assert_eq!(
        route_target_from_authority("API.example.com", route_port(80)).expect("authority is valid"),
        route_target("api.example.com", 80)
    );
}

#[test]
fn http_authority_accepts_explicit_port() {
    assert_eq!(
        route_target_from_authority("api.example.com:8443", route_port(80))
            .expect("authority is valid"),
        route_target("api.example.com", 8443)
    );
}

#[test]
fn http_authority_rejects_empty_host() {
    assert_eq!(
        route_target_from_authority("  ", route_port(80)).expect_err("empty host is rejected"),
        HttpRouteTargetError::EmptyHost
    );
}

#[test]
fn http_authority_rejects_user_info() {
    assert_eq!(
        route_target_from_authority("user@api.example.com", route_port(80))
            .expect_err("user info is rejected"),
        HttpRouteTargetError::UserInfo
    );
}

#[test]
fn http_authority_rejects_missing_host_before_port() {
    assert_eq!(
        route_target_from_authority(":443", route_port(80)).expect_err("missing host is rejected"),
        HttpRouteTargetError::MissingHost
    );
}

#[test]
fn http_authority_rejects_missing_port_after_colon() {
    assert_eq!(
        route_target_from_authority("api.example.com:", route_port(80))
            .expect_err("missing port is rejected"),
        HttpRouteTargetError::MissingPort
    );
}

#[test]
fn http_authority_rejects_invalid_port() {
    assert_eq!(
        route_target_from_authority("api.example.com:not-a-port", route_port(80))
            .expect_err("invalid port is rejected"),
        HttpRouteTargetError::InvalidPort
    );
    assert_eq!(
        route_target_from_authority("api.example.com:0", route_port(80))
            .expect_err("zero port is rejected"),
        HttpRouteTargetError::InvalidPort
    );
}

#[test]
fn http_authority_rejects_ipv6_for_now() {
    assert_eq!(
        route_target_from_authority("[::1]:443", route_port(80))
            .expect_err("ipv6 is not supported yet"),
        HttpRouteTargetError::UnsupportedIpv6
    );
}

#[test]
fn http_authority_rejects_invalid_hostname() {
    assert_eq!(
        route_target_from_authority("-api.example.com", route_port(80))
            .expect_err("invalid hostname is rejected"),
        HttpRouteTargetError::InvalidHostname {
            source: RouteHostnameError::Invalid {
                value: "-api.example.com".to_owned(),
            },
        }
    );
}

fn selected_addr(registry: &PingoraRouteRegistry, target: &RouteTarget) -> SocketAddr {
    let backend = registry
        .select_backend(target, &BTreeSet::new())
        .expect("backend is selected");
    match backend.addr {
        PingoraSocketAddr::Inet(addr) => addr,
        PingoraSocketAddr::Unix(_) => panic!("gateway tests only use TCP backends"),
    }
}

fn projection(routes: impl IntoIterator<Item = GatewayProjectedRoute>) -> GatewayProjection {
    GatewayProjection {
        routes: routes.into_iter().collect(),
    }
}

fn projected_route_to_endpoint(
    hostname: &str,
    port: u16,
    endpoint_ip: &str,
    endpoint_port: u16,
) -> GatewayProjectedRoute {
    GatewayProjectedRoute {
        target: route_target(hostname, port),
        upstreams: vec![upstream_to_endpoint(endpoint_ip, endpoint_port)],
        unroutable_containers: vec![],
    }
}

fn upstream_to_endpoint(ip: &str, port: u16) -> GatewayUpstream {
    GatewayUpstream {
        machine_id: machine_id("machine_1"),
        container_id: container_id("ctr_1"),
        address: SocketAddr::new(ip.parse().expect("valid endpoint ip"), port),
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
}

fn socket_addr(value: &str) -> SocketAddr {
    value.parse().expect("valid socket address")
}
