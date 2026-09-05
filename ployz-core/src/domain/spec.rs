use std::{
    collections::BTreeMap,
    net::IpAddr,
    num::{NonZeroU16, NonZeroU32},
};

use ipnet::IpNet;
use serde::{Deserialize, Serialize};

use super::{ServiceConfigGraph, ServiceSpecGraphError, ServiceVolumeGraph};
use crate::{
    ClusterDomainLabel, ContainerHostname, ContainerLabels, ContainerPath, ExtraHost, IngressHost,
    MachinePath, MachineTarget, PidMode, RestartPolicy, ServiceId, ServiceMount, ServiceName,
    ServiceVolume, ValueError,
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

/// Opaque Ingress Proxy configuration tagged with the backend that understands it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
#[serde(
    rename_all = "snake_case",
    tag = "backend",
    try_from = "IngressProxyFragmentWire"
)]
pub enum IngressProxyFragment {
    /// Raw Caddy configuration evaluated only by the Caddy backend.
    #[non_exhaustive]
    Caddy {
        /// Non-empty Caddy configuration.
        config: String,
    },
}

impl IngressProxyFragment {
    /// Parse a non-empty raw Caddy fragment.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when `config` is empty after trimming.
    pub fn parse_caddy(config: impl Into<String>) -> Result<Self, ValueError> {
        let config = config.into();
        let trimmed = config.trim();
        if trimmed.is_empty() {
            return Err(ValueError::new(
                "Caddy Ingress Proxy Fragment",
                config,
                "non-empty configuration",
            ));
        }
        Ok(Self::Caddy {
            config: trimmed.to_owned(),
        })
    }

    /// Borrow the raw fragment when it is for Caddy.
    #[must_use]
    pub fn as_caddy(&self) -> Option<&str> {
        match self {
            Self::Caddy { config } => Some(config),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "backend")]
enum IngressProxyFragmentWire {
    Caddy { config: String },
}

impl TryFrom<IngressProxyFragmentWire> for IngressProxyFragment {
    type Error = ValueError;

    fn try_from(fragment: IngressProxyFragmentWire) -> Result<Self, Self::Error> {
        match fragment {
            IngressProxyFragmentWire::Caddy { config } => Self::parse_caddy(config),
        }
    }
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
    ClusterDomain {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        label: Option<ClusterDomainLabel>,
    },
    Explicit {
        hostname: IngressHost,
    },
}

impl IngressHostname {
    /// Automatic `{service}-{project}` Cluster Domain assignment.
    #[must_use]
    pub fn cluster_domain() -> Self {
        Self::ClusterDomain { label: None }
    }

    /// Chosen Cluster Domain label with no Project suffix.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when `label` is not a lowercase DNS label.
    pub fn cluster_domain_label(label: impl Into<String>) -> Result<Self, ValueError> {
        Ok(Self::ClusterDomain {
            label: Some(ClusterDomainLabel::parse(label)?),
        })
    }

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

    /// The explicit hostname when this intent is already a concrete Ingress Hostname.
    #[must_use]
    pub fn as_explicit_host(&self) -> Option<&IngressHost> {
        match self {
            Self::Explicit { hostname } => Some(hostname),
            Self::ClusterDomain { .. } => None,
        }
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
    /// User Docker labels, excluding Ployz's reserved management namespace.
    #[serde(default)]
    pub labels: ContainerLabels,
    /// The container's UTS hostname, with no Ployz identity or routing meaning.
    #[serde(default)]
    pub hostname: Option<ContainerHostname>,
    /// Container-local Docker `/etc/hosts` entries.
    #[serde(default)]
    pub extra_hosts: Vec<ExtraHost>,
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
    /// Backend-tagged custom configuration for this Service.
    pub ingress_proxy_fragment: Option<IngressProxyFragment>,
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
    pub volume_graph: crate::ResolvedServiceVolumeGraph,
    pub config_graph: ServiceConfigGraph,
    pub pre_deploy: Option<PreDeployHook>,
    /// Backend-tagged custom configuration for this Service.
    pub ingress_proxy_fragment: Option<IngressProxyFragment>,
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
    ingress_proxy_fragment: Option<IngressProxyFragment>,
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
    volumes: Vec<ResolvedServiceVolumeWire>,
    #[serde(default)]
    mounts: Vec<ServiceMount>,
    #[serde(default)]
    configs: Vec<ConfigSpec>,
    #[serde(default)]
    pre_deploy: Option<PreDeployHook>,
    #[serde(default)]
    ingress_proxy_fragment: Option<IngressProxyFragment>,
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
            ingress_proxy_fragment: wire.ingress_proxy_fragment,
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
            ingress_proxy_fragment: spec.ingress_proxy_fragment,
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
            volume_graph: ServiceVolumeGraph::parse(
                wire.volumes
                    .into_iter()
                    .map(|volume| ServiceVolume {
                        reference: volume.reference,
                        source: volume.source.to_requested(),
                    })
                    .collect(),
                wire.mounts,
            )?
            .try_into()?,
            config_graph: ServiceConfigGraph::parse(wire.configs, wire.container.config_mounts)?,
            pre_deploy: wire.pre_deploy,
            ingress_proxy_fragment: wire.ingress_proxy_fragment,
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
            volumes: volumes
                .into_iter()
                .map(|volume| ResolvedServiceVolumeWire {
                    reference: volume.reference,
                    source: volume
                        .source
                        .try_into()
                        .expect("resolved graph establishes scope"),
                })
                .collect(),
            mounts,
            configs,
            pre_deploy: spec.pre_deploy,
            ingress_proxy_fragment: spec.ingress_proxy_fragment,
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
    ///
    /// # Errors
    ///
    /// Rejects managed sources that have not been scoped to a Project.
    pub fn to_resolved(
        &self,
        service_id: ServiceId,
        update: ResolvedUpdateConfig,
    ) -> Result<ResolvedServiceSpec, crate::ServiceVolumeGraphError> {
        Ok(ResolvedServiceSpec {
            service_id,
            name: self.name.clone(),
            mode: self.mode.clone(),
            container: self.container.clone(),
            placement: self.placement.clone(),
            ports: self.ports.clone(),
            volume_graph: self.volume_graph.clone().try_into()?,
            config_graph: self.config_graph.clone(),
            pre_deploy: self.pre_deploy.clone(),
            ingress_proxy_fragment: self.ingress_proxy_fragment.clone(),
            update,
        })
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
            volume_graph: self.volume_graph.to_requested(),
            config_graph: self.config_graph.clone(),
            pre_deploy: self.pre_deploy.clone(),
            ingress_proxy_fragment: self.ingress_proxy_fragment.clone(),
            update: UpdateConfig {
                order: Some(self.update.order),
                monitor_millis: self.update.monitor_millis,
            },
        }
    }
}

mod serving_shape;
pub use serving_shape::ServingShape;

#[must_use]
pub fn compare_specs(
    current: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
) -> SpecChange {
    // TODO: ingress, placement, unused-volume, bind-option/default, and mutable-resource changes still
    // recreate until the Machine API supports the narrower in-place updates.
    if requested.container.pull_policy == PullPolicy::Always
        || current.serving_shape() != requested.serving_shape()
    {
        return SpecChange::NeedsRecreate;
    }
    resource_change(&current.container.resources, &requested.container.resources)
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

#[derive(Serialize, Deserialize)]
struct ResolvedServiceVolumeWire {
    reference: crate::ServiceVolumeReference,
    source: crate::ResolvedVolumeSource,
}
