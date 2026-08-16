mod create;
mod lifecycle;
mod managed_service;
mod observe;
mod spec_store;
mod stream;
mod unregistry;
mod volume;

#[cfg(test)]
mod integration_tests;

use std::{
    collections::HashMap,
    net::Ipv4Addr,
    path::{Path, PathBuf},
};

use bollard::{
    Docker,
    models::{ContainerInspectResponse, HealthConfig, ImageManifestSummaryKindEnum},
    query_parameters::{ListContainersOptionsBuilder, ListImagesOptionsBuilder},
};
use ployz_core::{
    ConfiguredHealthcheck, ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, HEALTHCHECK_DISABLE_SENTINEL, HealthObservation,
    HealthcheckCommand, HealthcheckSpec, ImageSummary, MachineId, MachineImages, RpcErrorCode,
    ServiceId, ServiceName, ValueError,
};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;

use observe::ObservationSink;

pub(crate) use managed_service::ManagedService;
pub use spec_store::{Error as SpecStoreError, MachineSpecStore};
pub use unregistry::{RunningUnregistry, unregistry_matches};

#[cfg(test)]
use create::{docker_healthcheck, docker_mounts, docker_ports, docker_resources};

pub const LABEL_MANAGED: &str = "ployz.managed";
pub const LABEL_SERVICE_ID: &str = "ployz.service.id";
pub const LABEL_SERVICE_NAME: &str = "ployz.service.name";
pub const LABEL_HOOK: &str = "ployz.service.hook";
pub const LABEL_HOOK_PRE_DEPLOY: &str = "pre-deploy";

#[derive(Clone)]
pub struct LocalDocker {
    client: Docker,
}

impl LocalDocker {
    pub fn connect() -> Result<Self, Error> {
        Ok(Self {
            client: Docker::connect_with_defaults()?,
        })
    }

    #[cfg(test)]
    fn connect_socket(socket: &str) -> Result<Self, Error> {
        let socket = socket.trim_start_matches("unix://");
        Ok(Self {
            client: Docker::connect_with_socket(socket, 120, bollard::API_DEFAULT_VERSION)?,
        })
    }

    pub async fn uses_containerd_store(&self) -> Result<bool, Error> {
        Ok(self
            .client
            .info()
            .await?
            .driver_status
            .as_deref()
            .is_some_and(containerd_store))
    }

