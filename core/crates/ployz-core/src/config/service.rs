//! Shared authored service settings and their compiled environment and attachment projections.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::{RestartPolicy, SealedValue, ServiceName, UpdateOrder};

/// Authored Cloud settings. Runtime specs are compiled from this configuration;
/// they cannot reconstruct repository authority, build policy, or credentials.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthoredServiceConfig {
    #[ts(type = "2")]
    pub version: u8,
    pub source: ServiceSource,
    pub pre_deploy_command: Option<String>,
    pub start_command: Option<String>,
    pub healthcheck: ServiceHealthcheck,
    pub restart_policy: ServiceRestartPolicy,
    #[serde(default = "default_max_retries")]
    pub max_retries: u8,
    /// None lets each deploy choose: stop-first with a Volume, else start-first.
    /// Skipped when unset, so documents without it hash the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub update_order: Option<ServiceUpdateOrder>,
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
    pub managed_hostnames: Vec<ServiceManagedHostname>,
    #[serde(default)]
    pub build: ServiceBuildConfig,
    /// The Service Template it was created from; authoring metadata only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub template: Option<ServiceTemplate>,
}

/// Which Service Template, and which version of it, a Service was created from.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceTemplate {
    pub id: crate::ServiceTemplateId,
    /// Starts at 1; zero is rejected.
    #[ts(type = "number")]
    pub version: std::num::NonZeroU32,
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
    /// Written only when there are some, so documents from before Configs hash the same.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<ServiceDeployConfig>>")]
    pub configs: Vec<ServiceDeployConfig>,
}

impl From<AuthoredServiceConfig> for ServiceConfig {
    fn from(settings: AuthoredServiceConfig) -> Self {
        Self {
            settings,
            env: BTreeMap::new(),
            mounts: Vec::new(),
            configs: Vec::new(),
        }
    }
}

/// How often an on-failure restart policy restarts a replica when authored config names none.
#[must_use]
pub const fn default_max_retries() -> u8 {
    10
}
/// How many replicas a Service runs when its authored config names none.
#[must_use]
pub const fn default_replicas() -> u8 {
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
        access: ServiceGitAccess,
        root_dir: String,
        branch: ServiceGitBranch,
    },
    Image {
        #[ts(type = "1")]
        version: u8,
        image: String,
        credentials: ServiceImageCredentials,
    },
}

/// How Cloud reads a GitHub repository; public access never borrows credentials.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServiceGitAccess {
    Public,
    GithubInstallation { installation_id: u64 },
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

/// A stable credential reference. Private material is resolved by Cloud for each attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServiceImageCredentials {
    None,
    Configured { credential_id: String },
}

/// An authored HTTP or shell-command readiness check, or none, which also disables the image's own check.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServiceHealthcheck {
    None,
    Http {
        path: String,
        timeout_seconds: u16,
    },
    Command {
        command: String,
        timeout_seconds: u16,
    },
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

/// How a Deploy replaces a Service's containers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceUpdateOrder {
    StartFirst,
    StopFirst,
    Rolling,
}

impl From<ServiceUpdateOrder> for UpdateOrder {
    fn from(order: ServiceUpdateOrder) -> Self {
        match order {
            ServiceUpdateOrder::StartFirst => Self::StartFirst,
            ServiceUpdateOrder::StopFirst => Self::StopFirst,
            ServiceUpdateOrder::Rolling => Self::Rolling,
        }
    }
}

/// A stable public route identity and hostname. A null target follows container PORT.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceRoute {
    pub id: String,
    pub hostname: String,
    pub target_port: Option<u16>,
}

/// A managed hostname label. A null target follows container PORT.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceManagedHostname {
    pub prefix: String,
    pub target_port: Option<u16>,
}

/// How a repository source's image is described for building.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "kebab-case")]
pub enum BuildMethod {
    Dockerfile,
    #[default]
    #[serde(alias = "auto")]
    Railpack,
}

/// Build settings for the selected Build Method.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceBuildConfig {
    pub build_method: BuildMethod,
    pub dockerfile_path: Option<String>,
    /// Override Railpack’s build command; None preserves detection. Ignored for Dockerfiles.
    pub command: Option<String>,
}

/// A compiled Volume relationship retaining its Cloud owner identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceDeployMount {
    pub volume_resource_id: String,
    pub volume_name: String,
    pub mount_path: String,
}

