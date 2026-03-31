use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use ployz_runtime_api::{
    DevicePeer, EndpointDiscovery, ObserveMode, Result, RuntimeError, WireGuardDevice,
    WireguardBackend, WireguardBackendMode, WireguardDriver,
};
use ployz_types::model::{MachineRecord, OverlayIp, PublicKey};
use tokio::time::Instant;

pub struct StaticEndpointDiscovery {
    endpoints: Arc<Vec<String>>,
}

impl StaticEndpointDiscovery {
    #[must_use]
    pub fn new(endpoints: Vec<String>) -> Self {
        Self {
            endpoints: Arc::new(endpoints),
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self::new(Vec::new())
    }
}

#[async_trait]
impl EndpointDiscovery for StaticEndpointDiscovery {
    async fn detect_endpoints(&self, _listen_port: u16) -> Result<Vec<String>> {
        Ok(self.endpoints.as_ref().clone())
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

    #[must_use]
    pub fn is_up(&self) -> bool {
        self.lock_inner().is_up
    }

    pub fn fail_up(&self, fail: bool) {
        self.lock_inner().fail_up = fail;
    }

    pub fn set_fail_up(&self, mode: ObserveMode) {
        self.fail_up(matches!(mode, ObserveMode::Enabled));
    }

    pub fn fail_down(&self, fail: bool) {
        self.lock_inner().fail_down = fail;
    }

    pub fn set_fail_down(&self, mode: ObserveMode) {
        self.fail_down(matches!(mode, ObserveMode::Enabled));
    }

    pub fn set_device_peers(&self, peers: Vec<DevicePeer>) {
        self.lock_inner().device_peers = peers;
    }

    #[must_use]
    pub fn peers(&self) -> Vec<MachineRecord> {
        self.lock_inner().peers.clone()
    }

    #[must_use]
    pub fn current_peers(&self) -> Vec<MachineRecord> {
        self.peers()
    }

    #[must_use]
    pub fn set_peers_count(&self) -> usize {
        self.lock_inner().set_peers_count
    }

    pub async fn up(&self) -> Result<()> {
        let mut inner = self.lock_inner();
        if inner.fail_up {
            return Err(RuntimeError::operation(
                "memory_wireguard_up",
                "injected failure",
            ));
        }
        inner.is_up = true;
        Ok(())
    }

    pub async fn down(&self) -> Result<()> {
        let mut inner = self.lock_inner();
        if inner.fail_down {
            return Err(RuntimeError::operation(
                "memory_wireguard_down",
                "injected failure",
            ));
        }
        inner.is_up = false;
        Ok(())
    }

    pub async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        let mut inner = self.lock_inner();
        inner.peers = peers.to_vec();
        inner.set_peers_count += 1;
        Ok(())
    }

    pub async fn has_remote_handshake(&self) -> bool {
        self.lock_inner()
            .device_peers
            .iter()
            .any(|peer| peer.last_handshake.is_some())
    }

    pub async fn bridge_ip(&self) -> Option<OverlayIp> {
        None
    }

    pub async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        Ok(self.lock_inner().device_peers.clone())
    }

    pub async fn set_peer_endpoint(&self, key: &PublicKey, endpoint: &str) -> Result<()> {
        let mut inner = self.lock_inner();
        let Some(peer) = inner
            .device_peers
            .iter_mut()
            .find(|peer| peer.public_key == *key)
        else {
            return Err(RuntimeError::operation(
                "memory_wireguard_set_peer_endpoint",
                "peer not found",
            ));
        };
        peer.endpoint = Some(endpoint.to_string());
        peer.last_handshake = Some(Instant::now());
        Ok(())
    }
}

impl ployz_runtime_api::MeshNetwork for MemoryWireGuard {
    fn up(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        Self::up(self)
    }

    fn down(&self) -> impl Future<Output = Result<()>> + Send + '_ {
        Self::down(self)
    }

    fn set_peers<'a>(
        &'a self,
        peers: &'a [MachineRecord],
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        Self::set_peers(self, peers)
    }

    fn has_remote_handshake(&self) -> impl Future<Output = bool> + Send + '_ {
        Self::has_remote_handshake(self)
    }

    fn bridge_ip(&self) -> impl Future<Output = Option<OverlayIp>> + Send + '_ {
        Self::bridge_ip(self)
    }
}

impl WireGuardDevice for MemoryWireGuard {
    fn read_peers(&self) -> impl Future<Output = Result<Vec<DevicePeer>>> + Send + '_ {
        Self::read_peers(self)
    }

    fn set_peer_endpoint<'a>(
        &'a self,
        key: &'a PublicKey,
        endpoint: &'a str,
    ) -> impl Future<Output = Result<()>> + Send + 'a {
        Self::set_peer_endpoint(self, key, endpoint)
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

    async fn has_remote_handshake(&self) -> bool {
        self.memory.has_remote_handshake().await
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        self.memory.bridge_ip().await
    }

    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        self.memory.read_peers().await
    }

    async fn set_peer_endpoint(&self, key: &PublicKey, endpoint: &str) -> Result<()> {
        self.memory.set_peer_endpoint(key, endpoint).await
    }
}

#[must_use]
pub fn memory_wireguard_driver(memory: Arc<MemoryWireGuard>) -> WireguardDriver {
    WireguardDriver::from_backend(Arc::new(MemoryWireguardBackend { memory }))
}
