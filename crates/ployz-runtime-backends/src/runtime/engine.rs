use std::collections::HashMap;
use std::time::Duration;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, ListContainersOptionsBuilder, RemoveContainerOptionsBuilder,
    StopContainerOptionsBuilder,
};
use ployz_types::{Error, Result};
use tracing::{info, warn};

use super::diff::{ChangedField, SpecChange, eval_spec_change, parent_id_matches};
use super::probe::ProbeRunner;
use super::spec::{ObservedContainer, observe};
use super::{PullPolicy, RuntimeContainerSpec};

pub struct ContainerEngine {
    docker: Docker,
    probe_runner: ProbeRunner,
}

pub struct EnsureResult {
    pub container_id: String,
    pub container_name: String,
    pub action: EnsureAction,
    pub ip_address: Option<std::net::IpAddr>,
    pub networks: HashMap<String, String>,
}

#[derive(Debug)]
pub enum EnsureAction {
    Adopted,
    Created,
    Recreated { changed: Vec<ChangedField> },
}

impl ContainerEngine {
    #[must_use]
    pub fn new(docker: Docker) -> Self {
        let probe_runner = ProbeRunner::new(docker.clone());
        Self {
            docker,
            probe_runner,
        }
    }

    pub async fn connect() -> Result<Self> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;
        docker
            .ping()
            .await
            .map_err(|e| Error::operation("docker ping", e.to_string()))?;
        Ok(Self::new(docker))
    }

    #[must_use]
    pub fn probe_runner(&self) -> &ProbeRunner {
        &self.probe_runner
    }

    #[must_use]
    pub fn docker(&self) -> &Docker {
        &self.docker
    }

    /// Ensure a container matches the desired spec.
    /// Inspects by container_name, diffs, then adopts/creates/recreates.
    pub async fn ensure(&self, spec: &RuntimeContainerSpec) -> Result<EnsureResult> {
        let observed = self.inspect(&spec.container_name).await?;

        // Check parent ID if network_mode is container:X
        let parent_id = self.resolve_parent_id(spec).await?;

        let change = match &observed {
            Some(obs) => {
                if !parent_id_matches(obs, parent_id.as_deref()) {
                    info!(
                        name = %spec.container_name,
                        "parent container changed, recreating"
                    );
                    SpecChange::Drifted {
                        fields: vec![ChangedField::NetworkMode],
                    }
                } else {
                    eval_spec_change(Some(obs), spec)
                }
            }
            None => SpecChange::Missing,
        };

        match change {
            SpecChange::InSync => {
                let Some(obs) = observed else {
                    return Err(Error::operation(
                        "ensure",
                        "InSync but no observed container",
                    ));
                };
                info!(name = %spec.container_name, "adopted existing container");
                Ok(EnsureResult {
                    container_id: obs.container_id.clone(),
                    container_name: spec.container_name.clone(),
                    action: EnsureAction::Adopted,
                    ip_address: obs.ip_address,
                    networks: obs.networks.clone(),
                })
            }
            SpecChange::Missing => {
                self.pull_image(&spec.image, spec.pull_policy).await?;
                let (container_id, ip_address, networks) =
                    self.create_and_start(spec, parent_id.as_deref()).await?;
                info!(
                    name = %spec.container_name,
                    image = %spec.image,
                    "container created"
                );
                Ok(EnsureResult {
                    container_id,
                    container_name: spec.container_name.clone(),
                    action: EnsureAction::Created,
                    ip_address,
                    networks,
                })
            }
            SpecChange::Drifted { fields } => {
                info!(
                    name = %spec.container_name,
                    changed = ?fields,
                    "config drift detected, recreating"
                );
                self.pull_image(&spec.image, spec.pull_policy).await?;
                self.force_remove(&spec.container_name).await;
                let (container_id, ip_address, networks) =
                    self.create_and_start(spec, parent_id.as_deref()).await?;
                info!(
                    name = %spec.container_name,
                    image = %spec.image,
                    "container recreated"
                );
                Ok(EnsureResult {
                    container_id,
                    container_name: spec.container_name.clone(),
                    action: EnsureAction::Recreated { changed: fields },
                    ip_address,
                    networks,
                })
            }
        }
    }

    pub async fn remove(&self, container_name: &str, grace_period: Duration) -> Result<()> {
        self.stop(container_name, grace_period).await?;

        let remove_opts = RemoveContainerOptionsBuilder::default().build();
        match self
            .docker
            .remove_container(container_name, Some(remove_opts))
            .await
        {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => return Err(Error::operation("docker remove", e.to_string())),
        }

        info!(name = %container_name, "container removed");
        Ok(())
    }

    pub async fn stop(&self, container_name: &str, grace_period: Duration) -> Result<()> {
        let grace_secs = grace_period.as_secs() as i32;
        let stop_opts = StopContainerOptionsBuilder::default().t(grace_secs).build();
        match self
            .docker
            .stop_container(container_name, Some(stop_opts))
            .await
        {
            Ok(()) => {
                info!(name = %container_name, "container stopped");
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304 | 404,
                ..
            }) => Ok(()),
            Err(e) => Err(Error::operation("docker stop", e.to_string())),
        }
    }

    pub async fn start(&self, container_name: &str) -> Result<()> {
        match self.docker.start_container(container_name, None).await {
            Ok(()) => {
                info!(name = %container_name, "container started");
                Ok(())
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 304, ..
            }) => Ok(()),
            Err(e) => Err(Error::operation("docker start", e.to_string())),
        }
    }

    pub async fn inspect(&self, container_name: &str) -> Result<Option<ObservedContainer>> {
        match self.docker.inspect_container(container_name, None).await {
            Ok(info) => Ok(Some(observe(&info))),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(Error::operation("docker inspect", e.to_string())),
        }
    }

    pub async fn is_running(&self, container_name: &str) -> Result<bool> {
        let Some(observed) = self.inspect(container_name).await? else {
            return Ok(false);
        };
        Ok(observed.running)
    }

    pub async fn list_by_labels(&self, filters: &[(&str, &str)]) -> Result<Vec<ObservedContainer>> {
        let mut label_filters = HashMap::new();
        let filter_strings: Vec<String> = filters.iter().map(|(k, v)| format!("{k}={v}")).collect();
        label_filters.insert("label".to_string(), filter_strings);

        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&label_filters)
            .build();

        let containers = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(|e| Error::operation("list_containers", e.to_string()))?;

        let mut result = Vec::new();
        for summary in containers {
            let Some(ref id) = summary.id else {
                continue;
            };
            match self.docker.inspect_container(id, None).await {
                Ok(info) => result.push(observe(&info)),
                Err(e) => {
                    warn!(?e, container_id = %id, "failed to inspect container during list");
                }
            }
        }
        Ok(result)
    }

    pub async fn pull_image(&self, image: &str, policy: PullPolicy) -> Result<()> {
        match policy {
            PullPolicy::Never => return Ok(()),
            PullPolicy::IfNotPresent => {
                if self.docker.inspect_image(image).await.is_ok() {
                    return Ok(());
                }
            }
            PullPolicy::Always => {}
        }

        super::pull_docker_image(&self.docker, image).await;
        Ok(())
    }

    /// Resolve the parent container ID from a `container:X` network mode.
    async fn resolve_parent_id(&self, spec: &RuntimeContainerSpec) -> Result<Option<String>> {
        let Some(ref mode) = spec.network_mode else {
            return Ok(None);
        };
        let Some(parent_name) = mode.strip_prefix("container:") else {
            return Ok(None);
        };
        match self.docker.inspect_container(parent_name, None).await {
            Ok(info) => Ok(info.id),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(e) => Err(Error::operation(
                "resolve_parent_id",
                format!("inspect parent {parent_name}: {e}"),
            )),
        }
    }

    async fn create_and_start(
        &self,
        spec: &RuntimeContainerSpec,
        parent_id: Option<&str>,
    ) -> Result<(String, Option<std::net::IpAddr>, HashMap<String, String>)> {
        // Force remove any existing container first
        self.force_remove(&spec.container_name).await;

        let mut labels = spec.labels.clone();
        if let Some(pid) = parent_id {
            labels.insert(super::labels::LABEL_PARENT_ID.into(), pid.into());
        }

        let env: Vec<String> = spec.env.iter().map(|(k, v)| format!("{k}={v}")).collect();

        let host_config = HostConfig {
            binds: none_if_empty(&spec.binds),
            dns: none_if_empty(&spec.dns_servers),
            network_mode: spec.network_mode.clone(),
            port_bindings: spec.port_bindings.clone(),
            cap_add: none_if_empty(&spec.cap_add),
            cap_drop: none_if_empty(&spec.cap_drop),
            privileged: Some(spec.privileged),
            restart_policy: spec.restart_policy.clone(),
            memory: spec.memory_bytes,
            nano_cpus: spec.nano_cpus,
            sysctls: none_if_empty_map(&spec.sysctls),
            tmpfs: none_if_empty_map(&spec.tmpfs),
            pid_mode: spec.pid_mode.clone(),
            ..Default::default()
        };

        let container_config = ContainerCreateBody {
            image: Some(spec.image.clone()),
            cmd: spec.cmd.clone(),
            entrypoint: spec.entrypoint.clone(),
            env: if env.is_empty() { None } else { Some(env) },
            labels: Some(labels),
            user: spec.user.clone(),
            host_config: Some(host_config),
            exposed_ports: spec.exposed_ports.clone(),
            stop_timeout: spec.stop_timeout,
            ..Default::default()
        };

        let create_opts = CreateContainerOptionsBuilder::default()
            .name(&spec.container_name)
            .build();

        self.docker
            .create_container(Some(create_opts), container_config)
            .await
            .map_err(|e| Error::operation("docker create", e.to_string()))?;

        self.docker
            .start_container(&spec.container_name, None)
            .await
            .map_err(|e| Error::operation("docker start", e.to_string()))?;

        // Inspect to get container ID and IP
        let info = self
            .docker
            .inspect_container(&spec.container_name, None)
            .await
            .map_err(|e| Error::operation("docker inspect", e.to_string()))?;

        let observed = observe(&info);
        Ok((
            observed.container_id,
            observed.ip_address,
            observed.networks,
        ))
    }

    async fn force_remove(&self, container_name: &str) {
        let options = RemoveContainerOptionsBuilder::default().force(true).build();
        if let Err(e) = self
            .docker
            .remove_container(container_name, Some(options))
            .await
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
}

fn none_if_empty<T: Clone>(v: &[T]) -> Option<Vec<T>> {
    if v.is_empty() { None } else { Some(v.to_vec()) }
}

fn none_if_empty_map<K: Clone, V: Clone>(m: &HashMap<K, V>) -> Option<HashMap<K, V>> {
    if m.is_empty() { None } else { Some(m.clone()) }
}
