use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ployz_core::ids::NodeId;
use ployz_nats::connect::NatsClientEndpoint;
use ployz_transport::iroh_endpoint::IrohEndpoint;
use ployzd::iroh_tunnel::{PreparedTunnelService, TunnelRestartPolicy};

#[test]
fn core_tunnel_service_forwards_to_local_nats() {
    let service = PreparedTunnelService::core(
        "ployz-nats-tunnel-core",
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4222),
    );

    assert_eq!(
        service.local_client_endpoint(),
        NatsClientEndpoint::loopback(4222)
    );
    assert_eq!(
        service.restart_policy,
        TunnelRestartPolicy::Always {
            initial_backoff_ms: 250,
            max_backoff_ms: 5_000,
        }
    );
}

#[test]
fn edge_tunnel_service_exposes_loopback_endpoint_for_async_nats() {
    let service = PreparedTunnelService::edge(
        "ployz-nats-tunnel-edge",
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7422),
        IrohEndpoint::new(node_id("core_1"), "core-public-key"),
    );

    assert_eq!(
        service.local_client_endpoint(),
        NatsClientEndpoint::tcp("127.0.0.1", 7422)
    );
    assert_eq!(
        service.restart_policy,
        TunnelRestartPolicy::Always {
            initial_backoff_ms: 250,
            max_backoff_ms: 5_000,
        }
    );
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
