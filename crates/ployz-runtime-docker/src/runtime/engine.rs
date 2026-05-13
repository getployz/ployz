use std::collections::HashMap;
use std::time::Duration;

use bollard::Docker;
use bollard::models::{ContainerCreateBody, ContainerStatsResponse, HostConfig};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, ImportImageOptionsBuilder,
    ListContainersOptionsBuilder, RemoveContainerOptionsBuilder, RemoveVolumeOptionsBuilder,
    StatsOptionsBuilder, StopContainerOptionsBuilder,
};
use futures_util::{StreamExt, TryStreamExt};
use ployz_error::RuntimeError;
use ployz_error::{Error, Result};
use ployz_model::{ImageDigest, ImagePlatform};
use ployz_runtime_api::{
    ImageArchiveReader, ImageDiskPreflight, ImageReceivePreflightRequest, RuntimeImage,
    RuntimeImageBackend, RuntimeImageError, RuntimeImageImportResult,
};
use tokio_util::codec::{BytesCodec, FramedRead};
use tokio_util::io::StreamReader;
use tracing::{info, warn};

use super::diff::{ChangedField, SpecChange, eval_spec_change, parent_id_matches};
use super::labels::{LABEL_KIND, LABEL_MANAGED, extract_workload_labels};
use super::probe::ProbeRunner;
use super::spec::{
    Observation, ObservedContainer, observe, port_map_to_bollard, restart_policy_to_bollard,
};
use super::{PullPolicy, RuntimeContainerSpec, digest_from_image_ref, parse_docker_image_ref};

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

#[derive(Debug, Clone, PartialEq)]
pub struct WorkloadResourceSnapshot {
    pub namespace: String,
    pub service: String,
    pub instance_id: String,
    pub machine_id: String,
    pub cpu_total_seconds: f64,
    pub memory_usage_bytes: u64,
    pub memory_limit_bytes: u64,
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
                    container_id: require_observed(&obs.container_id, "container_id")?.clone(),
                    container_name: spec.container_name.clone(),
                    action: EnsureAction::Adopted,
                    ip_address: require_observed(&obs.ip_address, "ip_address")?,
                    networks: require_observed(&obs.networks, "networks")?.clone(),
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

    pub async fn remove_volume(&self, volume_name: &str) -> Result<()> {
        let options = RemoveVolumeOptionsBuilder::default().force(true).build();
        match self.docker.remove_volume(volume_name, Some(options)).await {
            Ok(()) => {}
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {}
            Err(e) => return Err(Error::operation("docker remove volume", e.to_string())),
        }

        info!(name = %volume_name, "volume removed");
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
        Ok(observed.running == Observation::Observed(true))
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

    pub async fn list_workload_resource_snapshots(&self) -> Result<Vec<WorkloadResourceSnapshot>> {
        let observed = self
            .list_by_labels(&[(LABEL_MANAGED, "true"), (LABEL_KIND, "workload")])
            .await?;

        let mut snapshots = Vec::new();
        for container in observed {
            let Some(snapshot) = self.workload_resource_snapshot(&container).await? else {
                continue;
            };
            snapshots.push(snapshot);
        }
        Ok(snapshots)
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

        let parsed = parse_docker_image_ref(image);
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
                        info!(image = %image, %status, "pulling");
                    }
                }
                Err(e) => {
                    warn!(?e, image = %image, "pull failed, trying cached image");
                    break;
                }
            }
        }
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
            port_bindings: spec.port_bindings.as_ref().map(port_map_to_bollard),
            cap_add: none_if_empty(&spec.cap_add),
            cap_drop: none_if_empty(&spec.cap_drop),
            privileged: Some(spec.privileged),
            restart_policy: spec.restart_policy.as_ref().map(restart_policy_to_bollard),
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
            require_observed(&observed.container_id, "container_id")?,
            require_observed(&observed.ip_address, "ip_address")?,
            require_observed(&observed.networks, "networks")?,
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

    async fn workload_resource_snapshot(
        &self,
        container: &ObservedContainer,
    ) -> Result<Option<WorkloadResourceSnapshot>> {
        if container.running != Observation::Observed(true) {
            return Ok(None);
        }

        let Some(container_id) = container.container_id.as_observed() else {
            return Ok(None);
        };
        let options = StatsOptionsBuilder::default()
            .stream(false)
            .one_shot(true)
            .build();
        let mut stats_stream = self.docker.stats(container_id, Some(options));

        let stats_result = stats_stream.next().await;
        let Some(stats_result) = stats_result else {
            return Ok(None);
        };
        match stats_result {
            Ok(stats) => Ok(workload_resource_snapshot(container, &stats)),
            Err(error) => {
                warn!(
                    ?error,
                    container = ?container.container_name,
                    "failed to fetch workload stats"
                );
                Ok(None)
            }
        }
    }
}

