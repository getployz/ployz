use std::collections::HashMap;
use std::net::IpAddr;

#[cfg(feature = "docker")]
use bollard::models::ContainerInspectResponse;

pub use ployz_types::spec::PullPolicy;

/// Local mirror of the subset of container port/restart metadata used by the
/// runtime seam. Keeping these out of `bollard::models` lets upstream callers
/// depend on `ployz-runtime-backends` without dragging the Docker API client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortBinding {
    pub host_ip: Option<String>,
    pub host_port: Option<String>,
}

pub type PortMap = HashMap<String, Option<Vec<PortBinding>>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestartPolicy {
    No,
    Always,
    UnlessStopped,
    OnFailure { maximum_retry_count: i64 },
}

/// Flat declarative spec for a managed container.
/// Callers construct this from their own domain types.
pub struct RuntimeContainerSpec {
    pub key: String,
    pub container_name: String,
    pub image: String,
    pub pull_policy: PullPolicy,
    pub cmd: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    pub env: Vec<(String, String)>,
    pub labels: HashMap<String, String>,
    pub binds: Vec<String>,
    pub tmpfs: HashMap<String, String>,
    pub dns_servers: Vec<String>,
    pub network_mode: Option<String>,
    pub port_bindings: Option<PortMap>,
    pub exposed_ports: Option<Vec<String>>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub privileged: bool,
    pub user: Option<String>,
    pub restart_policy: Option<RestartPolicy>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub sysctls: HashMap<String, String>,
    pub stop_timeout: Option<i64>,
    pub pid_mode: Option<String>,
}

impl RuntimeContainerSpec {
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        container_name: impl Into<String>,
        image: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            container_name: container_name.into(),
            image: image.into(),
            pull_policy: PullPolicy::IfNotPresent,
            cmd: None,
            entrypoint: None,
            env: Vec::new(),
            labels: HashMap::new(),
            binds: Vec::new(),
            tmpfs: HashMap::new(),
            dns_servers: Vec::new(),
            network_mode: None,
            port_bindings: None,
            exposed_ports: None,
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            privileged: false,
            user: None,
            restart_policy: None,
            memory_bytes: None,
            nano_cpus: None,
            sysctls: HashMap::new(),
            stop_timeout: None,
            pid_mode: None,
        }
    }
}

/// Observed state of a container from `docker inspect`.
#[derive(Debug, Clone)]
pub struct ObservedContainer {
    pub container_id: Observation<String>,
    pub container_name: Observation<String>,
    pub running: Observation<bool>,
    pub image: Observation<String>,
    pub cmd: Observation<Option<Vec<String>>>,
    pub entrypoint: Observation<Option<Vec<String>>>,
    pub env: Observation<Vec<(String, String)>>,
    pub labels: Observation<HashMap<String, String>>,
    pub binds: Observation<Vec<String>>,
    pub tmpfs: Observation<HashMap<String, String>>,
    pub dns_servers: Observation<Vec<String>>,
    pub network_mode: Observation<Option<String>>,
    pub port_bindings: Observation<Option<PortMap>>,
    pub cap_add: Observation<Vec<String>>,
    pub cap_drop: Observation<Vec<String>>,
    pub privileged: Observation<bool>,
    pub user: Observation<Option<String>>,
    pub restart_policy: Observation<Option<RestartPolicy>>,
    pub memory_bytes: Observation<Option<i64>>,
    pub nano_cpus: Observation<Option<i64>>,
    pub sysctls: Observation<HashMap<String, String>>,
    pub stop_timeout: Observation<Option<i64>>,
    pub pid_mode: Observation<Option<String>>,
    pub ip_address: Observation<Option<IpAddr>>,
    pub networks: Observation<HashMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation<T> {
    Observed(T),
    Missing,
    Malformed(String),
    Unknown,
}

impl<T> Observation<T> {
    #[must_use]
    pub fn observed(value: T) -> Self {
        Self::Observed(value)
    }

    #[must_use]
    pub fn as_observed(&self) -> Option<&T> {
        match self {
            Self::Observed(value) => Some(value),
            Self::Missing | Self::Malformed(_) | Self::Unknown => None,
        }
    }

    #[must_use]
    pub fn as_observed_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Observed(value) => Some(value),
            Self::Missing | Self::Malformed(_) | Self::Unknown => None,
        }
    }

    #[must_use]
    pub fn is_observed(&self) -> bool {
        matches!(self, Self::Observed(_))
    }
}

/// Parse env string "KEY=VALUE" into (key, value) tuple.
#[cfg(feature = "docker")]
fn parse_env_pair(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) => Ok((k.to_string(), v.to_string())),
        None => Err(format!("environment entry '{s}' is missing '='")),
    }
}

