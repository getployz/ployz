use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Namespace(pub String);

impl Namespace {
    #[must_use] 
    pub fn system() -> Self {
        Self("system".into())
    }

    #[must_use] 
    pub fn default_ns() -> Self {
        Self("default".into())
    }

    #[must_use] 
    pub fn is_system(&self) -> bool {
        self.0 == "system"
    }
}

impl std::fmt::Display for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self(inner) = self;
        f.write_str(inner)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployManifest {
    pub services: Vec<ServiceSpec>,
}

impl DeployManifest {
    pub fn validate(&self, namespace: &Namespace) -> Result<(), String> {
        if self.services.is_empty() {
            return Err("manifest must contain at least one service".into());
        }

        let mut seen = BTreeSet::new();
        for service in &self.services {
            if service.namespace != *namespace {
                return Err(format!(
                    "service '{}' belongs to namespace '{}' but deploy requested '{}'",
                    service.name, service.namespace, namespace
                ));
            }
            if !seen.insert(service.name.clone()) {
                return Err(format!(
                    "manifest contains duplicate service '{}'",
                    service.name
                ));
            }
            service.validate()?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceSpec {
    pub name: String,
    pub namespace: Namespace,
    pub placement: Placement,
    pub template: ContainerSpec,
    pub network: NetworkMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub service_ports: Vec<ServicePort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub publish: Vec<PublishedPort>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<RouteSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ReadinessProbe>,
    #[serde(default = "RolloutStrategy::recreate")]
    pub rollout: RolloutStrategy,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub labels: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_grace_period: Option<String>,
    #[serde(default = "RestartPolicy::unless_stopped")]
    pub restart: RestartPolicy,
}

impl ServiceSpec {
    #[must_use] 
    pub fn fqn(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }

    pub fn canonical_revision_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| format!("serialize revision: {e}"))
    }

    pub fn revision_hash(&self) -> Result<String, String> {
        let json = self.canonical_revision_json()?;
        Ok(stable_hash_hex(json.as_bytes()))
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("service name cannot be empty".into());
        }

        if self.namespace.0.trim().is_empty() {
            return Err(format!("service '{}' has an empty namespace", self.name));
        }

        let mut seen_ports = BTreeSet::new();
        for port in &self.service_ports {
            if port.name.trim().is_empty() {
                return Err(format!(
                    "service '{}' has a service port with an empty name",
                    self.name
                ));
            }
            if !seen_ports.insert(port.name.clone()) {
                return Err(format!(
                    "service '{}' defines duplicate service port '{}'",
                    self.name, port.name
                ));
            }
        }

        for publish in &self.publish {
            if !seen_ports.contains(&publish.service_port) {
                return Err(format!(
                    "service '{}' publishes unknown service port '{}'",
                    self.name, publish.service_port
                ));
            }
        }

        for route in &self.routes {
            match route {
                RouteSpec::Http(route) => {
                    if !seen_ports.contains(&route.service_port) {
                        return Err(format!(
                            "service '{}' HTTP route references unknown service port '{}'",
                            self.name, route.service_port
                        ));
                    }
                }
                RouteSpec::Tcp(route) => {
                    if !seen_ports.contains(&route.service_port) {
                        return Err(format!(
                            "service '{}' TCP route references unknown service port '{}'",
                            self.name, route.service_port
                        ));
                    }
                }
            }
        }

        if let Some(readiness) = &self.readiness {
            match readiness {
                ReadinessProbe::Http { service_port, .. }
                | ReadinessProbe::Tcp { service_port } => {
                    if !seen_ports.contains(service_port) {
                        return Err(format!(
                            "service '{}' readiness probe references unknown service port '{}'",
                            self.name, service_port
                        ));
                    }
                }
                ReadinessProbe::Exec { command } => {
                    if command.is_empty() {
                        return Err(format!(
                            "service '{}' exec readiness probe must define at least one argument",
                            self.name
                        ));
                    }
                }
            }
        }

        match self.rollout {
            RolloutStrategy::Recreate => {}
            RolloutStrategy::BlueGreen => {
                let Some(_) = self.readiness else {
                    return Err(format!(
                        "service '{}' uses blue_green rollout but does not define readiness",
                        self.name
                    ));
                };
                if !self.publish.is_empty() {
                    return Err(format!(
                        "service '{}' cannot use blue_green rollout with published host ports",
                        self.name
                    ));
                }
                if matches!(self.network, NetworkMode::Host) {
                    return Err(format!(
                        "service '{}' cannot use blue_green rollout with host networking",
                        self.name
                    ));
                }
                if self
                    .template
                    .volumes
                    .iter()
                    .any(|mount| matches!(mount.source, VolumeSource::Bind(_)))
                {
                    return Err(format!(
                        "service '{}' cannot use blue_green rollout with bind mounts",
                        self.name
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    Global,
    Singleton,
    Replicated { count: u16 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Overlay,
    #[serde(rename = "service")]
    Service(String),
    Host,
    None,
}

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
    #[serde(default = "PullPolicy::if_not_present")]
    pub pull_policy: PullPolicy,
    #[serde(default = "Resources::empty")]
    pub resources: Resources,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub sysctls: BTreeMap<String, String>,
}

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
pub struct ServicePort {
    pub name: String,
    pub container_port: u16,
    #[serde(default = "PortProtocol::tcp")]
    pub protocol: PortProtocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishedPort {
    pub service_port: String,
    pub host_port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_ip: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteSpec {
    Http(HttpRoute),
    Tcp(TcpRoute),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRoute {
    pub service_port: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hostnames: Vec<String>,
    #[serde(default = "default_http_path_prefix")]
    pub path_prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpRoute {
    pub service_port: String,
    pub listen_port: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadinessProbe {
    Http { service_port: String, path: String },
    Tcp { service_port: String },
    Exec { command: Vec<String> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortProtocol {
    Tcp,
    Udp,
}

impl PortProtocol {
    #[must_use] 
    pub fn tcp() -> Self {
        Self::Tcp
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullPolicy {
    IfNotPresent,
    Always,
    Never,
}

impl PullPolicy {
    #[must_use] 
    pub fn if_not_present() -> Self {
        Self::IfNotPresent
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RolloutStrategy {
    Recreate,
    BlueGreen,
}

impl RolloutStrategy {
    #[must_use] 
    pub fn recreate() -> Self {
        Self::Recreate
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    UnlessStopped,
    Always,
    OnFailure,
    No,
}

impl RestartPolicy {
    #[must_use] 
    pub fn unless_stopped() -> Self {
        Self::UnlessStopped
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpu_millicores: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_bytes: Option<u64>,
}

impl Resources {
    #[must_use] 
    pub fn empty() -> Self {
        Self {
            cpu_millicores: None,
            memory_bytes: None,
        }
    }
}

pub trait ResourcesExt {
    fn cpu_nano(&self) -> Option<i64>;
}

impl ResourcesExt for Resources {
    fn cpu_nano(&self) -> Option<i64> {
        self.cpu_millicores.map(|m| i64::from(m) * 1_000_000)
    }
}

fn default_http_path_prefix() -> String {
    "/".into()
}

fn stable_hash_hex(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x00000100000001b3;

    let mut hash = OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }

    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_spec() -> ServiceSpec {
        ServiceSpec {
            name: "api".into(),
            namespace: Namespace::default_ns(),
            placement: Placement::Replicated { count: 2 },
            template: ContainerSpec {
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
            service_ports: vec![ServicePort {
                name: "http".into(),
                container_port: 8080,
                protocol: PortProtocol::Tcp,
            }],
            publish: vec![],
            routes: vec![RouteSpec::Http(HttpRoute {
                service_port: "http".into(),
                hostnames: vec!["api.example.test".into()],
                path_prefix: "/".into(),
            })],
            readiness: Some(ReadinessProbe::Http {
                service_port: "http".into(),
                path: "/ready".into(),
            }),
            rollout: RolloutStrategy::BlueGreen,
            labels: BTreeMap::from([("env".into(), "prod".into())]),
            stop_grace_period: Some("10s".into()),
            restart: RestartPolicy::UnlessStopped,
        }
    }

    #[test]
    fn serde_roundtrip() {
        let spec = sample_spec();
        let json = serde_json::to_string_pretty(&spec).expect("serialize spec");
        let deserialized: ServiceSpec = serde_json::from_str(&json).expect("deserialize spec");
        assert_eq!(spec, deserialized);
    }

    #[test]
    fn revision_hash_is_stable() {
        let spec = sample_spec();
        let hash_a = spec.revision_hash().expect("hash a");
        let hash_b = spec.revision_hash().expect("hash b");
        assert_eq!(hash_a, hash_b);
    }

    #[test]
    fn blue_green_requires_readiness() {
        let mut spec = sample_spec();
        spec.readiness = None;
        let error = spec.validate().expect_err("missing readiness should fail");
        assert!(error.contains("does not define readiness"));
    }

    #[test]
    fn host_ports_block_blue_green() {
        let mut spec = sample_spec();
        spec.publish.push(PublishedPort {
            service_port: "http".into(),
            host_port: 8080,
            host_ip: None,
        });
        let error = spec.validate().expect_err("publish should fail blue_green");
        assert!(error.contains("published host ports"));
    }

    #[test]
    fn manifest_rejects_duplicate_services() {
        let spec = sample_spec();
        let manifest = DeployManifest {
            services: vec![spec.clone(), spec],
        };
        let error = manifest
            .validate(&Namespace::default_ns())
            .expect_err("duplicates should fail");
        assert!(error.contains("duplicate service"));
    }
}
