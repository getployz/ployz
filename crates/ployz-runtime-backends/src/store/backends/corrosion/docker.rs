use crate::error::Result;
use crate::runtime::labels::build_system_labels;
use crate::runtime::{ContainerEngine, EnsureAction, PullPolicy, RuntimeContainerSpec};
use ployz_types::store::StoreRuntimeControl;
use std::time::Duration;
use tracing::info;

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

pub struct DockerCorrosion {
    engine: ContainerEngine,
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
    #[allow(dead_code)]
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
        let engine = ContainerEngine::connect().await?;

        Ok(DockerCorrosion {
            engine,
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

    fn to_runtime_spec(&self) -> RuntimeContainerSpec {
        let key = "system/corrosion".to_string();

        let env: Vec<(String, String)> = self
            .env
            .iter()
            .map(|s| match s.split_once('=') {
                Some((k, v)) => (k.to_string(), v.to_string()),
                None => (s.clone(), String::new()),
            })
            .collect();

        let labels = build_system_labels(&key, None);

        RuntimeContainerSpec {
            key,
            container_name: self.container_name.clone(),
            image: self.image.clone(),
            pull_policy: PullPolicy::IfNotPresent,
            cmd: self.cmd.clone(),
            env,
            labels,
            binds: self.volumes.clone(),
            network_mode: self.network_mode.clone(),
            ..Default::default()
        }
    }
}

impl StoreRuntimeControl for DockerCorrosion {
    async fn start(&self) -> Result<()> {
        let spec = self.to_runtime_spec();

        let result = self.engine.ensure(&spec).await?;

        match &result.action {
            EnsureAction::Adopted => {
                info!(name = %self.container_name, "adopted existing container");
            }
            EnsureAction::Created => {
                info!(name = %self.container_name, image = %self.image, "container started");
            }
            EnsureAction::Recreated { changed } => {
                info!(
                    name = %self.container_name,
                    image = %self.image,
                    changed = ?changed,
                    "container recreated"
                );
            }
        }

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        self.engine
            .remove(&self.container_name, STOP_GRACE_PERIOD)
            .await
    }

    async fn healthy(&self) -> bool {
        match self.engine.inspect(&self.container_name).await {
            Ok(Some(observed)) => observed.running,
            Ok(None) => false,
            Err(_) => false,
        }
    }
}
