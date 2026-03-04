use crate::dataplane::traits::{PortError, PortResult, ServiceControl};
use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use futures_util::StreamExt;
use tracing::{info, warn};

pub struct DockerCorrosion {
    docker: Docker,
    container_name: String,
    image: String,
    cmd: Option<Vec<String>>,
    env: Vec<String>,
    volumes: Vec<String>,
    network_mode: Option<String>,
}

pub struct DockerCorrosionBuilder {
    container_name: String,
    image: String,
    cmd: Option<Vec<String>>,
    env: Vec<String>,
    volumes: Vec<String>,
    network_mode: Option<String>,
}

impl DockerCorrosionBuilder {
    pub fn cmd(mut self, cmd: Vec<String>) -> Self {
        self.cmd = Some(cmd);
        self
    }

    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push(format!("{key}={value}"));
        self
    }

    pub fn volume(mut self, host: &str, container: &str) -> Self {
        self.volumes.push(format!("{host}:{container}"));
        self
    }

    pub fn network_mode(mut self, mode: &str) -> Self {
        self.network_mode = Some(mode.to_string());
        self
    }

    pub async fn build(self) -> PortResult<DockerCorrosion> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| PortError::operation("docker connect", e.to_string()))?;

        // Verify the daemon is reachable.
        docker
            .ping()
            .await
            .map_err(|e| PortError::operation("docker ping", e.to_string()))?;

        Ok(DockerCorrosion {
            docker,
            container_name: self.container_name,
            image: self.image,
            cmd: self.cmd,
            env: self.env,
            volumes: self.volumes,
            network_mode: self.network_mode,
        })
    }
}

impl DockerCorrosion {
    pub fn new(container_name: &str, image: &str) -> DockerCorrosionBuilder {
        DockerCorrosionBuilder {
            container_name: container_name.to_string(),
            image: image.to_string(),
            cmd: None,
            env: Vec::new(),
            volumes: Vec::new(),
            network_mode: None,
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
        let options = RemoveContainerOptionsBuilder::default().force(true).build();

        if let Err(e) = self
            .docker
            .remove_container(&self.container_name, Some(options))
            .await
        {
            // 404 (not found) is expected — anything else is worth logging.
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
}

impl ServiceControl for DockerCorrosion {
    async fn start(&self) -> PortResult<()> {
        // Already running — nothing to do.
        if self.healthy().await {
            info!(name = %self.container_name, "container already running");
            return Ok(());
        }

        // Best-effort pull — if it fails and the image is cached, creation will still work.
        if let Err(e) = self.pull_image().await {
            warn!(?e, image = %self.image, "pull failed, trying cached image");
        }

        self.remove_existing().await;

        let host_config = HostConfig {
            binds: if self.volumes.is_empty() {
                None
            } else {
                Some(self.volumes.clone())
            },
            network_mode: self.network_mode.clone(),
            ..Default::default()
        };

        let config = ContainerCreateBody {
            image: Some(self.image.clone()),
            cmd: self.cmd.clone(),
            env: if self.env.is_empty() {
                None
            } else {
                Some(self.env.clone())
            },
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

        info!(name = %self.container_name, image = %self.image, "container started");
        Ok(())
    }

    async fn stop(&self) -> PortResult<()> {
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

        info!(name = %self.container_name, "container stopped and removed");
        Ok(())
    }

    async fn healthy(&self) -> bool {
        match self
            .docker
            .inspect_container(&self.container_name, None)
            .await
        {
            Ok(info) => info.state.and_then(|s| s.running).unwrap_or(false),
            Err(_) => false,
        }
    }
}
