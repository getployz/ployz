//! Port-forward tunnels: one TCP stream from an admitted Management Client to an
//! address inside the Cluster network, relayed over its own [`TUNNEL_ALPN`] connection.
//!
//! A tunnel is Machine RPC power, not more: only a slot's accepted key opens one, the
//! target must be an IPv4 address in this Machine's Cluster network (never loopback or
//! the internet), and revoking the key or stopping the daemon ends it at once.

use std::{net::SocketAddr, sync::Arc, time::Duration};

use iroh::endpoint::{Connection, VarInt};
use ployz_core::{TUNNEL_FAILED, TUNNEL_OPEN};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::TcpStream,
    sync::watch,
};
use tokio_util::sync::CancellationToken;

use super::{REVOKED, refusal, revoked};
use crate::machine::LocalMachineRecord;

/// Bounds the client's header and the Machine's dial, so a stalled open never lingers.
const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Serve one tunnel on an authenticated connection until either side ends it, the key
/// is revoked, or `shutdown`.
pub(super) async fn serve(
    connection: Connection,
    mut records: watch::Receiver<Arc<LocalMachineRecord>>,
    shutdown: CancellationToken,
) {
    let remote = *connection.remote_id().as_bytes();
    let record = records.borrow_and_update().clone();
    // A pending candidate may only negotiate; tunnels are operational.
    if !record.accepts_management_client(&remote) {
        let code = refusal(&record, &remote).unwrap_or(super::CLIENT_REFUSED);
        connection.close(code, b"management access refused");
        return;
    }
    let revoked = revoked(records, remote);
    tokio::pin!(revoked);
    let opened = tokio::select! {
        () = &mut revoked => return connection.close(REVOKED, b"revoked"),
        () = shutdown.cancelled() => return connection.close(VarInt::from_u32(0), b"shutdown"),
        opened = tokio::time::timeout(OPEN_TIMEOUT, open(&connection, &record)) => opened
            .unwrap_or_else(|_| Err("the tunnel did not open in time".into())),
    };
    let (mut tcp, mut stream) = match opened {
        Ok(opened) => opened,
        Err(reason) => {
            return connection.close(VarInt::from_u32(TUNNEL_FAILED), reason.as_bytes());
        }
    };
    tokio::select! {
        () = &mut revoked => connection.close(REVOKED, b"revoked"),
        () = shutdown.cancelled() => connection.close(VarInt::from_u32(0), b"shutdown"),
        copied = tokio::io::copy_bidirectional(&mut tcp, &mut stream) => {
            if let Err(error) = copied {
                tracing::debug!(%error, "tunnel ended");
            }
            // Wait for the peer to hold the final bytes before the close discards them.
            let _ = tokio::time::timeout(OPEN_TIMEOUT, stream.writer().stopped()).await;
            connection.close(VarInt::from_u32(0), b"tunnel closed");
        }
    }
}

type TunnelStream = tokio::io::Join<iroh::endpoint::RecvStream, iroh::endpoint::SendStream>;

/// Read the target, check it is in the Cluster network, dial it and acknowledge.
async fn open(
    connection: &Connection,
    record: &LocalMachineRecord,
) -> Result<(TcpStream, TunnelStream), String> {
    let (mut send, mut recv) = connection
        .accept_bi()
        .await
        .map_err(|error| format!("no tunnel stream: {error}"))?;
    let length = recv
        .read_u8()
        .await
        .map_err(|error| format!("tunnel header: {error}"))?;
    let mut target = vec![0; usize::from(length)];
    recv.read_exact(&mut target)
        .await
        .map_err(|error| format!("tunnel header: {error}"))?;
    let target = std::str::from_utf8(&target)
        .ok()
        .and_then(|target| target.parse::<SocketAddr>().ok())
        .ok_or("the tunnel target is not an ip:port address")?;
    let network = record
        .cluster_network()
        .ok_or("this Machine is not in a Cluster")?;
    if !matches!(target, SocketAddr::V4(address) if network.contains(address.ip())) {
        return Err(format!("{target} is outside the Cluster network {network}"));
    }
    let tcp = TcpStream::connect(target)
        .await
        .map_err(|error| format!("{target} refused the connection: {error}"))?;
    send.write_u8(TUNNEL_OPEN)
        .await
        .map_err(|error| format!("tunnel acknowledgement: {error}"))?;
    Ok((tcp, tokio::io::join(recv, send)))
}