#[async_trait::async_trait]
impl RuntimeImageBackend for ContainerEngine {
    async fn inspect_image(
        &self,
        reference: &str,
    ) -> std::result::Result<Option<RuntimeImage>, RuntimeImageError> {
        match self.docker.inspect_image(reference).await {
            Ok(info) => Ok(Some(runtime_image_from_inspect(reference, info))),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => Ok(None),
            Err(error) => Err(RuntimeImageError::backend(
                "docker inspect image",
                error.to_string(),
            )),
        }
    }

    async fn export_image_archive<'a>(
        &'a self,
        reference: &'a str,
    ) -> std::result::Result<ImageArchiveReader<'a>, RuntimeImageError> {
        if self.inspect_image(reference).await?.is_none() {
            return Err(RuntimeImageError::NotFound {
                reference: reference.into(),
            });
        }
        let reference_for_error = reference.to_string();
        let stream = self.docker.export_image(reference).map_err(move |error| {
            std::io::Error::other(format!(
                "docker export image '{reference_for_error}': {error}"
            ))
        });
        Ok(Box::pin(StreamReader::new(stream)))
    }

    async fn import_image_archive(
        &self,
        archive: ImageArchiveReader<'static>,
    ) -> std::result::Result<RuntimeImageImportResult, RuntimeImageError> {
        let byte_stream = FramedRead::new(archive, BytesCodec::new())
            .map_ok(|bytes| bytes.freeze())
            .map_err(std::io::Error::other);
        let options = ImportImageOptionsBuilder::default().build();
        let mut responses = self.docker.import_image_stream(options, byte_stream, None);
        let mut messages = Vec::new();
        while let Some(response) = responses.next().await {
            let info = response.map_err(|error| {
                RuntimeImageError::backend("docker import image", error.to_string())
            })?;
            if let Some(stream) = info.stream {
                messages.push(stream);
            }
            if let Some(status) = info.status {
                messages.push(status);
            }
        }
        Ok(RuntimeImageImportResult { messages })
    }

    async fn preflight_image_receive(
        &self,
        request: ImageReceivePreflightRequest,
    ) -> std::result::Result<ImageDiskPreflight, RuntimeImageError> {
        let _ = request;
        Ok(ImageDiskPreflight::Unknown)
    }
}

fn runtime_image_from_inspect(
    reference: &str,
    info: bollard::models::ImageInspect,
) -> RuntimeImage {
    RuntimeImage {
        reference: reference.into(),
        id: info.id,
        repo_digests: image_repo_digests(info.repo_digests),
        platform: image_platform(info.os, info.architecture, info.variant),
        size_bytes: info.size.and_then(|size| u64::try_from(size).ok()),
    }
}

fn image_repo_digests(repo_digests: Option<Vec<String>>) -> Vec<ImageDigest> {
    repo_digests
        .into_iter()
        .flatten()
        .filter_map(|repo_digest| repo_digest_digest(&repo_digest))
        .collect()
}

fn repo_digest_digest(repo_digest: &str) -> Option<ImageDigest> {
    digest_from_image_ref(repo_digest)
}

