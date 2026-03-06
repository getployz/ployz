use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// --- Namespace ---

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Namespace(pub String);

impl Namespace {
    pub fn system() -> Self {
        Self("system".into())
    }

    pub fn default_ns() -> Self {
        Self("default".into())
    }

    pub fn is_system(&self) -> bool {
        self.0 == "system"
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

// --- ServiceSpec ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSpec {
    pub name: String,
    pub namespace: Namespace,
    pub schedule: Schedule,
    pub container: ContainerSpec,
    pub network: NetworkMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<PortBinding>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_grace_period: Option<String>,
    #[serde(default)]
    pub restart: RestartPolicy,
}

impl ServiceSpec {
    pub fn fqn(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }
}

// --- Schedule ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Schedule {
    Global,
    Singleton,
    Imperative,
}

// --- NetworkMode ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Overlay,
    #[serde(rename = "service")]
    Service(String),
    Host,
    None,
}

// --- ContainerSpec ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContainerSpec {
    pub image: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeMount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_add: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cap_drop: Vec<String>,
    #[serde(default)]
    pub privileged: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default)]
    pub pull_policy: PullPolicy,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sysctls: BTreeMap<String, String>,
}

// --- Supporting types ---

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VolumeMount {
    pub source: VolumeSource,
    pub target: String,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VolumeSource {
    Bind(String),
    Managed(ManagedVolumeSpec),
    Tmpfs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedVolumeSpec {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub options: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    pub host_port: u16,
    pub container_port: u16,
    #[serde(default)]
    pub protocol: PortProtocol,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    #[default]
    Tcp,
    Udp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PullPolicy {
    #[default]
    IfNotPresent,
    Always,
    Never,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    UnlessStopped,
    Always,
    OnFailure,
    No,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_millicores: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
}

pub trait ResourcesExt {
    fn cpu_nano(&self) -> Option<i64>;
}

impl ResourcesExt for Resources {
    /// Convert millicores to Docker's nano-CPU format (1 CPU = 1e9 nano-CPUs).
    fn cpu_nano(&self) -> Option<i64> {
        self.cpu_millicores
            .map(|m| i64::from(m) * 1_000_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> ServiceSpec {
        ServiceSpec {
            name: "api".into(),
            namespace: Namespace::default_ns(),
            schedule: Schedule::Imperative,
            container: ContainerSpec {
                image: "myapp:latest".into(),
                command: Some(vec!["serve".into()]),
                entrypoint: None,
                env: BTreeMap::from([("PORT".into(), "8080".into())]),
                volumes: vec![VolumeMount {
                    source: VolumeSource::Managed(ManagedVolumeSpec {
                        name: "data".into(),
                        driver: Some("zfs".into()),
                        options: BTreeMap::from([("quota".into(), "10G".into())]),
                    }),
                    target: "/data".into(),
                    readonly: false,
                }],
                cap_add: vec![],
                cap_drop: vec![],
                privileged: false,
                user: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources {
                    cpu_millicores: Some(1000),
                    memory_bytes: Some(512 * 1024 * 1024),
                },
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            ports: vec![PortBinding {
                host_port: 8080,
                container_port: 8080,
                protocol: PortProtocol::Tcp,
                host_ip: None,
            }],
            labels: BTreeMap::from([("env".into(), "prod".into())]),
            stop_grace_period: Some("10s".into()),
            restart: RestartPolicy::UnlessStopped,
        }
    }

    #[test]
    fn serde_roundtrip() {
        let spec = sample_spec();
        let json = serde_json::to_string_pretty(&spec).unwrap();
        let deserialized: ServiceSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn serde_minimal_spec() {
        let json = r#"{
            "name": "web",
            "namespace": "default",
            "schedule": "global",
            "container": { "image": "nginx:latest" },
            "network": "overlay"
        }"#;
        let spec: ServiceSpec = serde_json::from_str(json).unwrap();
        assert_eq!(spec.name, "web");
        assert_eq!(spec.namespace, Namespace::default_ns());
        assert!(spec.ports.is_empty());
        assert_eq!(spec.restart, RestartPolicy::UnlessStopped);
        assert_eq!(spec.container.pull_policy, PullPolicy::IfNotPresent);
    }

    #[test]
    fn namespace_display() {
        assert_eq!(Namespace::system().to_string(), "system");
        assert_eq!(Namespace::default_ns().to_string(), "default");
        assert!(Namespace::system().is_system());
        assert!(!Namespace::default_ns().is_system());
    }

    #[test]
    fn fqn() {
        let spec = sample_spec();
        assert_eq!(spec.fqn(), "default/api");
    }

    #[test]
    fn network_mode_service_serde() {
        let mode = NetworkMode::Service("wireguard".into());
        let json = serde_json::to_string(&mode).unwrap();
        assert_eq!(json, r#"{"service":"wireguard"}"#);
        let deserialized: NetworkMode = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, mode);
    }
}
