use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ployz_core::ids::NodeId;
use ployz_transport::iroh_endpoint::{IrohEndpoint, IrohPath};
use ployz_transport::nats_tunnel::{
    CoreNatsTunnelConfig, EdgeNatsTunnelConfig, NATS_TUNNEL_ALPN, NatsTunnelConfig,
    NatsTunnelStatus,
};

#[test]
fn edge_tunnel_presents_loopback_nats_endpoint() {
    let core = IrohEndpoint::new(node_id("core_1"), "core-public-key");
    let local_listen = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7422);
    let tunnel = NatsTunnelConfig::Edge(EdgeNatsTunnelConfig::new(local_listen, core));

    assert_eq!(
        tunnel.local_socket(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7422)
    );
}

#[test]
fn core_tunnel_presents_local_nats_endpoint() {
    let tunnel = NatsTunnelConfig::Core(CoreNatsTunnelConfig::new(SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        4222,
    )));

    assert_eq!(
        tunnel.local_socket(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4222)
    );
}

#[test]
fn tunnel_protocol_is_pinned_for_core_and_edge() {
    let core = CoreNatsTunnelConfig::new(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 4222));
    let edge = EdgeNatsTunnelConfig::new(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 7422),
        IrohEndpoint::new(node_id("core_1"), "core-public-key"),
    );

    assert_eq!(core.protocol(), NATS_TUNNEL_ALPN);
    assert_eq!(edge.protocol(), NATS_TUNNEL_ALPN);
}

#[test]
fn tunnel_status_marks_only_connected_paths_usable() {
    let direct = NatsTunnelStatus::Connected {
        path: IrohPath::Direct {
            remote_address: SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5555),
            latency_ms: 3,
        },
    };
    let relayed = NatsTunnelStatus::Connected {
        path: IrohPath::Relayed {
            relay_url: "https://relay.example".to_owned(),
            latency_ms: 80,
        },
    };
    let reconnecting = NatsTunnelStatus::Reconnecting {
        last_error: "connection reset".to_owned(),
    };
    let down = NatsTunnelStatus::Down {
        reason: "missing core endpoint".to_owned(),
    };

    assert!(direct.is_usable());
    assert!(relayed.is_usable());
    assert!(!reconnecting.is_usable());
    assert!(!down.is_usable());
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
