use crate::mesh::dataplane::DefaultDataplaneFactory;
use crate::mesh::endpoints::HostEndpointDiscovery;
use crate::mesh::wireguard::{DockerWireGuard, HostWireGuard};
use crate::network::docker_bridge_network;
use async_trait::async_trait;
use ployz_runtime_api::{
    ContainerNetwork, DataplaneFactory, DevicePeer, EndpointDiscovery, MeshNetwork,
    Result as RuntimeResult, RuntimeError, WireGuardDevice, WireguardBackend, WireguardBackendMode,
    WireguardDriver,
};
use ployz_types::model::{Identity, MachineRecord, OverlayIp, PublicKey};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

pub struct ConcreteMeshComponents {
    pub network: WireguardDriver,
    pub container_network: Option<ContainerNetwork>,
    pub endpoint_discovery: Arc<dyn EndpointDiscovery>,
    pub dataplane_factory: Option<Arc<dyn DataplaneFactory>>,
}

pub async fn docker(
    identity: &Identity,
    overlay_ip: OverlayIp,
    network_dir: &Path,
    exposed_tcp_ports: &[u16],
    bridged_api_port: u16,
    image: &str,
) -> RuntimeResult<WireguardDriver> {
    let overlay_api = SocketAddr::new(IpAddr::V6(overlay_ip.0), bridged_api_port);
    let local_api = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), bridged_api_port);

    let mut builder = DockerWireGuard::builder(
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
        .map_err(|error| RuntimeError::operation("docker wireguard", error.to_string()))?;

    Ok(WireguardDriver::from_backend(Arc::new(
        DockerWireguardBackend {
            inner: Arc::new(wireguard),
        },
    )))
}

// Docker mesh startup resolves several runtime seams in one place.
#[expect(clippy::too_many_arguments)]
pub async fn docker_components(
    identity: &Identity,
    overlay_ip: OverlayIp,
    network_dir: &Path,
    network_name: &str,
    subnet: ipnet::Ipv4Net,
    exposed_tcp_ports: &[u16],
    bridged_api_port: u16,
    image: &str,
) -> RuntimeResult<ConcreteMeshComponents> {
    let network = docker(
        identity,
        overlay_ip,
        network_dir,
        exposed_tcp_ports,
        bridged_api_port,
        image,
    )
    .await?;
    let container_network = Some(
        docker_bridge_network(network_name, subnet)
            .await
            .map_err(RuntimeError::from)?,
    );
    Ok(ConcreteMeshComponents {
        network,
        container_network,
        endpoint_discovery: Arc::new(HostEndpointDiscovery),
        dataplane_factory: Some(Arc::new(DefaultDataplaneFactory::new("ployz-networking"))),
    })
}

pub fn host(
    identity: &Identity,
    overlay_ip: OverlayIp,
    network_name: &str,
    subnet: ipnet::Ipv4Net,
) -> RuntimeResult<WireguardDriver> {
    let ifname = format!("plz-{network_name}");
    #[cfg(target_os = "linux")]
    let wireguard =
        HostWireGuard::kernel(&ifname, identity.private_key.clone(), overlay_ip, subnet)
            .map_err(|error| RuntimeError::operation("host wireguard", error.to_string()))?;
    #[cfg(not(target_os = "linux"))]
    let wireguard =
        HostWireGuard::userspace(&ifname, identity.private_key.clone(), overlay_ip, subnet)
            .map_err(|error| RuntimeError::operation("host wireguard", error.to_string()))?;

    Ok(WireguardDriver::from_backend(Arc::new(
        HostWireguardBackend {
            inner: Arc::new(wireguard),
        },
    )))
}

pub async fn host_components(
    identity: &Identity,
    overlay_ip: OverlayIp,
    network_name: &str,
    subnet: ipnet::Ipv4Net,
) -> RuntimeResult<ConcreteMeshComponents> {
    let network = host(identity, overlay_ip, network_name, subnet)?;
    let container_network = Some(
        docker_bridge_network(network_name, subnet)
            .await
            .map_err(RuntimeError::from)?,
    );
    Ok(ConcreteMeshComponents {
        network,
        container_network,
        endpoint_discovery: Arc::new(HostEndpointDiscovery),
        dataplane_factory: Some(Arc::new(DefaultDataplaneFactory::new("ployz-networking"))),
    })
}

struct DockerWireguardBackend {
    inner: Arc<DockerWireGuard>,
}

#[async_trait]
impl WireguardBackend for DockerWireguardBackend {
    fn mode(&self) -> WireguardBackendMode {
        WireguardBackendMode::Docker
    }

    async fn up(&self) -> RuntimeResult<()> {
        self.inner.up().await
    }

    async fn down(&self) -> RuntimeResult<()> {
        self.inner.down().await
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> RuntimeResult<()> {
        self.inner.set_peers(peers).await
    }

    async fn has_remote_handshake(&self) -> bool {
        self.inner.has_remote_handshake().await
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        self.inner.bridge_ip().await
    }

    async fn read_peers(&self) -> RuntimeResult<Vec<DevicePeer>> {
        self.inner.read_peers().await
    }

    async fn set_peer_endpoint(&self, key: &PublicKey, endpoint: &str) -> RuntimeResult<()> {
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

    async fn up(&self) -> RuntimeResult<()> {
        self.inner.up().await
    }

    async fn down(&self) -> RuntimeResult<()> {
        self.inner.down().await
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> RuntimeResult<()> {
        self.inner.set_peers(peers).await
    }

    async fn has_remote_handshake(&self) -> bool {
        self.inner.has_remote_handshake().await
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        self.inner.bridge_ip().await
    }

    async fn read_peers(&self) -> RuntimeResult<Vec<DevicePeer>> {
        self.inner.read_peers().await
    }

    async fn set_peer_endpoint(&self, key: &PublicKey, endpoint: &str) -> RuntimeResult<()> {
        self.inner.set_peer_endpoint(key, endpoint).await
    }
}
