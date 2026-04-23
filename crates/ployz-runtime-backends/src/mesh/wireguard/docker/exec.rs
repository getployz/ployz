use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{PortBinding, PortMap};
use bollard::query_parameters::{CreateImageOptionsBuilder, RemoveContainerOptionsBuilder};
use futures_util::StreamExt;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::mesh::DevicePeer;
use crate::model::PublicKey;
use crate::runtime::parse_docker_image_ref;

use super::{BRIDGE_HOST_LOOPBACK, DEFAULT_MTU, DockerWireGuard, INTERFACE_NAME};
use crate::mesh::wireguard::config::decode_key;

impl DockerWireGuard {
    pub(super) async fn pull_image(&self) -> Result<()> {
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

    pub(super) fn bridge_peer_endpoint(&self) -> SocketAddr {
        SocketAddr::new(std::net::Ipv4Addr::LOCALHOST.into(), self.listen_port)
    }

    pub(super) fn port_bindings(&self) -> PortMap {
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

    pub(super) async fn container_running(&self) -> bool {
        match self
            .docker
            .inspect_container(&self.container_name, None)
            .await
        {
            Ok(info) => info.state.and_then(|s| s.running).unwrap_or(false),
            Err(_) => false,
        }
    }

    pub(super) async fn remove_existing(&self) {
        docker_force_remove(&self.docker, &self.container_name).await;
    }

    pub(super) async fn exec_in_container(&self, cmd: &[&str]) -> Result<()> {
        self.exec_in_container_capture(cmd).await.map(|_| ())
    }

    pub(super) async fn exec_in_container_capture(&self, cmd: &[&str]) -> Result<String> {
        docker_exec_capture(&self.docker, &self.container_name, cmd, "docker exec").await
    }

    pub(super) async fn log_interface_diagnostics(&self, stage: &str) {
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

    pub(super) async fn read_device_peers(&self) -> Result<Vec<DevicePeer>> {
        let output = self
            .exec_in_container_capture(&["wg", "show", INTERFACE_NAME, "latest-handshakes"])
            .await?;

        output
            .lines()
            .map(parse_device_peer_line)
            .collect::<Result<Vec<_>>>()
    }

    pub(super) async fn setup_interface(&self) -> Result<()> {
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

        Ok(())
    }

    pub(super) async fn interface_ready(&self) -> bool {
        self.exec_in_container_capture(&["wg", "show", INTERFACE_NAME, "listen-port"])
            .await
            .is_ok()
    }
}

pub(crate) async fn docker_force_remove(docker: &Docker, container_name: &str) {
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

pub(crate) async fn docker_exec_capture(
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
