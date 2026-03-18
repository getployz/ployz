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
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing::{debug, info, warn};

use std::net::Ipv4Addr;

use crate::error::{Error, Result};
use crate::mesh::{DevicePeer, MeshNetwork, WireGuardDevice};
use crate::model::{MachineRecord, OverlayIp, PrivateKey, PublicKey};
use crate::runtime::parse_docker_image_ref;

use super::PERSISTENT_KEEPALIVE_SECS;
use super::bridge::{OutboundForward, OverlayBridge};
use super::config::{
    BridgePeerInfo, WgPaths, decode_key, encode_key, write_private_key,
    write_sync_config_with_extra_peers,
};

const DEFAULT_IMAGE: &str = "ghcr.io/getployz/ployz-networking:latest";
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
    exposed_tcp_ports: Vec<u16>,
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
    exposed_tcp_ports: Vec<u16>,
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

    /// Expose a TCP port on the container (for sidecar containers sharing this netns).
    #[must_use]
    pub fn expose_tcp(mut self, port: u16) -> Self {
        self.exposed_tcp_ports.push(port);
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

        let public_key_bytes =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(self.private_key.0))
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
            exposed_tcp_ports: self.exposed_tcp_ports,
            bridge: Mutex::new(None),
            bridge_overlay_ip: Mutex::new(None),
            extra_peers: Mutex::new(Vec::new()),
        })
    }
}

impl DockerWireGuard {
    #[must_use]
    pub fn builder(
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

    async fn pull_image(&self) -> Result<()> {
        let parsed = parse_docker_image_ref(&self.image);
        let builder = CreateImageOptionsBuilder::default().from_image(parsed.from_image);
        let options = match parsed.tag {
            Some(tag) => builder.tag(tag).build(),
            None => builder.build(),
        };

        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            match result {
                Ok(info) => {
                    if let Some(status) = info.status {
                        info!(image = %self.image, %status, "pulling");
                    }
                }
                Err(e) => {
                    warn!(?e, image = %self.image, "pull failed, trying cached image");
                    break;
                }
            }
        }
        Ok(())
    }

    fn bridge_peer_endpoint(&self) -> SocketAddr {
        SocketAddr::new(Ipv4Addr::LOCALHOST.into(), self.listen_port)
    }

    fn port_bindings(&self) -> PortMap {
        let mut port_bindings: PortMap = PortMap::new();
        let port_key = format!("{}/udp", self.listen_port);
        port_bindings.insert(
            port_key,
            Some(vec![PortBinding {
                host_ip: Some(BRIDGE_HOST_LOOPBACK.to_string()),
                host_port: Some(self.listen_port.to_string()),
            }]),
        );
        for &port in &self.exposed_tcp_ports {
            let key = format!("{port}/tcp");
            port_bindings.insert(
                key,
                Some(vec![PortBinding {
                    host_ip: None,
                    host_port: Some(port.to_string()),
                }]),
            );
        }
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
        docker_force_remove(&self.docker, &self.container_name).await;
    }

    async fn exec_in_container(&self, cmd: &[&str]) -> Result<()> {
        self.exec_in_container_capture(cmd).await.map(|_| ())
    }

