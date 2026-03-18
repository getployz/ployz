use crate::mesh::wireguard::{DockerWireGuard, HostWireGuard};
use async_trait::async_trait;
use ployz_corrosion::config as corrosion_config;
use ployz_orchestrator::WireguardDriver;
use ployz_orchestrator::mesh::driver::{WireguardBackend, WireguardBackendMode};
use ployz_orchestrator::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
use ployz_runtime_api::Identity;
use ployz_types::Result;
use ployz_types::model::{MachineRecord, OverlayIp, PublicKey};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

pub async fn docker(
    identity: &Identity,
    overlay_ip: OverlayIp,
    network_dir: &Path,
    exposed_tcp_ports: &[u16],
    image: &str,
) -> std::result::Result<WireguardDriver, String> {
    let api_port = corrosion_config::DEFAULT_API_PORT;
    let overlay_api = SocketAddr::new(IpAddr::V6(overlay_ip.0), api_port);
    let local_api = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), api_port);

    let mut builder = DockerWireGuard::new(
        "ployz-networking",
        network_dir,
        identity.private_key.clone(),
        overlay_ip,
    )
    .image(image)
    .with_bridge_forward(local_api, overlay_api);
    for &port in exposed_tcp_ports {
        builder = builder.expose_tcp(port);
    }
    let wireguard = builder
        .build()
        .await
        .map_err(|error| format!("docker wireguard: {error}"))?;

    Ok(WireguardDriver::from_backend(Arc::new(
        DockerWireguardBackend {
            inner: Arc::new(wireguard),
        },
    )))
}

pub fn host(
    identity: &Identity,
    overlay_ip: OverlayIp,
    network_name: &str,
    subnet: ipnet::Ipv4Net,
) -> std::result::Result<WireguardDriver, String> {
    let ifname = format!("plz-{network_name}");
    #[cfg(target_os = "linux")]
    let wireguard =
        HostWireGuard::kernel(&ifname, identity.private_key.clone(), overlay_ip, subnet)
            .map_err(|error| format!("host wireguard: {error}"))?;
    #[cfg(not(target_os = "linux"))]
    let wireguard =
        HostWireGuard::userspace(&ifname, identity.private_key.clone(), overlay_ip, subnet)
            .map_err(|error| format!("host wireguard: {error}"))?;

    Ok(WireguardDriver::from_backend(Arc::new(
        HostWireguardBackend {
            inner: Arc::new(wireguard),
        },
    )))
}

struct DockerWireguardBackend {
    inner: Arc<DockerWireGuard>,
}

#[async_trait]
impl WireguardBackend for DockerWireguardBackend {
    fn mode(&self) -> WireguardBackendMode {
        WireguardBackendMode::Docker
    }

    async fn up(&self) -> Result<()> {
        self.inner.up().await
    }

    async fn down(&self) -> Result<()> {
        self.inner.down().await
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        self.inner.set_peers(peers).await
    }

    async fn has_remote_handshake(&self) -> bool {
        self.inner.has_remote_handshake().await
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        self.inner.bridge_ip().await
    }

    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        self.inner.read_peers().await
    }

    async fn set_peer_endpoint(&self, key: &PublicKey, endpoint: &str) -> Result<()> {
        self.inner.set_peer_endpoint(key, endpoint).await
    }
}

struct HostWireguardBackend {
    inner: Arc<HostWireGuard>,
}

#[async_trait]
impl WireguardBackend for HostWireguardBackend {
    fn mode(&self) -> WireguardBackendMode {
        WireguardBackendMode::Host
    }

    fn host_interface_name(&self) -> Option<&str> {
        Some(self.inner.ifname())
    }

    async fn up(&self) -> Result<()> {
        self.inner.up().await
    }

    async fn down(&self) -> Result<()> {
        self.inner.down().await
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        self.inner.set_peers(peers).await
    }

    async fn has_remote_handshake(&self) -> bool {
        self.inner.has_remote_handshake().await
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        self.inner.bridge_ip().await
    }

    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        self.inner.read_peers().await
    }

    async fn set_peer_endpoint(&self, key: &PublicKey, endpoint: &str) -> Result<()> {
        self.inner.set_peer_endpoint(key, endpoint).await
    }
}
