use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use ployz_core::ids::NodeId;
use ployz_nats::connect::NatsClientEndpoint;
use ployz_transport::iroh_endpoint::IrohEndpoint;
use ployzd::config::TunnelProcessConfig;
use ployzd::iroh_tunnel::{
    PreparedTunnelService, TunnelRestartPolicy, start_tunnel_runtime_with_iroh_bind_addr,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;

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

#[tokio::test]
async fn edge_tunnel_forwards_server_first_nats_bytes_over_iroh() {
    let upstream = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .await
        .expect("upstream binds");
    let upstream_addr = upstream.local_addr().expect("upstream local addr reads");
    let (received_tx, received_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = upstream.accept().await.expect("upstream accepts");
        socket
            .write_all(b"INFO {}\r\n")
            .await
            .expect("upstream writes greeting");
        let mut request = [0_u8; 6];
        socket
            .read_exact(&mut request)
            .await
            .expect("upstream reads ping");
        received_tx
            .send(request)
            .expect("test receiver remains alive");
        socket
            .write_all(b"PONG\r\n")
            .await
            .expect("upstream writes pong");
    });

    let core_config = TunnelProcessConfig::new(PreparedTunnelService::core(
        "ployz-nats-tunnel-core",
        upstream_addr,
    ));
    let core = start_tunnel_runtime_with_iroh_bind_addr(
        &core_config,
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
    )
    .await
    .expect("core tunnel starts");
    let core_addr = core
        .endpoint_direct_addresses()
        .into_iter()
        .find(|address| address.ip().is_loopback())
        .expect("core exposes loopback iroh address");
    let edge_config = TunnelProcessConfig::new(PreparedTunnelService::edge(
        "ployz-nats-tunnel-edge",
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        IrohEndpoint::new(node_id("core_1"), core.endpoint_public_key())
            .with_direct_address(core_addr),
    ));
    let edge = start_tunnel_runtime_with_iroh_bind_addr(
        &edge_config,
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
    )
    .await
    .expect("edge tunnel starts");

    let mut client = TcpStream::connect(edge.listen_addr().expect("edge listens locally"))
        .await
        .expect("client connects through edge tunnel");
    let mut greeting = [0_u8; 9];
    client
        .read_exact(&mut greeting)
        .await
        .expect("client reads server-first NATS greeting");
    client
        .write_all(b"PING\r\n")
        .await
        .expect("client writes ping");
    let mut response = [0_u8; 6];
    client
        .read_exact(&mut response)
        .await
        .expect("client reads pong");

    assert_eq!(greeting, *b"INFO {}\r\n");
    assert_eq!(
        received_rx.await.expect("upstream captured request"),
        *b"PING\r\n"
    );
    assert_eq!(response, *b"PONG\r\n");

    edge.shutdown().await;
    core.shutdown().await;
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
