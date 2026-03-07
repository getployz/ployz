use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{
    ContainerCreateBody, HostConfig, PortBinding, PortMap, RestartPolicy, RestartPolicyNameEnum,
};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use futures_util::StreamExt;
use std::net::SocketAddr;
use std::path::Path;
use tokio::sync::Mutex;
use tracing::{debug, info, warn};

use std::net::Ipv4Addr;

use crate::error::{Error, Result};
use crate::mesh::MeshNetwork;
use crate::model::{MachineRecord, OverlayIp, PrivateKey};

use super::bridge::{InboundForward, OutboundForward, OverlayBridge};
use super::config::{
    BridgePeerInfo, WgPaths, encode_key, write_private_key, write_sync_config_with_extra_peers,
};

const DEFAULT_IMAGE: &str = "ghcr.io/getployz/ployz-ebpf:latest";
use super::DEFAULT_LISTEN_PORT;
const DEFAULT_MTU: u16 = 1420;
const INTERFACE_NAME: &str = "wg0";
const BRIDGE_HOST_LOOPBACK: &str = "127.0.0.1";

pub struct DockerWireGuard {
    docker: Docker,
    container_name: String,
    image: String,
    paths: WgPaths,
    private_key: PrivateKey,
    public_key_bytes: [u8; 32],
    overlay_ip: OverlayIp,
    listen_port: u16,
    outbound_forwards: Vec<OutboundForward>,
    inbound_forwards: Vec<InboundForward>,
    bridge: Mutex<Option<OverlayBridge>>,
    bridge_overlay_ip: Mutex<Option<OverlayIp>>,
    extra_peers: Mutex<Vec<BridgePeerInfo>>,
}

pub struct DockerWireGuardBuilder {
    container_name: String,
    image: String,
    data_dir: std::path::PathBuf,
    private_key: PrivateKey,
    overlay_ip: OverlayIp,
    listen_port: u16,
    outbound_forwards: Vec<OutboundForward>,
    inbound_forwards: Vec<InboundForward>,
}

impl DockerWireGuardBuilder {
    #[must_use]
    pub fn image(mut self, image: &str) -> Self {
        self.image = image.to_string();
        self
    }

    #[must_use]
    pub fn listen_port(mut self, port: u16) -> Self {
        self.listen_port = port;
        self
    }

    /// Add an outbound forward rule (host TCP → overlay TCP via bridge).
    #[must_use]
    pub fn with_bridge_forward(mut self, local_addr: SocketAddr, overlay_dest: SocketAddr) -> Self {
        self.outbound_forwards.push(OutboundForward {
            local_addr,
            overlay_dest,
        });
        self
    }

    /// Add an inbound forward rule (overlay TCP → host TCP via bridge).
    #[must_use]
    pub fn with_inbound_forward(mut self, overlay_port: u16, local_dest: SocketAddr) -> Self {
        self.inbound_forwards.push(InboundForward {
            overlay_port,
            local_dest,
        });
        self
    }

    pub async fn build(self) -> Result<DockerWireGuard> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;

        docker
            .ping()
            .await
            .map_err(|e| Error::operation("docker ping", e.to_string()))?;

        let paths = WgPaths::new(&self.data_dir);

        let public_key_bytes = x25519_dalek::PublicKey::from(
            &x25519_dalek::StaticSecret::from(self.private_key.0),
        )
        .to_bytes();

        Ok(DockerWireGuard {
            docker,
            container_name: self.container_name,
            image: self.image,
            paths,
            private_key: self.private_key,
            public_key_bytes,
            overlay_ip: self.overlay_ip,
            listen_port: self.listen_port,
            outbound_forwards: self.outbound_forwards,
            inbound_forwards: self.inbound_forwards,
            bridge: Mutex::new(None),
            bridge_overlay_ip: Mutex::new(None),
            extra_peers: Mutex::new(Vec::new()),
        })
    }
}

