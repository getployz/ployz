use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecResults};
use bollard::models::{ContainerCreateBody, HostConfig, RestartPolicy, RestartPolicyNameEnum};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use futures_util::StreamExt;
use std::path::Path;
use tracing::{info, warn};

use crate::error::{PortError, PortResult};
use crate::mesh::MeshNetwork;
use crate::store::model::{MachineRecord, OverlayIp, PrivateKey};

use super::config::{WgPaths, write_private_key, write_sync_config};

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
}

pub struct DockerWireGuardBuilder {
    container_name: String,
    image: String,
    data_dir: std::path::PathBuf,
    private_key: PrivateKey,
    overlay_ip: OverlayIp,
    listen_port: u16,
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

    pub async fn build(self) -> PortResult<DockerWireGuard> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| PortError::operation("docker connect", e.to_string()))?;

        docker
            .ping()
            .await
            .map_err(|e| PortError::operation("docker ping", e.to_string()))?;

        let paths = WgPaths::new(&self.data_dir);

        Ok(DockerWireGuard {
            docker,
            container_name: self.container_name,
            image: self.image,
            paths,
            private_key: self.private_key,
            overlay_ip: self.overlay_ip,
            listen_port: self.listen_port,
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
        }
    }

    async fn pull_image(&self) -> PortResult<()> {
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
                    return Err(PortError::operation("docker pull", e.to_string()));
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

    async fn exec_in_container(&self, cmd: &[&str]) -> PortResult<()> {
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
            .map_err(|e| PortError::operation("docker exec create", e.to_string()))?;

        let exec_id = exec.id.clone();

        match self
            .docker
            .start_exec(&exec.id, None)
            .await
            .map_err(|e| PortError::operation("docker exec start", e.to_string()))?
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
                            return Err(PortError::operation("docker exec", e.to_string()));
                        }
                        _ => {}
                    }
                }

                let inspect = self
                    .docker
                    .inspect_exec(&exec_id)
                    .await
                    .map_err(|e| PortError::operation("docker exec inspect", e.to_string()))?;

                if let Some(code) = inspect.exit_code {
                    if code != 0 {
                        let detail = if stderr_buf.is_empty() {
                            format!("exit code {code}")
                        } else {
                            format!("exit code {code}: {}", stderr_buf.trim())
                        };
                        return Err(PortError::operation("docker exec", detail));
                    }
                }
            }
            StartExecResults::Detached => {}
        }

        Ok(())
    }

    async fn setup_interface(&self) -> PortResult<()> {
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

        Ok(())
    }
}

impl MeshNetwork for DockerWireGuard {
    async fn up(&self) -> PortResult<()> {
        write_private_key(&self.paths, &self.private_key)
            .map_err(|e| PortError::operation("write private key", e.to_string()))?;

        if let Err(e) = self.pull_image().await {
            warn!(?e, image = %self.image, "pull failed, trying cached image");
        }

        self.remove_existing().await;

        let wg_dir = self.paths.dir.to_string_lossy().into_owned();

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
            ..Default::default()
        };

        let config = ContainerCreateBody {
            image: Some(self.image.clone()),
            cmd: Some(vec!["sleep".into(), "infinity".into()]),
            host_config: Some(host_config),
            ..Default::default()
        };

        let options = CreateContainerOptionsBuilder::default()
            .name(&self.container_name)
            .build();

        self.docker
            .create_container(Some(options), config)
            .await
            .map_err(|e| PortError::operation("docker create", e.to_string()))?;

        self.docker
            .start_container(&self.container_name, None)
            .await
            .map_err(|e| PortError::operation("docker start", e.to_string()))?;

        self.setup_interface().await?;

        info!(name = %self.container_name, "wireguard container started");
        Ok(())
    }

    async fn down(&self) -> PortResult<()> {
        let stop_opts = StopContainerOptionsBuilder::default().t(10).build();

        self.docker
            .stop_container(&self.container_name, Some(stop_opts))
            .await
            .map_err(|e| PortError::operation("docker stop", e.to_string()))?;

        let remove_opts = RemoveContainerOptionsBuilder::default().build();

        self.docker
            .remove_container(&self.container_name, Some(remove_opts))
            .await
            .map_err(|e| PortError::operation("docker remove", e.to_string()))?;

        info!(name = %self.container_name, "wireguard container stopped");
        Ok(())
    }

    async fn set_peers(&self, peers: &[MachineRecord]) -> PortResult<()> {
        write_sync_config(&self.paths, &self.private_key, self.listen_port, peers)
            .map_err(|e| PortError::operation("write sync config", e.to_string()))?;

        let sync_path = self.paths.sync_config.to_string_lossy().into_owned();
        self.exec_in_container(&["wg", "syncconf", INTERFACE_NAME, &sync_path])
            .await?;

        info!(peer_count = peers.len(), "synced wireguard peers");
        Ok(())
    }
}