#[cfg(feature = "docker")]
fn restart_policy_from_bollard(policy: &bollard::models::RestartPolicy) -> RestartPolicy {
    match policy.name.as_ref() {
        Some(bollard::models::RestartPolicyNameEnum::NO)
        | Some(bollard::models::RestartPolicyNameEnum::EMPTY)
        | None => RestartPolicy::No,
        Some(bollard::models::RestartPolicyNameEnum::ALWAYS) => RestartPolicy::Always,
        Some(bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED) => {
            RestartPolicy::UnlessStopped
        }
        Some(bollard::models::RestartPolicyNameEnum::ON_FAILURE) => RestartPolicy::OnFailure {
            maximum_retry_count: policy.maximum_retry_count.unwrap_or(0),
        },
    }
}

#[cfg(feature = "docker")]
pub(crate) fn restart_policy_to_bollard(policy: &RestartPolicy) -> bollard::models::RestartPolicy {
    let (name, maximum_retry_count) = match policy {
        RestartPolicy::No => (bollard::models::RestartPolicyNameEnum::NO, None),
        RestartPolicy::Always => (bollard::models::RestartPolicyNameEnum::ALWAYS, None),
        RestartPolicy::UnlessStopped => {
            (bollard::models::RestartPolicyNameEnum::UNLESS_STOPPED, None)
        }
        RestartPolicy::OnFailure {
            maximum_retry_count,
        } => (
            bollard::models::RestartPolicyNameEnum::ON_FAILURE,
            Some(*maximum_retry_count),
        ),
    };
    bollard::models::RestartPolicy {
        name: Some(name),
        maximum_retry_count,
    }
}

#[cfg(feature = "docker")]
fn port_map_from_bollard(src: &bollard::models::PortMap) -> PortMap {
    src.iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_ref()
                    .map(|items| items.iter().map(port_binding_from_bollard).collect()),
            )
        })
        .collect()
}

#[cfg(feature = "docker")]
fn port_binding_from_bollard(src: &bollard::models::PortBinding) -> PortBinding {
    PortBinding {
        host_ip: src.host_ip.clone(),
        host_port: src.host_port.clone(),
    }
}

#[cfg(feature = "docker")]
pub(crate) fn port_map_to_bollard(src: &PortMap) -> bollard::models::PortMap {
    src.iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value
                    .as_ref()
                    .map(|items| items.iter().map(port_binding_to_bollard).collect()),
            )
        })
        .collect()
}

#[cfg(feature = "docker")]
fn port_binding_to_bollard(src: &PortBinding) -> bollard::models::PortBinding {
    bollard::models::PortBinding {
        host_ip: src.host_ip.clone(),
        host_port: src.host_port.clone(),
    }
}

