use crate::error::Result;
use crate::mesh::wireguard::MemoryWireGuard;
use crate::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
use crate::model::{MachineRecord, OverlayIp, PublicKey};
use async_trait::async_trait;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireguardBackendMode {
    Memory,
    Docker,
    Host,
}

#[async_trait]
pub trait WireguardBackend: Send + Sync {
    fn mode(&self) -> WireguardBackendMode;

    fn host_interface_name(&self) -> Option<&str> {
        None
    }

    async fn up(&self) -> Result<()>;
    async fn down(&self) -> Result<()>;
    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()>;

    async fn has_remote_handshake(&self) -> bool {
        true
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        None
    }

    async fn read_peers(&self) -> Result<Vec<DevicePeer>>;
    async fn set_peer_endpoint(&self, key: &PublicKey, endpoint: &str) -> Result<()>;
}

#[derive(Clone)]
pub struct WireguardDriver {
    backend: Arc<dyn WireguardBackend>,
    memory: Option<Arc<MemoryWireGuard>>,
}

impl WireguardDriver {
    #[must_use]
    pub fn memory() -> Self {
        Self::memory_with(Arc::new(MemoryWireGuard::new()))
    }

    #[must_use]
    pub fn memory_with(memory: Arc<MemoryWireGuard>) -> Self {
        Self {
            backend: Arc::new(MemoryWireguardBackend {
                memory: Arc::clone(&memory),
            }),
            memory: Some(memory),
        }
    }

    #[doc(hidden)]
    #[must_use]
    pub fn from_backend(backend: Arc<dyn WireguardBackend>) -> Self {
        Self {
            backend,
            memory: None,
        }
    }

    #[must_use]
    pub fn mode(&self) -> WireguardBackendMode {
        self.backend.mode()
    }

    #[must_use]
    pub fn runs_probe_listener(&self) -> bool {
        self.mode() != WireguardBackendMode::Memory
    }

    #[must_use]
    pub fn ebpf_attachment_ifname(&self, bridge_ifname: &str) -> String {
        match self.backend.host_interface_name() {
            Some(ifname) if self.mode() == WireguardBackendMode::Host => ifname.to_string(),
            _ => bridge_ifname.to_string(),
        }
    }

    #[must_use]
    pub fn memory_backend(&self) -> Option<Arc<MemoryWireGuard>> {
        self.memory.as_ref().map(Arc::clone)
    }
}

impl MeshNetwork for WireguardDriver {
    fn up(&self) -> impl std::future::Future<Output = Result<()>> + Send + '_ {
        async move { self.backend.up().await }
    }

    fn down(&self) -> impl std::future::Future<Output = Result<()>> + Send + '_ {
        async move { self.backend.down().await }
    }

    fn set_peers<'a>(
        &'a self,
        peers: &'a [MachineRecord],
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.set_peers(peers).await }
    }

    fn has_remote_handshake(&self) -> impl std::future::Future<Output = bool> + Send + '_ {
        async move { self.backend.has_remote_handshake().await }
    }

    fn bridge_ip(&self) -> impl std::future::Future<Output = Option<OverlayIp>> + Send + '_ {
        async move { self.backend.bridge_ip().await }
    }
}

impl WireGuardDevice for WireguardDriver {
    fn read_peers(&self) -> impl std::future::Future<Output = Result<Vec<DevicePeer>>> + Send + '_ {
        async move { self.backend.read_peers().await }
    }

    fn set_peer_endpoint<'a>(
        &'a self,
        key: &'a PublicKey,
        endpoint: &'a str,
    ) -> impl std::future::Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.set_peer_endpoint(key, endpoint).await }
    }
}

struct MemoryWireguardBackend {
    memory: Arc<MemoryWireGuard>,
}

#[async_trait]
impl WireguardBackend for MemoryWireguardBackend {
    fn mode(&self) -> WireguardBackendMode {
        WireguardBackendMode::Memory
    }

    async fn up(&self) -> Result<()> {
        self.memory.up().await
    }

    async fn down(&self) -> Result<()> {
        self.memory.down().await
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        self.memory.set_peers(peers).await
    }

    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        self.memory.read_peers().await
    }

    async fn set_peer_endpoint(&self, key: &PublicKey, endpoint: &str) -> Result<()> {
        self.memory.set_peer_endpoint(key, endpoint).await
    }
}
