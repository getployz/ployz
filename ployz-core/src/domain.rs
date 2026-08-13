use std::{
    collections::BTreeMap,
    net::IpAddr,
    num::{NonZeroU16, NonZeroU32},
};

use ipnet::IpNet;
use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Map, Value};

use crate::{
    AdvertisedEndpoint, ContainerAddress, ContainerId, ContainerPath, DockerVolumeName, MachineId,
    MachineName, MachinePath, MachineSelector, MachineSubnet, ManagementAddress, SelectedEndpoint,
    ServiceId, ServiceName, ServiceVolumeReference, WireGuardPublicKey,
};

/// A name lookup result. Duplicate names are a normal observable state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "match", content = "values")]
pub enum NameMatches<T> {
    None,
    One(T),
    Ambiguous(Vec<T>),
}

impl<T> NameMatches<T> {
    #[must_use]
    pub fn from_matches(mut matches: Vec<T>) -> Self {
        match matches.len() {
            0 => Self::None,
            1 => Self::One(matches.pop().expect("length checked")),
            _ => Self::Ambiguous(matches),
        }
    }
}

/// A successful response from one fan-out target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineSuccess<T> {
    pub machine_id: MachineId,
    pub value: T,
}

/// A typed failure returned for one fan-out target.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineFailure<E> {
    pub machine_id: MachineId,
    pub error: E,
}

/// All outcomes observed during a fan-out operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PartialResult<T, E> {
    #[serde(default)]
    pub successes: Vec<MachineSuccess<T>>,
    #[serde(default)]
    pub failures: Vec<MachineFailure<E>>,
    /// Targets selected by the entry Machine that produced no terminal response.
    #[serde(default)]
    pub omissions: Vec<MachineId>,
}

impl<T, E> PartialResult<T, E> {
    #[must_use]
    pub fn all_targets_succeeded(&self) -> bool {
        self.failures.is_empty() && self.omissions.is_empty()
    }
}

/// One Machine's durable advertised record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Machine {
    pub id: MachineId,
    pub name: MachineName,
    pub subnet: MachineSubnet,
    pub management_address: ManagementAddress,
    pub public_key: WireGuardPublicKey,
    #[serde(default)]
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
}

/// An observer-relative view layered over a Machine's advertised record.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineObservation {
    pub machine: Machine,
    pub membership: MembershipObservation,
    #[serde(default)]
    pub selected_endpoint: Option<SelectedEndpoint>,
}

crate::value::open_string_enum!(LocalMachinePhase, Unrecognized {
    Uninitialized => "uninitialized",
    Joining => "joining",
    Participating => "participating",
    Resetting => "resetting",
});

crate::value::open_string_enum!(MembershipObservation, Unrecognized {
    Unknown => "unknown",
    Up => "up",
    Suspect => "suspect",
    Down => "down",
});

crate::value::open_string_enum!(HealthObservation, Unrecognized {
    NotConfigured => "not_configured",
    Starting => "starting",
    Healthy => "healthy",
    Unhealthy => "unhealthy",
});

/// Docker state as observed, including the untouched value of a future state.
#[derive(Clone, Debug, PartialEq)]
pub enum ContainerRuntimeObservation {
    Created,
    Running { health: HealthObservation },
    Paused,
    Restarting,
    Exited { code: i64 },
    Removing,
    Dead,
    Unknown { raw: Value },
}

impl Serialize for ContainerRuntimeObservation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = Map::new();
        match self {
            Self::Created => insert_state(&mut object, "created"),
            Self::Running { health } => {
                insert_state(&mut object, "running");
                object.insert(
                    "health".into(),
                    serde_json::to_value(health).map_err(serde::ser::Error::custom)?,
                );
            }
            Self::Paused => insert_state(&mut object, "paused"),
            Self::Restarting => insert_state(&mut object, "restarting"),
            Self::Exited { code } => {
                insert_state(&mut object, "exited");
                object.insert("code".into(), Value::from(*code));
            }
            Self::Removing => insert_state(&mut object, "removing"),
            Self::Dead => insert_state(&mut object, "dead"),
            Self::Unknown { raw } => return raw.serialize(serializer),
        }
        Value::Object(object).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ContainerRuntimeObservation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        let Some(object) = raw.as_object() else {
            return Ok(Self::Unknown { raw });
        };
        let Some(state) = object.get("state").and_then(Value::as_str) else {
            return Ok(Self::Unknown { raw });
        };

        match state {
            "created" => Ok(Self::Created),
            "running" => {
                let health = object
                    .get("health")
                    .cloned()
                    .ok_or_else(|| D::Error::missing_field("health"))?;
                Ok(Self::Running {
                    health: serde_json::from_value(health).map_err(D::Error::custom)?,
                })
            }
            "paused" => Ok(Self::Paused),
            "restarting" => Ok(Self::Restarting),
            "exited" => {
                let code = object
                    .get("code")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| D::Error::missing_field("code"))?;
                Ok(Self::Exited { code })
            }
            "removing" => Ok(Self::Removing),
            "dead" => Ok(Self::Dead),
            _ => Ok(Self::Unknown { raw }),
        }
    }
}

fn insert_state(object: &mut Map<String, Value>, state: &'static str) {
    object.insert("state".into(), Value::String(state.into()));
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode")]
pub enum ServiceMode {
    Replicated { replicas: NonZeroU32 },
    Global,
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
        recursive: Option<String>,
    },
    Named {
        name: DockerVolumeName,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    ServiceContainer,
    PreDeployHook,
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
    pub pid_mode: Option<String>,
    #[serde(default)]
    pub log_driver: Option<LogDriver>,
    #[serde(default)]
    pub resources: ContainerResources,
    #[serde(default)]
    pub stop_grace_period_millis: Option<u64>,
    #[serde(default)]
    pub sysctls: BTreeMap<String, String>,
    #[serde(default)]
    pub config_mounts: Vec<ConfigMount>,
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
    pub update: UpdateConfig,
}

/// The redacted, replicated observation of one managed container.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerObservation {
    pub container_id: ContainerId,
    /// Generated Docker name for display, never identity or selection.
    pub display_name: String,
    pub machine_id: MachineId,
    pub service_id: ServiceId,
    pub service_name: ServiceName,
    pub kind: ContainerKind,
    pub runtime: ContainerRuntimeObservation,
    /// Historical spec used to create this container; not a current Service spec.
    pub resolved_spec: ResolvedServiceSpec,
    #[serde(default)]
    pub address: Option<ContainerAddress>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}
