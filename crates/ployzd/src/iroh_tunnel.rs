//! Supervised iroh tunnel service preparation and runtime.

use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, EndpointId, RelayMode, RelayUrl, SecretKey};
use ployz_nats::connect::NatsClientEndpoint;
use ployz_transport::iroh_endpoint::IrohEndpoint;
use ployz_transport::nats_tunnel::NATS_TUNNEL_ALPN;
use ployz_transport::nats_tunnel::{CoreNatsTunnelConfig, EdgeNatsTunnelConfig, NatsTunnelConfig};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

use crate::config::TunnelProcessConfig;
use crate::role::TunnelSide;

const TUNNEL_DIAL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedTunnelService {
    pub service_name: String,
    pub tunnel: NatsTunnelConfig,
    pub restart_policy: TunnelRestartPolicy,
}

impl PreparedTunnelService {
    #[must_use]
    pub fn core(service_name: impl Into<String>, nats_socket: SocketAddr) -> Self {
        Self {
            service_name: service_name.into(),
            tunnel: NatsTunnelConfig::Core(CoreNatsTunnelConfig::new(nats_socket)),
            restart_policy: TunnelRestartPolicy::Always {
                initial_backoff_ms: 250,
                max_backoff_ms: 5_000,
            },
        }
    }

    #[must_use]
    pub fn edge(
        service_name: impl Into<String>,
        local_listen: SocketAddr,
        core_endpoint: IrohEndpoint,
    ) -> Self {
        Self {
            service_name: service_name.into(),
            tunnel: NatsTunnelConfig::Edge(EdgeNatsTunnelConfig::new(local_listen, core_endpoint)),
            restart_policy: TunnelRestartPolicy::Always {
                initial_backoff_ms: 250,
                max_backoff_ms: 5_000,
            },
        }
    }

    #[must_use]
    pub fn local_client_endpoint(&self) -> NatsClientEndpoint {
        NatsClientEndpoint::from_socket(self.tunnel.local_socket())
    }

    #[must_use]
    pub const fn side(&self) -> TunnelSide {
        match &self.tunnel {
            NatsTunnelConfig::Core(_) => TunnelSide::Core,
            NatsTunnelConfig::Edge(_) => TunnelSide::Edge,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelRestartPolicy {
    Always {
        initial_backoff_ms: u64,
        max_backoff_ms: u64,
    },
}

pub struct RunningTunnelRuntime {
    side: TunnelSide,
    endpoint: Endpoint,
    listen_addr: Option<SocketAddr>,
    shutdown: broadcast::Sender<()>,
    tasks: Vec<JoinHandle<()>>,
    task_exits: mpsc::UnboundedReceiver<TunnelRuntimeError>,
}

impl RunningTunnelRuntime {
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(());
        for task in self.tasks {
            let _ = task.await;
        }
        self.endpoint.close().await;
    }

    async fn wait_until_stopped(&mut self) -> TunnelRuntimeError {
        self.task_exits
            .recv()
            .await
            .unwrap_or(TunnelRuntimeError::TunnelTaskExited { side: self.side })
    }

    #[must_use]
    pub fn endpoint_public_key(&self) -> String {
        self.endpoint.id().to_z32()
    }

    #[must_use]
    pub fn endpoint_direct_addresses(&self) -> Vec<SocketAddr> {
        self.endpoint.bound_sockets()
    }

    #[must_use]
    pub const fn listen_addr(&self) -> Option<SocketAddr> {
        self.listen_addr
    }
}

pub async fn start_tunnel_runtime(
    config: &TunnelProcessConfig,
) -> Result<RunningTunnelRuntime, TunnelRuntimeError> {
    start_tunnel_runtime_with_iroh_bind_addr(config, SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)))
        .await
}

