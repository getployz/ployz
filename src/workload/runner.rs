use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig, PortBinding, PortMap};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use std::collections::HashMap;
use tracing::info;

use crate::error::{Error, Result};

use super::spec::{
    ContainerSpec, NetworkMode, PullPolicy, ResourcesExt, RestartPolicy, ServiceSpec, VolumeSource,
};

const LABEL_NAMESPACE: &str = "ployz.namespace";
const LABEL_SERVICE: &str = "ployz.service";
const LABEL_MANAGED: &str = "ployz.managed";
const LABEL_COMPOSE_PROJECT: &str = "com.docker.compose.project";
const LABEL_COMPOSE_SERVICE: &str = "com.docker.compose.service";

pub struct ServiceRunner {
    docker: Docker,
    /// Docker network name for overlay mode (e.g. "ployz-mynet"). None = default bridge.
    overlay_network: Option<String>,
}

/// Summary of a running service container.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RunningService {
    pub container_id: String,
    pub name: String,
    pub namespace: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub ip: String,
}

impl ServiceRunner {
    pub fn new(overlay_network: Option<String>) -> Result<Self> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;
        Ok(Self {
            docker,
            overlay_network,
        })
    }

    pub async fn run(&self, spec: &ServiceSpec) -> Result<String> {
        let container_name = format!("ployz-{}-{}", spec.namespace, spec.name);

        // Remove existing container with same name
        self.force_remove(&container_name).await;

        // Pull image if needed
        match spec.container.pull_policy {
            PullPolicy::Always => {
                self.pull_image(&spec.container.image).await?;
            }
            PullPolicy::IfNotPresent => {
                if !self.image_exists(&spec.container.image).await {
                    self.pull_image(&spec.container.image).await?;
                }
            }
            PullPolicy::Never => {}
        }

        let config = self.build_container_config(spec)?;

        let options = CreateContainerOptionsBuilder::default()
            .name(&container_name)
            .build();

        self.docker
            .create_container(Some(options), config)
            .await
            .map_err(|e| Error::operation("service create", e.to_string()))?;

        self.docker
            .start_container(&container_name, None)
            .await
            .map_err(|e| Error::operation("service start", e.to_string()))?;

        info!(name = %spec.name, namespace = %spec.namespace.0, image = %spec.container.image, "service started");
        Ok(container_name)
    }

    pub async fn list(&self) -> Result<Vec<RunningService>> {
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec![LABEL_MANAGED.to_string()]);

        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();

        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(|e| Error::operation("service list", e.to_string()))?;

        Ok(containers
            .into_iter()
            .map(|c| {
                let labels = c.labels.unwrap_or_default();
                RunningService {
                    container_id: c.id.unwrap_or_default(),
                    name: labels
                        .get(LABEL_SERVICE)
                        .cloned()
                        .unwrap_or_else(|| "unknown".into()),
                    namespace: labels
                        .get(LABEL_NAMESPACE)
                        .cloned()
                        .unwrap_or_else(|| "unknown".into()),
                    image: c.image.unwrap_or_default(),
                    state: c.state.map(|s| format!("{s:?}")).unwrap_or_default(),
                    status: c.status.unwrap_or_default(),
                    ip: c
                        .network_settings
                        .and_then(|ns| ns.networks)
                        .and_then(|nets| {
                            nets.into_values()
                                .find_map(|n| n.ip_address.filter(|ip| !ip.is_empty()))
                        })
                        .unwrap_or_default(),
                }
            })
            .collect())
    }

    pub async fn remove(&self, name: &str, namespace: &str) -> Result<()> {
        let container_name = format!("ployz-{namespace}-{name}");

        let stop_opts = StopContainerOptionsBuilder::default().t(10).build();
        match self
            .docker
            .stop_container(&container_name, Some(stop_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304 | 404,
                ..
            }) => {}
            Err(e) => return Err(Error::operation("service stop", e.to_string())),
        }

        let remove_opts = RemoveContainerOptionsBuilder::default().build();
        match self
            .docker
            .remove_container(&container_name, Some(remove_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            }) => {
                return Err(Error::operation(
                    "service remove",
                    format!("service '{name}' not found"),
                ));
            }
            Err(e) => return Err(Error::operation("service remove", e.to_string())),
        }

        info!(name, namespace, "service removed");
        Ok(())
    }

    fn build_container_config(&self, spec: &ServiceSpec) -> Result<ContainerCreateBody> {
        let container = &spec.container;

        let mut labels = HashMap::new();
        labels.insert(LABEL_MANAGED.to_string(), "true".to_string());
        labels.insert(LABEL_NAMESPACE.to_string(), spec.namespace.0.clone());
        labels.insert(LABEL_SERVICE.to_string(), spec.name.clone());
        labels.insert(LABEL_COMPOSE_PROJECT.to_string(), format!("ployz-{}", spec.namespace));
        labels.insert(LABEL_COMPOSE_SERVICE.to_string(), spec.name.clone());
        for (k, v) in &spec.labels {
            labels.insert(k.clone(), v.clone());
        }

        let port_bindings = self.build_port_bindings(spec);

        let host_config = HostConfig {
            network_mode: match &spec.network {
                NetworkMode::Host => Some("host".to_string()),
                NetworkMode::None => Some("none".to_string()),
                NetworkMode::Service(svc) => {
                    Some(format!("container:ployz-{}-{svc}", spec.namespace))
                }
                NetworkMode::Overlay => self.overlay_network.clone(),
            },
            binds: Some(self.build_binds(container)),
            port_bindings: if port_bindings.is_empty() {
                None
            } else {
                Some(port_bindings)
            },
            cap_add: if container.cap_add.is_empty() {
                None
            } else {
                Some(container.cap_add.clone())
            },
            cap_drop: if container.cap_drop.is_empty() {
                None
            } else {
                Some(container.cap_drop.clone())
            },
            privileged: Some(container.privileged),
            restart_policy: Some(self.build_restart_policy(&spec.restart)),
            memory: container.resources.memory_bytes.map(|b| b as i64),
            nano_cpus: container
                .resources
                .cpu_nano(),
            sysctls: if container.sysctls.is_empty() {
                None
            } else {
                Some(container.sysctls.clone().into_iter().collect())
            },
            tmpfs: {
                let tmpfs_mounts: HashMap<String, String> = container
                    .volumes
                    .iter()
                    .filter(|v| matches!(v.source, VolumeSource::Tmpfs))
                    .map(|v| (v.target.clone(), String::new()))
                    .collect();
                if tmpfs_mounts.is_empty() {
                    None
                } else {
                    Some(tmpfs_mounts)
                }
            },
            ..Default::default()
        };

        let env: Vec<String> = container
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();

        Ok(ContainerCreateBody {
            image: Some(container.image.clone()),
            cmd: container.command.clone(),
            entrypoint: container.entrypoint.clone(),
            env: if env.is_empty() { None } else { Some(env) },
            labels: Some(labels),
            user: container.user.clone(),
            host_config: Some(host_config),
            stop_timeout: spec
                .stop_grace_period
                .as_ref()
                .and_then(|s| parse_duration_secs(s)),
            ..Default::default()
        })
    }

    fn build_binds(&self, container: &ContainerSpec) -> Vec<String> {
        container
            .volumes
            .iter()
            .filter_map(|v| match &v.source {
                VolumeSource::Bind(src) => {
                    let ro = if v.readonly { ":ro" } else { "" };
                    Some(format!("{src}:{}{ro}", v.target))
                }
                VolumeSource::Managed(m) => {
                    let ro = if v.readonly { ":ro" } else { "" };
                    Some(format!("{}:{}{ro}", m.name, v.target))
                }
                VolumeSource::Tmpfs => None, // handled via tmpfs in host_config
            })
            .collect()
    }

    fn build_port_bindings(&self, spec: &ServiceSpec) -> PortMap {
        let mut map = PortMap::new();
        for port in &spec.ports {
            let proto = match port.protocol {
                super::spec::PortProtocol::Tcp => "tcp",
                super::spec::PortProtocol::Udp => "udp",
            };
            let key = format!("{}/{proto}", port.container_port);
            let binding = PortBinding {
                host_ip: port.host_ip.clone(),
                host_port: Some(port.host_port.to_string()),
            };
            map.entry(key)
                .or_insert_with(|| Some(vec![]))
                .as_mut()
                .unwrap()
                .push(binding);
        }
        map
    }

    fn build_restart_policy(
        &self,
        policy: &RestartPolicy,
    ) -> bollard::models::RestartPolicy {
        let (name, max) = match policy {
            RestartPolicy::No => (
                bollard::models::RestartPolicyNameEnum::NO,
                0,
            ),
            RestartPolicy::Always => (
                bollard::models::RestartPolicyNameEnum::ALWAYS,
                0,
            ),
            RestartPolicy::OnFailure => (
                bollard::models::RestartPolicyNameEnum::ON_FAILURE,
                0,
            ),
            RestartPolicy::UnlessStopped => (
                bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED,
                0,
            ),
        };
        bollard::models::RestartPolicy {
            name: Some(name),
            maximum_retry_count: Some(max),
        }
    }

    async fn image_exists(&self, image: &str) -> bool {
        self.docker.inspect_image(image).await.is_ok()
    }

    async fn pull_image(&self, image: &str) -> Result<()> {
        use bollard::query_parameters::CreateImageOptionsBuilder;
        use futures_util::StreamExt;

        let (from_image, tag) = match image.splitn(2, ':').collect::<Vec<_>>().as_slice() {
            [img, t] => (*img, *t),
            _ => (image, "latest"),
        };

        let options = CreateImageOptionsBuilder::default()
            .from_image(from_image)
            .tag(tag)
            .build();

        let mut stream = self.docker.create_image(Some(options), None, None);
        while let Some(result) = stream.next().await {
            result.map_err(|e| Error::operation("image pull", e.to_string()))?;
        }
        Ok(())
    }

    async fn force_remove(&self, container_name: &str) {
        let options = RemoveContainerOptionsBuilder::default()
            .force(true)
            .build();
        let _ = self
            .docker
            .remove_container(container_name, Some(options))
            .await;
    }
}

fn parse_duration_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if let Some(rest) = s.strip_suffix('s') {
        rest.trim().parse::<i64>().ok()
    } else if let Some(rest) = s.strip_suffix('m') {
        rest.trim().parse::<i64>().ok().map(|m| m * 60)
    } else {
        s.parse::<i64>().ok()
    }
}
