use std::net::SocketAddr;
#[cfg(target_os = "linux")]
use std::process::Command;
use std::sync::Mutex;
use std::time::SystemTime;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use defguard_wireguard_rs::key::Key;
use defguard_wireguard_rs::net::IpAddrMask;
use defguard_wireguard_rs::peer::Peer;
use defguard_wireguard_rs::{InterfaceConfiguration, WGApi, WireguardInterfaceApi};

use crate::error::{Error, Result};
use crate::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
use crate::model::{MachineRecord, OverlayIp, PrivateKey, PublicKey};

use super::config::encode_key;

use super::{DEFAULT_LISTEN_PORT, PERSISTENT_KEEPALIVE_SECS};

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
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    subnet: ipnet::Ipv4Net,
}

impl HostWireGuard {
    #[cfg(target_os = "linux")]
    pub fn kernel(
        ifname: &str,
        private_key: PrivateKey,
        overlay_ip: OverlayIp,
        subnet: ipnet::Ipv4Net,
    ) -> Result<Self> {
        let api = WGApi::new(ifname.to_string())
            .map_err(|e| Error::operation("kernel wg init", e.to_string()))?;
        Ok(Self {
            backend: Mutex::new(WgBackend::Kernel(api)),
            ifname: ifname.to_string(),
            private_key,
            overlay_ip,
            listen_port: DEFAULT_LISTEN_PORT,
            subnet,
        })
    }

    pub fn userspace(
        ifname: &str,
        private_key: PrivateKey,
        overlay_ip: OverlayIp,
        subnet: ipnet::Ipv4Net,
    ) -> Result<Self> {
        let api = WGApi::new(ifname.to_string())
            .map_err(|e| Error::operation("userspace wg init", e.to_string()))?;
        Ok(Self {
            backend: Mutex::new(WgBackend::Userspace(api)),
            ifname: ifname.to_string(),
            private_key,
            overlay_ip,
            listen_port: DEFAULT_LISTEN_PORT,
            subnet,
        })
    }

    pub fn ifname(&self) -> &str {
        &self.ifname
    }

    #[must_use]
    pub fn with_listen_port(mut self, port: u16) -> Self {
        self.listen_port = port;
        self
    }

    fn lock_backend(&self) -> std::sync::MutexGuard<'_, WgBackend> {
        self.backend
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn with_api<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce(&mut WgBackend) -> Result<R>,
    {
        let mut guard = self.lock_backend();
        f(&mut guard)
    }
}

fn machine_to_peer(record: &MachineRecord) -> Result<Peer> {
    let key = Key::new(record.public_key.0);

    let allowed_ips: Vec<IpAddrMask> = record
        .allowed_cidrs()
        .iter()
        .map(|cidr| {
            cidr.parse()
                .map_err(|e| Error::operation("parse allowed ip", format!("{e}")))
        })
        .collect::<Result<Vec<_>>>()?;

    let endpoint: Option<SocketAddr> = record.endpoints.first().and_then(|ep| ep.parse().ok());

    let mut peer = Peer::new(key);
    peer.allowed_ips = allowed_ips;
    peer.endpoint = endpoint;
    peer.persistent_keepalive_interval = Some(PERSISTENT_KEEPALIVE_SECS);
    Ok(peer)
}

fn wg_create_interface(backend: &mut WgBackend) -> Result<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .create_interface()
            .map_err(|e| Error::operation("create interface", e.to_string())),
        WgBackend::Userspace(api) => api
            .create_interface()
            .map_err(|e| Error::operation("create interface", e.to_string())),
    }
}

fn wg_remove_interface(backend: &WgBackend) -> Result<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .remove_interface()
            .map_err(|e| Error::operation("remove interface", e.to_string())),
        WgBackend::Userspace(api) => api
            .remove_interface()
            .map_err(|e| Error::operation("remove interface", e.to_string())),
    }
}

fn wg_configure_interface(backend: &WgBackend, config: &InterfaceConfiguration) -> Result<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .configure_interface(config)
            .map_err(|e| Error::operation("configure interface", e.to_string())),
        WgBackend::Userspace(api) => api
            .configure_interface(config)
            .map_err(|e| Error::operation("configure interface", e.to_string())),
    }
}

fn wg_read_peers(backend: &WgBackend) -> Result<Vec<Peer>> {
    let host = match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .read_interface_data()
            .map_err(|e| Error::operation("read interface", e.to_string()))?,
        WgBackend::Userspace(api) => api
            .read_interface_data()
            .map_err(|e| Error::operation("read interface", e.to_string()))?,
    };
    Ok(host.peers.into_values().collect())
}

fn wg_configure_peer(backend: &WgBackend, peer: &Peer) -> Result<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .configure_peer(peer)
            .map_err(|e| Error::operation("configure peer", e.to_string())),
        WgBackend::Userspace(api) => api
            .configure_peer(peer)
            .map_err(|e| Error::operation("configure peer", e.to_string())),
    }
}

fn wg_remove_peer(backend: &WgBackend, key: &Key) -> Result<()> {
    match backend {
        #[cfg(target_os = "linux")]
        WgBackend::Kernel(api) => api
            .remove_peer(key)
            .map_err(|e| Error::operation("remove peer", e.to_string())),
        WgBackend::Userspace(api) => api
            .remove_peer(key)
            .map_err(|e| Error::operation("remove peer", e.to_string())),
    }
}

fn system_time_to_instant(value: SystemTime) -> Option<Instant> {
    let elapsed = SystemTime::now().duration_since(value).ok()?;
    Instant::now().checked_sub(elapsed)
}

