use std::net::SocketAddr;

use tokio::net::TcpStream;

use crate::error::Result;
use crate::model::OverlayIp;

/// Abstracts how the daemon binds listeners and dials overlay addresses.
///
/// In host mode the overlay IP is directly reachable. In Docker mode the
/// WireGuard interface lives inside a container, so the daemon binds on
/// localhost and the bridge relays overlay traffic.
#[async_trait::async_trait]
pub trait OverlayCarrier: Send + Sync {
    /// Returns the address the daemon should bind for the given overlay
    /// ip + port, or `None` if no network listener is needed (memory mode).
    fn bind_addr(&self, overlay_ip: OverlayIp, port: u16) -> Option<SocketAddr>;

    /// Connect to a remote machine's overlay address.
    async fn connect(&self, target: SocketAddr) -> Result<TcpStream>;
}