/// Extract `ObservedContainer` from a Docker inspect response.
#[cfg(feature = "docker")]
#[must_use]
pub fn observe(info: &ContainerInspectResponse) -> ObservedContainer {
    let config = info.config.as_ref();
    let host_config = info.host_config.as_ref();
    let state = info.state.as_ref();

    let labels = match config {
        Some(config) => config
            .labels
            .clone()
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    let env = match config {
        Some(config) => match config.env.as_ref() {
            Some(vars) => vars
                .iter()
                .map(|s| parse_env_pair(s))
                .collect::<Result<Vec<_>, _>>()
                .map(Observation::Observed)
                .unwrap_or_else(Observation::Malformed),
            None => Observation::Missing,
        },
        None => Observation::Unknown,
    };

    let binds = match host_config {
        Some(host_config) => host_config
            .binds
            .clone()
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    let tmpfs = match host_config {
        Some(host_config) => host_config
            .tmpfs
            .clone()
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    let dns_servers = match host_config {
        Some(host_config) => host_config
            .dns
            .clone()
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    let network_mode = match host_config {
        Some(host_config) => Observation::Observed(host_config.network_mode.clone()),
        None => Observation::Unknown,
    };

    let port_bindings = match host_config {
        Some(host_config) => Observation::Observed(
            host_config
                .port_bindings
                .as_ref()
                .map(port_map_from_bollard),
        ),
        None => Observation::Unknown,
    };

    let cap_add = match host_config {
        Some(host_config) => host_config
            .cap_add
            .clone()
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    let cap_drop = match host_config {
        Some(host_config) => host_config
            .cap_drop
            .clone()
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    let privileged = match host_config {
        Some(host_config) => host_config
            .privileged
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    let user = match config {
        Some(config) => Observation::Observed(config.user.clone().filter(|u| !u.is_empty())),
        None => Observation::Unknown,
    };

    let restart_policy = match host_config {
        Some(host_config) => Observation::Observed(
            host_config
                .restart_policy
                .as_ref()
                .map(restart_policy_from_bollard),
        ),
        None => Observation::Unknown,
    };

    let memory_bytes = match host_config {
        Some(host_config) => Observation::Observed(host_config.memory.filter(|&m| m > 0)),
        None => Observation::Unknown,
    };

    let nano_cpus = match host_config {
        Some(host_config) => Observation::Observed(host_config.nano_cpus.filter(|&c| c > 0)),
        None => Observation::Unknown,
    };

    let sysctls = match host_config {
        Some(host_config) => host_config
            .sysctls
            .clone()
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    let pid_mode = match host_config {
        Some(host_config) => Observation::Observed(host_config.pid_mode.clone()),
        None => Observation::Unknown,
    };

    let stop_timeout = match config {
        Some(config) => Observation::Observed(config.stop_timeout),
        None => Observation::Unknown,
    };

    let image = match config {
        Some(config) => config
            .image
            .clone()
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    let cmd = match config {
        Some(config) => Observation::Observed(config.cmd.clone()),
        None => Observation::Unknown,
    };
    let entrypoint = match config {
        Some(config) => Observation::Observed(config.entrypoint.clone()),
        None => Observation::Unknown,
    };

    let running = match state {
        Some(state) => state
            .running
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        None => Observation::Unknown,
    };

    // Extract IP address and network names from network settings
    let mut ip_address: Option<IpAddr> = None;
    let mut networks = HashMap::new();
    let mut malformed_network = None;
    let networks = match info.network_settings.as_ref() {
        Some(net_settings) => match net_settings.networks.as_ref() {
            Some(nets) => {
                for (name, endpoint) in nets {
                    if let Some(ref ip) = endpoint.ip_address
                        && !ip.is_empty()
                    {
                        match ip.parse::<IpAddr>() {
                            Ok(addr) => {
                                if ip_address.is_none() {
                                    ip_address = Some(addr);
                                }
                                networks.insert(name.clone(), ip.clone());
                            }
                            Err(error) => {
                                malformed_network = Some(format!(
                                    "network '{name}' has invalid IP '{ip}': {error}"
                                ));
                                break;
                            }
                        }
                    }
                }
                match malformed_network {
                    Some(message) => Observation::Malformed(message),
                    None => Observation::Observed(networks),
                }
            }
            None => Observation::Missing,
        },
        None => Observation::Unknown,
    };

    let ip_address = match &networks {
        Observation::Observed(_) => Observation::Observed(ip_address),
        Observation::Missing => Observation::Missing,
        Observation::Malformed(message) => Observation::Malformed(message.clone()),
        Observation::Unknown => Observation::Unknown,
    };

    let container_name = info
        .name
        .as_ref()
        .map(|n| Observation::Observed(n.trim_start_matches('/').to_string()))
        .unwrap_or(Observation::Missing);

    ObservedContainer {
        container_id: info
            .id
            .clone()
            .map(Observation::Observed)
            .unwrap_or(Observation::Missing),
        container_name,
        running,
        image,
        cmd,
        entrypoint,
        env,
        labels,
        binds,
        tmpfs,
        dns_servers,
        network_mode,
        port_bindings,
        cap_add,
        cap_drop,
        privileged,
        user,
        restart_policy,
        memory_bytes,
        nano_cpus,
        sysctls,
        stop_timeout,
        pid_mode,
        ip_address,
        networks,
    }
}

#[cfg(all(test, feature = "docker"))]
mod tests {
    use super::{Observation, observe};
    use bollard::models::{ContainerConfig, ContainerInspectResponse, HostConfig};
    use std::collections::HashMap;

    #[test]
    fn observe_reads_dns_servers_from_host_config() {
        let observed = observe(&ContainerInspectResponse {
            name: Some("/ployz-test".into()),
            config: Some(ContainerConfig {
                image: Some("busybox:latest".into()),
                ..Default::default()
            }),
            host_config: Some(HostConfig {
                dns: Some(vec!["10.210.0.2".into()]),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(
            observed.container_name,
            Observation::Observed("ployz-test".into())
        );
        assert_eq!(
            observed.dns_servers,
            Observation::Observed(vec!["10.210.0.2".into()])
        );
    }

    #[test]
    fn observe_marks_missing_host_config_fields_as_missing() {
        let observed = observe(&ContainerInspectResponse {
            name: Some("/ployz-test".into()),
            config: Some(ContainerConfig {
                image: Some("busybox:latest".into()),
                ..Default::default()
            }),
            host_config: Some(HostConfig {
                dns: None,
                ..Default::default()
            }),
            ..Default::default()
        });

        assert_eq!(observed.dns_servers, Observation::Missing);
    }

    #[test]
    fn observe_marks_absent_host_config_as_unknown() {
        let observed = observe(&ContainerInspectResponse {
            name: Some("/ployz-test".into()),
            config: Some(ContainerConfig {
                image: Some("busybox:latest".into()),
                ..Default::default()
            }),
            host_config: None,
            ..Default::default()
        });

        assert_eq!(observed.dns_servers, Observation::Unknown);
    }

    #[test]
    fn observe_marks_invalid_network_ip_as_malformed() {
        use bollard::models::{EndpointSettings, NetworkSettings};

        let mut networks = HashMap::new();
        networks.insert(
            "ployz".into(),
            EndpointSettings {
                ip_address: Some("not-an-ip".into()),
                ..Default::default()
            },
        );

        let observed = observe(&ContainerInspectResponse {
            name: Some("/ployz-test".into()),
            config: Some(ContainerConfig {
                image: Some("busybox:latest".into()),
                ..Default::default()
            }),
            network_settings: Some(NetworkSettings {
                networks: Some(networks),
                ..Default::default()
            }),
            ..Default::default()
        });

        assert!(matches!(observed.networks, Observation::Malformed(_)));
    }
}
