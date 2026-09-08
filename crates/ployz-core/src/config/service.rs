//! Shared authored service settings and their compiled environment and attachment projections.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{RestartPolicy, ServiceName};

/// Authored Cloud settings. Runtime specs are compiled from this configuration;
/// they cannot reconstruct repository authority, build policy, or credentials.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoredServiceConfig {
    #[ts(type = "2")]
    pub version: u8,
    pub name: String,
    pub source: ServiceSource,
    pub pre_deploy_command: Option<String>,
    pub start_command: Option<String>,
    pub healthcheck: ServiceHealthcheck,
    pub restart_policy: ServiceRestartPolicy,
    #[serde(default = "default_max_retries")]
    pub max_retries: u8,
    #[serde(default)]
    pub cron: Option<String>,
    #[serde(default = "default_replicas")]
    pub replicas: u8,
    #[serde(default)]
    pub cpu_limit: Option<f64>,
    #[serde(default)]
    pub mem_limit: Option<f64>,
    pub private_dns: ServiceName,
    #[serde(default)]
    pub routes: Vec<ServiceRoute>,
    #[serde(default)]
    pub managed_hostname: Option<ServiceManagedHostname>,
    #[serde(default)]
    pub build: ServiceBuildConfig,
}

/// Service settings with environment and attachments derived by compilation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceConfig {
    /// The same setting schema used by the authored document.
    #[serde(flatten)]
    #[ts(flatten)]
    pub settings: AuthoredServiceConfig,
    #[serde(default)]
    pub env: BTreeMap<String, ServiceEnvValue>,
    #[serde(default)]
    pub mounts: Vec<ServiceDeployMount>,
    #[serde(default)]
    pub variable_group_attachments: Vec<super::VariableGroupAttachment>,
}

impl From<AuthoredServiceConfig> for ServiceConfig {
    fn from(settings: AuthoredServiceConfig) -> Self {
        Self {
            settings,
            env: BTreeMap::new(),
            mounts: Vec::new(),
            variable_group_attachments: Vec::new(),
        }
    }
}

const fn default_max_retries() -> u8 {
    10
}
const fn default_replicas() -> u8 {
    1
}

/// Source selection and the authority required to build or pull it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServiceSource {
    Empty {
        #[ts(type = "1")]
        version: u8,
        root_dir: String,
    },
    Git {
        #[ts(type = "2")]
        version: u8,
        repository: String,
        repository_id: u64,
        installation_id: u64,
        root_dir: String,
        branch: ServiceGitBranch,
        auto_deploy: bool,
        wait_for_ci: bool,
    },
    Image {
        #[ts(type = "1")]
        version: u8,
        image: String,
        auto_update: ServiceImageAutoUpdate,
        credentials: ServiceImageCredentials,
    },
}

/// An attached repository branch or evidence of a disconnected selection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServiceGitBranch {
    Connected { name: String },
    Disconnected { previous_name: Option<String> },
}

/// Explicit image-tag tracking policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ServiceImageAutoUpdate {
    Off,
    TrackTag { tag: String },
}

/// Public registry-credential availability and optional revision evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ServiceImageCredentials {
    None,
    Configured {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        revision: Option<String>,
    },
}

/// An authored HTTP readiness check or an explicitly disabled check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServiceHealthcheck {
    None,
    Http { path: String, timeout_seconds: u16 },
}

/// Cloud's retry count is authored separately; parsing reuses Docker policy admission.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "'unless-stopped' | 'always' | 'on-failure' | 'no'")]
pub struct ServiceRestartPolicy(pub RestartPolicy);

impl TryFrom<String> for ServiceRestartPolicy {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.contains(':') {
            return Err("Retry count belongs in maxRetries");
        }
        RestartPolicy::parse(&value)
            .map(Self)
            .map_err(|_| "Invalid restart policy")
    }
}

impl From<ServiceRestartPolicy> for String {
    fn from(value: ServiceRestartPolicy) -> Self {
        match value.0 {
            RestartPolicy::No => "no",
            RestartPolicy::Always => "always",
            RestartPolicy::UnlessStopped => "unless-stopped",
            RestartPolicy::OnFailure { .. } => "on-failure",
        }
        .into()
    }
}

/// A stable public route identity, hostname, and target container port.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceRoute {
    pub id: String,
    pub hostname: String,
    pub target_port: u16,
}

/// A managed hostname label with an optional explicit target port.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceManagedHostname {
    pub prefix: String,
    pub target_port: Option<u16>,
}

/// The build implementation selected for a repository source.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceBuilder {
    Dockerfile,
    #[default]
    Auto,
}

/// Build policy supplied to the selected builder.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceBuildConfig {
    pub builder: ServiceBuilder,
    pub dockerfile_path: Option<String>,
    pub watch_paths: Vec<String>,
}

/// A compiled Volume relationship retaining its Cloud owner identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceDeployMount {
    pub volume_resource_id: String,
    pub volume_name: String,
    pub mount_path: String,
}

/// The Variable Group owner responsible for a derived environment value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvSource {
    #[ts(type = "'variable_group'")]
    pub kind: String,
    pub resource_id: String,
    pub resource_name: String,
    pub variable_group_id: String,
    pub key: String,
}

/// The stable producer scope referenced by a template expression.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "scope",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ValuePartOwner {
    #[serde(rename = "self")]
    Self_,
    Service {
        lineage_id: String,
    },
    VariableGroup {
        lineage_id: String,
    },
}

/// Literal template text or a reference to an owner-qualified variable.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ValuePart {
    Text { value: String },
    Ref { owner: ValuePartOwner, key: String },
}

/// Opaque encrypted material; configuration rules do not decrypt it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct EncryptedSecretValue {
    #[ts(type = "1")]
    pub version: u8,
    pub iv: String,
    pub tag: String,
    pub ciphertext: String,
}

/// A compiled literal or sealed environment value with optional producer evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServiceEnvValue {
    Literal {
        value: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        source: Option<EnvSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        parts: Option<Vec<ValuePart>>,
    },
    Secret {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        variable_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_value: Option<EncryptedSecretValue>,
        fingerprint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        source: Option<EnvSource>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        interpolated: Option<bool>,
    },
}