fn image_platform(
    os: Option<String>,
    architecture: Option<String>,
    variant: Option<String>,
) -> Option<ImagePlatform> {
    let (Some(os), Some(architecture)) = (os, architecture) else {
        return None;
    };
    Some(ImagePlatform {
        os,
        architecture,
        variant,
    })
}

fn none_if_empty<T: Clone>(v: &[T]) -> Option<Vec<T>> {
    if v.is_empty() { None } else { Some(v.to_vec()) }
}

fn none_if_empty_map<K: Clone, V: Clone>(m: &HashMap<K, V>) -> Option<HashMap<K, V>> {
    if m.is_empty() { None } else { Some(m.clone()) }
}

fn workload_resource_snapshot(
    container: &ObservedContainer,
    stats: &ContainerStatsResponse,
) -> Option<WorkloadResourceSnapshot> {
    let labels = container.labels.as_observed()?;
    if labels.get(LABEL_MANAGED).map(String::as_str) != Some("true") {
        return None;
    }
    if labels.get(LABEL_KIND).map(String::as_str) != Some("workload") {
        return None;
    }
    let labels = extract_workload_labels(labels)?;
    let cpu_total_nanos = stats
        .cpu_stats
        .as_ref()
        .and_then(|cpu| cpu.cpu_usage.as_ref())
        .and_then(|cpu_usage| cpu_usage.total_usage)
        .unwrap_or(0);
    let memory_usage_bytes = stats
        .memory_stats
        .as_ref()
        .and_then(|memory| memory.usage)
        .unwrap_or(0);
    let memory_limit_bytes = stats
        .memory_stats
        .as_ref()
        .and_then(|memory| memory.limit)
        .unwrap_or(0);

    Some(WorkloadResourceSnapshot {
        namespace: labels.namespace,
        service: labels.service,
        instance_id: labels.instance_id,
        machine_id: labels.machine_id,
        cpu_total_seconds: cpu_total_nanos as f64 / 1_000_000_000.0,
        memory_usage_bytes,
        memory_limit_bytes,
    })
}