#[cfg(target_os = "linux")]
fn add_route(ifname: &str, cidr: &str) -> Result<()> {
    let is_v6 = cidr.contains(':');
    let mut args = Vec::new();
    if is_v6 {
        args.push("-6");
    }
    args.extend(["route", "replace", cidr, "dev", ifname]);

    let output = Command::new("ip")
        .args(&args)
        .output()
        .map_err(|e| Error::operation("add route", e.to_string()))?;
    if output.status.success() {
        info!(ifname, cidr, "overlay route added");
        return Ok(());
    }
    Err(Error::operation(
        "add route",
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    ))
}

#[cfg(target_os = "linux")]
fn del_route(cidr: &str, ifname: &str) -> Result<()> {
    let is_v6 = cidr.contains(':');
    let mut args = Vec::new();
    if is_v6 {
        args.push("-6");
    }
    args.extend(["route", "del", cidr, "dev", ifname]);

    let output = Command::new("ip")
        .args(&args)
        .output()
        .map_err(|e| Error::operation("del route", e.to_string()))?;
    if output.status.success()
        || String::from_utf8_lossy(&output.stderr).contains("No such process")
    {
        return Ok(());
    }
    Err(Error::operation(
        "del route",
        String::from_utf8_lossy(&output.stderr).trim().to_string(),
    ))
}

impl MeshNetwork for HostWireGuard {
    async fn up(&self) -> Result<()> {
        self.with_api(|backend| {
            wg_create_interface(backend)?;

            let overlay_addr: IpAddrMask = format!("{}/128", self.overlay_ip.0)
                .parse()
                .map_err(|e| Error::operation("parse overlay addr", format!("{e}")))?;

            // Only the IPv6 management address goes on the WG interface.
            // IPv4 subnet is owned by the Docker bridge — per-peer routes
            // with src= handle IPv4 routing (same approach as uncloud).
            let config = InterfaceConfiguration {
                name: self.ifname.clone(),
                prvkey: encode_key(&self.private_key.0),
                addresses: vec![overlay_addr],
                port: self.listen_port,
                peers: Vec::new(),
                mtu: None,
                fwmark: None,
            };
            wg_configure_interface(backend, &config)
        })?;

        // Only the IPv6 overlay route goes on at startup. IPv4 per-peer
        // subnet routes are managed in set_peers() so the Docker bridge
        // owns the local subnet without conflicts.
        #[cfg(target_os = "linux")]
        add_route(&self.ifname, "fd00::/8")?;
        #[cfg(not(target_os = "linux"))]
        warn!("overlay route configuration not implemented on this platform");

        info!(ifname = %self.ifname, "wireguard interface up");
        Ok(())
    }

    async fn down(&self) -> Result<()> {
        #[cfg(target_os = "linux")]
        {
            let _ = del_route("fd00::/8", &self.ifname);
        }
        #[cfg(not(target_os = "linux"))]
        warn!("overlay route removal not implemented on this platform");
        let backend = self.lock_backend();
        wg_remove_interface(&backend)?;
        info!(ifname = %self.ifname, "wireguard interface down");
        Ok(())
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        let desired: Vec<Peer> = peers
            .iter()
            .map(machine_to_peer)
            .collect::<Result<Vec<_>>>()?;

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

        // Sync per-peer IPv4 subnet routes. Use machine's first subnet IP as
        // src so outbound overlay traffic has a routable return address.
        #[cfg(target_os = "linux")]
        {
            let src_ip = self.subnet.hosts().next().map(|ip| ip.to_string());
            let desired_subnets: std::collections::HashSet<String> = peers
                .iter()
                .filter_map(|p| p.subnet.map(|s| s.to_string()))
                .collect();

            // Read current wg0 IPv4 routes
            let output = Command::new("ip")
                .args(["route", "show", "dev", &self.ifname])
                .output()
                .map_err(|e| Error::operation("list routes", e.to_string()))?;
            let current_routes: std::collections::HashSet<String> =
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| line.split_whitespace().next())
                    .filter(|dest| dest.contains('/') && !dest.contains(':'))
                    .map(|s| s.to_string())
                    .collect();

            for subnet in desired_subnets.difference(&current_routes) {
                let mut args = vec!["route", "replace", subnet.as_str(), "dev", &self.ifname];
                if let Some(ref src) = src_ip {
                    args.extend(["src", src.as_str()]);
                }
                let _ = Command::new("ip").args(&args).output();
            }
            for subnet in current_routes.difference(&desired_subnets) {
                let _ = Command::new("ip")
                    .args(["route", "del", subnet.as_str(), "dev", &self.ifname])
                    .output();
            }
        }

        debug!(peer_count = desired.len(), "synced wireguard peers");
        Ok(())
    }
}

impl WireGuardDevice for HostWireGuard {
    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        let backend = self.lock_backend();
        let peers = wg_read_peers(&backend)?;
        let device_peers = peers
            .into_iter()
            .map(|peer| DevicePeer {
                public_key: PublicKey(peer.public_key.as_array()),
                endpoint: peer.endpoint.map(|endpoint| endpoint.to_string()),
                last_handshake: peer.last_handshake.and_then(system_time_to_instant),
            })
            .collect();
        Ok(device_peers)
    }

    async fn set_peer_endpoint<'a>(&'a self, key: &'a PublicKey, endpoint: &'a str) -> Result<()> {
        let endpoint: SocketAddr = endpoint
            .parse()
            .map_err(|e| Error::operation("set peer endpoint", format!("{e}")))?;
        let backend = self.lock_backend();
        let mut peers = wg_read_peers(&backend)?;
        let key = Key::new(key.0);
        let Some(peer) = peers.iter_mut().find(|peer| peer.public_key == key) else {
            return Err(Error::operation(
                "set peer endpoint",
                "peer not found".to_string(),
            ));
        };
        peer.endpoint = Some(endpoint);
        wg_configure_peer(&backend, peer)
    }
}