    async fn exec_in_container_capture(&self, cmd: &[&str]) -> Result<String> {
        docker_exec_capture(&self.docker, &self.container_name, cmd, "docker exec").await
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

    async fn read_device_peers(&self) -> Result<Vec<DevicePeer>> {
        let output = self
            .exec_in_container_capture(&["wg", "show", INTERFACE_NAME, "latest-handshakes"])
            .await?;

        output
            .lines()
            .map(parse_device_peer_line)
            .collect::<Result<Vec<_>>>()
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
        let keepalive_secs = PERSISTENT_KEEPALIVE_SECS.to_string();

        self.exec_in_container(&[
            "wg",
            "set",
            INTERFACE_NAME,
            "peer",
            &pubkey_b64,
            "allowed-ips",
            &allowed,
            "persistent-keepalive",
            &keepalive_secs,
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

        self.exec_in_container(&["wg", "set", INTERFACE_NAME, "peer", &pubkey_b64, "remove"])
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
        if self.outbound_forwards.is_empty() {
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
        let keepalive_secs = PERSISTENT_KEEPALIVE_SECS.to_string();

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
            &keepalive_secs,
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

/// Force-remove a Docker container, ignoring 404 (already gone).
///
/// Shared between `DockerWireGuard` and `WgSidecar`.
pub(super) async fn docker_force_remove(docker: &Docker, container_name: &str) {
    let options = RemoveContainerOptionsBuilder::default().force(true).build();
    if let Err(e) = docker.remove_container(container_name, Some(options)).await
        && !matches!(
            e,
            bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            }
        )
    {
        warn!(?e, name = %container_name, "failed to remove existing container");
    }
}

/// Execute a command inside a Docker container and capture stdout.
///
/// Shared between `DockerWireGuard` and `WgSidecar` to avoid duplicating
/// the exec + inspect + exit-code-check boilerplate.
pub(super) async fn docker_exec_capture(
    docker: &Docker,
    container_name: &str,
    cmd: &[&str],
    operation: &'static str,
) -> Result<String> {
    let exec = docker
        .create_exec(
            container_name,
            CreateExecOptions::<String> {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(cmd.iter().map(|s| s.to_string()).collect()),
                ..Default::default()
            },
        )
        .await
        .map_err(|e| Error::operation(operation, format!("create exec: {e}")))?;

    let exec_id = exec.id.clone();

    let mut stdout_buf = String::new();
    let mut stderr_buf = String::new();

    match docker
        .start_exec(&exec.id, None)
        .await
        .map_err(|e| Error::operation(operation, format!("start exec: {e}")))?
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
                        return Err(Error::operation(operation, e.to_string()));
                    }
                    _ => {}
                }
            }

            let inspect = docker
                .inspect_exec(&exec_id)
                .await
                .map_err(|e| Error::operation(operation, format!("inspect exec: {e}")))?;

            if let Some(code) = inspect.exit_code
                && code != 0
            {
                let detail = if stderr_buf.is_empty() {
                    format!("exit code {code}")
                } else {
                    format!("exit code {code}: {}", stderr_buf.trim())
                };
                return Err(Error::operation(operation, detail));
            }
        }
        StartExecResults::Detached => {}
    }

    Ok(stdout_buf)
}

fn unix_seconds_to_instant(seconds: u64) -> Option<Instant> {
    let timestamp = UNIX_EPOCH.checked_add(Duration::from_secs(seconds))?;
    let elapsed = SystemTime::now().duration_since(timestamp).ok()?;
    Instant::now().checked_sub(elapsed)
}

fn parse_device_peer_line(line: &str) -> Result<DevicePeer> {
    let Some((key_b64, handshake_secs)) = line.split_once('\t') else {
        return Err(Error::operation(
            "docker wireguard read_peers",
            format!("invalid latest-handshakes line: {line:?}"),
        ));
    };

    let public_key = PublicKey(
        decode_key(key_b64).map_err(|e| Error::operation("docker wireguard read_peers", e))?,
    );
    let handshake = handshake_secs
        .trim()
        .parse::<u64>()
        .map_err(|e| Error::operation("docker wireguard read_peers", e.to_string()))?;

    Ok(DevicePeer {
        public_key,
        endpoint: None,
        last_handshake: if handshake == 0 {
            None
        } else {
            unix_seconds_to_instant(handshake)
        },
    })
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
            port_bindings: Some(self.port_bindings()),
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
                status_code: 404, ..
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
        let syncconf_cmd: &[&str] = &["wg", "syncconf", INTERFACE_NAME, &sync_path];
        // Retry once after a short delay — on macOS Docker Desktop, VirtioFS
        // can take a moment to propagate host-written files into the container.
        if let Err(first) = self.exec_in_container(syncconf_cmd).await {
            tokio::time::sleep(Duration::from_millis(150)).await;
            self.exec_in_container(syncconf_cmd)
                .await
                .map_err(|_| first)?;
        }

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
            .exec_in_container_capture(&[
                "sh",
                "-c",
                "ip -4 addr show eth1 | awk '/inet /{split($2,a,\"/\");print a[1]}'",
            ])
            .await
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Always replace desired routes (idempotent) to ensure src is set
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

        // Collect extra_peers pubkeys (bridge + sidecars) — these handshake locally
        // and must be excluded from the remote handshake check.
        let extra = self.extra_peers.lock().await;
        let local_keys: std::collections::HashSet<String> =
            extra.iter().map(|p| encode_key(&p.public_key)).collect();

        // Output format: "<pubkey>\t<unix_timestamp>\n" per peer. Timestamp 0 = no handshake.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;

    fn sample_wireguard() -> DockerWireGuard {
        let private_key = PrivateKey([7; 32]);
        let public_key_bytes =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private_key.0))
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
