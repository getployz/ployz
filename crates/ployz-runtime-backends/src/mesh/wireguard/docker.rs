mod bridge_ops;
mod builder;
mod exec;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, RestartPolicy, RestartPolicyNameEnum};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, RemoveContainerOptionsBuilder, StopContainerOptionsBuilder,
};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
use crate::model::{OverlayIp, PrivateKey, PublicKey, WireGuardPeerSpec};

use super::DEFAULT_LISTEN_PORT;
use super::PERSISTENT_KEEPALIVE_SECS;
use super::bridge::{OutboundForward, OverlayBridge};
use super::config::{
    BridgePeerInfo, WgPaths, encode_key, write_private_key, write_sync_config_with_extra_peers,
};

const DEFAULT_IMAGE: &str = "ghcr.io/getployz/ployz-networking:latest";
const DEFAULT_MTU: u16 = 1420;
const INTERFACE_NAME: &str = "wg0";
const BRIDGE_HOST_LOOPBACK: &str = "127.0.0.1";

pub struct DockerWireGuard {
    pub(super) docker: Docker,
    pub(super) container_name: String,
    pub(super) image: String,
    pub(super) paths: WgPaths,
    pub(super) private_key: PrivateKey,
    pub(super) public_key_bytes: [u8; 32],
    pub(super) overlay_ip: OverlayIp,
    pub(super) listen_port: u16,
    pub(super) outbound_forwards: Vec<OutboundForward>,
    pub(super) exposed_tcp_ports: Vec<u16>,
    pub(super) bridge: Mutex<Option<OverlayBridge>>,
    pub(super) bridge_overlay_ip: Mutex<Option<OverlayIp>>,
    pub(super) extra_peers: Mutex<Vec<BridgePeerInfo>>,
}

pub struct DockerWireGuardBuilder {
    pub(super) container_name: String,
    pub(super) image: String,
    pub(super) data_dir: std::path::PathBuf,
    pub(super) private_key: PrivateKey,
    pub(super) overlay_ip: OverlayIp,
    pub(super) listen_port: u16,
    pub(super) outbound_forwards: Vec<OutboundForward>,
    pub(super) exposed_tcp_ports: Vec<u16>,
}

impl DockerWireGuard {
    #[must_use]
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        container_name: &str,
        data_dir: &Path,
        private_key: PrivateKey,
        overlay_ip: OverlayIp,
    ) -> DockerWireGuardBuilder {
        DockerWireGuardBuilder {
            container_name: container_name.to_string(),
            image: DEFAULT_IMAGE.to_string(),
            data_dir: data_dir.to_path_buf(),
            private_key,
            overlay_ip,
            listen_port: DEFAULT_LISTEN_PORT,
            outbound_forwards: Vec::new(),
            exposed_tcp_ports: Vec::new(),
        }
    }

    pub fn public_key_bytes(&self) -> &[u8; 32] {
        &self.public_key_bytes
    }

    pub fn container_name(&self) -> &str {
        &self.container_name
    }

    pub fn image(&self) -> &str {
        &self.image
    }
}