    async fn managed_container_ids(&self) -> Result<Vec<ContainerId>, Error> {
        let filters = HashMap::from([("label", vec![LABEL_MANAGED, LABEL_SERVICE_ID])]);
        let options = ListContainersOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .build();
        self.client
            .list_containers(Some(options))
            .await?
            .into_iter()
            .map(|container| {
                let id = container.id.ok_or(Error::MissingField("container ID"))?;
                ContainerId::parse(id).map_err(|source| Error::InvalidValue {
                    field: "container ID",
                    source,
                })
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct ContainerRuntime {
    docker: LocalDocker,
    specs: MachineSpecStore,
    sink: Option<ObservationSink>,
}

impl ContainerRuntime {
    #[must_use]
    pub fn new(docker: LocalDocker, specs: MachineSpecStore) -> Self {
        Self {
            docker,
            specs,
            sink: None,
        }
    }

    pub async fn open(spec_store: impl Into<PathBuf>) -> Result<Self, Error> {
        Ok(Self::new(
            LocalDocker::connect()?,
            MachineSpecStore::open(spec_store).await?,
        ))
    }

    pub async fn list_images(&self, reference: Option<&str>) -> Result<MachineImages, Error> {
        let filters = reference
            .map(|reference| HashMap::from([("reference", vec![reference])]))
            .unwrap_or_default();
        let options = ListImagesOptionsBuilder::default()
            .all(true)
            .filters(&filters)
            .manifests(true)
            .build();
        let images = self
            .docker
            .client
            .list_images(Some(options))
            .await?
            .into_iter()
            .map(project_image)
            .collect();
        let info = self.docker.client.info().await?;
        Ok(MachineImages {
            containerd_store: info.driver_status.as_deref().is_some_and(containerd_store),
            images,
        })
    }

    pub async fn start_unregistry(
        &self,
        gateway: Ipv4Addr,
        configured_socket: Option<&Path>,
    ) -> Result<Option<RunningUnregistry>, Error> {
        self.docker
            .start_unregistry(gateway, configured_socket)
            .await
    }

    pub async fn list_managed(
        &self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Error> {
        let mut observations = Vec::new();
        for container_id in self.docker.managed_container_ids().await? {
            match self.inspect_managed(&container_id, machine_id).await {
                Ok(observation) => observations.push(observation),
                Err(error) if malformed_container(&error) => {
                    eprintln!("ignoring malformed managed container {container_id}: {error}");
                }
                Err(error) => return Err(error),
            }
        }
        Ok(observations)
    }

    pub async fn inspect_managed(
        &self,
        container_id: &ContainerId,
        machine_id: &MachineId,
    ) -> Result<ContainerObservation, Error> {
        let inspected = match self
            .docker
            .client
            .inspect_container(container_id.as_str(), None)
            .await
        {
            Ok(inspected) => RawContainerInspect::from_typed(&inspected)?,
            Err(bollard::errors::Error::JsonDataError { contents, .. }) => {
                serde_json::from_str(&contents)?
            }
            Err(error) => return Err(docker_error(container_id, error)),
        };
        let address = container_address(&inspected);
        let labels = inspected
            .config
            .as_ref()
            .and_then(|config| config.labels.as_ref())
            .ok_or(Error::MissingField("container labels"))?;
        let managed = ManagedLabels::parse(labels)?;
        let resolved_spec = self
            .specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(*container_id))?;

        Ok(ContainerObservation {
            container_id: *container_id,
            display_name: display_name(inspected.name.as_deref()),
            created_at_unix_nanos: inspected
                .created
                .as_deref()
                .and_then(|created| chrono::DateTime::parse_from_rfc3339(created).ok())
                .and_then(|created| created.timestamp_nanos_opt())
                .unwrap_or_default(),
            machine_id: *machine_id,
            service_id: managed.service_id,
            service_name: managed.service_name,
            kind: managed.kind,
            runtime: runtime_observation(inspected.state.as_ref()),
            effective_healthcheck: effective_healthcheck(inspected.config.as_ref()),
            resolved_spec,
            address,
            labels: labels
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        })
    }
}

#[allow(clippy::wildcard_enum_match_arm)]
fn docker_error(container_id: &ContainerId, error: bollard::errors::Error) -> Error {
    match error {
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404, ..
        } => Error::ContainerNotFound(*container_id),
        error => Error::Docker(error),
    }
}

fn malformed_container(error: &Error) -> bool {
    matches!(
        error,
        Error::Json(_)
            | Error::SpecStore(SpecStoreError::Json(_))
            | Error::MissingField(_)
            | Error::MissingLabel(_)
            | Error::InvalidValue { .. }
            | Error::NotManaged
            | Error::SpecNotFound(_)
            | Error::ContainerNotFound(_)
    )
}

fn some_map<K, V>(values: impl IntoIterator<Item = (K, V)>) -> Option<HashMap<K, V>>
where
    K: Eq + std::hash::Hash,
{
    let values = values.into_iter().collect::<HashMap<_, _>>();
    (!values.is_empty()).then_some(values)
}

fn project_image(image: bollard::models::ImageSummary) -> ImageSummary {
    let mut platforms = image
        .manifests
        .unwrap_or_default()
        .into_iter()
        .filter(|manifest| {
            manifest.available && manifest.kind == Some(ImageManifestSummaryKindEnum::IMAGE)
        })
        .filter_map(|manifest| {
            let platform = manifest.image_data?.platform;
            let mut value = format!("{}/{}", platform.os?, platform.architecture?);
            if let Some(variant) = platform.variant {
                value.push('/');
                value.push_str(&variant);
            }
            Some(value)
        })
        .collect::<Vec<_>>();
    platforms.sort();
    platforms.dedup();
    ImageSummary {
        id: image.id,
        repo_tags: image.repo_tags,
        created: image.created,
        size: image.size,
        containers: image.containers,
        platforms,
    }
}

fn containerd_store(status: &[Vec<String>]) -> bool {
    status.iter().flatten().any(|value| {
        value.eq_ignore_ascii_case("io.containerd.snapshotter.v1")
            || value.to_ascii_lowercase().contains("containerd")
    })
}

fn display_name(name: Option<&str>) -> String {
    name.unwrap_or_default().trim_start_matches('/').to_owned()
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawContainerInspect {
    name: Option<String>,
    created: Option<String>,
    config: Option<RawContainerConfig>,
    state: Option<serde_json::Value>,
    network_settings: Option<RawNetworkSettings>,
}

impl RawContainerInspect {
    fn from_typed(inspected: &ContainerInspectResponse) -> Result<Self, serde_json::Error> {
        Ok(Self {
            name: inspected.name.clone(),
            created: inspected.created.clone(),
            config: inspected.config.as_ref().map(|config| RawContainerConfig {
                labels: config.labels.clone(),
                healthcheck: config.healthcheck.clone(),
            }),
            state: inspected
                .state
                .as_ref()
                .map(serde_json::to_value)
                .transpose()?,
            network_settings: Some(RawNetworkSettings {
                networks: inspected
                    .network_settings
                    .as_ref()
                    .and_then(|settings| settings.networks.as_ref())
                    .map(|networks| {
                        networks
                            .iter()
                            .map(|(name, endpoint)| {
                                (
                                    name.clone(),
                                    RawEndpointSettings {
                                        ip_address: endpoint.ip_address.clone(),
                                    },
                                )
                            })
                            .collect()
                    }),
            }),
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawContainerConfig {
    labels: Option<HashMap<String, String>>,
    healthcheck: Option<HealthConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawNetworkSettings {
    networks: Option<HashMap<String, RawEndpointSettings>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawEndpointSettings {
    ip_address: Option<String>,
}

struct ManagedLabels {
    service_id: ServiceId,
    service_name: ServiceName,
    kind: ContainerKind,
}

impl ManagedLabels {
    fn parse(labels: &HashMap<String, String>) -> Result<Self, Error> {
        if !labels.contains_key(LABEL_MANAGED) {
            return Err(Error::NotManaged);
        }
        let service_id = required_label(labels, LABEL_SERVICE_ID)?;
        let service_name = required_label(labels, LABEL_SERVICE_NAME)?;
        let kind = match labels.get(LABEL_HOOK) {
            None => ContainerKind::ServiceContainer,
            Some(_) => ContainerKind::PreDeployHook,
        };
        Ok(Self {
            service_id: ServiceId::parse(service_id).map_err(|source| Error::InvalidValue {
                field: LABEL_SERVICE_ID,
                source,
            })?,
            service_name: ServiceName::parse(service_name).map_err(|source| {
                Error::InvalidValue {
                    field: LABEL_SERVICE_NAME,
                    source,
                }
            })?,
            kind,
        })
    }
}

fn required_label<'a>(
    labels: &'a HashMap<String, String>,
    name: &'static str,
) -> Result<&'a str, Error> {
    labels
        .get(name)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
        .ok_or(Error::MissingLabel(name))
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectState {
    #[serde(default)]
    status: String,
    exit_code: Option<i64>,
    health: Option<InspectHealth>,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct InspectHealth {
    status: Option<String>,
}

fn runtime_observation(state: Option<&serde_json::Value>) -> ContainerRuntimeObservation {
    let Some(state) = state else {
        return ContainerRuntimeObservation::Unknown {
            raw: json!({ "state": null }),
        };
    };
    let parsed = InspectState::deserialize(state).unwrap_or_default();
    runtime_from_parts_with_raw(
        &parsed.status,
        parsed.exit_code,
        parsed
            .health
            .as_ref()
            .and_then(|health| health.status.as_deref()),
        state,
    )
}

fn effective_healthcheck(config: Option<&RawContainerConfig>) -> Option<HealthcheckSpec> {
    let healthcheck = config?.healthcheck.as_ref()?;
    let test = healthcheck.test.clone().unwrap_or_default();
    if test
        .first()
        .is_some_and(|command| command == HEALTHCHECK_DISABLE_SENTINEL)
    {
        return Some(HealthcheckSpec::Disabled);
    }
    Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
        test: HealthcheckCommand::parse(test).ok()?,
        interval_millis: nanos_to_millis(healthcheck.interval),
        timeout_millis: nanos_to_millis(healthcheck.timeout),
        start_period_millis: nanos_to_millis(healthcheck.start_period),
        start_interval_millis: nanos_to_millis(healthcheck.start_interval),
        retries: healthcheck
            .retries
            .filter(|retries| *retries > 0)
            .and_then(|retries| u32::try_from(retries).ok()),
    }))
}

fn nanos_to_millis(nanos: Option<i64>) -> Option<u64> {
    nanos
        .filter(|nanos| *nanos > 0)
        .and_then(|nanos| u64::try_from(nanos).ok())
        .map(|nanos| nanos.div_ceil(1_000_000))
}

#[cfg(test)]
fn runtime_from_parts(
    status: &str,
    exit_code: Option<i64>,
    health: Option<&str>,
) -> ContainerRuntimeObservation {
    runtime_from_parts_with_raw(
        status,
        exit_code,
        health,
        &json!({ "Status": status, "ExitCode": exit_code, "Health": health }),
    )
}

fn runtime_from_parts_with_raw(
    status: &str,
    exit_code: Option<i64>,
    health: Option<&str>,
    raw: &serde_json::Value,
) -> ContainerRuntimeObservation {
    match status {
        "created" => ContainerRuntimeObservation::Created,
        "running" => ContainerRuntimeObservation::Running {
            health: match health {
                None | Some("none") => HealthObservation::NotConfigured,
                Some("starting") => HealthObservation::Starting,
                Some("healthy") => HealthObservation::Healthy,
                Some("unhealthy") => HealthObservation::Unhealthy,
                Some(value) => HealthObservation::Unrecognized(value.to_owned()),
            },
        },
        "paused" => ContainerRuntimeObservation::Paused,
        "restarting" => ContainerRuntimeObservation::Restarting,
        "exited" if exit_code.is_some() => ContainerRuntimeObservation::Exited {
            code: exit_code.expect("checked"),
        },
        "removing" => ContainerRuntimeObservation::Removing,
        "dead" => ContainerRuntimeObservation::Dead,
        _ => ContainerRuntimeObservation::Unknown { raw: raw.clone() },
    }
}

fn container_address(inspected: &RawContainerInspect) -> Option<ContainerAddress> {
    inspected
        .network_settings
        .as_ref()?
        .networks
        .as_ref()?
        .get(crate::network::DOCKER_NETWORK_NAME)?
        .ip_address
        .as_deref()?
        .parse::<Ipv4Addr>()
        .ok()
        .map(ContainerAddress)
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("Docker operation failed: {0}")]
    Docker(#[from] bollard::errors::Error),
    #[error("Docker inspect JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Network(#[from] crate::network::NetworkError),
    #[error(transparent)]
    SpecStore(#[from] SpecStoreError),
    #[error(transparent)]
    ReplicatedStore(#[from] crate::corrosion::Error),
    #[error("managed container is missing {0}")]
    MissingField(&'static str),
    #[error("managed container is missing label {0}")]
    MissingLabel(&'static str),
    #[error("managed container has invalid {field}: {source}")]
    InvalidValue {
        field: &'static str,
        #[source]
        source: ValueError,
    },
    #[error("invalid container configuration: {0}")]
    InvalidContainerConfig(String),
    #[error("container is not managed by Ployz")]
    NotManaged,
    #[error("resolved spec not found in machine.db for {0}")]
    SpecNotFound(ContainerId),
    #[error("container {0} was not found")]
    ContainerNotFound(ContainerId),
    #[error("pre-deploy container requested without a pre-deploy hook")]
    MissingPreDeployHook,
    #[error("config {0:?} referenced by a mount was not found")]
    ConfigNotFound(String),
    #[error("Volume {0:?} referenced by a mount was not found")]
    VolumeNotFound(String),
    #[error("invalid bind propagation: {0}")]
    InvalidMountPropagation(String),
    #[error("container duration exceeds Docker's range")]
    DurationOverflow,
    #[error("container size exceeds Docker's range")]
    SizeOverflow,
    #[error("Docker event stream closed")]
    EventStreamClosed,
    #[error("local Machine record lock poisoned")]
    LocalStorePoisoned,
    #[error("system clock cannot be represented for Docker event replay: {0}")]
    Clock(String),
}

impl Error {
    #[must_use]
    pub const fn rpc_code(&self) -> RpcErrorCode {
        match self {
            Self::ContainerNotFound(_)
            | Self::SpecNotFound(_)
            | Self::Docker(bollard::errors::Error::DockerResponseServerError {
                status_code: 404,
                ..
            }) => RpcErrorCode::NotFound,
            Self::Docker(bollard::errors::Error::DockerResponseServerError {
                status_code: 409,
                ..
            }) => RpcErrorCode::Conflict,
            Self::MissingPreDeployHook
            | Self::ConfigNotFound(_)
            | Self::VolumeNotFound(_)
            | Self::InvalidMountPropagation(_)
            | Self::DurationOverflow
            | Self::SizeOverflow
            | Self::InvalidContainerConfig(_)
            | Self::SpecStore(SpecStoreError::InvalidConfig(_))
            | Self::MissingField(_)
            | Self::MissingLabel(_)
            | Self::InvalidValue { .. }
            | Self::NotManaged => RpcErrorCode::InvalidArgument,
            Self::Docker(_)
            | Self::Json(_)
            | Self::Network(_)
            | Self::SpecStore(_)
            | Self::ReplicatedStore(_)
            | Self::EventStreamClosed
            | Self::LocalStorePoisoned
            | Self::Clock(_) => RpcErrorCode::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{MachineGateway, ResolvedServiceSpec};

    use super::*;
    use bollard::models::{
        ImageManifestSummary, ImageManifestSummaryImageData, ImageManifestSummaryKindEnum,
        OciPlatform,
    };

    #[test]
    fn image_projection_keeps_only_available_runnable_platforms_sorted() {
        let manifest = |available, kind, os: &str, architecture: &str| ImageManifestSummary {
            available,
            kind: Some(kind),
            image_data: Some(ImageManifestSummaryImageData {
                platform: OciPlatform {
                    os: Some(os.into()),
                    architecture: Some(architecture.into()),
                    ..Default::default()
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let image = bollard::models::ImageSummary {
            id: "sha256:abc".into(),
            repo_tags: vec!["example.test/api:1".into()],
            created: 1,
            size: 2,
            containers: 3,
            manifests: Some(vec![
                manifest(true, ImageManifestSummaryKindEnum::IMAGE, "linux", "arm64"),
                manifest(
                    true,
                    ImageManifestSummaryKindEnum::ATTESTATION,
                    "linux",
                    "amd64",
                ),
                manifest(false, ImageManifestSummaryKindEnum::IMAGE, "linux", "386"),
                manifest(true, ImageManifestSummaryKindEnum::IMAGE, "linux", "amd64"),
            ]),
            ..Default::default()
        };

        assert_eq!(
            project_image(image).platforms,
            ["linux/amd64", "linux/arm64"]
        );
        assert!(containerd_store(&[vec![
            "driver-type".into(),
            "io.containerd.snapshotter.v1".into()
        ]]));
    }

    #[test]
    fn create_body_translates_runtime_fields_and_hook_overrides() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let gateway = MachineGateway(Ipv4Addr::new(10, 210, 0, 1));
        let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "alpine:3.23.3",
                "command": ["serve"],
                "environment": { "TOKEN": "service" },
                "pull_policy": "missing",
                "privileged": false,
                "resources": { "memory_bytes": 1048576 },
                "healthcheck": { "state": "configured", "test": ["CMD", "true"], "interval_millis": 1000 }
            },
            "ports": [{
                "mode": "host",
                "bind": { "kind": "address", "address": "127.0.0.1" },
                "published_port": 8080,
                "container_port": 80,
                "transport_protocol": "tcp"
            }],
            "pre_deploy": {
                "command": ["migrate"],
                "environment": {
                    "TOKEN": "hook",
                    "PLOYZ_MACHINE_ID": "not-the-machine"
                },
                "privileged": true,
                "user": "1000"
            }
        }))
        .unwrap();

        let regular = create::container_create_body(
            &machine_id,
            gateway,
            ContainerKind::ServiceContainer,
            &spec,
        )
        .unwrap();
        assert!(
            regular
                .env
                .as_ref()
                .unwrap()
                .contains(&format!("PLOYZ_MACHINE_ID={machine_id}"))
        );
        assert_eq!(regular.cmd, Some(vec!["serve".into()]));
        let regular_host = regular.host_config.unwrap();
        assert_eq!(regular_host.dns, Some(vec![gateway.0.to_string()]));
        assert_eq!(regular_host.dns_search, Some(vec!["internal".into()]));
        assert_eq!(regular_host.dns_options, Some(vec!["ndots:1".into()]));
        assert_eq!(regular_host.memory, Some(1_048_576));
        assert_eq!(
            regular_host.restart_policy.unwrap().name,
            Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED)
        );
        assert!(regular_host.port_bindings.is_some());

        let hook = create::container_create_body(
            &machine_id,
            gateway,
            ContainerKind::PreDeployHook,
            &spec,
        )
        .unwrap();
        assert_eq!(hook.cmd, Some(vec!["migrate".into()]));
        assert_eq!(hook.user.as_deref(), Some("1000"));
        let hook_env = hook.env.as_ref().unwrap();
        assert!(hook_env.contains(&"TOKEN=hook".into()));
        assert!(hook_env.contains(&format!("PLOYZ_MACHINE_ID={machine_id}")));
        assert!(!hook_env.contains(&"PLOYZ_MACHINE_ID=not-the-machine".into()));
        assert!(hook_env.contains(&"PLOYZ_HOOK_PRE_DEPLOY=true".into()));
        assert_eq!(hook.healthcheck.unwrap().test, Some(vec!["NONE".into()]));
        let hook_host = hook.host_config.unwrap();
        assert_eq!(hook_host.dns, Some(vec![gateway.0.to_string()]));
        assert_eq!(hook_host.dns_search, Some(vec!["internal".into()]));
        assert_eq!(hook_host.dns_options, Some(vec!["ndots:1".into()]));
        assert_eq!(hook_host.privileged, Some(true));
        assert!(hook_host.port_bindings.is_none());
        assert_eq!(
            hook_host.restart_policy.unwrap().name,
            Some(bollard::models::RestartPolicyNameEnum::NO)
        );
    }

    #[test]
    fn create_body_maps_docker_restart_pid_and_stop_timeout() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let gateway = MachineGateway(Ipv4Addr::new(10, 210, 0, 1));
        let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "alpine:3.23.3",
                "pull_policy": "missing",
                "pid_mode": "container:abc",
                "stop_timeout_secs": 1,
                "restart": { "name": "on-failure", "maximum_retry_count": 3 }
            }
        }))
        .unwrap();
        let body = create::container_create_body(
            &machine_id,
            gateway,
            ContainerKind::ServiceContainer,
            &spec,
        )
        .unwrap();
        assert_eq!(body.stop_timeout, Some(1));
        let host = body.host_config.unwrap();
        let restart = host.restart_policy.unwrap();
        assert_eq!(
            restart.name,
            Some(bollard::models::RestartPolicyNameEnum::ON_FAILURE)
        );
        assert_eq!(restart.maximum_retry_count, Some(3));
        assert_eq!(host.pid_mode.as_deref(), Some("container:abc"));
    }

    #[test]
    fn run_service_container_does_not_restart_after_exit_and_keeps_bind_mounts() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
        let gateway = MachineGateway(Ipv4Addr::new(10, 210, 0, 1));
        let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "a".repeat(32),
            "name": "oneshot",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": {
                "image": "alpine:3.23.3",
                "command": ["cat", "/host-os"],
                "pull_policy": "missing",
                "restart": { "name": "no" }
            },
            "volumes": [{
                "reference": "host",
                "source": { "kind": "bind", "machine_path": "/etc/os-release" }
            }],
            "mounts": [{ "volume": "host", "target": "/host-os" }]
        }))
        .unwrap();

        let body = create::container_create_body(
            &machine_id,
            gateway,
            ContainerKind::ServiceContainer,
            &spec,
        )
        .unwrap();
        let host = body.host_config.unwrap();
        assert_eq!(
            host.restart_policy.unwrap().name,
            Some(bollard::models::RestartPolicyNameEnum::NO)
        );
        let mounts = host.mounts.expect("bind mount must still be applied");
        let [bind_mount] = mounts.as_slice() else {
            panic!("expected one bind mount: {mounts:?}")
        };
        assert_eq!(bind_mount.typ, Some(bollard::models::MountType::BIND));
        assert_eq!(bind_mount.source.as_deref(), Some("/etc/os-release"));
        assert_eq!(bind_mount.target.as_deref(), Some("/host-os"));
    }

    #[test]
    fn resolved_mounts_map_bind_named_alias_and_tmpfs_without_inventing_defaults() {
        use bollard::models::MountType;
        use ployz_core::ResolvedServiceSpec;

        let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "11111111111111111111111111111111",
            "name": "api",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" },
            "volumes": [
                {"reference":"host","source":{"kind":"bind","machine_path":"/srv/api"}},
                {"reference":"alias","source":{"kind":"named","name":"database","driver":{"name":"local","options":{"type":"none"}},"labels":{"purpose":"db"}}},
                {"reference":"memory","source":{"kind":"tmpfs","size_bytes":4096,"mode":448}}
            ],
            "mounts": [
                {"volume":"host","target":"/host"},
                {"volume":"alias","target":"/data","read_only":true},
                {"volume":"memory","target":"/run/cache"}
            ]
        }))
        .unwrap();

        let mounts = docker_mounts(&spec.volumes, &spec.mounts).unwrap();
        let [bind_mount, named_mount, tmpfs_mount] = mounts.as_slice() else {
            panic!("expected three mounts: {mounts:?}")
        };
        assert_eq!(bind_mount.typ, Some(MountType::BIND));
        assert_eq!(bind_mount.source.as_deref(), Some("/srv/api"));
        let bind = bind_mount.bind_options.as_ref().unwrap();
        assert!(bind.propagation.is_none());
        assert!(bind.non_recursive.is_none());
        assert!(bind.read_only_non_recursive.is_none());
        assert!(bind.read_only_force_recursive.is_none());

        for (recursive, expected) in [
            (
                ployz_core::BindRecursive::Disabled,
                (Some(true), None, None),
            ),
            (
                ployz_core::BindRecursive::Writable,
                (None, Some(true), None),
            ),
            (
                ployz_core::BindRecursive::Readonly,
                (None, None, Some(true)),
            ),
        ] {
            let mut recursive_spec = spec.clone();
            let ployz_core::VolumeSource::Bind {
                recursive: setting, ..
            } = &mut recursive_spec
                .volumes
                .first_mut()
                .expect("fixture has a bind volume")
                .source
            else {
                panic!("expected bind volume")
            };
            *setting = Some(recursive);
            let translated = docker_mounts(&recursive_spec.volumes, &recursive_spec.mounts)
                .unwrap()
                .remove(0)
                .bind_options
                .unwrap();
            assert_eq!(
                (
                    translated.non_recursive,
                    translated.read_only_non_recursive,
                    translated.read_only_force_recursive,
                ),
                expected
            );
        }
        assert_eq!(named_mount.typ, Some(MountType::VOLUME));
        assert_eq!(named_mount.source.as_deref(), Some("database"));
        assert_eq!(named_mount.read_only, Some(true));
        assert_eq!(
            named_mount
                .volume_options
                .as_ref()
                .unwrap()
                .driver_config
                .as_ref()
                .unwrap()
                .name
                .as_deref(),
            Some("local")
        );
        assert_eq!(tmpfs_mount.typ, Some(MountType::TMPFS));
        assert!(tmpfs_mount.source.is_none());
        assert_eq!(
            tmpfs_mount.tmpfs_options.as_ref().unwrap().size_bytes,
            Some(4096)
        );

        let mut missing = spec;
        missing.mounts.first_mut().unwrap().volume =
            ployz_core::ServiceVolumeReference::parse("missing").unwrap();
        assert!(matches!(
            docker_mounts(&missing.volumes, &missing.mounts),
            Err(Error::VolumeNotFound(reference)) if reference == "missing"
        ));
    }

    #[test]
    fn container_resources_map_to_docker_without_dropping_limits() {
        let resources = serde_json::from_value(serde_json::json!({
            "cpu_nanos": 500_000_000,
            "memory_bytes": 104_857_600,
            "memory_reservation_bytes": 52_428_800,
            "shared_memory_bytes": 26_214_400,
            "devices": [{
                "machine_path": "/dev/fuse",
                "container_path": "/dev/fuse",
                "cgroup_permissions": "rwm"
            }],
            "device_reservations": [{
                "driver": "nvidia",
                "count": -1,
                "capabilities": [["gpu"]],
                "options": {"virtualization": "false"}
            }],
            "ulimits": {"nofile": {"soft": 20_000, "hard": 40_000}}
        }))
        .unwrap();

        let mapped = docker_resources(&resources);
        assert_eq!(mapped.nano_cpus, Some(500_000_000));
        assert_eq!(mapped.memory, Some(104_857_600));
        assert_eq!(mapped.memory_reservation, Some(52_428_800));
        assert_eq!(mapped.shm_size, Some(26_214_400));
        let [device] = mapped.devices.as_deref().unwrap() else {
            panic!("expected one Docker device: {mapped:?}")
        };
        assert_eq!(device.path_on_host.as_deref(), Some("/dev/fuse"));
        assert_eq!(device.path_in_container.as_deref(), Some("/dev/fuse"));
        assert_eq!(device.cgroup_permissions.as_deref(), Some("rwm"));
        let [request] = mapped.device_requests.as_deref().unwrap() else {
            panic!("expected one Docker device request: {mapped:?}")
        };
        assert_eq!(request.driver.as_deref(), Some("nvidia"));
        assert_eq!(request.count, Some(-1));
        assert_eq!(request.capabilities, Some(vec![vec!["gpu".into()]]));
        assert_eq!(
            request
                .options
                .as_ref()
                .and_then(|options| options.get("virtualization"))
                .map(String::as_str),
            Some("false")
        );
        let [ulimit] = mapped.ulimits.as_deref().unwrap() else {
            panic!("expected one Docker ulimit: {mapped:?}")
        };
        assert_eq!(ulimit.name.as_deref(), Some("nofile"));
        assert_eq!((ulimit.soft, ulimit.hard), (Some(20_000), Some(40_000)));
    }

    #[test]
    fn host_ports_and_healthchecks_map_without_losing_bind_or_timing_details() {
        let ports: Vec<ployz_core::PortPublication> = serde_json::from_value(serde_json::json!([
            {
                "mode":"host",
                "bind":{"kind":"all"},
                "published_port":8080,
                "container_port":80,
                "transport_protocol":"tcp"
            },
            {
                "mode":"host",
                "bind":{"kind":"prefix","prefix":"192.0.2.0/24"},
                "published_port":5353,
                "container_port":53,
                "transport_protocol":"udp"
            },
            {
                "mode":"ingress",
                "hostname":"api.example.com",
                "load_balancer_port":443,
                "container_port":8443,
                "http_protocol":"https"
            }
        ]))
        .unwrap();
        let (exposed, bindings) = docker_ports(
            &ports,
            &[
                "192.0.2.4".parse().unwrap(),
                "198.51.100.8".parse().unwrap(),
            ],
        )
        .unwrap();
        assert_eq!(exposed.unwrap(), ["53/udp", "80/tcp"]);
        let bindings = bindings.unwrap();
        let tcp = bindings
            .get("80/tcp")
            .and_then(Option::as_ref)
            .and_then(|bindings| bindings.first())
            .unwrap();
        let udp = bindings
            .get("53/udp")
            .and_then(Option::as_ref)
            .and_then(|bindings| bindings.first())
            .unwrap();
        assert_eq!(tcp.host_ip, None);
        assert_eq!(udp.host_ip.as_deref(), Some("192.0.2.4"));
        assert_eq!(udp.host_port.as_deref(), Some("5353"));

        let healthcheck = serde_json::from_value(serde_json::json!({
            "state":"configured",
            "test":["CMD","true"],
            "interval_millis":1000,
            "timeout_millis":2000,
            "start_period_millis":3000,
            "start_interval_millis":4000,
            "retries":5
        }))
        .unwrap();
        let mapped = docker_healthcheck(&healthcheck).unwrap();
        assert_eq!(mapped.test.unwrap(), ["CMD", "true"]);
        assert_eq!(mapped.interval, Some(1_000_000_000));
        assert_eq!(mapped.timeout, Some(2_000_000_000));
        assert_eq!(mapped.start_period, Some(3_000_000_000));
        assert_eq!(mapped.start_interval, Some(4_000_000_000));
        assert_eq!(mapped.retries, Some(5));

        let disabled = docker_healthcheck(&ployz_core::HealthcheckSpec::Disabled).unwrap();
        assert_eq!(disabled.test.unwrap(), ["NONE"]);
        assert_eq!(disabled.interval, None);
        assert_eq!(disabled.timeout, None);
        assert_eq!(disabled.retries, None);
    }

    #[test]
    fn managed_labels_distinguish_service_and_hook_containers() {
        assert!(matches!(
            ManagedLabels::parse(&HashMap::new()),
            Err(Error::NotManaged)
        ));
        let mut labels = HashMap::from([
            (LABEL_MANAGED.to_owned(), String::new()),
            (LABEL_SERVICE_ID.to_owned(), "a".repeat(32)),
            (LABEL_SERVICE_NAME.to_owned(), "api".to_owned()),
        ]);
        assert_eq!(
            ManagedLabels::parse(&labels).unwrap().kind,
            ContainerKind::ServiceContainer
        );
        labels.insert(LABEL_HOOK.to_owned(), LABEL_HOOK_PRE_DEPLOY.to_owned());
        assert_eq!(
            ManagedLabels::parse(&labels).unwrap().kind,
            ContainerKind::PreDeployHook
        );
        labels.insert(LABEL_HOOK.to_owned(), "future-hook".to_owned());
        assert_eq!(
            ManagedLabels::parse(&labels).unwrap().kind,
            ContainerKind::PreDeployHook
        );
    }

    #[test]
    fn runtime_and_health_keep_unknown_values_explicit() {
        assert_eq!(
            runtime_from_parts("created", None, None),
            ContainerRuntimeObservation::Created
        );
        assert_eq!(
            runtime_from_parts("running", None, None),
            ContainerRuntimeObservation::Running {
                health: HealthObservation::NotConfigured
            }
        );
        assert_eq!(
            runtime_from_parts("running", None, Some("degraded")),
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unrecognized("degraded".into())
            }
        );
        for (health, expected) in [
            ("starting", HealthObservation::Starting),
            ("healthy", HealthObservation::Healthy),
            ("unhealthy", HealthObservation::Unhealthy),
        ] {
            assert_eq!(
                runtime_from_parts("running", None, Some(health)),
                ContainerRuntimeObservation::Running { health: expected }
            );
        }
        assert_eq!(
            runtime_from_parts("paused", None, Some("healthy")),
            ContainerRuntimeObservation::Paused
        );
        assert_eq!(
            runtime_from_parts("restarting", None, Some("healthy")),
            ContainerRuntimeObservation::Restarting
        );
        assert_eq!(
            runtime_from_parts("exited", Some(17), None),
            ContainerRuntimeObservation::Exited { code: 17 }
        );
        assert_eq!(
            runtime_from_parts("removing", None, None),
            ContainerRuntimeObservation::Removing
        );
        assert_eq!(
            runtime_from_parts("dead", None, None),
            ContainerRuntimeObservation::Dead
        );
        assert!(matches!(
            runtime_from_parts("stopping", None, None),
            ContainerRuntimeObservation::Unknown { raw }
                if raw.get("Status") == Some(&json!("stopping"))
        ));
        assert!(matches!(
            runtime_from_parts("future", None, Some("future-health")),
            ContainerRuntimeObservation::Unknown { raw }
                if raw.get("Health") == Some(&json!("future-health"))
        ));
    }

    #[test]
    fn raw_docker_inspect_keeps_future_state_and_effective_healthcheck() {
        let inspected: RawContainerInspect = serde_json::from_value(json!({
            "Config": {
                "Healthcheck": {
                    "Test": ["CMD", "true"],
                    "Interval": 1_500_000,
                    "Timeout": 2_000_000,
                    "Retries": 4,
                    "StartPeriod": 300_000_001,
                    "StartInterval": 4_000_000
                }
            },
            "State": {
                "Status": "future-state",
                "ExitCode": 7,
                "Health": { "Status": "future-health" }
            }
        }))
        .unwrap();
        assert!(matches!(
            runtime_observation(inspected.state.as_ref()),
            ContainerRuntimeObservation::Unknown { raw }
                if raw == json!({
                    "Status": "future-state",
                    "ExitCode": 7,
                    "Health": { "Status": "future-health" }
                })
        ));
        assert_eq!(
            effective_healthcheck(inspected.config.as_ref()),
            Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
                test: HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
                interval_millis: Some(2),
                timeout_millis: Some(2),
                start_period_millis: Some(301),
                start_interval_millis: Some(4),
                retries: Some(4),
            }))
        );

        let disabled: RawContainerInspect = serde_json::from_value(json!({
            "Config": { "Healthcheck": { "Test": ["NONE"], "Interval": 1_000_000 } }
        }))
        .unwrap();
        assert_eq!(
            effective_healthcheck(disabled.config.as_ref()),
            Some(HealthcheckSpec::Disabled)
        );

        let inherited: RawContainerInspect = serde_json::from_value(json!({
            "Config": { "Healthcheck": { "Test": [] } }
        }))
        .unwrap();
        assert_eq!(effective_healthcheck(inherited.config.as_ref()), None);
    }
}
