use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use ployz_api::{DaemonRequest, DaemonResponse};
use ployz_types::model::OverlayIp;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

pub(super) const PEER_RPC_TIMEOUT: Duration = Duration::from_secs(3);

pub(super) async fn overlay_rpc(
    overlay_ip: OverlayIp,
    peer_rpc_port: u16,
    request: DaemonRequest,
) -> Result<DaemonResponse, String> {
    let address = SocketAddr::new(IpAddr::V6(overlay_ip.0), peer_rpc_port);
    let stream = timeout(PEER_RPC_TIMEOUT, TcpStream::connect(address))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc connect {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc connect {address}: {error}"))?;
    let (reader, mut writer) = stream.into_split();
    let mut line = serde_json::to_string(&request)
        .map_err(|error| format!("encode overlay rpc request: {error}"))?;
    line.push('\n');
    timeout(PEER_RPC_TIMEOUT, writer.write_all(line.as_bytes()))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc write {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc write {address}: {error}"))?;
    timeout(PEER_RPC_TIMEOUT, writer.shutdown())
        .await
        .map_err(|_| {
            format!(
                "overlay rpc shutdown {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
            )
        })?
        .map_err(|error| format!("overlay rpc shutdown {address}: {error}"))?;

    let mut response_line = String::new();
    let mut reader = BufReader::new(reader);
    timeout(PEER_RPC_TIMEOUT, reader.read_line(&mut response_line))
        .await
        .map_err(|_| {
            format!(
                "overlay rpc read {address} timed out after {:?}",
                PEER_RPC_TIMEOUT
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
    let response = overlay_rpc(overlay_ip, peer_rpc_port, request).await?;
    if response.ok {
        return Ok(());
    }
    Err(format!(
        "remote daemon error [{}]: {}",
        response.code, response.message
    ))
}
