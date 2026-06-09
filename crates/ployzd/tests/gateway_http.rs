use ployz_core::ids::{ContainerId, NodeId};
use ployz_core::node::ContainerEndpoint;
use ployz_core::ops::{RouteHostname, RouteHostnameError, RoutePort, RouteTarget};
use ployzd::gateway::{GatewayProjectedRoute, GatewayProjection, GatewayUpstream};
use ployzd::gateway_http::{
    GatewayHttpRouteError, HttpRouteTargetError, route_target_from_authority, select_http_upstream,
};
use ployzd::gateway_runtime::{GatewayRouteSelectionError, GatewayRouteTable};

#[test]
fn http_gateway_selects_upstream_from_host_authority() {
    let table = route_table([projected_route("api.example.com", 443)]);

    assert_eq!(
        select_http_upstream(&table, "API.example.com", route_port(443))
            .expect("host has upstream"),
        upstream()
    );
}

#[test]
fn http_gateway_uses_explicit_authority_port() {
    let table = route_table([projected_route("api.example.com", 8443)]);

    assert_eq!(
        select_http_upstream(&table, "api.example.com:8443", route_port(443))
            .expect("explicit authority port has upstream"),
        upstream()
    );
}

#[test]
fn http_gateway_reports_route_without_upstream() {
    let table = route_table([GatewayProjectedRoute {
        target: route_target("api.example.com", 443),
        upstreams: Vec::new(),
        unroutable_containers: vec![],
    }]);

    assert_eq!(
        select_http_upstream(&table, "api.example.com", route_port(443))
            .expect_err("host route has no upstream"),
        GatewayHttpRouteError::RouteSelection(GatewayRouteSelectionError::NoUpstream {
            target: route_target("api.example.com", 443),
        })
    );
}

#[test]
fn http_authority_defaults_to_listener_port() {
    assert_eq!(
        route_target_from_authority("API.example.com", route_port(443))
            .expect("authority is valid"),
        route_target("api.example.com", 443)
    );
}

#[test]
fn http_authority_accepts_explicit_port() {
    assert_eq!(
        route_target_from_authority("api.example.com:8443", route_port(443))
            .expect("authority is valid"),
        route_target("api.example.com", 8443)
    );
}

#[test]
fn http_authority_rejects_empty_host() {
    assert_eq!(
        route_target_from_authority("  ", route_port(443)).expect_err("empty host is rejected"),
        HttpRouteTargetError::EmptyHost
    );
}

#[test]
fn http_authority_rejects_user_info() {
    assert_eq!(
        route_target_from_authority("user@api.example.com", route_port(443))
            .expect_err("user info is rejected"),
        HttpRouteTargetError::UserInfo
    );
}

#[test]
fn http_authority_rejects_missing_host_before_port() {
    assert_eq!(
        route_target_from_authority(":443", route_port(443)).expect_err("missing host is rejected"),
        HttpRouteTargetError::MissingHost
    );
}

#[test]
fn http_authority_rejects_missing_port_after_colon() {
    assert_eq!(
        route_target_from_authority("api.example.com:", route_port(443))
            .expect_err("missing port is rejected"),
        HttpRouteTargetError::MissingPort
    );
}

#[test]
fn http_authority_rejects_invalid_port() {
    assert_eq!(
        route_target_from_authority("api.example.com:not-a-port", route_port(443))
            .expect_err("invalid port is rejected"),
        HttpRouteTargetError::InvalidPort
    );
    assert_eq!(
        route_target_from_authority("api.example.com:0", route_port(443))
            .expect_err("zero port is rejected"),
        HttpRouteTargetError::InvalidPort
    );
}

#[test]
fn http_authority_rejects_ipv6_for_now() {
    assert_eq!(
        route_target_from_authority("[::1]:443", route_port(443))
            .expect_err("ipv6 is not supported yet"),
        HttpRouteTargetError::UnsupportedIpv6
    );
}

#[test]
fn http_authority_rejects_invalid_hostname() {
    assert_eq!(
        route_target_from_authority("-api.example.com", route_port(443))
            .expect_err("invalid hostname is rejected"),
        HttpRouteTargetError::InvalidHostname {
            source: RouteHostnameError::Invalid {
                value: "-api.example.com".to_owned(),
            },
        }
    );
}

fn route_table(routes: impl IntoIterator<Item = GatewayProjectedRoute>) -> GatewayRouteTable {
    GatewayRouteTable::from_projection(GatewayProjection {
        routes: routes.into_iter().collect(),
    })
}

fn projected_route(hostname: &str, port: u16) -> GatewayProjectedRoute {
    GatewayProjectedRoute {
        target: route_target(hostname, port),
        upstreams: vec![upstream()],
        unroutable_containers: vec![],
    }
}

fn upstream() -> GatewayUpstream {
    GatewayUpstream {
        node_id: node_id("node_1"),
        container_id: container_id("ctr_1"),
        endpoint: ContainerEndpoint {
            ip: "10.0.0.1".parse().expect("valid endpoint ip"),
            port: route_port(8080),
        },
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
