pub mod orchestrator;
pub mod peer;
pub(crate) mod peer_state;
pub mod phase;
pub(crate) mod tasks;

use crate::error::Result;
use crate::model::{MachineRecord, OverlayIp, PublicKey};
use std::future::Future;
use tokio::time::Instant;

pub trait MeshNetwork: Send + Sync {
    fn up(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn down(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn set_peers<'a>(
        &'a self,
        peers: &'a [MachineRecord],
    ) -> impl Future<Output = Result<()>> + Send + 'a;
    /// Returns true if at least one remote mesh peer has completed a WG handshake.
    /// Must exclude local peers (bridge, sidecars) that handshake immediately.
    fn has_remote_handshake(&self) -> impl Future<Output = bool> + Send + '_ {
        async { true }
    }
    /// Returns the overlay IP of the bridge tunnel, if one is running.
    fn bridge_ip(&self) -> impl Future<Output = Option<OverlayIp>> + Send + '_ {
        async { None }
    }
}

#[derive(Debug, Clone)]
pub struct DevicePeer {
    pub public_key: PublicKey,
    pub endpoint: Option<String>,
    pub last_handshake: Option<Instant>,
}

pub trait WireGuardDevice: Send + Sync {
    fn read_peers(&self) -> impl Future<Output = Result<Vec<DevicePeer>>> + Send + '_;
    fn set_peer_endpoint<'a>(
        &'a self,
        key: &'a PublicKey,
        endpoint: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a;
}
