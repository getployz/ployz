use std::future::Future;
use std::sync::Arc;

use async_trait::async_trait;
use ipnet::Ipv4Net;
use ployz_types::model::{MachineRecord, OverlayIp, PublicKey};
use tokio::time::Instant;

use crate::Result;

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

#[async_trait]
pub trait EndpointDiscovery: Send + Sync {
    async fn detect_endpoints(&self, listen_port: u16) -> Result<Vec<String>>;
}

pub struct AttachedDataplane {
    pub dataplane: Arc<dyn MeshDataplane>,
    pub wg_ifindex: u32,
}

#[async_trait]
pub trait DataplaneFactory: Send + Sync {
    async fn attach(
        &self,
        network: &WireguardDriver,
        container_network: &ContainerNetwork,
    ) -> Result<AttachedDataplane>;
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
}

impl WireguardDriver {
    #[doc(hidden)]
    #[must_use]
    pub fn from_backend(backend: Arc<dyn WireguardBackend>) -> Self {
        Self { backend }
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
}

impl MeshNetwork for WireguardDriver {
    async fn up(&self) -> Result<()> {
        self.backend.up().await
    }

    async fn down(&self) -> Result<()> {
        self.backend.down().await
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        self.backend.set_peers(peers).await
    }

    async fn has_remote_handshake(&self) -> bool {
        self.backend.has_remote_handshake().await
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        self.backend.bridge_ip().await
    }
}

impl WireGuardDevice for WireguardDriver {
    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        self.backend.read_peers().await
    }

    async fn set_peer_endpoint(&self, key: &PublicKey, endpoint: &str) -> Result<()> {
        self.backend.set_peer_endpoint(key, endpoint).await
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
