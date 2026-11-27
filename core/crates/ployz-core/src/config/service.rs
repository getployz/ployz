//! Shared authored service settings and their compiled environment and attachment projections.

use std::collections::BTreeMap;
use std::num::NonZeroU8;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::{ClientSealedValue, Region, RestartPolicy, ServiceName, UpdateOrder};

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
    /// None sends SIGTERM when `start_command` replaces the image's command, else the
    /// image's own STOPSIGNAL. Skipped when unset, so documents without it hash the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_signal: Option<ServiceStopSignal>,
    /// Seconds a replica has to stop before it is killed. None is
    /// [`STOP_GRACE_PERIOD_DEFAULT`](super::STOP_GRACE_PERIOD_DEFAULT). Skipped when unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub stop_grace_period: Option<u16>,
    #[serde(default = "default_replicas")]
    pub replicas: Replicas,
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
pub const fn default_replicas() -> Replicas {
    Replicas::Count(1)
}

/// How many replicas a Service runs. A count runs in the Environment's region, or on
/// any Server when it has none. Counts by region run in those regions instead.
/// A count is written as a bare number, so documents from before regions hash the same.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum Replicas {
    Count(u8),
    PerRegion(RegionReplicas),
}

impl Replicas {
    /// Every replica, over all its regions.
    #[must_use]
    pub fn total(&self) -> u32 {
        match self {
            Self::Count(count) => u32::from(*count),
            Self::PerRegion(regions) => {
                regions.0.values().map(|count| u32::from(count.get())).sum()
            }
        }
    }

    #[must_use]
    pub fn per_region(&self) -> Option<&BTreeMap<Region, NonZeroU8>> {
        match self {
            Self::Count(_) => None,
            Self::PerRegion(regions) => Some(&regions.0),
        }
    }

    /// The regions its counts are by. Empty for one count.
    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.per_region().into_iter().flat_map(BTreeMap::keys)
    }

    /// `count` replicas where one count goes: when the counts are by region, in the
    /// region with the most, the first by name of several, so a PR Environment's one
    /// replica runs where most of the Service does.
    #[must_use]
    pub fn only(&self, count: NonZeroU8) -> Self {
        match self {
            Self::Count(_) => Self::Count(count.get()),
            Self::PerRegion(regions) => Self::PerRegion(RegionReplicas(
                regions
                    .0
                    .iter()
                    .max_by_key(|(region, replicas)| (**replicas, std::cmp::Reverse(*region)))
                    .map(|(region, _)| (region.clone(), count))
                    .into_iter()
                    .collect(),
            )),
        }
    }
}

/// A replica count for each region a Service runs in. Never empty.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    try_from = "BTreeMap<String, NonZeroU8>",
    into = "BTreeMap<Region, NonZeroU8>"
)]
#[ts(type = "{ [region in string]: number }")]
pub struct RegionReplicas(BTreeMap<Region, NonZeroU8>);

/// Why counts by region were refused.
#[derive(Debug, thiserror::Error)]
pub enum RegionReplicasError {
    #[error("replicas by region name no region")]
    Empty,
    #[error("region {0} has two counts")]
    Repeated(Region),
    #[error("a region is the value of a Server's region label")]
    Region(#[source] crate::ValueError),
}

impl RegionReplicas {
    /// # Errors
    /// Refuses no regions, and two names for one region, such as `fsn` and `FSN`.
    pub fn new(
        counts: impl IntoIterator<Item = (Region, NonZeroU8)>,
    ) -> Result<Self, RegionReplicasError> {
        let mut regions = BTreeMap::new();
        for (region, count) in counts {
            if regions.insert(region.clone(), count).is_some() {
                return Err(RegionReplicasError::Repeated(region));
            }
        }
        if regions.is_empty() {
            return Err(RegionReplicasError::Empty);
        }
        Ok(Self(regions))
    }
}

impl TryFrom<BTreeMap<String, NonZeroU8>> for RegionReplicas {
    type Error = RegionReplicasError;
    fn try_from(counts: BTreeMap<String, NonZeroU8>) -> Result<Self, Self::Error> {
        Self::new(
            counts
                .into_iter()
                .map(|(region, count)| Ok((Region::parse(region)?, count)))
                .collect::<Result<Vec<_>, crate::ValueError>>()
                .map_err(RegionReplicasError::Region)?,
        )
    }
}

impl From<RegionReplicas> for BTreeMap<Region, NonZeroU8> {
    fn from(regions: RegionReplicas) -> Self {
        regions.0
    }
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

impl ServiceSource {
    /// The source a Service has until it gets one: nothing, at the repository root.
    #[must_use]
    pub fn empty() -> Self {
        Self::Empty {
            version: 1,
            root_dir: "/".to_owned(),
        }
    }

    /// The image Service `service`'s build or upload will produce, before it exists:
    /// binding replaces it with verified content.
    #[must_use]
    pub fn pending(service: &ServiceName) -> Self {
        Self::Image {
            version: 1,
            image: crate::pending_image(service),
            credentials: ServiceImageCredentials::None,
        }
    }

