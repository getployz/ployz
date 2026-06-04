use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use ployz_nats::connect::NatsClientEndpoint;

#[test]
fn socket_endpoint_renders_ipv4_url() {
    let endpoint =
        NatsClientEndpoint::from_socket(SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 4222));

    assert_eq!(endpoint.url(), "nats://127.0.0.1:4222");
    assert_eq!(
        endpoint.socket_addr(),
        Some(SocketAddr::new(IpAddr::from([127, 0, 0, 1]), 4222))
    );
}

#[test]
fn socket_endpoint_renders_ipv6_url_with_brackets() {
    let endpoint =
        NatsClientEndpoint::from_socket(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 4222));

    assert_eq!(endpoint.url(), "nats://[::1]:4222");
    assert_eq!(
        endpoint.socket_addr(),
        Some(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 4222))
    );
}

#[test]
fn host_endpoint_renders_url_without_claiming_socket_identity() {
    let endpoint = NatsClientEndpoint::tcp("nats.internal", 4222);

    assert_eq!(endpoint.url(), "nats://nats.internal:4222");
    assert_eq!(endpoint.socket_addr(), None);
}
