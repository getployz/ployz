use std::collections::HashSet;
use std::net::SocketAddr;
use std::process::Command;
use std::sync::Mutex;

use async_trait::async_trait;
use defguard_wireguard_rs::key::Key;
use defguard_wireguard_rs::net::IpAddrMask;
use defguard_wireguard_rs::peer::Peer;
use defguard_wireguard_rs::{InterfaceConfiguration, WGApi, WireguardInterfaceApi};
use mvp_bus::IslandId;

use crate::{
    MeshError, MeshResult, WireGuardAppliedSnapshot, WireGuardBackend, WireGuardPeer,
    WireGuardPrivateKey, WireGuardSnapshotPaths, load_applied_snapshot, write_applied_snapshot,
};

const DEFAULT_LISTEN_PORT: u16 = 51820;
const PERSISTENT_KEEPALIVE_SECS: u16 = 5;

pub struct LinuxWireGuardConfig {
    pub island: IslandId,
    pub ifname: String,
    pub private_key: WireGuardPrivateKey,
    pub listen_port: u16,
    pub snapshot_paths: WireGuardSnapshotPaths,
}

impl LinuxWireGuardConfig {
    #[must_use]
    pub fn new(
        island: IslandId,
        ifname: impl Into<String>,
        private_key: WireGuardPrivateKey,
        snapshot_paths: WireGuardSnapshotPaths,
    ) -> Self {
        Self {
            island,
            ifname: ifname.into(),
            private_key,
            listen_port: DEFAULT_LISTEN_PORT,
            snapshot_paths,
        }
    }

    #[must_use]
    pub fn with_listen_port(mut self, listen_port: u16) -> Self {
        self.listen_port = listen_port;
        self
    }
}

pub struct LinuxWireGuardBackend {
    config: LinuxWireGuardConfig,
    api: Mutex<WGApi<defguard_wireguard_rs::Kernel>>,
}

impl LinuxWireGuardBackend {
    pub fn new(config: LinuxWireGuardConfig) -> MeshResult<Self> {
        let api = WGApi::new(config.ifname.clone()).map_err(|source| MeshError::Backend {
            operation: "wireguard init",
            message: source.to_string(),
        })?;
        Ok(Self {
            config,
            api: Mutex::new(api),
        })
    }

    fn with_api<F, T>(&self, operation: &'static str, f: F) -> MeshResult<T>
    where
        F: FnOnce(&mut WGApi<defguard_wireguard_rs::Kernel>) -> MeshResult<T>,
    {
        let mut api = self.api.lock().map_err(|source| MeshError::Backend {
            operation,
            message: source.to_string(),
        })?;
        f(&mut api)
    }

    fn ensure_interface(&self, snapshot: &WireGuardAppliedSnapshot) -> MeshResult<()> {
        self.with_api("wireguard configure interface", |api| {
            let _ = api.create_interface();
            let overlay_addr: IpAddrMask = format!("{}/128", snapshot.local_overlay_ip)
                .parse::<IpAddrMask>()
                .map_err(|source| MeshError::Backend {
                    operation: "parse local overlay address",
                    message: source.to_string(),
                })?;
            let config = InterfaceConfiguration {
                name: self.config.ifname.clone(),
                prvkey: self.config.private_key.to_base64(),
                addresses: vec![overlay_addr],
                port: self.config.listen_port,
                peers: Vec::new(),
                mtu: None,
                fwmark: None,
            };
            api.configure_interface(&config)
                .map_err(|source| MeshError::Backend {
                    operation: "wireguard configure interface",
                    message: source.to_string(),
                })
        })?;
        add_route(&self.config.ifname, "fd00::/8")
    }

    fn reconcile_peers(&self, peers: &[WireGuardPeer]) -> MeshResult<()> {
        let desired = peers
            .iter()
            .map(peer_to_wg_peer)
            .collect::<MeshResult<Vec<_>>>()?;
        self.with_api("wireguard reconcile peers", |api| {
            let current = api
                .read_interface_data()
                .map_err(|source| MeshError::Backend {
                    operation: "wireguard read interface",
                    message: source.to_string(),
                })?
                .peers
                .into_values()
                .collect::<Vec<_>>();
            let desired_keys = desired
                .iter()
                .map(|peer| peer.public_key.clone())
                .collect::<HashSet<_>>();
            let current_keys = current
                .iter()
                .map(|peer| peer.public_key.clone())
                .collect::<HashSet<_>>();
            for key in current_keys.difference(&desired_keys) {
                api.remove_peer(key).map_err(|source| MeshError::Backend {
                    operation: "wireguard remove peer",
                    message: source.to_string(),
                })?;
            }
            for peer in &desired {
                api.configure_peer(peer)
                    .map_err(|source| MeshError::Backend {
                        operation: "wireguard configure peer",
                        message: source.to_string(),
                    })?;
            }
            Ok(())
        })
    }
}

#[async_trait]
impl WireGuardBackend for LinuxWireGuardBackend {
    async fn apply(&self, snapshot: WireGuardAppliedSnapshot) -> MeshResult<()> {
        self.ensure_interface(&snapshot)?;
        self.reconcile_peers(&snapshot.peers)?;
        write_applied_snapshot(&self.config.snapshot_paths, &snapshot)
    }