pub async fn start_tunnel_runtime_with_iroh_bind_addr(
    config: &TunnelProcessConfig,
    iroh_bind_addr: SocketAddr,
) -> Result<RunningTunnelRuntime, TunnelRuntimeError> {
    match &config.service.tunnel {
        NatsTunnelConfig::Core(core) => start_core_tunnel(core, iroh_bind_addr).await,
        NatsTunnelConfig::Edge(edge) => start_edge_tunnel(edge, iroh_bind_addr).await,
    }
}

pub async fn run_tunnel_until_shutdown(
    config: &TunnelProcessConfig,
) -> Result<(), TunnelRuntimeError> {
    let mut runtime = start_tunnel_runtime(config).await?;
    tokio::select! {
        shutdown = wait_for_shutdown_signal() => {
            shutdown.map_err(TunnelRuntimeError::ShutdownSignal)?;
        }
        error = runtime.wait_until_stopped() => {
            return Err(error);
        }
    }
    runtime.shutdown().await;
    Ok(())
}

async fn start_core_tunnel(
    config: &CoreNatsTunnelConfig,
    iroh_bind_addr: SocketAddr,
) -> Result<RunningTunnelRuntime, TunnelRuntimeError> {
    let endpoint =
        bind_iroh_endpoint(TunnelSide::Core, Some(NATS_TUNNEL_ALPN), iroh_bind_addr).await?;
    let (shutdown, _) = broadcast::channel(2);
    let (task_exits_tx, task_exits) = mpsc::unbounded_channel();
    let mut task_shutdown = shutdown.subscribe();
    let task_endpoint = endpoint.clone();
    let nats_socket = config.nats_socket;
    let task = tokio::spawn(async move {
        serve_core_tunnel(task_endpoint, nats_socket, &mut task_shutdown).await;
        let _ = task_exits_tx.send(TunnelRuntimeError::TunnelTaskExited {
            side: TunnelSide::Core,
        });
    });

    Ok(RunningTunnelRuntime {
        side: TunnelSide::Core,
        endpoint,
        listen_addr: None,
        shutdown,
        tasks: vec![task],
        task_exits,
    })
}

async fn start_edge_tunnel(
    config: &EdgeNatsTunnelConfig,
    iroh_bind_addr: SocketAddr,
) -> Result<RunningTunnelRuntime, TunnelRuntimeError> {
    let endpoint = bind_iroh_endpoint(TunnelSide::Edge, None, iroh_bind_addr).await?;
    let listener = TcpListener::bind(config.local_listen)
        .await
        .map_err(|source| TunnelRuntimeError::BindEdgeListener {
            addr: config.local_listen,
            source,
        })?;
    let listen_addr = listener
        .local_addr()
        .map_err(TunnelRuntimeError::ReadEdgeListenerAddr)?;
    let core_addr = endpoint_addr_from_config(&config.core_endpoint)?;
    let (shutdown, _) = broadcast::channel(2);
    let (task_exits_tx, task_exits) = mpsc::unbounded_channel();
    let mut task_shutdown = shutdown.subscribe();
    let task_endpoint = endpoint.clone();
    let task = tokio::spawn(async move {
        serve_edge_tunnel(listener, task_endpoint, core_addr, &mut task_shutdown).await;
        let _ = task_exits_tx.send(TunnelRuntimeError::TunnelTaskExited {
            side: TunnelSide::Edge,
        });
    });

    Ok(RunningTunnelRuntime {
        side: TunnelSide::Edge,
        endpoint,
        listen_addr: Some(listen_addr),
        shutdown,
        tasks: vec![task],
        task_exits,
    })
}

async fn bind_iroh_endpoint(
    side: TunnelSide,
    alpn: Option<&'static str>,
    bind_addr: SocketAddr,
) -> Result<Endpoint, TunnelRuntimeError> {
    let mut builder = Endpoint::builder(presets::N0)
        .secret_key(SecretKey::generate())
        .relay_mode(RelayMode::Disabled)
        .bind_addr(bind_addr)
        .map_err(|source| TunnelRuntimeError::BindIrohEndpoint {
            side,
            addr: bind_addr,
            source: source.to_string(),
        })?;
    if let Some(alpn) = alpn {
        builder = builder.alpns(vec![alpn.as_bytes().to_vec()]);
    }
    builder
        .bind()
        .await
        .map_err(|source| TunnelRuntimeError::BindIrohEndpoint {
            side,
            addr: bind_addr,
            source: source.to_string(),
        })
}