impl MeshNetwork for DockerWireGuard {
    async fn up(&self) -> Result<()> {
        write_private_key(&self.paths, &self.private_key)
            .map_err(|e| Error::operation("write private key", e.to_string()))?;

        if self.container_running().await && self.interface_ready().await {
            info!(name = %self.container_name, "adopting existing wireguard container");
            self.log_interface_diagnostics("adopt_existing_before_start_bridge")
                .await;
            self.start_bridge().await?;
            self.log_interface_diagnostics("adopt_existing_after_start_bridge")
                .await;
            info!(name = %self.container_name, "wireguard container adopted");
            return Ok(());
        }

        if let Err(e) = self.pull_image().await {
            warn!(?e, image = %self.image, "pull failed, trying cached image");
        }

        self.remove_existing().await;

        let wg_dir = self.paths.dir.to_string_lossy().into_owned();

        let host_config = HostConfig {
            binds: Some(vec![
                format!("{wg_dir}:{wg_dir}"),
                "/dev/net/tun:/dev/net/tun".to_string(),
                "/sys/fs/bpf:/sys/fs/bpf:rw".to_string(),
            ]),
            privileged: Some(true),
            pid_mode: Some("host".to_string()),
            sysctls: Some(
                [
                    (
                        "net.ipv4.conf.all.src_valid_mark".to_string(),
                        "1".to_string(),
                    ),
                    (
                        "net.ipv6.conf.all.disable_ipv6".to_string(),
                        "0".to_string(),
                    ),
                ]
                .into_iter()
                .collect(),
            ),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::ALWAYS),
                maximum_retry_count: None,
            }),
            port_bindings: Some(self.port_bindings()),
            ..Default::default()
        };

        let labels: HashMap<String, String> = [
            ("com.docker.compose.project".into(), "ployz-system".into()),
            ("com.docker.compose.service".into(), "wireguard".into()),
        ]
        .into_iter()
        .collect();

        let config = ContainerCreateBody {
            image: Some(self.image.clone()),
            cmd: Some(vec!["sleep".into(), "infinity".into()]),
            labels: Some(labels),
            host_config: Some(host_config),
            exposed_ports: Some({
                let mut ports = vec![format!("{}/udp", self.listen_port)];
                for &port in &self.exposed_tcp_ports {
                    ports.push(format!("{port}/tcp"));
                }
                ports
            }),
            ..Default::default()
        };

        let options = CreateContainerOptionsBuilder::default()
            .name(&self.container_name)
            .build();

        self.docker
            .create_container(Some(options), config)
            .await
            .map_err(|e| Error::operation("docker create", e.to_string()))?;

        self.docker
            .start_container(&self.container_name, None)
            .await
            .map_err(|e| Error::operation("docker start", e.to_string()))?;

        self.setup_interface().await?;
        self.log_interface_diagnostics("after_setup_interface")
            .await;
        self.start_bridge().await?;
        self.log_interface_diagnostics("after_start_bridge").await;

        info!(name = %self.container_name, "wireguard container started");
        Ok(())
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        *self.bridge_overlay_ip.lock().await
    }

    async fn down(&self) -> Result<()> {
        if let Some(bridge) = self.bridge.lock().await.take() {
            bridge.stop().await;
        }
        *self.bridge_overlay_ip.lock().await = None;
        self.extra_peers.lock().await.clear();

        let stop_opts = StopContainerOptionsBuilder::default().t(10).build();

        match self
            .docker
            .stop_container(&self.container_name, Some(stop_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304 | 404,
                ..
            }) => {}
            Err(e) => return Err(Error::operation("docker stop", e.to_string())),
        }

        let remove_opts = RemoveContainerOptionsBuilder::default().build();

        match self
            .docker
            .remove_container(&self.container_name, Some(remove_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => return Err(Error::operation("docker remove", e.to_string())),
        }

        info!(name = %self.container_name, "wireguard container stopped");
        Ok(())
    }

    async fn set_peers(&self, peers: &[WireGuardPeerSpec]) -> Result<()> {
        let extra = self.extra_peers.lock().await;
        let extra_refs: Vec<&BridgePeerInfo> = extra.iter().collect();
        write_sync_config_with_extra_peers(
            &self.paths,
            &self.private_key,
            self.listen_port,
            peers,
            &extra_refs,
        )
        .map_err(|e| Error::operation("write sync config", e.to_string()))?;

        let sync_path = self.paths.sync_config.to_string_lossy().into_owned();
        let syncconf_cmd: &[&str] = &["wg", "syncconf", INTERFACE_NAME, &sync_path];
        if let Err(first) = self.exec_in_container(syncconf_cmd).await {
            tokio::time::sleep(Duration::from_millis(150)).await;
            self.exec_in_container(syncconf_cmd)
                .await
                .map_err(|_| first)?;
        }

        let desired: HashSet<String> = peers
            .iter()
            .filter_map(|p| p.subnet.map(|s| s.to_string()))
            .collect();

        let current_output = self
            .exec_in_container_capture(&["ip", "route", "show", "dev", INTERFACE_NAME])
            .await
            .unwrap_or_default();
        let current: HashSet<String> = current_output
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|dest| dest.contains('/') && !dest.contains(':'))
            .map(|s| s.to_string())
            .collect();

        let src_ip = self
            .exec_in_container_capture(&[
                "sh",
                "-c",
                "ip -4 addr show eth1 | awk '/inet /{split($2,a,\"/\");print a[1]}'",
            ])
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        for subnet in &desired {
            let mut args = vec![
                "ip",
                "route",
                "replace",
                subnet.as_str(),
                "dev",
                INTERFACE_NAME,
            ];
            if let Some(ref src) = src_ip {
                args.extend(["src", src.as_str()]);
            }
            let _ = self.exec_in_container(&args).await;
        }
        for subnet in current.difference(&desired) {
            let _ = self
                .exec_in_container(&["ip", "route", "del", subnet, "dev", INTERFACE_NAME])
                .await;
        }

        debug!(peer_count = peers.len(), "synced wireguard peers");
        Ok(())
    }

    async fn has_remote_handshake(&self) -> bool {
        let output = match self
            .exec_in_container_capture(&["wg", "show", INTERFACE_NAME, "latest-handshakes"])
            .await
        {
            Ok(o) => o,
            Err(_) => return false,
        };

        let extra = self.extra_peers.lock().await;
        let local_keys: HashSet<String> = extra.iter().map(|p| encode_key(&p.public_key)).collect();

        for line in output.lines() {
            let Some((pubkey_raw, ts_raw)) = line.split_once('\t') else {
                continue;
            };
            let pubkey = pubkey_raw.trim();
            let ts = ts_raw.trim().parse::<u64>().unwrap_or(0);
            let is_local = local_keys.contains(pubkey);
            let short_key = &pubkey[..pubkey.len().min(8)];

            if is_local {
                tracing::trace!(key = short_key, ts, "skipping local peer");
            } else if ts > 0 {
                info!(key = short_key, ts, "remote peer handshake confirmed");
                return true;
            } else {
                tracing::debug!(key = short_key, "remote peer awaiting handshake");
            }
        }
        false
    }
}

