use std::collections::HashMap;
use std::net::IpAddr;

use bollard::models::{ContainerInspectResponse, PortMap};

/// Pull policy for container images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullPolicy {
    Always,
    IfNotPresent,
    Never,
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
    pub network_mode: Option<String>,
    pub port_bindings: Option<PortMap>,
    pub exposed_ports: Option<Vec<String>>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub privileged: bool,
    pub user: Option<String>,
    pub restart_policy: Option<bollard::models::RestartPolicy>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub sysctls: HashMap<String, String>,
    pub stop_timeout: Option<i64>,
    pub pid_mode: Option<String>,
}

impl Default for RuntimeContainerSpec {
    fn default() -> Self {
        Self {
            key: String::new(),
            container_name: String::new(),
            image: String::new(),
            pull_policy: PullPolicy::IfNotPresent,
            cmd: None,
            entrypoint: None,
            env: Vec::new(),
            labels: HashMap::new(),
            binds: Vec::new(),
            tmpfs: HashMap::new(),
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
    pub container_id: String,
    pub container_name: String,
    pub running: bool,
    pub image: String,
    pub cmd: Option<Vec<String>>,
    pub entrypoint: Option<Vec<String>>,
    pub env: Vec<(String, String)>,
    pub labels: HashMap<String, String>,
    pub binds: Vec<String>,
    pub tmpfs: HashMap<String, String>,
    pub network_mode: Option<String>,
    pub port_bindings: Option<PortMap>,
    pub cap_add: Vec<String>,
    pub cap_drop: Vec<String>,
    pub privileged: bool,
    pub user: Option<String>,
    pub restart_policy: Option<bollard::models::RestartPolicy>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub sysctls: HashMap<String, String>,
    pub pid_mode: Option<String>,
    pub ip_address: Option<IpAddr>,
    pub networks: HashMap<String, String>,
}

/// Parse env string "KEY=VALUE" into (key, value) tuple.
fn parse_env_pair(s: &str) -> (String, String) {
    match s.split_once('=') {
        Some((k, v)) => (k.to_string(), v.to_string()),
        None => (s.to_string(), String::new()),
    }
}

/// Extract `ObservedContainer` from a Docker inspect response.
#[must_use]
pub fn observe(info: &ContainerInspectResponse) -> ObservedContainer {
    let config = info.config.as_ref();
    let host_config = info.host_config.as_ref();
    let state = info.state.as_ref();

    let labels = config
        .and_then(|c| c.labels.as_ref())
        .cloned()
        .unwrap_or_default();

    let env: Vec<(String, String)> = config
        .and_then(|c| c.env.as_ref())
        .map(|vars| vars.iter().map(|s| parse_env_pair(s)).collect())
        .unwrap_or_default();

    let binds = host_config
        .and_then(|h| h.binds.as_ref())
        .cloned()
        .unwrap_or_default();

    let tmpfs = host_config
        .and_then(|h| h.tmpfs.as_ref())
        .cloned()
        .unwrap_or_default();

    let network_mode = host_config.and_then(|h| h.network_mode.clone());

    let port_bindings = host_config.and_then(|h| h.port_bindings.clone());

    let cap_add = host_config
        .and_then(|h| h.cap_add.as_ref())
        .cloned()
        .unwrap_or_default();

    let cap_drop = host_config
        .and_then(|h| h.cap_drop.as_ref())
        .cloned()
        .unwrap_or_default();

    let privileged = host_config
        .and_then(|h| h.privileged)
        .unwrap_or(false);

    let user = config.and_then(|c| c.user.clone()).filter(|u| !u.is_empty());

    let restart_policy = host_config.and_then(|h| h.restart_policy.clone());

    let memory_bytes = host_config.and_then(|h| h.memory).filter(|&m| m > 0);

    let nano_cpus = host_config.and_then(|h| h.nano_cpus).filter(|&c| c > 0);

    let sysctls = host_config
        .and_then(|h| h.sysctls.as_ref())
        .cloned()
        .unwrap_or_default();

    let pid_mode = host_config.and_then(|h| h.pid_mode.clone());

    let image = config
        .and_then(|c| c.image.clone())
        .unwrap_or_default();

    let cmd = config.and_then(|c| c.cmd.clone());
    let entrypoint = config.and_then(|c| c.entrypoint.clone());

    let running = state.and_then(|s| s.running).unwrap_or(false);

    // Extract IP address and network names from network settings
    let mut ip_address: Option<IpAddr> = None;
    let mut networks = HashMap::new();
    if let Some(net_settings) = info.network_settings.as_ref()
        && let Some(nets) = net_settings.networks.as_ref()
    {
        for (name, endpoint) in nets {
            if let Some(ref ip) = endpoint.ip_address
                && !ip.is_empty()
                && let Ok(addr) = ip.parse::<IpAddr>()
            {
                if ip_address.is_none() {
                    ip_address = Some(addr);
                }
                networks.insert(name.clone(), ip.clone());
            }
        }
    }

    let container_name = info
        .name
        .as_ref()
        .map(|n| n.trim_start_matches('/').to_string())
        .unwrap_or_default();

    ObservedContainer {
        container_id: info.id.clone().unwrap_or_default(),
        container_name,
        running,
        image,
        cmd,
        entrypoint,
        env,
        labels,
        binds,
        tmpfs,
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
        pid_mode,
        ip_address,
        networks,
    }
}
