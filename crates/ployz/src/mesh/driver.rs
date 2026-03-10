//! Concrete driver enum that dispatches to backend-specific WireGuard adapters.
//!
//! Closed enum rather than `dyn Trait` because the set of backends is fixed at
//! compile time, exhaustive matching catches new variants, and there is no
//! vtable/`Arc` overhead on hot dispatch paths.

use crate::config::Mode;
use crate::error::Result;
use crate::mesh::wireguard::{DockerWireGuard, HostWireGuard, MemoryWireGuard};
use crate::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
use crate::model::{MachineRecord, OverlayIp, PublicKey};
use crate::node::identity::Identity;
use ployz_corrosion::config as corrosion_config;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::Arc;

#[derive(Clone)]
pub enum WireguardDriver {
    Memory(Arc<MemoryWireGuard>),
    Docker(Arc<DockerWireGuard>),
    Host(Arc<HostWireGuard>),
}

impl WireguardDriver {
    pub async fn from_mode(
        mode: Mode,
        identity: &Identity,
        overlay_ip: OverlayIp,
        network_dir: &Path,
        network_name: &str,
        subnet: ipnet::Ipv4Net,
        exposed_tcp_ports: &[u16],
    ) -> std::result::Result<Self, String> {
        match mode {
            Mode::Memory => Ok(Self::Memory(Arc::new(MemoryWireGuard::new()))),
            Mode::Docker => {
                let api_port = corrosion_config::DEFAULT_API_PORT;
                let overlay_api = SocketAddr::new(IpAddr::V6(overlay_ip.0), api_port);
                let local_api = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), api_port);

                let mut builder = DockerWireGuard::new(
                    "ployz-networking",
                    network_dir,
                    identity.private_key.clone(),
                    overlay_ip,
                )
                .with_bridge_forward(local_api, overlay_api);
                for &port in exposed_tcp_ports {
                    builder = builder.expose_tcp(port);
                }
                let wg = builder
                    .build()
                    .await
                    .map_err(|e| format!("docker wireguard: {e}"))?;
                Ok(Self::Docker(Arc::new(wg)))
            }
            Mode::HostExec | Mode::HostService => {
                let ifname = format!("plz-{network_name}");
                #[cfg(target_os = "linux")]
                let wg = HostWireGuard::kernel(
                    &ifname,
                    identity.private_key.clone(),
                    overlay_ip,
                    subnet,
                )
                .map_err(|e| format!("host wireguard: {e}"))?;
                #[cfg(not(target_os = "linux"))]
                let wg = HostWireGuard::userspace(
                    &ifname,
                    identity.private_key.clone(),
                    overlay_ip,
                    subnet,
                )
                .map_err(|e| format!("host wireguard: {e}"))?;
                Ok(Self::Host(Arc::new(wg)))
            }
        }
    }
}

impl MeshNetwork for WireguardDriver {
    async fn up(&self) -> Result<()> {
        match self {
            Self::Memory(n) => n.up().await,
            Self::Docker(n) => n.up().await,
            Self::Host(n) => n.up().await,
        }
    }

    async fn down(&self) -> Result<()> {
        match self {
            Self::Memory(n) => n.down().await,
            Self::Docker(n) => n.down().await,
            Self::Host(n) => n.down().await,
        }
    }

    async fn set_peers<'a>(&'a self, peers: &'a [MachineRecord]) -> Result<()> {
        match self {
            Self::Memory(n) => n.set_peers(peers).await,
            Self::Docker(n) => n.set_peers(peers).await,
            Self::Host(n) => n.set_peers(peers).await,
        }
    }

    async fn has_remote_handshake(&self) -> bool {
        match self {
            Self::Memory(n) => n.has_remote_handshake().await,
            Self::Docker(n) => n.has_remote_handshake().await,
            Self::Host(n) => n.has_remote_handshake().await,
        }
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        match self {
            Self::Memory(n) => n.bridge_ip().await,
            Self::Docker(n) => n.bridge_ip().await,
            Self::Host(n) => n.bridge_ip().await,
        }
    }
}

impl WireGuardDevice for WireguardDriver {
    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        match self {
            Self::Memory(n) => n.read_peers().await,
            Self::Docker(n) => n.read_peers().await,
            Self::Host(n) => n.read_peers().await,
        }
    }

    async fn set_peer_endpoint<'a>(&'a self, key: &'a PublicKey, endpoint: &'a str) -> Result<()> {
        match self {
            Self::Memory(n) => n.set_peer_endpoint(key, endpoint).await,
            Self::Docker(n) => n.set_peer_endpoint(key, endpoint).await,
            Self::Host(n) => n.set_peer_endpoint(key, endpoint).await,
        }
    }
}
