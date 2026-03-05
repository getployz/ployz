pub mod orchestrator;
pub mod peer;
pub(crate) mod peer_state;
pub mod phase;

use crate::error::Result;
use crate::model::{MachineRecord, PublicKey};
use std::future::Future;
use tokio::time::Instant;

pub trait MeshNetwork: Send + Sync {
    fn up(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn down(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn set_peers<'a>(
        &'a self,
        peers: &'a [MachineRecord],
    ) -> impl Future<Output = Result<()>> + Send + 'a;
}

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
