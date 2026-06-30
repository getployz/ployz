use ployz_core::machine_runtime::ContainerEndpoint;
use ployz_core::ops::{RouteHostnameError, RouteTarget};
use ployz_test_support::ids::{container_id, machine_id, route_hostname, route_port};
use ployzd::gateway::{GatewayProjectedRoute, GatewayProjection, GatewayUpstream};
use ployzd::gateway_http::{
    GatewayHttpProxyError, GatewayHttpRouteError, HttpRouteTargetError,
    proxy_connection_by_first_http_host, route_target_from_authority, select_http_upstream,
};
use ployzd::gateway_runtime::{GatewayRouteSelectionError, GatewayRouteTable};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[tokio::test]
async fn dumb_http_proxy_forwards_one_request_to_selected_upstream() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let upstream_addr = upstream_listener.local_addr().expect("upstream addr");
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream_listener.accept().await.expect("accept upstream");
        let mut request = Vec::new();
        stream
            .read_to_end(&mut request)
            .await
            .expect("read upstream request");
        assert_eq!(
            String::from_utf8(request).expect("request is utf8"),
            "GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n"
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke")
            .await
            .expect("write upstream response");
    });
    let table = route_table([projected_route_to_endpoint(
        "api.example.com",
        443,
        "127.0.0.1",
        upstream_addr.port(),
    )]);
    let (mut client, mut gateway) = tokio::io::duplex(1024);
    let gateway_task = tokio::spawn(async move {
        proxy_connection_by_first_http_host(&table, &mut gateway, route_port(443))
            .await
            .expect("proxy request succeeds");
    });

    client
        .write_all(b"GET /smoke HTTP/1.1\r\nHost: api.example.com\r\n\r\n")
        .await
        .expect("write client request");
    client.shutdown().await.expect("finish client request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read client response");

    assert_eq!(
        response,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    gateway_task.await.expect("gateway task completes");
    upstream_task.await.expect("upstream task completes");
}

#[tokio::test]
async fn dumb_http_proxy_finds_host_when_not_the_first_header() {
    // Regression: a colon-bearing header before `Host` must not hide it. The
    // HTTP/1.1 spec allows any header order, and many clients do not send Host
    // first.
    let upstream_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let upstream_addr = upstream_listener.local_addr().expect("upstream addr");
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream_listener.accept().await.expect("accept upstream");
        let mut request = Vec::new();
        stream
            .read_to_end(&mut request)
            .await
            .expect("read upstream request");
        assert_eq!(
            String::from_utf8(request).expect("request is utf8"),
            "GET /smoke HTTP/1.1\r\nAccept: */*\r\nHost: api.example.com\r\n\r\n"
        );
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke")
            .await
            .expect("write upstream response");
    });
    let table = route_table([projected_route_to_endpoint(
        "api.example.com",
        443,
        "127.0.0.1",
        upstream_addr.port(),
    )]);
    let (mut client, mut gateway) = tokio::io::duplex(1024);
    let gateway_task = tokio::spawn(async move {
        proxy_connection_by_first_http_host(&table, &mut gateway, route_port(443))
            .await
            .expect("proxy request succeeds");
    });

    client
        .write_all(b"GET /smoke HTTP/1.1\r\nAccept: */*\r\nHost: api.example.com\r\n\r\n")
        .await
        .expect("write client request");
    client.shutdown().await.expect("finish client request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read client response");

    assert_eq!(
        response,
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nsmoke"
    );
    gateway_task.await.expect("gateway task completes");
    upstream_task.await.expect("upstream task completes");
}

#[tokio::test]
async fn dumb_http_proxy_reports_missing_host_header() {
    let table = route_table([projected_route("api.example.com", 443)]);
    let (mut client, mut gateway) = tokio::io::duplex(1024);
    let gateway_task = tokio::spawn(async move {
        proxy_connection_by_first_http_host(&table, &mut gateway, route_port(443))
            .await
            .expect_err("missing host is reported")
    });

    client
        .write_all(b"GET /smoke HTTP/1.1\r\n\r\n")
        .await
        .expect("write client request");
    client.shutdown().await.expect("finish client request");

    assert_eq!(
        gateway_task.await.expect("gateway task completes"),
        GatewayHttpProxyError::MissingHost
    );
}

#[tokio::test]
async fn dumb_http_proxy_preserves_body_bytes_buffered_with_headers() {
    let upstream_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind upstream");
    let upstream_addr = upstream_listener.local_addr().expect("upstream addr");
    let upstream_task = tokio::spawn(async move {
        let (mut stream, _) = upstream_listener.accept().await.expect("accept upstream");
        let mut request = Vec::new();
        stream
            .read_to_end(&mut request)
            .await
            .expect("read upstream request");
        assert_eq!(
            String::from_utf8(request).expect("request is utf8"),
            "POST /smoke HTTP/1.1\r\nHost: api.example.com\r\nContent-Length: 5\r\n\r\nsmoke"
        );
        stream
            .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .expect("write upstream response");
    });
    let table = route_table([projected_route_to_endpoint(
        "api.example.com",
        443,
        "127.0.0.1",
        upstream_addr.port(),
    )]);
    let (mut client, mut gateway) = tokio::io::duplex(1024);
    let gateway_task = tokio::spawn(async move {
        proxy_connection_by_first_http_host(&table, &mut gateway, route_port(443))
            .await
            .expect("proxy request succeeds");
    });

    client
        .write_all(
            b"POST /smoke HTTP/1.1\r\nHost: api.example.com\r\nContent-Length: 5\r\n\r\nsmoke",
        )
        .await
        .expect("write client request");
    client.shutdown().await.expect("finish client request");
    let mut response = String::new();
    client
        .read_to_string(&mut response)
        .await
        .expect("read client response");

    assert_eq!(
        response,
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n"
    );
    gateway_task.await.expect("gateway task completes");
    upstream_task.await.expect("upstream task completes");
}

#[tokio::test]
async fn dumb_http_proxy_reports_incomplete_request_head() {
    let table = route_table([projected_route("api.example.com", 443)]);
    let (mut client, mut gateway) = tokio::io::duplex(1024);
    let gateway_task = tokio::spawn(async move {
        proxy_connection_by_first_http_host(&table, &mut gateway, route_port(443))
            .await
            .expect_err("incomplete head is reported")
    });

    client
        .write_all(b"GET /smoke HTTP/1.1\r\nHost: api.example.com")
        .await
        .expect("write incomplete client request");
    client.shutdown().await.expect("finish client request");

    assert_eq!(
        gateway_task.await.expect("gateway task completes"),
        GatewayHttpProxyError::ReadClient {
            source: std::io::ErrorKind::UnexpectedEof,
        }
    );
}

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
    projected_route_to_endpoint(hostname, port, "10.0.0.1", 8080)
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

fn upstream() -> GatewayUpstream {
    upstream_to_endpoint("10.0.0.1", 8080)
}

fn upstream_to_endpoint(ip: &str, port: u16) -> GatewayUpstream {
    GatewayUpstream {
        machine_id: machine_id("machine_1"),
        container_id: container_id("ctr_1"),
        endpoint: ContainerEndpoint {
            ip: ip.parse().expect("valid endpoint ip"),
            port: route_port(port),
        },
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget::new(route_hostname(hostname), route_port(port))
}