/// A compiled Config mount retaining its Cloud owner identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServiceDeployConfig {
    pub config_resource_id: String,
    pub config_name: String,
    pub mount_dir: String,
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

/// A Sealed value the browser sealed to one Environment's Sealing Key, bound to
/// the variable it was authored as. Cloud carries it and never opens it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientSealedValue {
    /// The Environment whose Sealing Key opens it: in any other it is Not set.
    #[ts(type = "string")]
    pub sealed_for: Uuid,
    pub value: SealedValue,
}

/// A secret's value sealed with the Store's own key: the envelope registry
/// credentials use.
pub type LegacyCiphertext = EncryptedSecretValue;

/// A secret's ciphertext. Untagged, so every stored value reads as it was written:
/// older Saved revisions, arrivals and held secrets included.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum SecretCiphertext {
    ClientSealed(ClientSealedValue),
    /// A value sealed with the Store's own key. Opaque: nothing on a Deploy, a
    /// claim, a Build or a pull request opens it, so a Deploy that ships its Service waits
    /// for the one-time reseal, which converts it.
    Legacy(LegacyCiphertext),
}

impl SecretCiphertext {
    /// The client-sealed value, when it is one.
    #[must_use]
    pub const fn client_sealed(&self) -> Option<&ClientSealedValue> {
        match self {
            Self::ClientSealed(value) => Some(value),
            Self::Legacy(_) => None,
        }
    }
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
        parts: Option<Vec<ValuePart>>,
    },
    Secret {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        variable_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        encrypted_value: Option<SecretCiphertext>,
        fingerprint: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        interpolated: Option<bool>,
    },
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;
    use crate::config::{SavedVariableIntent, SavedVariableValue};

    /// Base64 of 48 bytes: the shortest ciphertext a Sealed value takes.
    const SEALED: &str =
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+Pw==";

    fn client_sealed() -> Value {
        json!({
            "sealedFor": "00000000-0000-4000-8000-000000000002",
            "value": { "sealed": SEALED, "scope": { "service": "00000000-0000-4000-8000-000000000003" }, "key": "K" }
        })
    }

    #[test]
    fn a_legacy_row_round_trips_byte_identical() {
        let row = r#"{"id":"v1","key":"K","description":null,"exported":false,"valueFingerprint":"v1:ab","value":{"kind":"secret","encryptedValue":{"version":1,"iv":"aXY=","tag":"dGFn","ciphertext":"Y3Q="}}}"#;
        let parsed: SavedVariableIntent = serde_json::from_str(row).unwrap();
        assert!(matches!(
            parsed.value,
            SavedVariableValue::Secret {
                encrypted_value: Some(SecretCiphertext::Legacy(_))
            }
        ));
        assert_eq!(serde_json::to_string(&parsed).unwrap(), row);
        let compiled = r#"{"kind":"secret","variableId":"v1","encryptedValue":{"version":1,"iv":"aXY=","tag":"dGFn","ciphertext":"Y3Q="},"fingerprint":"v1:ab"}"#;
        let parsed: ServiceEnvValue = serde_json::from_str(compiled).unwrap();
        assert_eq!(serde_json::to_string(&parsed).unwrap(), compiled);
    }

    #[test]
    fn a_client_sealed_value_reads_as_its_own_kind_and_round_trips() {
        let value: SecretCiphertext = serde_json::from_value(client_sealed()).unwrap();
        let sealed = value.client_sealed().unwrap();
        assert_eq!(
            sealed.sealed_for,
            Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap()
        );
        assert_eq!(sealed.value.key, "K");
        assert_eq!(serde_json::to_value(&value).unwrap(), client_sealed());
    }

    #[test]
    fn a_client_sealed_value_with_an_unknown_field_is_refused() {
        let mut outer = client_sealed();
        outer
            .as_object_mut()
            .unwrap()
            .insert("version".into(), json!(1));
        assert!(serde_json::from_value::<SecretCiphertext>(outer).is_err());
        let mut inner = client_sealed();
        inner
            .get_mut("value")
            .and_then(Value::as_object_mut)
            .unwrap()
            .insert("extra".into(), json!(1));
        assert!(serde_json::from_value::<SecretCiphertext>(inner).is_err());
    }
}
