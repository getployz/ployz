use std::{collections::BTreeMap, path::PathBuf};

use ployz_core::RequestedServiceSpec;
use serde::Deserialize;
use serde_norway::Value;
use thiserror::Error;

/// A Compose build held as the raw spec. Additional contexts are read from `raw`.
#[derive(Clone, Debug, PartialEq)]
pub struct BuildSpec {
    pub raw: Value,
}

/// A project secret after validate: one source, or the resolved value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProjectSecret {
    Unresolved(SecretSource),
    Resolved(String),
}

/// How an unresolved project secret is obtained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SecretSource {
    File(String),
    Environment(String),
    Command(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ComposeProject {
    pub name: String,
    pub working_dir: PathBuf,
    pub context: Option<String>,
    pub services: BTreeMap<String, RequestedServiceSpec>,
    pub builds: BTreeMap<String, BuildSpec>,
    pub dependencies: BTreeMap<String, Vec<String>>,
    pub warnings: Vec<String>,
    pub service_profiles: BTreeMap<String, Vec<String>>,
    pub(super) volumes: BTreeMap<String, RawVolume>,
    pub(super) secrets: BTreeMap<String, ProjectSecret>,
    pub(super) environment: BTreeMap<String, String>,
}

impl ComposeProject {
    #[must_use]
    pub fn selected_context<'a>(
        &'a self,
        explicit_context: Option<&'a str>,
        explicit_connect: Option<&str>,
    ) -> Option<&'a str> {
        explicit_context.or_else(|| {
            explicit_connect
                .is_none()
                .then_some(self.context.as_deref())
                .flatten()
        })
    }

    pub fn select_services(&self, selected: &[String]) -> Result<Self, ComposeError> {
        if selected.is_empty() {
            return Ok(self.clone());
        }
        let mut included = std::collections::BTreeSet::new();
        let mut pending = selected.to_vec();
        while let Some(name) = pending.pop() {
            if !self.services.contains_key(&name) {
                return Err(ComposeError::Invalid(format!("undefined service '{name}'")));
            }
            if included.insert(name.clone()) {
                pending.extend(self.dependencies.get(&name).into_iter().flatten().cloned());
            }
        }
        let mut project = self.clone();
        project.services.retain(|name, _| included.contains(name));
        project.builds.retain(|name, _| included.contains(name));
        project
            .service_profiles
            .retain(|name, _| included.contains(name));
        project.dependencies.retain(|name, dependencies| {
            if !included.contains(name) {
                return false;
            }
            dependencies.retain(|dependency| included.contains(dependency));
            true
        });
        Ok(project)
    }

    /// Compose `profiles:` per loaded Service. Empty means the Service always starts.
    #[must_use]
    pub fn service_profiles(&self) -> BTreeMap<ployz_core::ServiceName, Vec<String>> {
        self.service_profiles
            .iter()
            .filter_map(|(name, profiles)| {
                Some((ployz_core::ServiceName::parse(name).ok()?, profiles.clone()))
            })
            .collect()
    }

    /// Service keys that start for `requested_profiles`. Unprofiled Services always start.
    #[must_use]
    pub fn enabled_service_names(&self, requested_profiles: &[String]) -> Vec<String> {
        self.services
            .keys()
            .filter(|name| {
                ployz_core::profiles_enable_start(
                    self.service_profiles
                        .get(*name)
                        .map(Vec::as_slice)
                        .unwrap_or(&[]),
                    requested_profiles,
                )
            })
            .cloned()
            .collect()
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ComposeError {
    #[error("{0}")]
    Prerequisite(String),
    #[error("{0}")]
    Compose(String),
    #[error("invalid normalized Compose project: {0}")]
    Invalid(String),
    #[error("{0}")]
    Io(String),
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawProject {
    pub name: Option<String>,
    #[serde(rename = "x-context")]
    pub context: Option<String>,
    #[serde(default)]
    pub services: BTreeMap<String, RawService>,
    #[serde(default)]
    pub volumes: BTreeMap<String, RawVolume>,
    #[serde(default)]
    pub configs: BTreeMap<String, RawConfig>,
    #[serde(default)]
    pub secrets: BTreeMap<String, RawSecret>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawService {
    pub image: Option<String>,
    pub build: Option<Value>,
    pub command: Option<Value>,
    pub entrypoint: Option<Value>,
    #[serde(default)]
    pub environment: Value,
    #[serde(default)]
    pub cap_add: Vec<String>,
    #[serde(default)]
    pub cap_drop: Vec<String>,
    pub healthcheck: Option<RawHealthcheck>,
    pub pull_policy: Option<String>,
    pub init: Option<bool>,
    pub user: Option<String>,
    pub working_dir: Option<String>,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub stdin_open: bool,
    #[serde(default)]
    pub privileged: bool,
    pub pid: Option<String>,
    pub restart: Option<String>,
    pub logging: Option<RawLogging>,
    pub stop_grace_period: Option<String>,
    #[serde(default)]
    pub sysctls: BTreeMap<String, String>,
    #[serde(default)]
    pub volumes: Vec<RawServiceVolume>,
    #[serde(default)]
    pub configs: Vec<RawServiceConfig>,
    #[serde(default)]
    pub ports: Vec<RawPort>,
    pub deploy: Option<RawDeploy>,
    pub scale: Option<u32>,
    #[serde(default)]
    pub depends_on: Value,
    #[serde(default)]
    pub secrets: Vec<Value>,
    pub cpus: Option<Value>,
    pub mem_limit: Option<Value>,
    pub mem_reservation: Option<Value>,
    pub shm_size: Option<Value>,
    #[serde(default)]
    pub devices: Vec<RawDevice>,
    #[serde(default)]
    pub gpus: Vec<RawDeviceRequest>,
    #[serde(default)]
    pub ulimits: BTreeMap<String, Value>,
    #[serde(default)]
    pub profiles: Vec<String>,
    #[serde(rename = "x-machines")]
    pub machines: Option<RawStringList>,
    #[serde(rename = "x-ports")]
    pub extension_ports: Option<RawStringList>,
    #[serde(rename = "x-caddy")]
    pub caddy: Option<RawCaddy>,
    #[serde(rename = "x-pre_deploy")]
    pub pre_deploy: Option<RawPreDeploy>,
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(super) enum RawStringList {
    String(String),
    List(Vec<String>),
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(super) enum RawCaddy {
    String(String),
    Object {
        #[serde(default)]
        config: String,
        #[serde(flatten)]
        other: BTreeMap<String, Value>,
    },
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawPreDeploy {
    pub command: Option<Value>,
    pub environment: Option<Value>,
    pub privileged: Option<bool>,
    pub timeout: Option<String>,
    pub user: Option<Value>,
    #[serde(flatten)]
    pub other: BTreeMap<String, Value>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawLogging {
    pub driver: Option<String>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawHealthcheck {
    #[serde(default)]
    pub test: Value,
    pub interval: Option<String>,
    pub timeout: Option<String>,
    pub start_period: Option<String>,
    pub start_interval: Option<String>,
    pub retries: Option<u32>,
    #[serde(default)]
    pub disable: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawDeploy {
    pub mode: Option<String>,
    pub replicas: Option<u32>,
    pub update_config: Option<RawUpdate>,
    pub resources: Option<RawDeployResources>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawUpdate {
    pub order: Option<String>,
    pub monitor: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawDeployResources {
    pub limits: Option<RawResourceValues>,
    pub reservations: Option<RawResourceValues>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawResourceValues {
    pub cpus: Option<Value>,
    pub memory: Option<Value>,
    #[serde(default)]
    pub devices: Vec<RawDeviceRequest>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawVolume {
    pub name: Option<String>,
    pub driver: Option<String>,
    #[serde(default)]
    pub driver_opts: BTreeMap<String, String>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub external: Value,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawConfig {
    pub file: Option<String>,
    pub content: Option<String>,
    pub environment: Option<String>,
    #[serde(default)]
    pub external: Value,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawSecret {
    pub file: Option<String>,
    pub environment: Option<String>,
    pub driver: Option<String>,
    #[serde(default)]
    pub driver_opts: BTreeMap<String, String>,
    #[serde(rename = "x-command")]
    pub command: Option<String>,
    #[serde(default)]
    pub external: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(super) enum RawServiceVolume {
    Short(String),
    Long(Box<RawServiceVolumeLong>),
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub(super) struct RawServiceVolumeLong {
    #[serde(rename = "type")]
    pub kind: String,
    pub source: Option<String>,
    pub target: String,
    #[serde(default)]
    pub read_only: bool,
    pub bind: Option<RawBind>,
    pub volume: Option<RawVolumeMount>,
    pub tmpfs: Option<RawTmpfs>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawBind {
    #[serde(default)]
    pub create_host_path: bool,
    pub propagation: Option<String>,
    pub recursive: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawVolumeMount {
    #[serde(default)]
    pub nocopy: bool,
    pub subpath: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawTmpfs {
    pub size: Option<Value>,
    pub mode: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(super) enum RawServiceConfig {
    Short(String),
    Long {
        source: String,
        target: Option<String>,
        uid: Option<String>,
        gid: Option<String>,
        mode: Option<Value>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(super) enum RawPort {
    Short(String),
    Long {
        target: u16,
        published: Option<Value>,
        host_ip: Option<String>,
        protocol: Option<String>,
        mode: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(super) enum RawDevice {
    Short(String),
    Long {
        source: String,
        target: String,
        permissions: Option<String>,
    },
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
pub(super) struct RawDeviceRequest {
    pub driver: Option<String>,
    pub count: Option<Value>,
    #[serde(default, alias = "ids")]
    pub device_ids: Vec<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub options: BTreeMap<String, String>,
}