fn endpoint_addr_from_config(endpoint: &IrohEndpoint) -> Result<EndpointAddr, TunnelRuntimeError> {
    let mut address = EndpointAddr::new(EndpointId::from_z32(&endpoint.public_key).map_err(
        |source| TunnelRuntimeError::InvalidCoreEndpointPublicKey {
            value: endpoint.public_key.clone(),
            source: source.to_string(),
        },
    )?);
    for direct_address in &endpoint.direct_addresses {
        address = address.with_ip_addr(*direct_address);
    }
    if let Some(relay_url) = &endpoint.relay_url {
        address = address.with_relay_url(relay_url.parse::<RelayUrl>().map_err(|source| {
            TunnelRuntimeError::InvalidCoreEndpointRelayUrl {
                value: relay_url.clone(),
                source: source.to_string(),
            }
        })?);
    }
    Ok(address)
}

async fn serve_core_tunnel(
    endpoint: Endpoint,
    nats_socket: SocketAddr,
    shutdown: &mut broadcast::Receiver<()>,
) {
    let mut connections = JoinSet::new();
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else {
                    break;
                };
                let mut connection_shutdown = shutdown.resubscribe();
                connections.spawn(async move {
                    let Ok(connection) = incoming.await else {
                        return;
                    };
                    handle_core_connection(connection, nats_socket, &mut connection_shutdown).await;
                });
            }
            _ = shutdown.recv() => break,
            Some(_) = connections.join_next() => {}
        }
    }
    connections.abort_all();
    while connections.join_next().await.is_some() {}
}

async fn handle_core_connection(
    connection: iroh::endpoint::Connection,
    nats_socket: SocketAddr,
    shutdown: &mut broadcast::Receiver<()>,
) {
    let mut streams = JoinSet::new();
    loop {
        tokio::select! {
            accepted = connection.accept_bi() => {
                let Ok((send, recv)) = accepted else {
                    break;
                };
                let mut stream_shutdown = shutdown.resubscribe();
                streams.spawn(async move {
                    handle_core_stream(send, recv, nats_socket, &mut stream_shutdown).await;
                });
            }
            _ = shutdown.recv() => break,
            Some(_) = streams.join_next() => {}
        }
    }
    streams.abort_all();
    while streams.join_next().await.is_some() {}
}

async fn handle_core_stream(
    send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    nats_socket: SocketAddr,
    shutdown: &mut broadcast::Receiver<()>,
) {
    let mut marker = [0_u8; 1];
    tokio::select! {
        result = recv.read_exact(&mut marker) => {
            if result.is_err() || marker != [0] {
                return;
            }
        }
        _ = shutdown.recv() => return,
    }
    let nats = tokio::select! {
        result = timeout(TUNNEL_DIAL_TIMEOUT, TcpStream::connect(nats_socket)) => {
            let Ok(Ok(nats)) = result else {
                return;
            };
            nats
        }
        _ = shutdown.recv() => return,
    };
    tokio::select! {
        _ = pipe_tcp_and_iroh(nats, send, recv) => {}
        _ = shutdown.recv() => {}
    }
}

async fn serve_edge_tunnel(
    listener: TcpListener,
    endpoint: Endpoint,
    core_addr: EndpointAddr,
    shutdown: &mut broadcast::Receiver<()>,
) {
    let mut sockets = JoinSet::new();
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((socket, _)) = accepted else {
                    continue;
                };
                let task_endpoint = endpoint.clone();
                let task_core_addr = core_addr.clone();
                let mut socket_shutdown = shutdown.resubscribe();
                sockets.spawn(async move {
                    handle_edge_socket(socket, task_endpoint, task_core_addr, &mut socket_shutdown).await;
                });
            }
            _ = shutdown.recv() => break,
            Some(_) = sockets.join_next() => {}
        }
    }
    sockets.abort_all();
    while sockets.join_next().await.is_some() {}
}

