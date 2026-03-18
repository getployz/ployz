mod deploy;
mod identity;
mod network;

use async_trait::async_trait;

pub use deploy::{DeployFrame, DeploySession, DeploySessionFactory, StartCandidateRequest};
pub use identity::{Identity, IdentityError};
pub use network::{
    ContainerNetwork, ContainerNetworkBackend, DevicePeer, MemoryWireGuard, MeshDataplane,
    MeshNetwork, WireGuardDevice, WireguardBackend, WireguardBackendMode, WireguardDriver,
    container_ip, machine_ip,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestartableWorkload {
    pub container_name: String,
    pub was_running: bool,
}

#[async_trait]
pub trait RuntimeHandle: Send + Sync {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String>;

    async fn detach(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}

pub struct NoopRuntimeHandle;

#[async_trait]
impl RuntimeHandle for NoopRuntimeHandle {
    async fn shutdown(self: Box<Self>) -> std::result::Result<(), String> {
        Ok(())
    }
}
