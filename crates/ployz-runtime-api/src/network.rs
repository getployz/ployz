use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use ipnet::Ipv4Net;
use ployz_types::Result;
use ployz_types::error::Error;
use ployz_types::model::{MachineRecord, OverlayIp, PublicKey};
use tokio::time::Instant;

pub trait MeshNetwork: Send + Sync {
    fn up(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn down(&self) -> impl Future<Output = Result<()>> + Send + '_;
    fn set_peers<'a>(
        &'a self,
        peers: &'a [MachineRecord],
    ) -> impl Future<Output = Result<()>> + Send + 'a;

    fn has_remote_handshake(&self) -> impl Future<Output = bool> + Send + '_ {
        async { true }
    }

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

#[async_trait]
pub trait MeshDataplane: Send + Sync {
    async fn set_observe(&self, mode: ObserveMode) -> Result<()>;
    async fn upsert_route(&self, subnet: Ipv4Net, ifindex: u32) -> Result<()>;
    async fn remove_route(&self, subnet: Ipv4Net) -> Result<()>;
    async fn detach(&self) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireguardBackendMode {
    Memory,
    Docker,
    Host,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveMode {
    Disabled,
    Enabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectMode {
    Graceful,
    Force,
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
            Some(_) | None => bridge_ifname.to_string(),
        }
    }

    #[must_use]
    pub fn memory_backend(&self) -> Option<Arc<MemoryWireGuard>> {
        self.memory.as_ref().map(Arc::clone)
    }
}

impl MeshNetwork for WireguardDriver {
    fn up(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        async move { self.backend.up().await }
    }

    fn down(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        async move { self.backend.down().await }
    }

    fn set_peers<'a>(
        &'a self,
        peers: &'a [MachineRecord],
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        async move { self.backend.set_peers(peers).await }
    }

    fn has_remote_handshake(&self) -> impl Future<Output = bool> + Send + '_ {
        async move { self.backend.has_remote_handshake().await }
    }

    fn bridge_ip(&self) -> impl Future<Output = Option<OverlayIp>> + Send + '_ {
        async move { self.backend.bridge_ip().await }
    }
}

impl WireGuardDevice for WireguardDriver {
    fn read_peers(&self) -> impl Future<Output = Result<Vec<DevicePeer>>> + Send + '_ {
        async move { self.backend.read_peers().await }
    }

    fn set_peer_endpoint<'a>(
        &'a self,
        key: &'a PublicKey,
        endpoint: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
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

#[async_trait]
pub trait ContainerNetworkBackend: Send + Sync {
    async fn ensure(&self) -> Result<()>;
    async fn connect(&self, container: &str, ipv4: Option<std::net::Ipv4Addr>) -> Result<()>;
    async fn disconnect(&self, container: &str, mode: DisconnectMode) -> Result<()>;
    async fn remove(&self) -> Result<()>;
    async fn resolve_bridge_ifname(&self) -> Result<String>;
    fn container_v4(&self) -> std::net::Ipv4Addr;
}

#[derive(Clone)]
pub struct ContainerNetwork {
    backend: Arc<dyn ContainerNetworkBackend>,
}

impl ContainerNetwork {
    #[doc(hidden)]
    #[must_use]
    pub fn from_backend(backend: Arc<dyn ContainerNetworkBackend>) -> Self {
        Self { backend }
    }

    pub async fn ensure(&self) -> Result<()> {
        self.backend.ensure().await
    }

    pub async fn connect(&self, container: &str, ipv4: Option<std::net::Ipv4Addr>) -> Result<()> {
        self.backend.connect(container, ipv4).await
    }

    pub async fn disconnect(&self, container: &str, mode: DisconnectMode) -> Result<()> {
        self.backend.disconnect(container, mode).await
    }

    pub async fn remove(&self) -> Result<()> {
        self.backend.remove().await
    }

    pub async fn resolve_bridge_ifname(&self) -> Result<String> {
        self.backend.resolve_bridge_ifname().await
    }

    #[must_use]
    pub fn container_v4(&self) -> std::net::Ipv4Addr {
        self.backend.container_v4()
    }
}

pub struct MemoryWireGuard {
    inner: Mutex<WgInner>,
}

struct WgInner {
    is_up: bool,
    peers: Vec<MachineRecord>,
    device_peers: Vec<DevicePeer>,
    set_peers_count: usize,
    fail_up: bool,
    fail_down: bool,
}

impl Default for MemoryWireGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryWireGuard {
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(WgInner {
                is_up: false,
                peers: Vec::new(),
                device_peers: Vec::new(),
                set_peers_count: 0,
                fail_up: false,
                fail_down: false,
            }),
        }
    }

    fn lock_inner(&self) -> MutexGuard<'_, WgInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_fail_up(&self, mode: ObserveMode) {
        self.lock_inner().fail_up = matches!(mode, ObserveMode::Enabled);
    }

    pub fn set_fail_down(&self, mode: ObserveMode) {
        self.lock_inner().fail_down = matches!(mode, ObserveMode::Enabled);
    }

    pub fn is_up(&self) -> bool {
        self.lock_inner().is_up
    }

    pub fn set_peers_count(&self) -> usize {
        self.lock_inner().set_peers_count
    }

    pub fn current_peers(&self) -> Vec<MachineRecord> {
        self.lock_inner().peers.clone()
    }

    pub fn set_device_peers(&self, peers: Vec<DevicePeer>) {
        self.lock_inner().device_peers = peers;
    }
}

impl MeshNetwork for MemoryWireGuard {
    async fn up(&self) -> Result<()> {
        let mut inner = self.lock_inner();
        if inner.fail_up {
            return Err(Error::operation("wireguard up", "injected failure"));
        }
        inner.is_up = true;
        Ok(())
    }

    async fn down(&self) -> Result<()> {
        let mut inner = self.lock_inner();
        if inner.fail_down {
            return Err(Error::operation("wireguard down", "injected failure"));
        }
        inner.is_up = false;
        Ok(())
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        let mut inner = self.lock_inner();
        inner.peers = peers.to_vec();
        inner.set_peers_count += 1;
        Ok(())
    }
}

impl WireGuardDevice for MemoryWireGuard {
    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        Ok(self.lock_inner().device_peers.clone())
    }

    async fn set_peer_endpoint<'a>(&'a self, key: &'a PublicKey, endpoint: &'a str) -> Result<()> {
        let mut inner = self.lock_inner();
        for peer in &mut inner.device_peers {
            if peer.public_key == *key {
                peer.endpoint = Some(endpoint.to_string());
            }
        }
        Ok(())
    }
}

#[must_use]
pub fn machine_ip(subnet: &Ipv4Net) -> std::net::Ipv4Addr {
    let start = u32::from(subnet.network());
    std::net::Ipv4Addr::from(start + 1)
}

#[must_use]
pub fn container_ip(subnet: &Ipv4Net) -> std::net::Ipv4Addr {
    let start = u32::from(subnet.network());
    std::net::Ipv4Addr::from(start + 2)
}
