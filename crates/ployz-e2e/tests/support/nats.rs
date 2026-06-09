use std::error::Error;
use std::io;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use ployz_core::ids::NodeId;
use ployz_transport::iroh_endpoint::IrohEndpoint;
use ployzd::config::TunnelProcessConfig;
use ployzd::iroh_tunnel::{
    PreparedTunnelService, RunningTunnelRuntime, start_tunnel_runtime_with_iroh_bind_addr,
};

pub struct TestNats {
    _server: nats_server::Server,
    url: String,
    socket_addr: SocketAddr,
}

impl TestNats {
    pub async fn start_jetstream() -> Result<Self, Box<dyn Error + Send + Sync>> {
        let config = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ployz-nats/tests/configs/jetstream.conf"
        );
        let server = nats_server::run_server(config);
        let url = server.client_url();
        wait_for_nats(&url).await?;
        let socket_addr = nats_url_socket(&url)?;

        Ok(Self {
            _server: server,
            url,
            socket_addr,
        })
    }

    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    #[must_use]
    pub const fn socket_addr(&self) -> SocketAddr {
        self.socket_addr
    }
}

async fn wait_for_nats(url: &str) -> Result<(), Box<dyn Error + Send + Sync>> {
    let mut last_error = "no connection attempt made".to_owned();
    for _ in 0..50 {
        match async_nats::connect(url).await {
            Ok(client) => {
                drop(client);
                return Ok(());
            }
            Err(error) => {
                last_error = error.to_string();
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("NATS test server did not become ready at {url}: {last_error}"),
    )
    .into())
}

pub struct EdgeNatsTunnel {
    url: String,
    edge: RunningTunnelRuntime,
    core: RunningTunnelRuntime,
}

impl EdgeNatsTunnel {
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    pub async fn shutdown(self) {
        self.edge.shutdown().await;
        self.core.shutdown().await;
    }
}

pub async fn start_edge_nats_tunnel(
    core_node_id: NodeId,
    core_nats_socket: SocketAddr,
) -> Result<EdgeNatsTunnel, Box<dyn Error + Send + Sync>> {
    let core_config = TunnelProcessConfig::new(PreparedTunnelService::core(
        "e2e-nats-tunnel-core",
        core_nats_socket,
    ));
    let core = start_tunnel_runtime_with_iroh_bind_addr(
        &core_config,
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
    )
    .await?;
    let core_iroh_addr = core
        .endpoint_direct_addresses()
        .into_iter()
        .find(|addr| addr.ip().is_loopback())
        .expect("core tunnel exposes a loopback iroh address");
    let edge_config = TunnelProcessConfig::new(PreparedTunnelService::edge(
        "e2e-nats-tunnel-edge",
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
        IrohEndpoint::new(core_node_id, core.endpoint_public_key())
            .with_direct_address(core_iroh_addr),
    ));
    let edge = start_tunnel_runtime_with_iroh_bind_addr(
        &edge_config,
        SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
    )
    .await?;
    let listen_addr = edge.listen_addr().expect("edge tunnel listens locally");
    let url = format!("nats://{listen_addr}");

    Ok(EdgeNatsTunnel { url, edge, core })
}

fn nats_url_socket(value: &str) -> Result<SocketAddr, io::Error> {
    let authority = value
        .strip_prefix("nats://")
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "NATS URL must use nats://"))?
        .trim_end_matches('/');
    let authority = authority
        .strip_prefix("localhost:")
        .map_or_else(|| authority.to_owned(), |port| format!("127.0.0.1:{port}"));
    authority.parse().map_err(|source| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("NATS URL {value:?} has invalid socket authority: {source}"),
        )
    })
}
