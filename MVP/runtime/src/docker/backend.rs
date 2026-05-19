use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::io::Write;
use std::net::{IpAddr, SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use futures_util::StreamExt;

use crate::{
    RuntimeBackend, RuntimeError, RuntimeInstance, RuntimeInstanceSpec, RuntimeInstanceState,
    RuntimeResult,
};

use super::labels::{LABEL_MANAGED, LABEL_NODE, instance_from_labels, labels_for};
use super::spec::{DockerRuntimeConfig, container_name};

#[derive(Clone)]
pub struct DockerRuntime {
    config: DockerRuntimeConfig,
    docker: Docker,
    executor: Arc<tokio::runtime::Runtime>,
}

impl DockerRuntime {
    pub fn connect(config: DockerRuntimeConfig) -> RuntimeResult<Self> {
        let docker = Docker::connect_with_socket_defaults().map_err(|source| {
            RuntimeError::DockerOperation {
                operation: "docker connect",
                message: source.to_string(),
            }
        })?;
        let executor = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .map_err(|source| RuntimeError::DockerOperation {
                    operation: "docker runtime",
                    message: source.to_string(),
                })?,
        );
        let runtime = Self {
            config,
            docker,
            executor,
        };
        runtime.run(async {
            runtime
                .docker
                .ping()
                .await
                .map_err(|source| RuntimeError::DockerOperation {
                    operation: "docker ping",
                    message: source.to_string(),
                })
        })?;
        Ok(runtime)
    }

    fn run<F, T>(&self, future: F) -> RuntimeResult<T>
    where
        F: Future<Output = RuntimeResult<T>>,
    {
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::task::block_in_place(|| self.executor.block_on(future))
        } else {
            self.executor.block_on(future)
        }
    }

    fn metadata_path(&self, instance_id: &mvp_deploy::InstanceId) -> PathBuf {
        self.config
            .root
            .join("docker-instances")
            .join(instance_id.as_str())
            .join("instance.json")
    }

    fn read_metadata(
        &self,
        instance_id: &mvp_deploy::InstanceId,
    ) -> RuntimeResult<Option<RuntimeInstance>> {
        let path = self.metadata_path(instance_id);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|source| RuntimeError::DecodeMetadata { path, source }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(source) => Err(RuntimeError::ReadMetadata { path, source }),
        }
    }

    fn write_metadata(&self, instance: &RuntimeInstance) -> RuntimeResult<()> {
        let path = self.metadata_path(&instance.instance_id);
        write_metadata(&path, instance)
    }

    async fn ensure_container(&self, spec: &RuntimeInstanceSpec) -> RuntimeResult<RuntimeInstance> {
        let name = container_name(&self.config.node_id, spec);
        if let Some(instance) = self.inspect_instance(&name).await? {
            if instance.service == spec.service && instance.revision == spec.revision {
                self.start_container(&name).await?;
                return self.wait_ready(instance).await;
            }
            self.remove_container(&name).await?;
        }

        self.pull_image(&self.config.image_for(spec)).await?;
        self.create_container(&name, spec).await?;
        self.start_container(&name).await?;
        let instance =
            self.inspect_instance(&name)
                .await?
                .ok_or_else(|| RuntimeError::DockerOperation {
                    operation: "docker inspect",
                    message: format!("container '{name}' disappeared after start"),
                })?;
        self.wait_ready(instance).await
    }

    async fn create_container(&self, name: &str, spec: &RuntimeInstanceSpec) -> RuntimeResult<()> {
        let host_config = HostConfig {
            network_mode: self.config.network.clone(),
            ..Default::default()
        };
        let labels = labels_for(
            &self.config.node_id,
            &spec.instance_id,
            &spec.service,
            &spec.revision,
            RuntimeInstanceState::Running,
        );
        let body = ContainerCreateBody {
            image: Some(self.config.image_for(spec)),
            cmd: self.config.command.clone(),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        };
        let options = CreateContainerOptionsBuilder::default().name(name).build();
        self.docker
            .create_container(Some(options), body)
            .await
            .map_err(|source| RuntimeError::DockerOperation {
                operation: "docker create",
                message: source.to_string(),
            })?;
        Ok(())
    }

    async fn start_container(&self, name: &str) -> RuntimeResult<()> {
        match self.docker.start_container(name, None).await {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(source) => Err(RuntimeError::DockerOperation {
                operation: "docker start",
                message: source.to_string(),
            }),
        }
    }

    async fn stop_container(&self, name: &str) -> RuntimeResult<()> {
        let timeout = self.config.stop_timeout.as_secs().min(i64::MAX as u64) as i32;
        let options = StopContainerOptionsBuilder::default().t(timeout).build();
        match self.docker.stop_container(name, Some(options)).await {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304 | 404,
                ..
            }) => Ok(()),
            Err(source) => Err(RuntimeError::DockerOperation {
                operation: "docker stop",
                message: source.to_string(),
            }),
        }
    }

    async fn remove_container(&self, name: &str) -> RuntimeResult<()> {
        let options = RemoveContainerOptionsBuilder::default().force(true).build();
        match self.docker.remove_container(name, Some(options)).await {
            Ok(()) => Ok(()),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(()),
            Err(source) => Err(RuntimeError::DockerOperation {
                operation: "docker remove",
                message: source.to_string(),
            }),
        }
    }

    async fn pull_image(&self, image: &str) -> RuntimeResult<()> {
        if self.docker.inspect_image(image).await.is_ok() {
            return Ok(());
        }
        let options = bollard::query_parameters::CreateImageOptionsBuilder::default()
            .from_image(image)
            .build();
        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            result.map_err(|source| RuntimeError::DockerOperation {
                operation: "docker pull",
                message: source.to_string(),
            })?;
        }
        Ok(())
    }

    async fn list_instances(&self) -> RuntimeResult<Vec<RuntimeInstance>> {
        let filters = HashMap::from([(
            "label".to_string(),
            vec![
                format!("{LABEL_MANAGED}=true"),
                format!("{}={}", LABEL_NODE, self.config.node_id.as_str()),
            ],
        )]);
        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        let summaries = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(|source| RuntimeError::DockerOperation {
                operation: "docker list",
                message: source.to_string(),
            })?;
        let mut instances = Vec::new();
        for summary in summaries {
            let Some(id) = summary.id else {
                continue;
            };
            if let Some(instance) = self.inspect_instance(&id).await? {
                instances.push(self.merge_metadata_state(instance)?);
            }
        }
        instances.sort_by(|left, right| left.instance_id.cmp(&right.instance_id));
        Ok(instances)
    }

    fn merge_metadata_state(
        &self,
        mut instance: RuntimeInstance,
    ) -> RuntimeResult<RuntimeInstance> {
        if let Some(metadata) = self.read_metadata(&instance.instance_id)? {
            instance.state = metadata.state;
        }
        Ok(instance)
    }

    async fn inspect_instance(&self, name_or_id: &str) -> RuntimeResult<Option<RuntimeInstance>> {
        let info = match self.docker.inspect_container(name_or_id, None).await {
            Ok(info) => info,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => return Ok(None),
            Err(source) => {
                return Err(RuntimeError::DockerOperation {
                    operation: "docker inspect",
                    message: source.to_string(),
                });
            }
        };
        let name = info
            .name
            .as_deref()
            .unwrap_or(name_or_id)
            .trim_start_matches('/')
            .to_string();
        let labels = info
            .config
            .as_ref()
            .and_then(|config| config.labels.clone())
            .unwrap_or_default();
        if labels.get(LABEL_MANAGED).map(String::as_str) != Some("true") {
            return Ok(None);
        }
        let mut instance = instance_from_labels(
            &name,
            info.id.clone(),
            &labels,
            self.address_from_inspect(&info),
        )?;
        let running = info
            .state
            .as_ref()
            .and_then(|state| state.running)
            .unwrap_or(false);
        if !running {
            instance.state = RuntimeInstanceState::Stopped;
        }
        Ok(Some(self.merge_metadata_state(instance)?))
    }

    fn address_from_inspect(&self, info: &bollard::models::ContainerInspectResponse) -> String {
        let ip = info
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .and_then(|networks| {
                networks
                    .values()
                    .filter_map(|endpoint| endpoint.ip_address.as_deref())
                    .find(|ip| !ip.is_empty())
            })
            .and_then(|ip| ip.parse::<IpAddr>().ok())
            .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
        SocketAddr::new(ip, self.config.service_port).to_string()
    }

    async fn wait_ready(&self, instance: RuntimeInstance) -> RuntimeResult<RuntimeInstance> {
        let deadline = Instant::now() + self.config.readiness_timeout;
        while Instant::now() < deadline {
            if TcpStream::connect(instance.address.as_str()).is_ok() {
                return Ok(instance);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        Err(RuntimeError::ReadinessTimeout {
            instance_id: instance.instance_id.to_string(),
            address: instance.address,
        })
    }
}

impl RuntimeBackend for DockerRuntime {
    fn prepare(&self, spec: &RuntimeInstanceSpec) -> RuntimeResult<RuntimeInstance> {
        let instance = RuntimeInstance {
            instance_id: spec.instance_id.clone(),
            service: spec.service.clone(),
            revision: spec.revision.clone(),
            address: format!("127.0.0.1:{}", self.config.service_port),
            backend_id: None,
            backend_name: Some(container_name(&self.config.node_id, spec)),
            pid: None,
            state: RuntimeInstanceState::Prepared,
        };
        self.write_metadata(&instance)?;
        Ok(instance)
    }

    fn start(&self, spec: &RuntimeInstanceSpec) -> RuntimeResult<RuntimeInstance> {
        let instance = self.run(self.ensure_container(spec))?;
        self.write_metadata(&instance)?;
        Ok(instance)
    }

    fn drain(
        &self,
        instance_id: &mvp_deploy::InstanceId,
    ) -> RuntimeResult<Option<RuntimeInstance>> {
        let Some(mut instance) = self.read_metadata(instance_id)? else {
            return Ok(None);
        };
        instance.state = RuntimeInstanceState::Draining;
        self.write_metadata(&instance)?;
        Ok(Some(instance))
    }

    fn stop(&self, instance_id: &mvp_deploy::InstanceId) -> RuntimeResult<Option<RuntimeInstance>> {
        let Some(mut instance) = self.read_metadata(instance_id)? else {
            return Ok(None);
        };
        if let Some(name) = &instance.backend_name {
            self.run(self.stop_container(name))?;
            self.run(self.remove_container(name))?;
        }
        instance.state = RuntimeInstanceState::Stopped;
        self.write_metadata(&instance)?;
        Ok(Some(instance))
    }

    fn list(&self) -> RuntimeResult<Vec<RuntimeInstance>> {
        self.run(self.list_instances())
    }

    fn adopt(&self) -> RuntimeResult<Vec<RuntimeInstance>> {
        self.list()
    }
}

fn write_metadata(path: &Path, instance: &RuntimeInstance) -> RuntimeResult<()> {
    let parent = path
        .parent()
        .expect("docker runtime metadata path has parent");
    fs::create_dir_all(parent).map_err(|source| RuntimeError::CreateDir {
        path: parent.to_path_buf(),
        source,
    })?;
    let bytes = serde_json::to_vec_pretty(instance)
        .map_err(|source| RuntimeError::EncodeMetadata { source })?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|source| RuntimeError::WriteMetadata {
            path: path.to_path_buf(),
            source,
        })?;
    file.write_all(&bytes)
        .and_then(|_| file.as_file_mut().sync_all())
        .map_err(|source| RuntimeError::WriteMetadata {
            path: path.to_path_buf(),
            source,
        })?;
    file.persist(path)
        .map(|_file| ())
        .map_err(|error| RuntimeError::WriteMetadata {
            path: path.to_path_buf(),
            source: error.error,
        })
}