impl DockerWireGuard {
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
            inbound_forwards: Vec::new(),
        }
    }

    async fn pull_image(&self) -> Result<()> {
        let (repo, tag) = match self.image.split_once(':') {
            Some((r, t)) => (r, t),
            None => (self.image.as_str(), "latest"),
        };

        let options = CreateImageOptionsBuilder::default()
            .from_image(repo)
            .tag(tag)
            .build();

        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        info!(image = %self.image, %status, "pulling");
                    }
                }
                Err(e) => {
                    return Err(Error::operation("docker pull", e.to_string()));
                }
            }
        }
        Ok(())
    }

    fn bridge_peer_endpoint(&self) -> SocketAddr {
        format!("{BRIDGE_HOST_LOOPBACK}:{}", self.listen_port)
            .parse()
            .expect("loopback bridge endpoint must parse")
    }

    fn udp_port_bindings(&self) -> PortMap {
        let mut port_bindings: PortMap = PortMap::new();
        let port_key = format!("{}/udp", self.listen_port);
        port_bindings.insert(
            port_key,
            Some(vec![PortBinding {
                host_ip: Some(BRIDGE_HOST_LOOPBACK.to_string()),
                host_port: Some(self.listen_port.to_string()),
            }]),
        );
        port_bindings
    }

    async fn container_running(&self) -> bool {
        match self
            .docker
            .inspect_container(&self.container_name, None)
            .await
        {
            Ok(info) => info.state.and_then(|s| s.running).unwrap_or(false),
            Err(_) => false,
        }
    }

    async fn remove_existing(&self) {
        let options = RemoveContainerOptionsBuilder::default().force(true).build();

        if let Err(e) = self
            .docker
            .remove_container(&self.container_name, Some(options))
            .await
        {
            if !matches!(
                e,
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404,
                    ..
                }
            ) {
                warn!(?e, name = %self.container_name, "failed to remove existing container");
            }
        }
    }

    async fn exec_in_container(&self, cmd: &[&str]) -> Result<()> {
        let exec = self
            .docker
            .create_exec(
                &self.container_name,
                CreateExecOptions::<String> {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| Error::operation("docker exec create", e.to_string()))?;

        let exec_id = exec.id.clone();

        match self
            .docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| Error::operation("docker exec start", e.to_string()))?
        {
            StartExecResults::Attached { mut output, .. } => {
                let mut stderr_buf = String::new();
                while let Some(result) = output.next().await {
                    match result {
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            stderr_buf.push_str(&String::from_utf8_lossy(&message));
                        }
                        Err(e) => {
                            return Err(Error::operation("docker exec", e.to_string()));
                        }
                        _ => {}
                    }
                }

                let inspect = self
                    .docker
                    .inspect_exec(&exec_id)
                    .await
                    .map_err(|e| Error::operation("docker exec inspect", e.to_string()))?;

                if let Some(code) = inspect.exit_code {
                    if code != 0 {
                        let detail = if stderr_buf.is_empty() {
                            format!("exit code {code}")
                        } else {
                            format!("exit code {code}: {}", stderr_buf.trim())
                        };
                        return Err(Error::operation("docker exec", detail));
                    }
                }
            }
            StartExecResults::Detached => {}
        }

        Ok(())
    }

    async fn exec_in_container_capture(&self, cmd: &[&str]) -> Result<String> {
        let exec = self
            .docker
            .create_exec(
                &self.container_name,
                CreateExecOptions::<String> {
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                    ..Default::default()
                },
            )
            .await
            .map_err(|e| Error::operation("docker exec create", e.to_string()))?;

        let exec_id = exec.id.clone();

        let mut stdout_buf = String::new();
        let mut stderr_buf = String::new();

        match self
            .docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| Error::operation("docker exec start", e.to_string()))?
        {
            StartExecResults::Attached { mut output, .. } => {
                while let Some(result) = output.next().await {
                    match result {
                        Ok(bollard::container::LogOutput::StdOut { message }) => {
                            stdout_buf.push_str(&String::from_utf8_lossy(&message));
                        }
                        Ok(bollard::container::LogOutput::StdErr { message }) => {
                            stderr_buf.push_str(&String::from_utf8_lossy(&message));
                        }
                        Err(e) => {
                            return Err(Error::operation("docker exec", e.to_string()));
                        }
                        _ => {}
                    }
                }

                let inspect = self
                    .docker
                    .inspect_exec(&exec_id)
                    .await
                    .map_err(|e| Error::operation("docker exec inspect", e.to_string()))?;

                if let Some(code) = inspect.exit_code
                    && code != 0
                {
                    let detail = if stderr_buf.is_empty() {
                        format!("exit code {code}")
                    } else {
                        format!("exit code {code}: {}", stderr_buf.trim())
                    };
                    return Err(Error::operation("docker exec", detail));
                }
            }
            StartExecResults::Detached => {}
        }

        Ok(stdout_buf)
    }

    async fn log_interface_diagnostics(&self, stage: &str) {
        let listen_port = self
            .exec_in_container_capture(&["wg", "show", INTERFACE_NAME, "listen-port"])
            .await;
        let peers = self
            .exec_in_container_capture(&["wg", "show", INTERFACE_NAME, "peers"])
            .await;
        let latest_handshakes = self
            .exec_in_container_capture(&["wg", "show", INTERFACE_NAME, "latest-handshakes"])
            .await;

        match (listen_port, peers, latest_handshakes) {
            (Ok(lp), Ok(ps), Ok(hs)) => {
                info!(
                    stage,
                    listen_port = lp.trim(),
                    peers = ps.trim(),
                    latest_handshakes = hs.trim(),
                    "wireguard diagnostics"
                );
            }
            (lp, ps, hs) => {
                warn!(
                    stage,
                    listen_port = ?lp.as_ref().map(|s| s.trim()),
                    peers = ?ps.as_ref().map(|s| s.trim()),
                    latest_handshakes = ?hs.as_ref().map(|s| s.trim()),
                    "wireguard diagnostics unavailable"
                );
            }
        }
    }

    async fn setup_interface(&self) -> Result<()> {
        let key_path = self.paths.private_key_file.to_string_lossy().into_owned();
        let overlay = format!("{}/128", self.overlay_ip.0);
        let port = self.listen_port.to_string();
        let mtu = DEFAULT_MTU.to_string();

        debug!(container = %self.container_name, "setup_interface: creating wireguard link");
        self.exec_in_container(&["ip", "link", "add", INTERFACE_NAME, "type", "wireguard"])
            .await?;

        debug!(container = %self.container_name, "setup_interface: setting mtu");
        self.exec_in_container(&["ip", "link", "set", INTERFACE_NAME, "mtu", &mtu])
            .await?;

        debug!(container = %self.container_name, "setup_interface: wg set private-key + listen-port");
        self.exec_in_container(&[
            "wg",
            "set",
            INTERFACE_NAME,
            "private-key",
            &key_path,
            "listen-port",
            &port,
        ])
        .await?;

        debug!(container = %self.container_name, "setup_interface: adding overlay address {overlay}");
        self.exec_in_container(&["ip", "addr", "add", &overlay, "dev", INTERFACE_NAME])
            .await?;

        debug!(container = %self.container_name, "setup_interface: bringing link up");
        self.exec_in_container(&["ip", "link", "set", INTERFACE_NAME, "up"])
            .await?;

        debug!(container = %self.container_name, "setup_interface: adding fd00::/8 route");
        self.exec_in_container(&[
            "ip",
            "-6",
            "route",
            "add",
            "fd00::/8",
            "dev",
            INTERFACE_NAME,
        ])
        .await?;

        // Note: IPv4 cluster CIDR route is NOT added here. In Docker mode, the bridge
        // network owns the local subnet (e.g. 10.210.0.0/24). A broad /16 route on the
        // WG interface conflicts with Docker's bridge assignment. Remote subnets are
        // reached via per-peer allowed-ips on the WG interface. The host-mode WG adds
        // the broad route since there's no Docker bridge conflict.

        Ok(())
    }

    async fn interface_ready(&self) -> bool {
        self.exec_in_container_capture(&["wg", "show", INTERFACE_NAME, "listen-port"])
            .await
            .is_ok()
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

    /// Add a sidecar as a WG peer on the backbone and register in extra_peers for syncconf protection.
    pub async fn add_sidecar_peer(&self, pubkey: [u8; 32], overlay_ip: Ipv4Addr) -> Result<()> {
        let pubkey_b64 = encode_key(&pubkey);
        let allowed = format!("{overlay_ip}/32");

        self.exec_in_container(&[
            "wg",
            "set",
            INTERFACE_NAME,
            "peer",
            &pubkey_b64,
            "allowed-ips",
            &allowed,
            "persistent-keepalive",
            "25",
        ])
        .await?;

        self.extra_peers.lock().await.push(BridgePeerInfo {
            public_key: pubkey,
            allowed_ips: vec![allowed],
        });

        info!(%overlay_ip, "added sidecar peer to backbone");
        Ok(())
    }

    /// Remove a sidecar peer from the backbone WG interface and extra_peers list.
    pub async fn remove_sidecar_peer(&self, pubkey: &[u8; 32]) -> Result<()> {
        let pubkey_b64 = encode_key(pubkey);

        self.exec_in_container(&[
            "wg",
            "set",
            INTERFACE_NAME,
            "peer",
            &pubkey_b64,
            "remove",
        ])
        .await?;

        self.extra_peers
            .lock()
            .await
            .retain(|p| &p.public_key != pubkey);

        info!("removed sidecar peer from backbone");
        Ok(())
    }

    /// Start the overlay bridge and register it as a WG peer on the container.
    async fn start_bridge(&self) -> Result<()> {
        if self.outbound_forwards.is_empty() && self.inbound_forwards.is_empty() {
            return Ok(());
        }

        if self.bridge.lock().await.is_some() {
            info!(name = %self.container_name, "bridge already running");
            return Ok(());
        }

        let container_pubkey_bytes = self.public_key_bytes;

        // Generate bridge keypair BEFORE starting, so we can register the peer first.
        // The container must know the bridge peer before the handshake arrives.
        let (bridge_secret, bridge_pub_bytes, bridge_overlay_ip) =
            OverlayBridge::generate_keypair();

        let bridge_pubkey_b64 = encode_key(&bridge_pub_bytes);
        let bridge_allowed = format!("{}/128", bridge_overlay_ip.0);

        // Register bridge peer on container BEFORE starting bridge event loop
        self.exec_in_container(&[
            "wg",
            "set",
            INTERFACE_NAME,
            "peer",
            &bridge_pubkey_b64,
            "allowed-ips",
            &bridge_allowed,
            "persistent-keepalive",
            "25",
        ])
        .await?;

        info!(bridge_ip = %bridge_overlay_ip, "registered bridge as WG peer");

        // Now start the bridge — handshake will find the peer already registered
        let peer_endpoint = self.bridge_peer_endpoint();

        let bridge = OverlayBridge::start(
            bridge_secret,
            &container_pubkey_bytes,
            self.overlay_ip,
            peer_endpoint,
            self.outbound_forwards.clone(),
            self.inbound_forwards.clone(),
        )
        .await
        .map_err(|e| Error::operation("bridge start", e.to_string()))?;

        let peer_info = BridgePeerInfo {
            public_key: bridge_pub_bytes,
            allowed_ips: vec![bridge_allowed],
        };

        self.extra_peers.lock().await.push(peer_info);
        *self.bridge_overlay_ip.lock().await = Some(bridge_overlay_ip);
        *self.bridge.lock().await = Some(bridge);

        Ok(())
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

        // Port bindings: publish WG UDP port for bridge access
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
            port_bindings: Some(self.udp_port_bindings()),
            ..Default::default()
        };

        let labels: std::collections::HashMap<String, String> = [
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
            exposed_ports: Some(vec![format!("{}/udp", self.listen_port)]),
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

        // Start overlay bridge after WG interface is up
        self.start_bridge().await?;
        self.log_interface_diagnostics("after_start_bridge").await;

        info!(name = %self.container_name, "wireguard container started");
        Ok(())
    }

    async fn bridge_ip(&self) -> Option<OverlayIp> {
        *self.bridge_overlay_ip.lock().await
    }

    async fn down(&self) -> Result<()> {
        // Stop bridge first
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
                status_code: 404,
                ..
            }) => {}
            Err(e) => return Err(Error::operation("docker remove", e.to_string())),
        }

        info!(name = %self.container_name, "wireguard container stopped");
        Ok(())
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        // Include extra peers (bridge + sidecars) in sync config to protect them from syncconf removal
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
        self.exec_in_container(&["wg", "syncconf", INTERFACE_NAME, &sync_path])
            .await?;

        // Sync per-peer subnet routes with src= set to our bridge IP (eth1)
        // so outbound IPv4 has a routable source address in the overlay.
        let desired: std::collections::HashSet<String> = peers
            .iter()
            .filter_map(|p| p.subnet.map(|s| s.to_string()))
            .collect();

        let current_output = self
            .exec_in_container_capture(&["ip", "route", "show", "dev", INTERFACE_NAME])
            .await
            .unwrap_or_default();
        let current: std::collections::HashSet<String> = current_output
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|dest| dest.contains('/') && !dest.contains(':'))
            .map(|s| s.to_string())
            .collect();

        // Get our bridge IP (eth1) for use as route src
        let src_ip = self
            .exec_in_container_capture(&["sh", "-c", "ip -4 addr show eth1 | awk '/inet /{split($2,a,\"/\");print a[1]}'"])
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Always replace desired routes (idempotent) to ensure src is set
        for subnet in &desired {
            let mut args = vec!["ip", "route", "replace", subnet.as_str(), "dev", INTERFACE_NAME];
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

        info!(peer_count = peers.len(), "synced wireguard peers");
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

        // Collect extra_peers pubkeys (bridge + sidecars) — these handshake locally
        // and must be excluded from the remote handshake check.
        let extra = self.extra_peers.lock().await;
        let local_keys: std::collections::HashSet<String> =
            extra.iter().map(|p| encode_key(&p.public_key)).collect();

        // Output format: "<pubkey>\t<unix_timestamp>\n" per peer. Timestamp 0 = no handshake.
        for line in output.lines() {
            let mut parts = line.split('\t');
            let pubkey = parts.next().unwrap_or("").trim();
            let ts = parts
                .next()
                .and_then(|ts| ts.trim().parse::<u64>().ok())
                .unwrap_or(0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    fn sample_wireguard() -> DockerWireGuard {
        let private_key = PrivateKey([7; 32]);
        let public_key_bytes = x25519_dalek::PublicKey::from(
            &x25519_dalek::StaticSecret::from(private_key.0),
        )
        .to_bytes();
        DockerWireGuard {
            docker: Docker::connect_with_socket_defaults().expect("docker client"),
            container_name: "test-wireguard".to_string(),
            image: DEFAULT_IMAGE.to_string(),
            paths: WgPaths::new(Path::new("/tmp/ployz-test")),
            private_key,
            public_key_bytes,
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            listen_port: DEFAULT_LISTEN_PORT,
            outbound_forwards: Vec::new(),
            inbound_forwards: Vec::new(),
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
        let bindings = wireguard.udp_port_bindings();
        let port = format!("{DEFAULT_LISTEN_PORT}/udp");
        let binding = bindings
            .get(&port)
            .and_then(|entry| entry.as_ref())
            .unwrap();
        let expected_port = DEFAULT_LISTEN_PORT.to_string();
        assert_eq!(binding.len(), 1);
        assert_eq!(binding[0].host_ip.as_deref(), Some(BRIDGE_HOST_LOOPBACK));
        assert_eq!(
            binding[0].host_port.as_deref(),
            Some(expected_port.as_str())
        );
    }
}
