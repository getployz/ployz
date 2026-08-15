use std::{
    collections::BTreeMap,
    net::IpAddr,
    num::{NonZeroU16, NonZeroU32},
};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use crate::{
    BindRecursive, ContainerPath, DockerVolumeId, DockerVolumeName, MachinePath, MachineSelector,
    PidMode, RestartPolicy, ServiceId, ServiceName, ServiceVolumeReference,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum PortPublication {
    Ingress {
        hostname: String,
        load_balancer_port: NonZeroU16,
        container_port: NonZeroU16,
        http_protocol: HttpProtocol,
    },
    /// A normalized Compose ingress publication that baseline validation rejects later for
    /// using TCP or UDP rather than HTTP(S). Keeping it typed preserves that incomplete flow.
    IngressTransport {
        #[serde(default)]
        load_balancer_port: Option<NonZeroU16>,
        container_port: NonZeroU16,
        transport_protocol: TransportProtocol,
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
        propagation: Option<String>,
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
    /// Machine Names or IDs. Resolution remains observer-relative and may be ambiguous.
    #[serde(default)]
    pub machines: Vec<MachineSelector>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HealthcheckSpec {
    #[serde(default)]
    pub test: Vec<String>,
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
    #[serde(default)]
    pub disabled: bool,
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
    pub config_mounts: Vec<ConfigMount>,
    #[serde(default)]
    pub restart: RestartPolicy,
}

/// Normalized deploy input before placement and container-specific resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RequestedServiceSpec {
    pub name: ServiceName,
    pub mode: ServiceMode,
    pub container: ServiceContainerSpec,
    #[serde(default)]
    pub placement: Placement,
    #[serde(default)]
    pub ports: Vec<PortPublication>,
    #[serde(default)]
    pub volumes: Vec<ServiceVolume>,
    #[serde(default)]
    pub mounts: Vec<ServiceMount>,
    #[serde(default)]
    pub configs: Vec<ConfigSpec>,
    #[serde(default)]
    pub pre_deploy: Option<PreDeployHook>,
    #[serde(default)]
    pub caddy_config: Option<String>,
    #[serde(default)]
    pub update: UpdateConfig,
}

/// The exact, fully resolved Service Spec attached to one created container.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ResolvedServiceSpec {
    pub service_id: ServiceId,
    pub name: ServiceName,
    pub mode: ServiceMode,
    pub container: ServiceContainerSpec,
    #[serde(default)]
    pub placement: Placement,
    #[serde(default)]
    pub ports: Vec<PortPublication>,
    #[serde(default)]
    pub volumes: Vec<ServiceVolume>,
    #[serde(default)]
    pub mounts: Vec<ServiceMount>,
    #[serde(default)]
    pub configs: Vec<ConfigSpec>,
    #[serde(default)]
    pub pre_deploy: Option<PreDeployHook>,
    #[serde(default)]
    pub caddy_config: Option<String>,
    #[serde(default)]
    pub update: ResolvedUpdateConfig,
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
        volumes: current_volumes,
        mounts: current_mounts,
        configs: current_configs,
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
        volumes: requested_volumes,
        mounts: requested_mounts,
        configs: requested_configs,
        pre_deploy: _,
        caddy_config: requested_caddy_config,
        update: _,
    } = requested;

    current_name != requested_name
        || !same_service_mode_kind(current_mode, requested_mode)
        || immutable_container_fields_changed(current_container, requested_container)
        || current_placement != requested_placement
        || !same_multiset(current_ports, requested_ports)
        || !same_multiset(current_volumes, requested_volumes)
        || !same_multiset(current_mounts, requested_mounts)
        || !same_multiset(current_configs, requested_configs)
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
        config_mounts: current_config_mounts,
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
        config_mounts: requested_config_mounts,
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
        || !same_multiset(current_config_mounts, requested_config_mounts)
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
