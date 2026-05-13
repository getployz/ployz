use ployz_types::spec::PullPolicy;
use std::collections::HashMap;
use std::net::IpAddr;

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