    /// Whether this builds from the Git repository `repository_id`.
    #[must_use]
    pub fn builds_repository(&self, repository_id: u64) -> bool {
        matches!(self, Self::Git { repository_id: at, .. } if *at == repository_id)
    }
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
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "type",
    rename_all = "kebab-case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ServiceHealthcheck {
    #[default]
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
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
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

/// The signal that asks a replica to stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "UPPERCASE")]
pub enum ServiceStopSignal {
    Sigterm,
    Sigint,
    Sigquit,
    Sighup,
    Sigusr1,
    Sigusr2,
    Sigwinch,
}

impl ServiceStopSignal {
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sigterm => "SIGTERM",
            Self::Sigint => "SIGINT",
            Self::Sigquit => "SIGQUIT",
            Self::Sighup => "SIGHUP",
            Self::Sigusr1 => "SIGUSR1",
            Self::Sigusr2 => "SIGUSR2",
            Self::Sigwinch => "SIGWINCH",
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

/// The Dockerfile a Dockerfile Build reads when `dockerfilePath` is unset.
pub const DEFAULT_DOCKERFILE_PATH: &str = "Dockerfile";

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

impl ValuePart {
    /// The Service lineage this part reads and the key it reads of it, if it is a
    /// reference to another Service: what orders a Deploy and links Services on the canvas.
    #[must_use]
    pub fn service_ref(&self) -> Option<(&str, &str)> {
        match self {
            Self::Ref {
                owner: ValuePartOwner::Service { lineage_id },
                key,
            } => Some((lineage_id.as_str(), key.as_str())),
            Self::Ref { .. } | Self::Text { .. } => None,
        }
    }

    /// The Service lineage this part reads, if it is a reference to one.
    #[must_use]
    pub fn service_lineage(&self) -> Option<&str> {
        self.service_ref().map(|(lineage, _)| lineage)
    }
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
    use uuid::Uuid;

    use super::*;
    use crate::config::{SavedVariableIntent, VariableValue};

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
    fn typed_defaults_serialize_as_the_literals_they_replace() {
        fn json(value: impl Serialize) -> String {
            json!(value).to_string()
        }
        assert_eq!(
            json(ServiceSource::empty()),
            r#"{"rootDir":"/","type":"empty","version":1}"#
        );
        assert_eq!(json(ServiceHealthcheck::default()), r#"{"type":"none"}"#);
        assert_eq!(json(ServiceRestartPolicy::default()), r#""unless-stopped""#);
        assert_eq!(json(default_max_retries()), "10");
        assert_eq!(json(default_replicas()), "1");
        assert_eq!(json(BuildMethod::default()), r#""railpack""#);
    }

    #[test]
    fn a_replica_count_stays_a_bare_number_beside_counts_by_region() {
        assert_eq!(json!(Replicas::Count(2)).to_string(), "2");
        let by_region: Replicas = serde_json::from_str(r#"{"syd":1,"FSN":2}"#).unwrap();
        assert_eq!(json!(by_region).to_string(), r#"{"fsn":2,"syd":1}"#);
        assert_eq!(by_region.total(), 3);
        for refused in ["{}", r#"{"fsn":0}"#, r#"{"fsn":1,"FSN":1}"#] {
            assert!(
                serde_json::from_str::<Replicas>(refused).is_err(),
                "{refused}"
            );
        }
    }

    #[test]
    fn one_replica_of_counts_by_region_runs_in_the_region_with_the_most() {
        let one = |counts: &str| {
            let counts: Replicas = serde_json::from_str(counts).unwrap();
            json!(counts.only(NonZeroU8::MIN)).to_string()
        };
        assert_eq!(one(r#"{"fsn":1,"syd":3}"#), r#"{"syd":1}"#);
        assert_eq!(one(r#"{"lhr":2,"fsn":2,"syd":1}"#), r#"{"fsn":1}"#);
        assert_eq!(one("4"), "1");
    }

    #[test]
    fn a_service_document_from_before_regions_round_trips_byte_identical() {
        let document = r#"{"version":2,"source":{"type":"git","version":2,"repository":"acme/api","repositoryId":42,"access":{"type":"github-installation","installationId":7},"rootDir":"/apps/api","branch":{"type":"connected","name":"main"}},"preDeployCommand":"migrate","startCommand":"serve","healthcheck":{"type":"http","path":"/up","timeoutSeconds":5},"restartPolicy":"always","maxRetries":3,"replicas":2,"cpuLimit":0.5,"memLimit":256.0,"privateDns":"api","routes":[],"managedHostnames":[],"build":{"buildMethod":"dockerfile","dockerfilePath":"Dockerfile.prod","command":"make"}}"#;
        let parsed: AuthoredServiceConfig = serde_json::from_str(document).unwrap();
        assert_eq!(parsed.replicas, Replicas::Count(2));
        assert_eq!(serde_json::to_string(&parsed).unwrap(), document);
    }

    #[test]
    fn a_legacy_row_round_trips_byte_identical() {
        let row = r#"{"id":"v1","key":"K","description":null,"exported":false,"valueFingerprint":"v1:ab","value":{"kind":"secret","encryptedValue":{"version":1,"iv":"aXY=","tag":"dGFn","ciphertext":"Y3Q="}}}"#;
        let parsed: SavedVariableIntent = serde_json::from_str(row).unwrap();
        assert!(matches!(
            parsed.value,
            VariableValue::Sealed {
                ciphertext: Some(SecretCiphertext::Legacy(_)),
                ..
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
