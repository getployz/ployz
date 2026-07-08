use std::net::{IpAddr, Ipv6Addr, SocketAddr};

use ployz_nats::connect::{NatsClientEndpoint, NatsClientUrl, NatsClientUrlError};

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

#[test]
fn client_url_preserves_connection_string_policy_without_endpoint_parsing() {
    let url =
        NatsClientUrl::try_new("tls://user:pass@nats.internal:4222").expect("URL is one line");

    assert_eq!(url.as_str(), "tls://user:pass@nats.internal:4222");
}

#[test]
fn client_url_can_be_derived_from_local_endpoints() {
    let endpoint =
        NatsClientEndpoint::from_socket(SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 4222));

    assert_eq!(
        NatsClientUrl::from_endpoint(&endpoint).as_str(),
        "nats://[::1]:4222"
    );
    assert_eq!(
        NatsClientUrl::loopback(4222).as_str(),
        "nats://127.0.0.1:4222"
    );
}

#[test]
fn client_url_rejects_values_that_cannot_survive_env_files() {
    assert_eq!(NatsClientUrl::try_new(""), Err(NatsClientUrlError::Empty));
    assert!(matches!(
        NatsClientUrl::try_new("nats://127.0.0.1:4222\nnext"),
        Err(NatsClientUrlError::UnsupportedEnvironmentValue { .. })
    ));
    assert!(matches!(
        NatsClientUrl::try_new("nats://127.0.0.1:4222 with-space"),
        Err(NatsClientUrlError::UnsupportedEnvironmentValue { .. })
    ));
}
