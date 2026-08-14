mod create;
mod lifecycle;
mod service_container;
mod spec_store;
mod volume;

#[cfg(test)]
mod integration_tests;

use std::{collections::HashMap, io, net::Ipv4Addr, time::Duration, time::SystemTime};

use bollard::{
    Docker,
    models::ContainerInspectResponse,
    query_parameters::{EventsOptionsBuilder, ListContainersOptionsBuilder},
};
use futures_util::StreamExt;
use ployz_core::{
    ContainerAddress, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, HealthObservation, MachineId, RpcErrorCode, ServiceId,
    ServiceName, ServiceVolumeReference, ValueError,
};
use serde::Deserialize;
use serde_json::json;
use thiserror::Error;
use tokio::sync::watch;

use crate::corrosion::{LocalContainerSnapshot, ReplicatedStore};

pub use spec_store::{Error as SpecStoreError, MachineSpecStore};

#[cfg(test)]
use service_container::{docker_mounts, docker_resources};

pub const LABEL_MANAGED: &str = "ployz.managed";
pub const LABEL_SERVICE_ID: &str = "ployz.service.id";
pub const LABEL_SERVICE_NAME: &str = "ployz.service.name";
pub const LABEL_HOOK: &str = "ployz.service.hook";
pub const LABEL_HOOK_PRE_DEPLOY: &str = "pre-deploy";
pub const RESCAN_INTERVAL: Duration = Duration::from_secs(30);
const EVENT_DEBOUNCE: Duration = Duration::from_millis(100);
const MAX_HOST_BIND_ADDRESSES: usize = 4096;

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

    pub async fn list_managed(
        &self,
        machine_id: &MachineId,
        specs: &MachineSpecStore,
    ) -> Result<Vec<ContainerObservation>, Error> {
        let mut observations = Vec::new();
        for container_id in self.managed_container_ids().await? {
            match self.inspect_managed(&container_id, machine_id, specs).await {
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
        specs: &MachineSpecStore,
    ) -> Result<ContainerObservation, Error> {
        let inspected = match self
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
        let resolved_spec = specs
            .get(container_id)
            .await?
            .ok_or_else(|| Error::SpecNotFound(container_id.clone()))?;

        Ok(ContainerObservation {
            container_id: container_id.clone(),
            display_name: display_name(inspected.name.as_deref()),
            machine_id: machine_id.clone(),
            service_id: managed.service_id,
            service_name: managed.service_name,
            kind: managed.kind,
            runtime: runtime_observation(inspected.state.as_ref()),
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
        } => Error::ContainerNotFound(container_id.clone()),
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

fn display_name(name: Option<&str>) -> String {
    name.unwrap_or_default().trim_start_matches('/').to_owned()
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawContainerInspect {
    name: Option<String>,
    config: Option<RawContainerConfig>,
    state: Option<serde_json::Value>,
    network_settings: Option<RawNetworkSettings>,
}

impl RawContainerInspect {
    fn from_typed(inspected: &ContainerInspectResponse) -> Result<Self, serde_json::Error> {
        Ok(Self {
            name: inspected.name.clone(),
            config: inspected.config.as_ref().map(|config| RawContainerConfig {
                labels: config.labels.clone(),
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

pub struct ContainerObserver {
    docker: LocalDocker,
    specs: MachineSpecStore,
    replicated: ReplicatedStore,
    machine_id: MachineId,
    rescan_interval: Duration,
}

impl ContainerObserver {
    #[must_use]
    pub fn new(
        docker: LocalDocker,
        specs: MachineSpecStore,
        replicated: ReplicatedStore,
        machine_id: MachineId,
    ) -> Self {
        Self {
            docker,
            specs,
            replicated,
            machine_id,
            rescan_interval: RESCAN_INTERVAL,
        }
    }

    #[cfg(test)]
    fn with_rescan_interval(mut self, interval: Duration) -> Self {
        self.rescan_interval = interval;
        self
    }

    pub async fn run(&self, mut shutdown: watch::Receiver<bool>) -> Result<(), Error> {
        while !*shutdown.borrow() {
            if let Err(error) = self.watch(&mut shutdown).await {
                eprintln!("local Docker observation failed, retrying: {error}");
                tokio::select! {
                    () = tokio::time::sleep(Duration::from_secs(1)) => {}
                    changed = shutdown.changed() => {
                        changed.map_err(|_| Error::ShutdownClosed)?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn watch(&self, shutdown: &mut watch::Receiver<bool>) -> Result<(), Error> {
        let filters = HashMap::from([
            ("type", vec!["container"]),
            ("scope", vec!["local"]),
            ("label", vec![LABEL_MANAGED]),
            (
                "event",
                vec![
                    "create",
                    "start",
                    "stop",
                    "pause",
                    "unpause",
                    "kill",
                    "die",
                    "oom",
                    "destroy",
                    "health_status",
                ],
            ),
        ]);
        let since = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|error| Error::Clock(error.to_string()))?
            .as_secs()
            .to_string();
        let options = EventsOptionsBuilder::default()
            .since(&since)
            .filters(&filters)
            .build();
        // Bollard opens this lazy stream when first polled. The cursor replays any event
        // between capturing `since` and completing the initial snapshot.
        let mut events = Box::pin(self.docker.client.events(Some(options)));
        self.sync_once().await?;

        let mut rescans = tokio::time::interval(self.rescan_interval);
        rescans.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        rescans.tick().await;
        let mut scan_at = None;
        loop {
            tokio::select! {
                event = events.next() => match event {
                    Some(Ok(_)) => {
                        scan_at = Some(tokio::time::Instant::now() + EVENT_DEBOUNCE);
                    }
                    Some(Err(error)) => return Err(error.into()),
                    None => return Err(Error::EventStreamClosed),
                },
                _ = rescans.tick() => self.sync_once().await?,
                () = async {
                    match scan_at {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending().await,
                    }
                } => {
                    scan_at = None;
                    self.sync_once().await?;
                }
                changed = shutdown.changed() => {
                    changed.map_err(|_| Error::ShutdownClosed)?;
                    return Ok(());
                }
            }
        }
    }

    async fn sync_once(&self) -> Result<(), Error> {
        // TODO(UT-120): preserve stale rows when Docker cannot provide a complete inventory.
        let inventory = self.docker.managed_container_ids().await?;
        let mut snapshot = LocalContainerSnapshot::from_inventory(inventory);
        let container_ids = snapshot.ids().cloned().collect::<Vec<_>>();
        for container_id in &container_ids {
            match self
                .docker
                .inspect_managed(container_id, &self.machine_id, &self.specs)
                .await
            {
                Ok(observation) => {
                    snapshot.observed(observation);
                }
                Err(error) => {
                    eprintln!("failed to inspect managed container {container_id}: {error}")
                }
            }
        }
        self.replicated
            .reconcile_local_containers(&self.machine_id, &snapshot)
            .await
            .map_err(Error::from)
    }
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

fn runtime_observation(state: Option<&serde_json::Value>) -> ContainerRuntimeObservation {
    let Some(state) = state else {
        return ContainerRuntimeObservation::Unknown {
            raw: json!({ "state": null }),
        };
    };
    let status = state
        .get("Status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let health = state
        .get("Health")
        .and_then(|health| health.get("Status"))
        .and_then(serde_json::Value::as_str);
    let exit_code = state.get("ExitCode").and_then(serde_json::Value::as_i64);
    runtime_from_parts_with_raw(status, exit_code, health, state)
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
    #[error("container archive failed: {0}")]
    Archive(#[from] io::Error),
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
    #[error("mount references an undeclared Service Volume: {0}")]
    UnknownVolumeReference(ServiceVolumeReference),
    #[error("invalid Docker mount: {0}")]
    InvalidMount(String),
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
    #[error("host bind prefix {0} expands beyond 4096 addresses")]
    PortBindPrefixTooLarge(String),
    #[error("Docker event stream closed")]
    EventStreamClosed,
    #[error("observer shutdown channel closed")]
    ShutdownClosed,
    #[error("system clock cannot be represented for Docker event replay: {0}")]
    Clock(String),
}

impl Error {
    #[must_use]
    pub const fn container_rpc_code(&self) -> RpcErrorCode {
        match self {
            Self::ContainerNotFound(_) | Self::SpecNotFound(_) => RpcErrorCode::NotFound,
            Self::MissingPreDeployHook
            | Self::ConfigNotFound(_)
            | Self::VolumeNotFound(_)
            | Self::InvalidMountPropagation(_)
            | Self::DurationOverflow
            | Self::SizeOverflow
            | Self::PortBindPrefixTooLarge(_)
            | Self::UnknownVolumeReference(_)
            | Self::InvalidMount(_)
            | Self::MissingField(_)
            | Self::MissingLabel(_)
            | Self::InvalidValue { .. }
            | Self::NotManaged => RpcErrorCode::InvalidArgument,
            Self::Docker(_)
            | Self::Json(_)
            | Self::Archive(_)
            | Self::SpecStore(_)
            | Self::ReplicatedStore(_)
            | Self::EventStreamClosed
            | Self::ShutdownClosed
            | Self::Clock(_) => RpcErrorCode::Internal,
        }
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::ResolvedServiceSpec;

    use super::*;

    #[test]
    fn create_body_translates_runtime_fields_and_hook_overrides() {
        let machine_id = MachineId::parse("1".repeat(32)).unwrap();
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
                "healthcheck": { "test": ["CMD", "true"], "interval_millis": 1000 }
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

        let regular =
            create::container_create_body(&machine_id, ContainerKind::ServiceContainer, &spec)
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
        assert_eq!(regular_host.memory, Some(1_048_576));
        assert_eq!(
            regular_host.restart_policy.unwrap().name,
            Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED)
        );
        assert!(regular_host.port_bindings.is_some());

        let hook = create::container_create_body(&machine_id, ContainerKind::PreDeployHook, &spec)
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
        assert_eq!(hook_host.privileged, Some(true));
        assert!(hook_host.port_bindings.is_none());
        assert_eq!(
            hook_host.restart_policy.unwrap().name,
            Some(bollard::models::RestartPolicyNameEnum::NO)
        );
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

        let mounts = docker_mounts(&spec).unwrap();
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
            docker_mounts(&missing),
            Err(Error::UnknownVolumeReference(reference)) if reference.as_str() == "missing"
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
    fn raw_docker_inspect_keeps_future_state_and_health_values() {
        let inspected: RawContainerInspect = serde_json::from_value(json!({
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
    }
}