    async fn last_applied(&self) -> MeshResult<Option<WireGuardAppliedSnapshot>> {
        if !self.config.snapshot_paths.applied.exists() {
            return Ok(None);
        }
        load_applied_snapshot(&self.config.snapshot_paths, &self.config.island).map(Some)
    }
}

fn peer_to_wg_peer(peer: &WireGuardPeer) -> MeshResult<Peer> {
    let mut wg_peer = Peer::new(Key::new(peer.public_key.to_bytes()?));
    wg_peer.allowed_ips =
        vec![
            peer.allowed_ip
                .to_string()
                .parse::<IpAddrMask>()
                .map_err(|source| MeshError::Backend {
                    operation: "parse peer allowed ip",
                    message: source.to_string(),
                })?,
        ];
    wg_peer.endpoint = peer.endpoint.as_str().parse::<SocketAddr>().ok();
    wg_peer.persistent_keepalive_interval = Some(PERSISTENT_KEEPALIVE_SECS);
    Ok(wg_peer)
}

fn add_route(ifname: &str, cidr: &str) -> MeshResult<()> {
    let is_v6 = cidr.contains(':');
    let mut args = Vec::new();
    if is_v6 {
        args.push("-6");
    }
    args.extend(["route", "replace", cidr, "dev", ifname]);

    let output = Command::new("ip")
        .args(&args)
        .output()
        .map_err(|source| MeshError::Backend {
            operation: "add route",
            message: source.to_string(),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(MeshError::Backend {
        operation: "add route",
        message: String::from_utf8_lossy(&output.stderr).trim().to_string(),
    })
}

#[cfg(test)]
mod tests {
    use mvp_identity::NodeId;

    use crate::{IrohEndpointId, WireGuardOverlayIp, WireGuardPeer, WireGuardPrivateKey};

    use super::peer_to_wg_peer;

    #[test]
    fn peer_conversion_uses_key_allowed_ip_and_optional_endpoint() {
        let private_key = WireGuardPrivateKey::from_seed("peer");
        let peer = WireGuardPeer {
            node_id: NodeId::new("node-b"),
            public_key: private_key.public_key(),
            allowed_ip: WireGuardOverlayIp::new("fd00::2".parse().expect("ip")).allowed_ip_cidr(),
            endpoint: IrohEndpointId::new("127.0.0.1:51820"),
        };

        let converted = peer_to_wg_peer(&peer).expect("peer converts");

        assert_eq!(converted.allowed_ips.len(), 1);
        assert_eq!(
            converted.endpoint.map(|endpoint| endpoint.to_string()),
            Some("127.0.0.1:51820".to_string())
        );
    }

    #[test]
    fn peer_conversion_allows_non_socket_endpoint_hints() {
        let private_key = WireGuardPrivateKey::from_seed("peer");
        let peer = WireGuardPeer {
            node_id: NodeId::new("node-b"),
            public_key: private_key.public_key(),
            allowed_ip: WireGuardOverlayIp::new("fd00::2".parse().expect("ip")).allowed_ip_cidr(),
            endpoint: IrohEndpointId::new("iroh-node-b"),
        };

        let converted = peer_to_wg_peer(&peer).expect("peer converts");

        assert!(converted.endpoint.is_none());
    }
}
