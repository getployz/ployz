use crate::error::{Error, Result};
use ployz_sdk::store::StoreRuntimeControl;
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
    #[must_use]
    pub fn cmd(mut self, cmd: Vec<String>) -> Self {
        self.cmd = Some(cmd);
        self
    }

    #[must_use]
    pub fn env(mut self, key: &str, value: &str) -> Self {
        self.env.push(format!("{key}={value}"));
        self
    }

    /// Add a volume/bind mount specification (e.g. `"/host/path:/container/path:ro"`
    /// or `"volume-name:/container/path"`).
    #[must_use]
    pub fn volume(mut self, spec: &str) -> Self {
        self.volumes.push(spec.to_string());
        self
    }

    #[must_use]
    pub fn network_mode(mut self, mode: &str) -> Self {
        self.network_mode = Some(mode.to_string());
        self
    }

    pub async fn build(self) -> Result<DockerCorrosion> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;

        // Verify the daemon is reachable.
        docker
            .ping()
            .await
            .map_err(|e| Error::operation("docker ping", e.to_string()))?;

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
    #[must_use]
    #[allow(clippy::new_ret_no_self)]
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

impl DockerCorrosion {
    /// Extract the parent container name from `network_mode` if it uses `container:X`.
    fn parent_container_name(&self) -> Option<&str> {
        self.network_mode
            .as_ref()
            .and_then(|m| m.strip_prefix("container:"))
    }

    /// Check if the existing container can be adopted (running + parent unchanged).
    async fn try_adopt(&self) -> bool {
        let Ok(info) = self
            .docker
            .inspect_container(&self.container_name, None)
            .await
        else {
            return false;
        };

        let is_running = info.state.as_ref().and_then(|s| s.running).unwrap_or(false);
        if !is_running {
            return false;
        }

        // If sharing a network namespace, verify the parent container is the same instance.
        if let Some(parent_name) = self.parent_container_name() {
            let labels = info
                .config
                .as_ref()
                .and_then(|c| c.labels.as_ref());
            let stored_parent = labels.and_then(|l| l.get(LABEL_PLOYZ_PARENT_ID));
            let current_parent = self
                .docker
                .inspect_container(parent_name, None)
                .await
                .ok()
                .and_then(|i| i.id);
            match (stored_parent, current_parent.as_deref()) {
                (Some(stored), Some(current)) if stored == current => {}
                _ => {
                    info!(
                        name = %self.container_name,
                        parent = %parent_name,
                        "parent container changed, recreating"
                    );
                    return false;
                }
            }
        }

        true
    }
}

const LABEL_PLOYZ_PARENT_ID: &str = "ployz.parent-container-id";

impl StoreRuntimeControl for DockerCorrosion {
    async fn start(&self) -> Result<()> {
        if self.try_adopt().await {
            info!(name = %self.container_name, "adopted existing container");
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

        let mut labels: std::collections::HashMap<String, String> = [
            ("com.docker.compose.project".into(), "ployz-system".into()),
            ("com.docker.compose.service".into(), "corrosion".into()),
        ]
        .into_iter()
        .collect();

        // Track parent container ID for future adopt checks.
        if let Some(parent_name) = self.parent_container_name()
            && let Some(parent_id) = self
                .docker
                .inspect_container(parent_name, None)
                .await
                .ok()
                .and_then(|i| i.id)
        {
            labels.insert(LABEL_PLOYZ_PARENT_ID.into(), parent_id);
        }

        let config = ContainerCreateBody {
            image: Some(self.image.clone()),
            cmd: self.cmd.clone(),
            env: if self.env.is_empty() {
                None
            } else {
                Some(self.env.clone())
            },
            labels: Some(labels),
            host_config: Some(host_config),
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

        info!(name = %self.container_name, image = %self.image, "container started");
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
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
