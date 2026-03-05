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
use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::mesh::MeshNetwork;
use crate::model::{MachineRecord, OverlayIp, PrivateKey};

use super::bridge::{InboundForward, OutboundForward, OverlayBridge};
use super::config::{
    BridgePeerInfo, WgPaths, encode_key, write_private_key, write_sync_config_with_bridge,
};

const DEFAULT_IMAGE: &str = "procustodibus/wireguard:latest";
const DEFAULT_LISTEN_PORT: u16 = 51820;
const DEFAULT_MTU: u16 = 1420;
const INTERFACE_NAME: &str = "wg0";

pub struct DockerWireGuard {
    docker: Docker,
    container_name: String,
    image: String,
    paths: WgPaths,
    private_key: PrivateKey,
    overlay_ip: OverlayIp,
    listen_port: u16,
    outbound_forwards: Vec<OutboundForward>,
    inbound_forwards: Vec<InboundForward>,
    bridge: Mutex<Option<OverlayBridge>>,
    bridge_peer: Mutex<Option<BridgePeerInfo>>,
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
    pub fn image(mut self, image: &str) -> Self {
        self.image = image.to_string();
        self
    }

    pub fn listen_port(mut self, port: u16) -> Self {
        self.listen_port = port;
        self
    }

    /// Add an outbound forward rule (host TCP → overlay TCP via bridge).
    pub fn with_bridge_forward(mut self, local_addr: SocketAddr, overlay_dest: SocketAddr) -> Self {
        self.outbound_forwards.push(OutboundForward {
            local_addr,
            overlay_dest,
        });
        self
    }

    /// Add an inbound forward rule (overlay TCP → host TCP via bridge).
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

        Ok(DockerWireGuard {
            docker,
            container_name: self.container_name,
            image: self.image,
            paths,
            private_key: self.private_key,
            overlay_ip: self.overlay_ip,
            listen_port: self.listen_port,
            outbound_forwards: self.outbound_forwards,
            inbound_forwards: self.inbound_forwards,
            bridge: Mutex::new(None),
            bridge_peer: Mutex::new(None),
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

    async fn remove_existing(&self) {
        let options = RemoveContainerOptionsBuilder::default()
            .force(true)
            .build();

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
                            stderr_buf
                                .push_str(&String::from_utf8_lossy(&message));
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

    async fn setup_interface(&self) -> Result<()> {
        let key_path = self.paths.private_key_file.to_string_lossy().into_owned();
        let overlay = format!("{}/128", self.overlay_ip.0);
        let port = self.listen_port.to_string();
        let mtu = DEFAULT_MTU.to_string();

        self.exec_in_container(&["ip", "link", "add", INTERFACE_NAME, "type", "wireguard"])
            .await?;

        self.exec_in_container(&["ip", "link", "set", INTERFACE_NAME, "mtu", &mtu])
            .await?;

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

        self.exec_in_container(&["ip", "addr", "add", &overlay, "dev", INTERFACE_NAME])
            .await?;

        self.exec_in_container(&["ip", "link", "set", INTERFACE_NAME, "up"])
            .await?;

        // Route the entire overlay prefix (fd00::/8 ULA) through the WG interface.
        // All overlay IPs are derived from fd00::/8 by management_ip_from_key(),
        // so a single route covers all peers — no per-peer route management needed.
        self.exec_in_container(&[
            "ip", "-6", "route", "add", "fd00::/8", "dev", INTERFACE_NAME,
        ])
        .await?;

        Ok(())
    }

    /// Start the overlay bridge and register it as a WG peer on the container.
    async fn start_bridge(&self) -> Result<()> {
        if self.outbound_forwards.is_empty() && self.inbound_forwards.is_empty() {
            return Ok(());
        }

        let container_pubkey = x25519_dalek::PublicKey::from(
            &x25519_dalek::StaticSecret::from(self.private_key.0),
        );
        let container_pubkey_bytes = container_pubkey.to_bytes();

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
        let peer_endpoint: SocketAddr = format!("127.0.0.1:{}", self.listen_port)
            .parse()
            .unwrap();

        let bridge = OverlayBridge::start(
            bridge_secret,
            &container_pubkey_bytes,
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

        *self.bridge_peer.lock().await = Some(peer_info);
        *self.bridge.lock().await = Some(bridge);

        Ok(())
    }

}

impl MeshNetwork for DockerWireGuard {
    async fn up(&self) -> Result<()> {
        write_private_key(&self.paths, &self.private_key)
            .map_err(|e| Error::operation("write private key", e.to_string()))?;

        if let Err(e) = self.pull_image().await {
            warn!(?e, image = %self.image, "pull failed, trying cached image");
        }

        self.remove_existing().await;

        let wg_dir = self.paths.dir.to_string_lossy().into_owned();

        // Port bindings: publish WG UDP port for bridge access
        let mut port_bindings: PortMap = PortMap::new();
        let port_key = format!("{}/udp", self.listen_port);
        port_bindings.insert(
            port_key,
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(self.listen_port.to_string()),
            }]),
        );

        let host_config = HostConfig {
            binds: Some(vec![
                format!("{wg_dir}:{wg_dir}"),
                "/dev/net/tun:/dev/net/tun".to_string(),
            ]),
            cap_add: Some(vec!["NET_ADMIN".to_string()]),
            sysctls: Some(
                [("net.ipv4.conf.all.src_valid_mark".to_string(), "1".to_string())]
                    .into_iter()
                    .collect(),
            ),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::ALWAYS),
                maximum_retry_count: None,
            }),
            port_bindings: Some(port_bindings),
            ..Default::default()
        };

        let config = ContainerCreateBody {
            image: Some(self.image.clone()),
            cmd: Some(vec!["sleep".into(), "infinity".into()]),
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

        // Start overlay bridge after WG interface is up
        self.start_bridge().await?;

        info!(name = %self.container_name, "wireguard container started");
        Ok(())
    }

    async fn down(&self) -> Result<()> {
        // Stop bridge first
        if let Some(bridge) = self.bridge.lock().await.take() {
            bridge.stop().await;
        }
        *self.bridge_peer.lock().await = None;

        let stop_opts = StopContainerOptionsBuilder::default().t(10).build();

        self.docker
            .stop_container(&self.container_name, Some(stop_opts))
            .await
            .map_err(|e| Error::operation("docker stop", e.to_string()))?;

        let remove_opts = RemoveContainerOptionsBuilder::default().build();

        self.docker
            .remove_container(&self.container_name, Some(remove_opts))
            .await
            .map_err(|e| Error::operation("docker remove", e.to_string()))?;

        info!(name = %self.container_name, "wireguard container stopped");
        Ok(())
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> Result<()> {
        // Include bridge peer in sync config to protect it from being removed by syncconf
        let bridge_peer = self.bridge_peer.lock().await;
        write_sync_config_with_bridge(
            &self.paths,
            &self.private_key,
            self.listen_port,
            peers,
            bridge_peer.as_ref(),
        )
        .map_err(|e| Error::operation("write sync config", e.to_string()))?;

        let sync_path = self.paths.sync_config.to_string_lossy().into_owned();
        self.exec_in_container(&["wg", "syncconf", INTERFACE_NAME, &sync_path])
            .await?;

        info!(peer_count = peers.len(), "synced wireguard peers");
        Ok(())
    }
}