impl WireGuardDevice for DockerWireGuard {
    async fn read_peers(&self) -> Result<Vec<DevicePeer>> {
        self.read_device_peers().await
    }

    async fn set_peer_endpoint<'a>(&'a self, key: &'a PublicKey, endpoint: &'a str) -> Result<()> {
        let key = encode_key(&key.0);
        self.exec_in_container(&[
            "wg",
            "set",
            INTERFACE_NAME,
            "peer",
            &key,
            "endpoint",
            endpoint,
        ])
        .await
    }
}

pub(super) use exec::{docker_exec_capture, docker_force_remove};

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    // Build a `Docker` handle that does not require `/var/run/docker.sock`.
    // The unix-socket constructors (`connect_with_socket_defaults`,
    // `connect_with_unix`) eagerly stat the socket and fail when it is
    // missing - which breaks pure-data tests in sandboxes/CI without a Docker
    // daemon. `connect_with_http` only builds an HTTP client and never
    // touches the network until a request is issued, so it works as a stub
    // for fixtures that store but never invoke the handle. Don't use this in
    // tests that actually exercise Docker requests.
    fn placeholder_docker() -> Docker {
        Docker::connect_with_http(
            "http://127.0.0.1:1",
            1,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("placeholder docker handle")
    }

    fn sample_wireguard() -> DockerWireGuard {
        let private_key = PrivateKey([7; 32]);
        let public_key_bytes =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private_key.0))
                .to_bytes();
        DockerWireGuard {
            docker: placeholder_docker(),
            container_name: "test-wireguard".to_string(),
            image: DEFAULT_IMAGE.to_string(),
            paths: WgPaths::new(Path::new("/tmp/ployz-test")),
            private_key,
            public_key_bytes,
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            listen_port: DEFAULT_LISTEN_PORT,
            outbound_forwards: Vec::new(),
            exposed_tcp_ports: Vec::new(),
            bridge: Mutex::new(None),
            bridge_overlay_ip: Mutex::new(None),
            extra_peers: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn bridge_peer_endpoint_uses_loopback() {
        let wireguard = sample_wireguard();
        assert_eq!(
            wireguard.bridge_peer_endpoint(),
            format!("{BRIDGE_HOST_LOOPBACK}:{DEFAULT_LISTEN_PORT}")
                .parse()
                .unwrap()
        );
    }

    #[test]
    fn udp_port_binding_uses_loopback() {
        let wireguard = sample_wireguard();
        let bindings = wireguard.port_bindings();
        let port = format!("{DEFAULT_LISTEN_PORT}/udp");
        let binding = bindings
            .get(&port)
            .and_then(|entry| entry.as_ref())
            .unwrap();
        let [binding] = binding.as_slice() else {
            panic!("expected one port binding");
        };
        let expected_port = DEFAULT_LISTEN_PORT.to_string();
        assert_eq!(binding.host_ip.as_deref(), Some(BRIDGE_HOST_LOOPBACK));
        assert_eq!(binding.host_port.as_deref(), Some(expected_port.as_str()));
    }
}
