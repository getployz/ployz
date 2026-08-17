use std::{
    collections::BTreeMap,
    net::IpAddr,
    num::{NonZeroU16, NonZeroU32},
};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use super::{ServiceConfigGraph, ServiceSpecGraphError, ServiceVolumeGraph};
use crate::{
    BindPropagation, BindRecursive, ContainerPath, DockerVolumeId, DockerVolumeName, IngressHost,
    MachinePath, MachineTarget, PidMode, RestartPolicy, ServiceId, ServiceName,
    ServiceVolumeReference, ValueError,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum ServiceMode {
    Replicated { replicas: NonZeroU32 },
    Global,
}

#[must_use]
pub fn same_service_mode_kind(left: &ServiceMode, right: &ServiceMode) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpProtocol {
    Http,
    Https,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportProtocol {
    Tcp,
    Udp,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum HostBind {
    All,
    Address { address: IpAddr },
    Prefix { prefix: IpNet },
}

/// How an HTTP ingress publication obtains its hostname.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum IngressHostname {
    AssignFromClusterDomain,
    Explicit { hostname: IngressHost },
}

impl IngressHostname {
    /// Parse a non-empty validated hostname as explicit ingress intent.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when `hostname` is empty or not a lowercase DNS hostname.
    pub fn explicit(hostname: impl Into<String>) -> Result<Self, ValueError> {
        Ok(Self::Explicit {
            hostname: IngressHost::parse(hostname)?,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum PortPublication {
    Ingress {
        hostname: IngressHostname,
        load_balancer_port: NonZeroU16,
        container_port: NonZeroU16,
        http_protocol: HttpProtocol,
    },
    Host {
        bind: HostBind,
        published_port: NonZeroU16,
        container_port: NonZeroU16,
        transport_protocol: TransportProtocol,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum VolumeSource {
    Bind {
        machine_path: MachinePath,
        #[serde(default)]
        create_machine_path: bool,
        #[serde(default)]
        propagation: Option<BindPropagation>,
        #[serde(default)]
        recursive: Option<BindRecursive>,
    },
    Named {
        name: DockerVolumeName,
        #[serde(default)]
        external: bool,
        #[serde(default)]
        driver: Option<VolumeDriver>,
        #[serde(default)]
        labels: BTreeMap<String, String>,
        #[serde(default)]
        no_copy: bool,
        #[serde(default)]
        subpath: Option<String>,
    },
    Tmpfs {
        #[serde(default)]
        size_bytes: Option<u64>,
        #[serde(default)]
        mode: Option<u32>,
        #[serde(default)]
        options: Vec<Vec<String>>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VolumeDriver {
    pub name: String,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

/// One Docker Volume observed on one Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DockerVolume {
    pub id: DockerVolumeId,
    pub driver: String,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

/// A storage source declared under a service-local reference.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceVolume {
    pub reference: ServiceVolumeReference,
    pub source: VolumeSource,
}

/// A container mount that refers to a declared Service Volume by its local name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceMount {
    pub volume: ServiceVolumeReference,
    pub target: ContainerPath,
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfigSpec {
    pub name: String,
    #[serde(default)]
    pub content: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfigMount {
    pub config_name: String,
    #[serde(default)]
    pub target: Option<ContainerPath>,
    #[serde(default)]
    pub uid: Option<u64>,
    #[serde(default)]
    pub gid: Option<u64>,
    #[serde(default)]
    pub mode: Option<u32>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Placement {
    /// Machine Targets. An empty list remains every eligible Machine.
    #[serde(default)]
    pub machines: Vec<MachineTarget>,
}

/// Docker's healthcheck disable token. Configured commands cannot begin with it.
pub const HEALTHCHECK_DISABLE_SENTINEL: &str = "NONE";

/// A present Healthcheck: explicitly disabled, or configured with a real command.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum HealthcheckSpec {
    Disabled,
    Configured(ConfiguredHealthcheck),
}

impl HealthcheckSpec {
    /// Borrow the Configured payload, if this Healthcheck is Configured.
    #[must_use]
    pub fn as_configured(&self) -> Option<&ConfiguredHealthcheck> {
        match self {
            Self::Configured(configured) => Some(configured),
            Self::Disabled => None,
        }
    }
}

/// A Healthcheck command that is non-empty and does not begin with Docker's disable sentinel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "Vec<String>", into = "Vec<String>")]
pub struct HealthcheckCommand(Vec<String>);

impl HealthcheckCommand {
    /// Parse a Healthcheck command.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when `test` is empty or begins with
    /// [`HEALTHCHECK_DISABLE_SENTINEL`].
    pub fn parse<I, S>(test: I) -> Result<Self, ValueError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let test = test.into_iter().map(Into::into).collect::<Vec<_>>();
        if test.is_empty()
            || test
                .first()
                .is_some_and(|command| command == HEALTHCHECK_DISABLE_SENTINEL)
        {
            return Err(ValueError::new(
                "healthcheck command",
                test.join(" "),
                "a non-empty command that does not begin with NONE",
            ));
        }
        Ok(Self(test))
    }

    /// Borrow the command tokens.
    #[must_use]
    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

impl From<HealthcheckCommand> for Vec<String> {
    fn from(command: HealthcheckCommand) -> Self {
        command.0
    }
}

impl TryFrom<Vec<String>> for HealthcheckCommand {
    type Error = ValueError;

    fn try_from(test: Vec<String>) -> Result<Self, Self::Error> {
        Self::parse(test)
    }
}

/// Timing and command for a Configured Healthcheck.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ConfiguredHealthcheck {
    pub test: HealthcheckCommand,
    #[serde(default)]
    pub interval_millis: Option<u64>,
    #[serde(default)]
    pub timeout_millis: Option<u64>,
    #[serde(default)]
    pub start_period_millis: Option<u64>,
    #[serde(default)]
    pub start_interval_millis: Option<u64>,
    #[serde(default)]
    pub retries: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogDriver {
    pub name: String,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceMapping {
    pub machine_path: MachinePath,
    pub container_path: ContainerPath,
    pub cgroup_permissions: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DeviceReservation {
    #[serde(default)]
    pub driver: Option<String>,
    #[serde(default)]
    pub count: Option<i64>,
    #[serde(default)]
    pub device_ids: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<Vec<String>>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Ulimit {
    pub soft: i64,
    pub hard: i64,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainerResources {
    #[serde(default)]
    pub cpu_nanos: Option<i64>,
    #[serde(default)]
    pub memory_bytes: Option<i64>,
    #[serde(default)]
    pub memory_reservation_bytes: Option<i64>,
    #[serde(default)]
    pub shared_memory_bytes: Option<i64>,
    #[serde(default)]
    pub devices: Vec<DeviceMapping>,
    #[serde(default)]
    pub device_reservations: Vec<DeviceReservation>,
    #[serde(default)]
    pub ulimits: BTreeMap<String, Ulimit>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PreDeployHook {
    pub command: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub privileged: Option<bool>,
    #[serde(default)]
    pub timeout_millis: Option<u64>,
    #[serde(default)]
    pub user: Option<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateConfig {
    /// Absence means derive the order from the deploy snapshot.
    #[serde(default)]
    pub order: Option<UpdateOrder>,
    #[serde(default)]
    pub monitor_millis: Option<u64>,
}

/// Update configuration after deploy-time order resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedUpdateConfig {
    pub order: UpdateOrder,
    #[serde(default)]
    pub monitor_millis: Option<u64>,
}

impl Default for ResolvedUpdateConfig {
    fn default() -> Self {
        Self {
            order: UpdateOrder::StartFirst,
            monitor_millis: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullPolicy {
    Always,
    Missing,
    Never,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateOrder {
    StartFirst,
    StopFirst,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecChange {
    UpToDate,
    NeedsUpdate,
    NeedsRecreate,
}

/// Runtime configuration shared by requested and resolved Service Specs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServiceContainerSpec {
    pub image: String,
    #[serde(default)]
    pub command: Vec<String>,
    #[serde(default)]
    pub entrypoint: Vec<String>,
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    #[serde(default)]
    pub cap_add: Vec<String>,
    #[serde(default)]
    pub cap_drop: Vec<String>,
    #[serde(default)]
    pub healthcheck: Option<HealthcheckSpec>,
    pub pull_policy: PullPolicy,
    #[serde(default)]
    pub init: Option<bool>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub working_directory: Option<ContainerPath>,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub open_stdin: bool,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default)]
    pub pid_mode: Option<PidMode>,
    #[serde(default)]
    pub log_driver: Option<LogDriver>,
    #[serde(default)]
    pub resources: ContainerResources,
    #[serde(default)]
    pub stop_timeout_secs: Option<i64>,
    #[serde(default)]
    pub sysctls: BTreeMap<String, String>,
    #[serde(default)]
    pub restart: RestartPolicy,
}

/// Normalized deploy input before placement and container-specific resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    try_from = "RequestedServiceSpecWire",
    into = "RequestedServiceSpecWire"
)]
pub struct RequestedServiceSpec {
    pub name: ServiceName,
    pub mode: ServiceMode,
    pub container: ServiceContainerSpec,
    pub placement: Placement,
    pub ports: Vec<PortPublication>,
    pub volume_graph: ServiceVolumeGraph,
    pub config_graph: ServiceConfigGraph,
    pub pre_deploy: Option<PreDeployHook>,
    pub caddy_config: Option<String>,
    pub update: UpdateConfig,
}

/// The exact, fully resolved Service Spec attached to one created container.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ResolvedServiceSpecWire", into = "ResolvedServiceSpecWire")]
pub struct ResolvedServiceSpec {
    pub service_id: ServiceId,
    pub name: ServiceName,
    pub mode: ServiceMode,
    pub container: ServiceContainerSpec,
    pub placement: Placement,
    pub ports: Vec<PortPublication>,
    pub volume_graph: ServiceVolumeGraph,
    pub config_graph: ServiceConfigGraph,
    pub pre_deploy: Option<PreDeployHook>,
    pub caddy_config: Option<String>,
    pub update: ResolvedUpdateConfig,
}

#[derive(Serialize, Deserialize)]
struct ServiceContainerSpecWire {
    #[serde(flatten)]
    spec: ServiceContainerSpec,
    #[serde(default)]
    config_mounts: Vec<ConfigMount>,
}

#[derive(Serialize, Deserialize)]
struct RequestedServiceSpecWire {
    name: ServiceName,
    mode: ServiceMode,
    container: ServiceContainerSpecWire,
    #[serde(default)]
    placement: Placement,
    #[serde(default)]
    ports: Vec<PortPublication>,
    #[serde(default)]
    volumes: Vec<ServiceVolume>,
    #[serde(default)]
    mounts: Vec<ServiceMount>,
    #[serde(default)]
    configs: Vec<ConfigSpec>,
    #[serde(default)]
    pre_deploy: Option<PreDeployHook>,
    #[serde(default)]
    caddy_config: Option<String>,
    #[serde(default)]
    update: UpdateConfig,
}

#[derive(Serialize, Deserialize)]
struct ResolvedServiceSpecWire {
    service_id: ServiceId,
    name: ServiceName,
    mode: ServiceMode,
    container: ServiceContainerSpecWire,
    #[serde(default)]
    placement: Placement,
    #[serde(default)]
    ports: Vec<PortPublication>,
    #[serde(default)]
    volumes: Vec<ServiceVolume>,
    #[serde(default)]
    mounts: Vec<ServiceMount>,
    #[serde(default)]
    configs: Vec<ConfigSpec>,
    #[serde(default)]
    pre_deploy: Option<PreDeployHook>,
    #[serde(default)]
    caddy_config: Option<String>,
    #[serde(default)]
    update: ResolvedUpdateConfig,
}

impl TryFrom<RequestedServiceSpecWire> for RequestedServiceSpec {
    type Error = ServiceSpecGraphError;

    fn try_from(wire: RequestedServiceSpecWire) -> Result<Self, Self::Error> {
        Ok(Self {
            name: wire.name,
            mode: wire.mode,
            container: wire.container.spec,
            placement: wire.placement,
            ports: wire.ports,
            volume_graph: ServiceVolumeGraph::parse(wire.volumes, wire.mounts)?,
            config_graph: ServiceConfigGraph::parse(wire.configs, wire.container.config_mounts)?,
            pre_deploy: wire.pre_deploy,
            caddy_config: wire.caddy_config,
            update: wire.update,
        })
    }
}

impl From<RequestedServiceSpec> for RequestedServiceSpecWire {
    fn from(spec: RequestedServiceSpec) -> Self {
        let (volumes, mounts) = spec.volume_graph.into_parts();
        let (configs, config_mounts) = spec.config_graph.into_parts();
        Self {
            name: spec.name,
            mode: spec.mode,
            container: ServiceContainerSpecWire {
                spec: spec.container,
                config_mounts,
            },
            placement: spec.placement,
            ports: spec.ports,
            volumes,
            mounts,
            configs,
            pre_deploy: spec.pre_deploy,
            caddy_config: spec.caddy_config,
            update: spec.update,
        }
    }
}

impl TryFrom<ResolvedServiceSpecWire> for ResolvedServiceSpec {
    type Error = ServiceSpecGraphError;

    fn try_from(wire: ResolvedServiceSpecWire) -> Result<Self, Self::Error> {
        Ok(Self {
            service_id: wire.service_id,
            name: wire.name,
            mode: wire.mode,
            container: wire.container.spec,
            placement: wire.placement,
            ports: wire.ports,
            volume_graph: ServiceVolumeGraph::parse(wire.volumes, wire.mounts)?,
            config_graph: ServiceConfigGraph::parse(wire.configs, wire.container.config_mounts)?,
            pre_deploy: wire.pre_deploy,
            caddy_config: wire.caddy_config,
            update: wire.update,
        })
    }
}

impl From<ResolvedServiceSpec> for ResolvedServiceSpecWire {
    fn from(spec: ResolvedServiceSpec) -> Self {
        let (volumes, mounts) = spec.volume_graph.into_parts();
        let (configs, config_mounts) = spec.config_graph.into_parts();
        Self {
            service_id: spec.service_id,
            name: spec.name,
            mode: spec.mode,
            container: ServiceContainerSpecWire {
                spec: spec.container,
                config_mounts,
            },
            placement: spec.placement,
            ports: spec.ports,
            volumes,
            mounts,
            configs,
            pre_deploy: spec.pre_deploy,
            caddy_config: spec.caddy_config,
            update: spec.update,
        }
    }
}

impl RequestedServiceSpec {
    /// Service Volume definitions in this spec's graph.
    #[must_use]
    pub fn volumes(&self) -> &[ServiceVolume] {
        self.volume_graph.volumes()
    }

    /// Volume mounts in this spec's graph.
    #[must_use]
    pub fn mounts(&self) -> &[ServiceMount] {
        self.volume_graph.mounts()
    }

    /// Config definitions in this spec's graph.
    #[must_use]
    pub fn configs(&self) -> &[ConfigSpec] {
        self.config_graph.configs()
    }

    /// Config mounts in this spec's graph.
    #[must_use]
    pub fn config_mounts(&self) -> &[ConfigMount] {
        self.config_graph.mounts()
    }

    /// Copy this spec onto a Service Container after placement.
    #[must_use]
    pub fn to_resolved(
        &self,
        service_id: ServiceId,
        update: ResolvedUpdateConfig,
    ) -> ResolvedServiceSpec {
        ResolvedServiceSpec {
            service_id,
            name: self.name.clone(),
            mode: self.mode.clone(),
            container: self.container.clone(),
            placement: self.placement.clone(),
            ports: self.ports.clone(),
            volume_graph: self.volume_graph.clone(),
            config_graph: self.config_graph.clone(),
            pre_deploy: self.pre_deploy.clone(),
            caddy_config: self.caddy_config.clone(),
            update,
        }
    }
}

impl ResolvedServiceSpec {
    /// Service Volume definitions in this spec's graph.
    #[must_use]
    pub fn volumes(&self) -> &[ServiceVolume] {
        self.volume_graph.volumes()
    }

    /// Volume mounts in this spec's graph.
    #[must_use]
    pub fn mounts(&self) -> &[ServiceMount] {
        self.volume_graph.mounts()
    }

    /// Config definitions in this spec's graph.
    #[must_use]
    pub fn configs(&self) -> &[ConfigSpec] {
        self.config_graph.configs()
    }

    /// Config mounts in this spec's graph.
    #[must_use]
    pub fn config_mounts(&self) -> &[ConfigMount] {
        self.config_graph.mounts()
    }

    /// Rebuild the deploy input this resolved spec came from.
    #[must_use]
    pub fn to_requested(&self) -> RequestedServiceSpec {
        RequestedServiceSpec {
            name: self.name.clone(),
            mode: self.mode.clone(),
            container: self.container.clone(),
            placement: self.placement.clone(),
            ports: self.ports.clone(),
            volume_graph: self.volume_graph.clone(),
            config_graph: self.config_graph.clone(),
            pre_deploy: self.pre_deploy.clone(),
            caddy_config: self.caddy_config.clone(),
            update: UpdateConfig {
                order: Some(self.update.order),
                monitor_millis: self.update.monitor_millis,
            },
        }
    }
}

#[must_use]
pub fn compare_specs(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> SpecChange {
    // TODO(UT-006, UT-011, UT-062 through UT-071, UT-080, UT-081, UT-091, UT-092):
    // ingress, placement, unused-volume, bind-option/default, and mutable-resource changes still
    // recreate until the Machine API supports the narrower in-place updates retained by the
    // baseline TODOs.
    if requested.container.pull_policy == PullPolicy::Always
        || immutable_service_fields_changed(current, requested)
    {
        return SpecChange::NeedsRecreate;
    }
    resource_change(&current.container.resources, &requested.container.resources)
}

fn immutable_service_fields_changed(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> bool {
    let ResolvedServiceSpec {
        service_id: _,
        name: current_name,
        mode: current_mode,
        container: current_container,
        placement: current_placement,
        ports: current_ports,
        volume_graph: current_volumes,
        config_graph: current_configs,
        pre_deploy: _,
        caddy_config: current_caddy_config,
        update: _,
    } = current;
    let RequestedServiceSpec {
        name: requested_name,
        mode: requested_mode,
        container: requested_container,
        placement: requested_placement,
        ports: requested_ports,
        volume_graph: requested_volumes,
        config_graph: requested_configs,
        pre_deploy: _,
        caddy_config: requested_caddy_config,
        update: _,
    } = requested;

    current_name != requested_name
        || !same_service_mode_kind(current_mode, requested_mode)
        || immutable_container_fields_changed(current_container, requested_container)
        || current_placement != requested_placement
        || !same_multiset(current_ports, requested_ports)
        || !same_multiset(current_volumes.volumes(), requested_volumes.volumes())
        || !same_multiset(current_volumes.mounts(), requested_volumes.mounts())
        || !same_multiset(current_configs.configs(), requested_configs.configs())
        || !same_multiset(current_configs.mounts(), requested_configs.mounts())
        || current_caddy_config.as_deref().map(str::trim)
            != requested_caddy_config.as_deref().map(str::trim)
}

fn immutable_container_fields_changed(
    current: &ServiceContainerSpec,
    requested: &ServiceContainerSpec,
) -> bool {
    let ServiceContainerSpec {
        image: current_image,
        command: current_command,
        entrypoint: current_entrypoint,
        environment: current_environment,
        cap_add: current_cap_add,
        cap_drop: current_cap_drop,
        healthcheck: current_healthcheck,
        pull_policy: _,
        init: current_init,
        user: current_user,
        working_directory: current_working_directory,
        tty: current_tty,
        open_stdin: current_open_stdin,
        privileged: current_privileged,
        pid_mode: current_pid_mode,
        log_driver: current_log_driver,
        resources: _,
        stop_timeout_secs: current_stop_timeout_secs,
        sysctls: current_sysctls,
        restart: current_restart,
    } = current;
    let ServiceContainerSpec {
        image: requested_image,
        command: requested_command,
        entrypoint: requested_entrypoint,
        environment: requested_environment,
        cap_add: requested_cap_add,
        cap_drop: requested_cap_drop,
        healthcheck: requested_healthcheck,
        pull_policy: _,
        init: requested_init,
        user: requested_user,
        working_directory: requested_working_directory,
        tty: requested_tty,
        open_stdin: requested_open_stdin,
        privileged: requested_privileged,
        pid_mode: requested_pid_mode,
        log_driver: requested_log_driver,
        resources: _,
        stop_timeout_secs: requested_stop_timeout_secs,
        sysctls: requested_sysctls,
        restart: requested_restart,
    } = requested;

    current_image != requested_image
        || current_command != requested_command
        || current_entrypoint != requested_entrypoint
        || current_environment != requested_environment
        || !same_multiset(current_cap_add, requested_cap_add)
        || !same_multiset(current_cap_drop, requested_cap_drop)
        || current_healthcheck != requested_healthcheck
        || current_init != requested_init
        || current_user != requested_user
        || current_working_directory != requested_working_directory
        || current_tty != requested_tty
        || current_open_stdin != requested_open_stdin
        || current_privileged != requested_privileged
        || current_pid_mode != requested_pid_mode
        || current_log_driver != requested_log_driver
        || current_stop_timeout_secs != requested_stop_timeout_secs
        || current_sysctls != requested_sysctls
        || current_restart != requested_restart
}

fn resource_change(current: &ContainerResources, requested: &ContainerResources) -> SpecChange {
    let ContainerResources {
        cpu_nanos: current_cpu_nanos,
        memory_bytes: current_memory_bytes,
        memory_reservation_bytes: current_memory_reservation_bytes,
        shared_memory_bytes: current_shared_memory_bytes,
        devices: current_devices,
        device_reservations: current_device_reservations,
        ulimits: current_ulimits,
    } = current;
    let ContainerResources {
        cpu_nanos: requested_cpu_nanos,
        memory_bytes: requested_memory_bytes,
        memory_reservation_bytes: requested_memory_reservation_bytes,
        shared_memory_bytes: requested_shared_memory_bytes,
        devices: requested_devices,
        device_reservations: requested_device_reservations,
        ulimits: requested_ulimits,
    } = requested;
    if current_devices != requested_devices
        || current_device_reservations != requested_device_reservations
        || current_ulimits != requested_ulimits
    {
        return SpecChange::NeedsRecreate;
    }
    if current_cpu_nanos != requested_cpu_nanos
        || current_memory_bytes != requested_memory_bytes
        || current_memory_reservation_bytes != requested_memory_reservation_bytes
        || current_shared_memory_bytes != requested_shared_memory_bytes
    {
        return SpecChange::NeedsUpdate;
    }
    SpecChange::UpToDate
}

fn same_multiset<T: PartialEq>(left: &[T], right: &[T]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    // ponytail: O(n²) avoids requiring Ord or cloning domain values; sort if specs become large.
    let mut matched = vec![false; right.len()];
    left.iter().all(|item| {
        right
            .iter()
            .zip(&mut matched)
            .find(|(candidate, used)| !**used && item == *candidate)
            .is_some_and(|(_, used)| {
                *used = true;
                true
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn configured(test: &[&str]) -> HealthcheckSpec {
        HealthcheckSpec::Configured(ConfiguredHealthcheck {
            test: HealthcheckCommand::parse(test.iter().copied()).unwrap(),
            interval_millis: Some(1_000),
            timeout_millis: Some(2_000),
            start_period_millis: Some(3_000),
            start_interval_millis: Some(4_000),
            retries: Some(5),
        })
    }

    #[test]
    fn healthcheck_command_rejects_empty_and_disable_sentinel() {
        assert!(HealthcheckCommand::parse(Vec::<String>::new()).is_err());
        assert!(HealthcheckCommand::parse(["NONE"]).is_err());
        assert!(HealthcheckCommand::parse(["NONE", "CMD", "true"]).is_err());
        assert_eq!(
            HealthcheckCommand::parse(["CMD", "true"])
                .unwrap()
                .as_slice(),
            ["CMD", "true"]
        );
    }

    #[test]
    fn healthcheck_spec_serializes_disabled_and_configured() {
        assert_eq!(
            serde_json::to_value(HealthcheckSpec::Disabled).unwrap(),
            json!({ "state": "disabled" })
        );
        assert_eq!(
            serde_json::from_value::<HealthcheckSpec>(json!({ "state": "disabled" })).unwrap(),
            HealthcheckSpec::Disabled
        );
        let configured = configured(&["CMD", "true"]);
        let value = serde_json::to_value(&configured).unwrap();
        assert_eq!(
            value,
            json!({
                "state": "configured",
                "test": ["CMD", "true"],
                "interval_millis": 1000,
                "timeout_millis": 2000,
                "start_period_millis": 3000,
                "start_interval_millis": 4000,
                "retries": 5
            })
        );
        assert_eq!(
            serde_json::from_value::<HealthcheckSpec>(value).unwrap(),
            configured
        );
    }

    #[test]
    fn healthcheck_spec_rejects_empty_configured_and_sentinel_command() {
        for invalid in [
            json!({ "state": "configured", "test": [] }),
            json!({ "state": "configured", "test": ["NONE"] }),
            json!({ "state": "configured", "test": ["NONE", "CMD", "true"] }),
            json!({ "test": ["CMD", "true"], "disabled": true }),
        ] {
            assert!(
                serde_json::from_value::<HealthcheckSpec>(invalid.clone()).is_err(),
                "{invalid} should be rejected"
            );
        }
    }

    #[test]
    fn disabled_healthchecks_compare_equal() {
        let left: HealthcheckSpec = serde_json::from_value(json!({ "state": "disabled" })).unwrap();
        let right: HealthcheckSpec =
            serde_json::from_value(json!({ "state": "disabled", "interval_millis": 9 })).unwrap();
        assert_eq!(left, right);
        assert_eq!(left, HealthcheckSpec::Disabled);
        assert_ne!(left, configured(&["CMD", "true"]));
    }
}
