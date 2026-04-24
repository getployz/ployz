use std::fmt::{Display, Formatter};
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use ployz_api::{DaemonRequest, DaemonResponse};
use ployz_types::model::OverlayIp;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub(crate) const PEER_RPC_TIMEOUT: Duration = Duration::from_secs(3);
pub(super) const PEER_RPC_DESTRUCTIVE_READ_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Copy)]
struct OverlayRpcTimeouts {
    connect: Duration,
    write: Duration,
    shutdown: Duration,
    read: Duration,
}

const DEFAULT_OVERLAY_RPC_TIMEOUTS: OverlayRpcTimeouts = OverlayRpcTimeouts {
    connect: PEER_RPC_TIMEOUT,
    write: PEER_RPC_TIMEOUT,
    shutdown: PEER_RPC_TIMEOUT,
    read: PEER_RPC_TIMEOUT,
};

pub(super) enum OverlayRpcExpectOkError {
    Transport(String),
    Remote { code: String, message: String },
}

impl Display for OverlayRpcExpectOkError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "{error}"),
            Self::Remote { code, message } => {
                write!(f, "remote daemon error [{code}]: {message}")
            }
        }
    }
}

pub(crate) async fn overlay_rpc(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
) -> Result<DaemonResponse, String> {
    overlay_rpc_with_timeouts(
        overlay_ip,
        peer_rpc_port,
        request,
        DEFAULT_OVERLAY_RPC_TIMEOUTS,
    )
    .await
}

async fn overlay_rpc_with_timeouts(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
    timeouts: OverlayRpcTimeouts,
) -> Result<DaemonResponse, String> {
    let address = SocketAddr::new(IpAddr::V6(overlay_ip.0), peer_rpc_port);
    let stream = timeout(timeouts.connect, TcpStream::connect(address))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc connect {address} timed out after {:?}",
                timeouts.connect
            )
        })?
        .map_err(|error| format!("overlay rpc connect {address}: {error}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut line = serde_json::to_string(&request)
        .map_err(|error| format!("encode overlay rpc request: {error}"))?;
    line.push('\n');
    timeout(timeouts.write, writer.write_all(line.as_bytes()))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc write {address} timed out after {:?}",
                timeouts.write
            )
        })?
        .map_err(|error| format!("overlay rpc write {address}: {error}"))?;
    timeout(timeouts.shutdown, writer.shutdown())
        .await
        .map_err(|_| {
            format!(
                "overlay rpc shutdown {address} timed out after {:?}",
                timeouts.shutdown
            )
        })?
        .map_err(|error| format!("overlay rpc shutdown {address}: {error}"))?;

    let mut response_line = String::new();
    let mut reader = BufReader::new(reader);
    timeout(timeouts.read, reader.read_line(&mut response_line))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc read {address} timed out after {:?}",
                timeouts.read
            )
        })?
        .map_err(|error| format!("overlay rpc read {address}: {error}"))?;
    serde_json::from_str(&response_line)
        .map_err(|error| format!("decode overlay rpc response: {error}"))
}

pub(super) async fn overlay_rpc_expect_ok(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
) -> Result<(), String> {
    overlay_rpc_expect_ok_classified(overlay_ip, peer_rpc_port, request)
        .await
        .map_err(|error| error.to_string())
}

pub(super) async fn overlay_rpc_expect_ok_classified(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
) -> Result<(), OverlayRpcExpectOkError> {
    overlay_rpc_expect_ok_classified_with_read_timeout(
        overlay_ip,
        peer_rpc_port,
        request,
        PEER_RPC_TIMEOUT,
    )
    .await
}

pub(crate) async fn overlay_rpc_expect_ok_with_read_timeout(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
    read_timeout: Duration,
) -> Result<(), String> {
    overlay_rpc_expect_ok_classified_with_read_timeout(
        overlay_ip,
        peer_rpc_port,
        request,
        read_timeout,
    )
    .await
    .map_err(|error| error.to_string())
}

pub(super) async fn overlay_rpc_expect_ok_classified_with_read_timeout(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
    read_timeout: Duration,
) -> Result<(), OverlayRpcExpectOkError> {
    let response = overlay_rpc_with_timeouts(
        overlay_ip,
        peer_rpc_port,
        request,
        OverlayRpcTimeouts {
            read: read_timeout,
            ..DEFAULT_OVERLAY_RPC_TIMEOUTS
        },
    )
    .await
    .map_err(OverlayRpcExpectOkError::Transport)?;
    if response.ok {
        return Ok(());
    }
    Err(OverlayRpcExpectOkError::Remote {
        code: response.code,
        message: response.message,
    })
}
