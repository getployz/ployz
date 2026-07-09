use std::collections::BTreeMap;

use serde::Deserialize;
use serde_yaml::Value;

#[derive(Debug, Deserialize)]
pub(crate) struct ComposeDocument {
    pub name: Option<String>,
    pub services: Option<BTreeMap<String, Value>>,
    pub version: Option<Value>,
    pub volumes: Option<Value>,
    pub networks: Option<Value>,
    pub secrets: Option<Value>,
    pub configs: Option<Value>,
    #[serde(flatten)]
    pub unrecognized: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ComposeService {
    pub image: Option<String>,
    pub command: Option<ComposeCommand>,
    pub entrypoint: Option<ComposeCommand>,
    pub environment: Option<ComposeEnvironment>,
    pub env_file: Option<ComposeEnvFile>,
    pub deploy: Option<ComposeDeploy>,
    pub stop_grace_period: Option<Value>,
    #[serde(rename = "x-route")]
    pub x_route: Option<ComposeRoutes>,
    pub build: Option<Value>,
    pub cap_add: Option<Value>,
    pub cap_drop: Option<Value>,
    pub cgroup_parent: Option<Value>,
    pub configs: Option<Value>,
    pub depends_on: Option<Value>,
    pub devices: Option<Value>,
    pub dns: Option<Value>,
    pub dns_search: Option<Value>,
    pub expose: Option<Value>,
    pub extra_hosts: Option<Value>,
    pub healthcheck: Option<Value>,
    pub init: Option<Value>,
    pub labels: Option<Value>,
    pub logging: Option<Value>,
    pub networks: Option<Value>,
    pub platform: Option<Value>,
    pub ports: Option<Value>,
    pub privileged: Option<Value>,
    pub profiles: Option<Value>,
    pub pull_policy: Option<Value>,
    pub restart: Option<Value>,
    pub secrets: Option<Value>,
    pub security_opt: Option<Value>,
    pub ulimits: Option<Value>,
    pub user: Option<Value>,
    pub volumes: Option<Value>,
    pub working_dir: Option<Value>,
    #[serde(rename = "x-pre_deploy")]
    pub x_pre_deploy: Option<Value>,
    #[serde(flatten)]
    pub unrecognized: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ComposeDeploy {
    pub replicas: Option<Value>,
    pub mode: Option<Value>,
    pub placement: Option<Value>,
    pub resources: Option<Value>,
    pub restart_policy: Option<Value>,
    pub update_config: Option<Value>,
    #[serde(flatten)]
    pub unrecognized: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ComposeCommand {
    Shell(String),
    Exec(Vec<Value>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ComposeEnvironment {
    Map(BTreeMap<String, Value>),
    List(Vec<String>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ComposeEnvFile {
    One(String),
    Many(Vec<ComposeEnvFileEntry>),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ComposeEnvFileEntry {
    Path(String),
    Options {
        path: String,
        required: Option<bool>,
        #[serde(flatten)]
        unrecognized: BTreeMap<String, Value>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ComposeRoutes {
    One(String),
    Many(Vec<String>),
}

impl ComposeRoutes {
    #[must_use]
    pub(crate) fn into_shorthands(self) -> Vec<String> {
        match self {
            Self::One(shorthand) => vec![shorthand],
            Self::Many(shorthands) => shorthands,
        }
    }
}
