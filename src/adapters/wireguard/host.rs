use std::net::SocketAddr;
use std::sync::Mutex;
use tracing::info;

use defguard_wireguard_rs::key::Key;
use defguard_wireguard_rs::net::IpAddrMask;
use defguard_wireguard_rs::peer::Peer;
use defguard_wireguard_rs::{InterfaceConfiguration, WGApi, WireguardInterfaceApi};

use crate::error::{PortError, PortResult};
use crate::mesh::MeshNetwork;
use crate::store::model::{MachineRecord, OverlayIp, PrivateKey};

use super::config::encode_key;

const DEFAULT_LISTEN_PORT: u16 = 51820;

enum WgBackend {
    #[cfg(target_os = "linux")]
    Kernel(WGApi<defguard_wireguard_rs::Kernel>),
    Userspace(WGApi<defguard_wireguard_rs::Userspace>),
}

pub struct HostWireGuard {
    backend: Mutex<WgBackend>,
    ifname: String,
    private_key: PrivateKey,
    overlay_ip: OverlayIp,
    listen_port: u16,
}

impl HostWireGuard {
    #[cfg(target_os = "linux")]
    pub fn kernel(
        ifname: &str,
        private_key: PrivateKey,
        overlay_ip: OverlayIp,
    ) -> PortResult<Self> {
        let api = WGApi::new(ifname.to_string())
            .map_err(|e| PortError::operation("kernel wg init", e.to_string()))?;
        Ok(Self {
            backend: Mutex::new(WgBackend::Kernel(api)),
            ifname: ifname.to_string(),
            private_key,
            overlay_ip,
            listen_port: DEFAULT_LISTEN_PORT,
        })
    }

    pub fn userspace(
        ifname: &str,
        private_key: PrivateKey,
        overlay_ip: OverlayIp,
    ) -> PortResult<Self> {
        let api = WGApi::new(ifname.to_string())
            .map_err(|e| PortError::operation("userspace wg init", e.to_string()))?;
        Ok(Self {
            backend: Mutex::new(WgBackend::Userspace(api)),
            ifname: ifname.to_string(),
            private_key,
            overlay_ip,
            listen_port: DEFAULT_LISTEN_PORT,
        })
    }

    pub fn with_listen_port(mut self, port: u16) -> Self {
        self.listen_port = port;
        self
    }

    fn lock_backend(&self) -> std::sync::MutexGuard<'_, WgBackend> {
        self.backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn with_api<F, R>(&self, f: F) -> PortResult<R>
    where
        F: FnOnce(&mut WgBackend) -> PortResult<R>,
    {
        let mut guard = self.lock_backend();
        f(&mut guard)
    }
}

fn machine_to_peer(record: &MachineRecord) -> PortResult<Peer> {
    let key = Key::new(record.public_key.0);

    let allowed_ip: IpAddrMask = format!("{}/128", record.overlay_ip.0)
        .parse()
        .map_err(|e| PortError::operation("parse allowed ip", format!("{e}")))?;

    let endpoint: Option<SocketAddr> = record.endpoints.first().and_then(|ep| ep.parse().ok());

    let mut peer = Peer::new(key);
    peer.allowed_ips = vec![allowed_ip];
    peer.endpoint = endpoint;
    peer.persistent_keepalive_interval = Some(25);
    Ok(peer)
}

fn wg_create_interface(backend: &mut WgBackend) -> PortResult<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .create_interface()
            .map_err(|e| PortError::operation("create interface", e.to_string())),
        WgBackend::Userspace(api) => api
            .create_interface()
            .map_err(|e| PortError::operation("create interface", e.to_string())),
    }
}

fn wg_remove_interface(backend: &WgBackend) -> PortResult<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .remove_interface()
            .map_err(|e| PortError::operation("remove interface", e.to_string())),
        WgBackend::Userspace(api) => api
            .remove_interface()
            .map_err(|e| PortError::operation("remove interface", e.to_string())),
    }
}

fn wg_configure_interface(
    backend: &WgBackend,
    config: &InterfaceConfiguration,
) -> PortResult<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .configure_interface(config)
            .map_err(|e| PortError::operation("configure interface", e.to_string())),
        WgBackend::Userspace(api) => api
            .configure_interface(config)
            .map_err(|e| PortError::operation("configure interface", e.to_string())),
    }
}

fn wg_read_peers(backend: &WgBackend) -> PortResult<Vec<Peer>> {
    let host = match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .read_interface_data()
            .map_err(|e| PortError::operation("read interface", e.to_string()))?,
        WgBackend::Userspace(api) => api
            .read_interface_data()
            .map_err(|e| PortError::operation("read interface", e.to_string()))?,
    };
    Ok(host.peers.into_values().collect())
}

fn wg_configure_peer(backend: &WgBackend, peer: &Peer) -> PortResult<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .configure_peer(peer)
            .map_err(|e| PortError::operation("configure peer", e.to_string())),
        WgBackend::Userspace(api) => api
            .configure_peer(peer)
            .map_err(|e| PortError::operation("configure peer", e.to_string())),
    }
}

fn wg_remove_peer(backend: &WgBackend, key: &Key) -> PortResult<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .remove_peer(key)
            .map_err(|e| PortError::operation("remove peer", e.to_string())),
        WgBackend::Userspace(api) => api
            .remove_peer(key)
            .map_err(|e| PortError::operation("remove peer", e.to_string())),
    }
}

impl MeshNetwork for HostWireGuard {
    async fn up(&self) -> PortResult<()> {
        self.with_api(|backend| {
            wg_create_interface(backend)?;

            let addr: IpAddrMask = format!("{}/128", self.overlay_ip.0)
                .parse()
                .map_err(|e| PortError::operation("parse overlay addr", format!("{e}")))?;

            let config = InterfaceConfiguration {
                name: self.ifname.clone(),
                prvkey: encode_key(&self.private_key.0),
                addresses: vec![addr],
                port: self.listen_port,
                peers: Vec::new(),
                mtu: None,
                fwmark: None,
            };
            wg_configure_interface(backend, &config)
        })?;

        info!(ifname = %self.ifname, "wireguard interface up");
        Ok(())
    }

    async fn down(&self) -> PortResult<()> {
        let backend = self.lock_backend();
        wg_remove_interface(&backend)?;
        info!(ifname = %self.ifname, "wireguard interface down");
        Ok(())
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> PortResult<()> {
        let desired: Vec<Peer> = peers
            .iter()
            .map(machine_to_peer)
            .collect::<PortResult<Vec<_>>>()?;

        let backend = self.lock_backend();

        let current = wg_read_peers(&backend)?;

        let desired_keys: std::collections::HashSet<_> =
            desired.iter().map(|p| p.public_key.clone()).collect();
        let current_keys: std::collections::HashSet<_> =
            current.iter().map(|p| p.public_key.clone()).collect();

        for key in current_keys.difference(&desired_keys) {
            wg_remove_peer(&backend, key)?;
        }

        for peer in &desired {
            wg_configure_peer(&backend, peer)?;
        }

        info!(peer_count = desired.len(), "synced wireguard peers");
        Ok(())
    }
}