fn require_observed<T: Clone>(observation: &Observation<T>, field: &str) -> Result<T> {
    match observation {
        Observation::Observed(value) => Ok(value.clone()),
        Observation::Missing => Err(Error::Runtime(RuntimeError::RequiredObservationMissing {
            field: field.to_string(),
        })),
        Observation::Malformed(message) => {
            Err(Error::Runtime(RuntimeError::RequiredObservationMalformed {
                field: field.to_string(),
                message: message.clone(),
            }))
        }
        Observation::Unknown => Err(Error::Runtime(RuntimeError::RequiredObservationUnknown {
            field: field.to_string(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::{
        Observation, ObservedContainer, WorkloadResourceSnapshot, require_observed,
        workload_resource_snapshot,
    };
    use crate::runtime::labels::{
        LABEL_INSTANCE, LABEL_KIND, LABEL_MACHINE, LABEL_MANAGED, LABEL_NAMESPACE, LABEL_SERVICE,
    };
    use bollard::models::{
        ContainerCpuStats, ContainerCpuUsage, ContainerMemoryStats, ContainerStatsResponse,
    };
    use ployz_error::Error;
    use ployz_error::RuntimeError;

    #[test]
    fn workload_resource_snapshot_reads_cpu_and_memory_stats() {
        let container = observed_workload_container();
        let snapshot = workload_resource_snapshot(
            &container,
            &ContainerStatsResponse {
                cpu_stats: Some(ContainerCpuStats {
                    cpu_usage: Some(ContainerCpuUsage {
                        total_usage: Some(2_500_000_000),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                memory_stats: Some(ContainerMemoryStats {
                    usage: Some(512),
                    limit: Some(2048),
                    ..Default::default()
                }),
                ..Default::default()
            },
        )
        .expect("snapshot should be produced");

        assert_eq!(
            snapshot,
            WorkloadResourceSnapshot {
                namespace: "ns".into(),
                service: "web".into(),
                instance_id: "web-1".into(),
                machine_id: "machine-a".into(),
                cpu_total_seconds: 2.5,
                memory_usage_bytes: 512,
                memory_limit_bytes: 2048,
            }
        );
    }

    #[test]
    fn workload_resource_snapshot_ignores_unlabeled_containers() {
        let mut container = observed_workload_container();
        container
            .labels
            .as_observed_mut()
            .expect("labels observed")
            .remove(LABEL_NAMESPACE);

        let snapshot = workload_resource_snapshot(&container, &ContainerStatsResponse::default());
        assert!(snapshot.is_none());
    }

    #[test]
    fn workload_resource_snapshot_ignores_non_workload_containers() {
        let mut container = observed_workload_container();
        container
            .labels
            .as_observed_mut()
            .expect("labels observed")
            .insert(LABEL_KIND.into(), "system".into());

        let snapshot = workload_resource_snapshot(&container, &ContainerStatsResponse::default());
        assert!(snapshot.is_none());
    }

    #[test]
    fn require_observed_returns_structured_missing_error() {
        let result = require_observed::<String>(&Observation::Missing, "container_id");

        assert_eq!(
            result,
            Err(Error::Runtime(RuntimeError::RequiredObservationMissing {
                field: String::from("container_id")
            }))
        );
    }

    #[test]
    fn require_observed_returns_structured_malformed_error() {
        let result = require_observed::<String>(
            &Observation::Malformed(String::from("not an IP address")),
            "ip_address",
        );

        assert_eq!(
            result,
            Err(Error::Runtime(RuntimeError::RequiredObservationMalformed {
                field: String::from("ip_address"),
                message: String::from("not an IP address")
            }))
        );
    }

    #[test]
    fn workload_resource_snapshot_ignores_unknown_or_malformed_labels() {
        let mut unknown = observed_workload_container();
        unknown.labels = Observation::Unknown;
        assert!(workload_resource_snapshot(&unknown, &ContainerStatsResponse::default()).is_none());

        let mut malformed = observed_workload_container();
        malformed.labels = Observation::Malformed("labels were not a map".into());
        assert!(
            workload_resource_snapshot(&malformed, &ContainerStatsResponse::default()).is_none()
        );
    }

    fn observed_workload_container() -> ObservedContainer {
        let mut labels = HashMap::new();
        labels.insert(LABEL_MANAGED.into(), "true".into());
        labels.insert(LABEL_KIND.into(), "workload".into());
        labels.insert(LABEL_NAMESPACE.into(), "ns".into());
        labels.insert(LABEL_SERVICE.into(), "web".into());
        labels.insert(LABEL_INSTANCE.into(), "web-1".into());
        labels.insert(LABEL_MACHINE.into(), "machine-a".into());
        labels.insert("dev.ployz.slot".into(), "slot-1".into());
        labels.insert("dev.ployz.revision".into(), "rev-1".into());
        labels.insert("dev.ployz.deploy".into(), "deploy-1".into());

        ObservedContainer {
            container_id: Observation::Observed("container-1".into()),
            container_name: Observation::Observed("ployz-ns-web-1".into()),
            running: Observation::Observed(true),
            image: Observation::Observed("busybox".into()),
            cmd: Observation::Observed(None),
            entrypoint: Observation::Observed(None),
            env: Observation::Observed(Vec::new()),
            labels: Observation::Observed(labels),
            binds: Observation::Observed(Vec::new()),
            tmpfs: Observation::Observed(HashMap::new()),
            dns_servers: Observation::Observed(Vec::new()),
            network_mode: Observation::Observed(None),
            port_bindings: Observation::Observed(None),
            cap_add: Observation::Observed(Vec::new()),
            cap_drop: Observation::Observed(Vec::new()),
            privileged: Observation::Observed(false),
            user: Observation::Observed(None),
            restart_policy: Observation::Observed(None),
            memory_bytes: Observation::Observed(None),
            nano_cpus: Observation::Observed(None),
            sysctls: Observation::Observed(HashMap::new()),
            stop_timeout: Observation::Observed(None),
            pid_mode: Observation::Observed(None),
            ip_address: Observation::Observed(None),
            networks: Observation::Observed(HashMap::new()),
        }
    }
}
