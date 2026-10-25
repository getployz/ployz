//! Port-forward tunnels: one TCP stream from an admitted Management Client to an
//! address inside the Cluster network, relayed over its own [`TUNNEL_ALPN`] connection.
//!
//! A tunnel is Machine RPC power, not more: only a `Full` slot's accepted key, proven
//! on the tunnel's first stream, opens one, the
//! target must be an IPv4 address in this Machine's Cluster network (never loopback or
//! the internet), and revoking the key or stopping the daemon ends it at once.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use iroh::endpoint::{Connection, VarInt};
use ployz_core::{ManagementScope, TUNNEL_FAILED, TUNNEL_NEEDS_FULL, TUNNEL_OPEN};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    sync::watch,
};
use tokio_util::sync::CancellationToken;

use super::{REVOKED, refusal, revoked};
use ipnet::Ipv4Net;

use crate::machine::{LocalMachine, LocalMachineRecord};
use crate::machine_api::FULL_ACCESS_NEXT;

/// Bounds the client's header and the Machine's dial, so a stalled open never lingers.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Bounds the wait for a refused client to hold [`TUNNEL_NEEDS_FULL`].
const REFUSAL_DELIVERY: Duration = Duration::from_secs(1);

/// Serve one tunnel on an authenticated connection until either side ends it, the key
/// is revoked, or `shutdown`.
pub(super) async fn serve(
    connection: Connection,
    local: LocalMachine,
    mut records: watch::Receiver<Arc<LocalMachineRecord>>,
    shutdown: CancellationToken,
) {
    let Some((remote, (send, recv))) = (tokio::select! {
        () = shutdown.cancelled() => return connection.close(VarInt::from_u32(0), b"shutdown"),
        proven = super::authenticate(&connection) => proven,
    }) else {
        return connection.close(super::CLIENT_REFUSED, b"management access refused");
    };
    let record = records.borrow_and_update().clone();
    // A pending candidate may only negotiate; tunnels are operational.
    if !record.accepts_management_client(&remote) {
        let code = refusal(&record, &remote).unwrap_or(super::CLIENT_REFUSED);
        connection.close(code, b"management access refused");
        return;
    }
    if record.management_scope(&remote) < Some(ManagementScope::Full) {
        return refuse_below_full(connection, send).await;
    }
    let revoked = revoked(records, remote);
    tokio::pin!(revoked);
    let opened = tokio::select! {
        () = &mut revoked => return connection.close(REVOKED, b"revoked"),
        () = shutdown.cancelled() => return connection.close(VarInt::from_u32(0), b"shutdown"),
        opened = tokio::time::timeout(OPEN_TIMEOUT, open(send, recv, &record, &local)) => opened
            .unwrap_or(Err(TunnelError::Timeout)),
    };
    let (mut tcp, mut stream) = match opened {
        Ok(opened) => opened,
        Err(reason) => {
            let reason = reason.to_string();
            return connection.close(VarInt::from_u32(TUNNEL_FAILED), reason.as_bytes());
        }
    };
    tokio::select! {
        () = &mut revoked => connection.close(REVOKED, b"revoked"),
        () = shutdown.cancelled() => connection.close(VarInt::from_u32(0), b"shutdown"),
        copied = tokio::io::copy_bidirectional(&mut tcp, &mut stream) => {
            if let Err(error) = copied {
                tracing::debug!(error = %ployz_core::error_chain::inline(&error), "tunnel ended");
            }
            // Wait for the peer to hold the final bytes before the close discards them.
            let _ = tokio::time::timeout(OPEN_TIMEOUT, stream.writer().stopped()).await;
            connection.close(VarInt::from_u32(0), b"tunnel closed");
        }
    }
}

/// Answer [`TUNNEL_NEEDS_FULL`], then close with [`TUNNEL_FAILED`] once the client holds
/// the answer, inside the two seconds a v0.3.4 CLI waits for the close it prints.
async fn refuse_below_full(connection: Connection, mut send: iroh::endpoint::SendStream) {
    if send.write_u8(TUNNEL_NEEDS_FULL).await.is_ok() && send.finish().is_ok() {
        let _ = tokio::time::timeout(REFUSAL_DELIVERY, send.stopped()).await;
    }
    let reason = format!("tunnels need Full access: {FULL_ACCESS_NEXT}");
    connection.close(VarInt::from_u32(TUNNEL_FAILED), reason.as_bytes());
}

/// Only the founder records the Cluster network locally; a joined Machine reads it from
/// the replicated Cluster record. `None` until it has replicated.
async fn cluster_network(record: &LocalMachineRecord, local: &LocalMachine) -> Option<Ipv4Net> {
    if let Some(network) = record.cluster_network() {
        return Some(network);
    }
    if record.phase() != ployz_core::LocalMachinePhase::Participating {
        return None;
    }
    match local.replicated().ok()?.cluster_network().await {
        Ok(network) => network,
        Err(error) => {
            tracing::debug!(error = %ployz_core::error_chain::inline(&error), "tunnel could not read the Cluster network");
            None
        }
    }
}

type TunnelStream = tokio::io::Join<iroh::endpoint::RecvStream, iroh::endpoint::SendStream>;

/// Why a tunnel didn't open; the client reads it as the close reason.
#[derive(Debug, thiserror::Error)]
enum TunnelError {
    #[error("the tunnel did not open in time")]
    Timeout,
    #[error("tunnel header: {0}")]
    Header(std::io::Error),
    #[error("the tunnel target is not an ip:port address")]
    Target,
    #[error("this Machine is not in a Cluster")]
    NoCluster,
    #[error("{target} is outside the Cluster network {network}")]
    Outside { target: SocketAddr, network: String },
    #[error("{target} refused the connection: {error}")]
    Refused {
        target: SocketAddr,
        error: std::io::Error,
    },
    #[error("tunnel acknowledgement: {0}")]
    Acknowledgement(std::io::Error),
}

/// Read the target, check it is in the Cluster network, dial it and acknowledge.
async fn open(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    record: &LocalMachineRecord,
    local: &LocalMachine,
) -> Result<(TcpStream, TunnelStream), TunnelError> {
    let length = recv.read_u8().await.map_err(TunnelError::Header)?;
    let mut target = vec![0; usize::from(length)];
    recv.read_exact(&mut target)
        .await
        .map_err(|error| TunnelError::Header(std::io::Error::other(error)))?;
    let target = std::str::from_utf8(&target)
        .ok()
        .and_then(|target| target.parse::<SocketAddr>().ok())
        .ok_or(TunnelError::Target)?;
    let network = cluster_network(record, local)
        .await
        .ok_or(TunnelError::NoCluster)?;
    if !matches!(target, SocketAddr::V4(address) if network.contains(address.ip())) {
        return Err(TunnelError::Outside {
            target,
            network: network.to_string(),
        });
    }
    let tcp = TcpStream::connect(target)
        .await
        .map_err(|error| TunnelError::Refused { target, error })?;
    send.write_u8(TUNNEL_OPEN)
        .await
        .map_err(TunnelError::Acknowledgement)?;
    Ok((tcp, tokio::io::join(recv, send)))
}

#[cfg(test)]
#[path = "tunnel_tests.rs"]
mod tests;