async fn handle_edge_socket(
    socket: TcpStream,
    endpoint: Endpoint,
    core_addr: EndpointAddr,
    shutdown: &mut broadcast::Receiver<()>,
) {
    let connection = tokio::select! {
        result = timeout(
            TUNNEL_DIAL_TIMEOUT,
            endpoint.connect(core_addr, NATS_TUNNEL_ALPN.as_bytes()),
        ) => {
            let Ok(Ok(connection)) = result else {
                return;
            };
            connection
        }
        _ = shutdown.recv() => return,
    };
    let (mut send, recv) = tokio::select! {
        result = timeout(TUNNEL_DIAL_TIMEOUT, connection.open_bi()) => {
            let Ok(Ok(streams)) = result else {
                return;
            };
            streams
        }
        _ = shutdown.recv() => return,
    };
    tokio::select! {
        result = send.write_all(&[0]) => {
            if result.is_err() {
                return;
            }
        }
        _ = shutdown.recv() => return,
    }
    tokio::select! {
        _ = pipe_tcp_and_iroh(socket, send, recv) => {}
        _ = shutdown.recv() => {}
    }
}

async fn pipe_tcp_and_iroh(
    mut socket: TcpStream,
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
) -> Result<(), std::io::Error> {
    let (mut socket_read, mut socket_write) = socket.split();
    let to_iroh = async {
        tokio::io::copy(&mut socket_read, &mut send).await?;
        send.finish()
            .map_err(|source| std::io::Error::other(source.to_string()))
    };
    let from_iroh = async {
        tokio::io::copy(&mut recv, &mut socket_write).await?;
        socket_write.shutdown().await
    };
    tokio::try_join!(to_iroh, from_iroh)?;
    Ok(())
}

async fn wait_for_shutdown_signal() -> Result<(), std::io::Error> {
    tokio::signal::ctrl_c().await
}

#[derive(Debug)]
pub enum TunnelRuntimeError {
    BindIrohEndpoint {
        side: TunnelSide,
        addr: SocketAddr,
        source: String,
    },
    BindEdgeListener {
        addr: SocketAddr,
        source: std::io::Error,
    },
    ReadEdgeListenerAddr(std::io::Error),
    InvalidCoreEndpointPublicKey {
        value: String,
        source: String,
    },
    InvalidCoreEndpointRelayUrl {
        value: String,
        source: String,
    },
    ShutdownSignal(std::io::Error),
    TunnelTaskExited {
        side: TunnelSide,
    },
}

impl fmt::Display for TunnelRuntimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BindIrohEndpoint { side, addr, source } => write!(
                formatter,
                "failed to bind ployzd tunnel {} iroh endpoint on {addr}: {source}",
                side.as_str()
            ),
            Self::BindEdgeListener { addr, source } => {
                write!(
                    formatter,
                    "failed to bind edge tunnel listener on {addr}: {source}"
                )
            }
            Self::ReadEdgeListenerAddr(error) => {
                write!(
                    formatter,
                    "failed to read edge tunnel listener address: {error}"
                )
            }
            Self::InvalidCoreEndpointPublicKey { value, source } => write!(
                formatter,
                "core iroh public key {value:?} is invalid: {source}"
            ),
            Self::InvalidCoreEndpointRelayUrl { value, source } => write!(
                formatter,
                "core iroh relay URL {value:?} is invalid: {source}"
            ),
            Self::ShutdownSignal(error) => write!(formatter, "shutdown signal failed: {error}"),
            Self::TunnelTaskExited { side } => {
                write!(formatter, "ployzd tunnel {} runtime stopped", side.as_str())
            }
        }
    }
}

impl std::error::Error for TunnelRuntimeError {}
