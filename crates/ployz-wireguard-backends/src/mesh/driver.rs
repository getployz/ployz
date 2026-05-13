#[cfg(feature = "docker")]
use crate::mesh::wireguard::DockerWireGuard;
#[cfg(feature = "userspace-wg")]
use crate::mesh::wireguard::HostWireGuard;
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
use async_trait::async_trait;
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
use ployz_error::Result;
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
use ployz_model::{OverlayIp, PublicKey, WireGuardPeerSpec};
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
use ployz_runtime_api::Identity;
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
use ployz_runtime_api::mesh::WireguardDriver;
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
use ployz_runtime_api::mesh::driver::{WireguardBackend, WireguardBackendMode};
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
use ployz_runtime_api::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
#[cfg(feature = "docker")]
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(feature = "docker")]
use std::path::Path;
#[cfg(any(feature = "docker", feature = "userspace-wg"))]
use std::sync::Arc;

#[cfg(feature = "docker")]
pub async fn docker(
    identity: &Identity,
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bridge_tcp_port: u16,
    exposed_tcp_ports: &[u16],
    image: &str,
) -> std::result::Result<WireguardDriver, String> {
    let overlay_api = SocketAddr::new(IpAddr::V6(overlay_ip.0), bridge_tcp_port);
    let local_api = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bridge_tcp_port);

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

#[cfg(feature = "userspace-wg")]
pub fn host(
    identity: &Identity,
    overlay_ip: OverlayIp,
    network_name: &str,
    subnet: Option<ipnet::Ipv4Net>,
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

#[cfg(feature = "docker")]
struct DockerWireguardBackend {
    inner: Arc<DockerWireGuard>,
}

#[cfg(feature = "docker")]
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

    async fn set_peers(&self, peers: &[WireGuardPeerSpec]) -> Result<()> {
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

#[cfg(feature = "userspace-wg")]
struct HostWireguardBackend {
    inner: Arc<HostWireGuard>,
}

#[cfg(feature = "userspace-wg")]
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

    async fn set_peers(&self, peers: &[WireGuardPeerSpec]) -> Result<()> {
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
