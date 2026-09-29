//! The Settings a Service exposes, addressed as `SERVICE.SETTING`: the Service half
//! of the settings catalog. The stored document behind them is never addressed
//! directly. Each Setting carries what `get`, `explain`, `schema`, validation and
//! completion need; [`crate::catalog`] renders them as JSON Schema.

use std::fmt;

use ployz_core::config::{
    AuthoredServiceConfig, COMMAND_MAX, CPU_LIMIT_MAX, HEALTHCHECK_PATH_MAX,
    HEALTHCHECK_TIMEOUT_MAX, IMAGE_MAX, MAX_RETRIES_MAX, MEM_LIMIT_MAX, REPLICAS_MAX,
    RESTART_POLICIES, ServiceHealthcheck, ServiceImageCredentials, ServiceSource,
    default_max_retries, default_replicas, parse_service_setting,
};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::error;
use crate::git::GitSetting;
use crate::id::VolumeName;
use crate::policy::{Policy, PolicySetting};
use crate::trusted::Trusted;
use crate::variables::VariableKey;

/// Whether a change waits for a Deploy or takes effect at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    /// Saved in Working State; running Services see it after the next Deploy.
    Staged,
    /// Takes effect as soon as it is written.
    Immediate,
}

/// One Setting of one Service. Names are the dashboard's field names, so `diff`,
/// `get` and `set` use the same words.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServiceSetting {
    CpuLimit,
    /// An HTTP readiness check each replica must pass.
    Healthcheck,
    Image,
    MaxRetries,
    MemLimit,
    PreDeployCommand,
    /// The name other Services reach it at; a rename keeps it.
    PrivateDns,
    /// What an image Service's private image is pulled with; see [`crate::registry`].
    RegistryCredential,
    Replicas,
    RestartPolicy,
    StartCommand,
    /// A Git-backed Service's source and build.
    Git(GitSetting),
    /// A Git-backed Service's Deployment Policy; see [`crate::policy`].
    Policy(PolicySetting),
}

impl ServiceSetting {
    /// Every Setting, in the order `get` lists them.
    pub(crate) const ALL: [Self; 21] = [
        Self::CpuLimit,
        Self::Healthcheck,
        Self::Image,
        Self::MaxRetries,
        Self::MemLimit,
        Self::PreDeployCommand,
        Self::PrivateDns,
        Self::RegistryCredential,
        Self::Replicas,
        Self::RestartPolicy,
        Self::StartCommand,
        Self::Git(GitSetting::Repository),
        Self::Git(GitSetting::Branch),
        Self::Git(GitSetting::RootDir),
        Self::Git(GitSetting::BuildMethod),
        Self::Git(GitSetting::DockerfilePath),
        Self::Git(GitSetting::BuildCommand),
        Self::Policy(PolicySetting::AutoDeploy),
        Self::Policy(PolicySetting::WaitForCi),
        Self::Policy(PolicySetting::WatchPaths),
        Self::Policy(PolicySetting::PreferredBuilder),
    ];

    /// The Setting a core change row at `field` writes, if one does.
    pub(crate) fn of_field(field: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|setting| setting.field() == field)
    }

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CpuLimit => "cpuLimit",
            Self::Healthcheck => "healthcheck",
            Self::Image => "image",
            Self::MaxRetries => "maxRetries",
            Self::MemLimit => "memLimit",
            Self::PreDeployCommand => "preDeployCommand",
            Self::PrivateDns => "privateDns",
            Self::RegistryCredential => "registryCredential",
            Self::Replicas => "replicas",
            Self::RestartPolicy => "restartPolicy",
            Self::StartCommand => "startCommand",
            Self::Git(git) => git.name(),
            Self::Policy(policy) => policy.name(),
        }
    }

    /// The dashboard's field label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CpuLimit => "CPU limit",
            Self::Healthcheck => "Healthcheck",
            Self::Image => "Container image",
            Self::MaxRetries => "Max retries",
            Self::MemLimit => "Memory limit",
            Self::PreDeployCommand => "Pre-deploy command",
            Self::PrivateDns => "Private DNS",
            Self::RegistryCredential => "Registry credentials",
            Self::Replicas => "Replicas",
            Self::RestartPolicy => "Restart policy",
            Self::StartCommand => "Start command",
            Self::Git(git) => git.label(),
            Self::Policy(policy) => policy.label(),
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::CpuLimit => "Most vCPUs each replica may use. Unset means no limit.",
            Self::Healthcheck => {
                "An HTTP check a new replica must pass before it takes traffic: a path on the container PORT, and how many seconds it may take (default 300). Text sets the path. Unset turns it off."
            }
            Self::Image => "The container image each replica runs.",
            Self::MaxRetries => "How often an on-failure restart policy restarts a replica.",
            Self::MemLimit => "Most memory each replica may use, in GB. Unset means no limit.",
            Self::PreDeployCommand => {
                "Runs once in a new replica before a Deploy starts the Service."
            }
            Self::PrivateDns => {
                "The name other Services in the Environment reach it at, as NAME.internal. Renaming the Service keeps it. Unset returns it to the Service's name."
            }
            Self::RegistryCredential => {
                "The username and secret a private image is pulled with. A new secret replaces the stored one at once; turning credentials on or off is staged, and unset keeps the stored secret for {\"secret\": true} to turn back on. Reads show {\"secret\": true}; set a secret with --secret or --patch -."
            }
            Self::Replicas => "How many copies of the Service run.",
            Self::RestartPolicy => "When a stopped replica restarts.",
            Self::StartCommand => "Overrides the image's command. Unset runs the image's own.",
            Self::Git(git) => git.description(),
            Self::Policy(policy) => policy.description(),
        }
    }

    /// The core config field this Setting writes, as change rows and restores name it.
    pub(crate) const fn field(self) -> &'static str {
        match self {
            Self::Image => "source.image",
            Self::RegistryCredential => "source.credentials",
            Self::Git(git) => git.field(),
            Self::Policy(policy) => policy.name(),
            Self::CpuLimit
            | Self::Healthcheck
            | Self::MaxRetries
            | Self::MemLimit
            | Self::PreDeployCommand
            | Self::PrivateDns
            | Self::Replicas
            | Self::RestartPolicy
            | Self::StartCommand => self.name(),
        }
    }

    pub(crate) const fn apply(self) -> Apply {
        match self {
            Self::CpuLimit
            | Self::Healthcheck
            | Self::Image
            | Self::MaxRetries
            | Self::MemLimit
            | Self::PreDeployCommand
            | Self::PrivateDns
            | Self::Replicas
            | Self::RestartPolicy
            | Self::StartCommand
            | Self::Git(_) => Apply::Staged,
            Self::RegistryCredential | Self::Policy(_) => Apply::Immediate,
        }
    }

    /// The value `unset` restores; `null` when the Setting has none.
    pub(crate) fn default(self) -> Value {
        match self {
            Self::CpuLimit
            | Self::Healthcheck
            | Self::Image
            | Self::MemLimit
            | Self::PreDeployCommand
            | Self::PrivateDns
            | Self::RegistryCredential
            | Self::StartCommand => Value::Null,
            Self::MaxRetries => json!(default_max_retries()),
            Self::Replicas => json!(default_replicas()),
            Self::RestartPolicy => json!("unless-stopped"),
            Self::Git(git) => git.default(),
            Self::Policy(policy) => policy.default(),
        }
    }

    /// The JSON Schema of a value: its type and allowed values or bounds.
    pub(crate) fn expected(self) -> Value {
        match self {
            Self::CpuLimit => {
                json!({ "type": "number", "exclusiveMinimum": 0, "maximum": CPU_LIMIT_MAX })
            }
            Self::MemLimit => {
                json!({ "type": "number", "exclusiveMinimum": 0, "maximum": MEM_LIMIT_MAX })
            }
            Self::Healthcheck => json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "pattern": "^/",
                        "minLength": 1,
                        "maxLength": HEALTHCHECK_PATH_MAX,
                    },
                    "timeoutSeconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": HEALTHCHECK_TIMEOUT_MAX,
                        "default": HEALTHCHECK_TIMEOUT_MAX,
                    },
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
            Self::Image => json!({ "type": "string", "minLength": 1, "maxLength": IMAGE_MAX }),
            Self::MaxRetries => {
                json!({ "type": "integer", "minimum": 0, "maximum": MAX_RETRIES_MAX })
            }
            Self::Replicas => json!({ "type": "integer", "minimum": 0, "maximum": REPLICAS_MAX }),
            Self::PreDeployCommand | Self::StartCommand => {
                json!({ "type": "string", "minLength": 1, "maxLength": COMMAND_MAX })
            }
            Self::RestartPolicy => json!({ "type": "string", "enum": RESTART_POLICIES }),
            Self::PrivateDns => {
                json!({ "type": "string", "pattern": crate::catalog::NODE_NAME, "maxLength": 63 })
            }
            Self::RegistryCredential => json!({
                "type": "object",
                "properties": {
                    "username": { "type": "string", "minLength": 1, "maxLength": 255 },
                    "secret": { "oneOf": [{ "type": "string", "minLength": 1 }, { "const": true }] },
                },
                "required": ["secret"],
                "additionalProperties": false,
            }),
            Self::Git(git) => git.expected(),
            Self::Policy(policy) => policy.expected(),
        }
    }

    /// One to three values `set` accepts.
    pub(crate) fn examples(self) -> Value {
        match self {
            Self::CpuLimit => json!([0.5, 2]),
            Self::Healthcheck => json!([{ "path": "/health", "timeoutSeconds": 30 }]),
            Self::Image => json!(["nginx:1.27", "ghcr.io/acme/web:1.4.0"]),
            Self::MaxRetries => json!([3]),
            Self::MemLimit => json!([0.5, 4]),
            Self::PreDeployCommand => json!(["npm run migrate"]),
            Self::PrivateDns => json!(["api", "api-v2"]),
            Self::RegistryCredential => json!([{ "secret": true }]),
            Self::Replicas => json!([3]),
            Self::RestartPolicy => json!(["on-failure"]),
            Self::StartCommand => json!(["npm start"]),
            Self::Git(git) => git.examples(),
            Self::Policy(policy) => policy.examples(),
        }
    }

    /// Whether this Setting means anything for a Service with `config`'s source:
    /// `image` for an image, the source and build Settings for a repository.
    pub(crate) const fn applies(self, config: &AuthoredServiceConfig) -> bool {
        match self {
            Self::Image | Self::RegistryCredential => {
                matches!(config.source, ServiceSource::Image { .. })
            }
            // A Service without a source builds from uploads, with the same build Settings.
            Self::Git(
                GitSetting::BuildMethod | GitSetting::DockerfilePath | GitSetting::BuildCommand,
            ) => !matches!(config.source, ServiceSource::Image { .. }),
            Self::Git(_) => matches!(config.source, ServiceSource::Git { .. }),
            Self::Policy(_) => PolicySetting::applies(config),
            Self::CpuLimit
            | Self::Healthcheck
            | Self::MaxRetries
            | Self::MemLimit
            | Self::PreDeployCommand
            | Self::PrivateDns
            | Self::Replicas
            | Self::RestartPolicy
            | Self::StartCommand => true,
        }
    }

    /// Its value for a Service with `config` and Deployment Policy `policy`.
    pub(crate) fn value(self, config: &AuthoredServiceConfig, policy: &Policy) -> Value {
        if let Self::Git(git) = self {
            return git.value(config);
        }
        if let Self::Policy(setting) = self {
            return setting.value(policy);
        }
        match (self, &config.source) {
            (Self::Image, ServiceSource::Image { image, .. }) => return json!(image),
            (Self::RegistryCredential, ServiceSource::Image { credentials, .. }) => {
                return self.shown(json!(credentials));
            }
            (Self::Image | Self::RegistryCredential, _) => return Value::Null,
            (Self::Healthcheck, _) => return self.shown(json!(config.healthcheck)),
            _ => {}
        }
        // Every other Setting is the stored field of the same name.
        serde_json::to_value(config)
            .expect("service settings are JSON")
            .get_mut(self.name())
            .map(Value::take)
            .unwrap_or_default()
    }

    /// A change row's value as `get` shows it, never the stored shape.
    pub(crate) fn shown(self, value: Value) -> Value {
        match self {
            Self::Git(git) => git.shown(value),
            // Never the stored credential reference: only whether one is on.
            Self::RegistryCredential => match value.get("type").and_then(Value::as_str) {
                Some("configured") => json!({ "secret": true }),
                _ if value == Value::Bool(true) => json!({ "secret": true }),
                _ => Value::Null,
            },
            // Off reads as none; on, its path and timeout.
            Self::Healthcheck => match value.get("type").and_then(Value::as_str) {
                Some("http") => json!({
                    "path": value.get("path"),
                    "timeoutSeconds": value.get("timeoutSeconds"),
                }),
                _ => Value::Null,
            },
            Self::Image
            | Self::Policy(_)
            | Self::CpuLimit
            | Self::MaxRetries
            | Self::MemLimit
            | Self::PreDeployCommand
            | Self::PrivateDns
            | Self::Replicas
            | Self::RestartPolicy
            | Self::StartCommand => value,
        }
    }

    /// Validate `value` against this Setting and write it. Text is accepted for any
    /// type, as `set PATH=VALUE` sends it. A repository needs `trusted` evidence.
    pub(crate) fn set(
        self,
        config: &mut AuthoredServiceConfig,
        value: Value,
        trusted: &Trusted,
    ) -> Result<(), RpcError> {
        if value.is_null() {
            return Err(self.invalid("null never clears a Setting; unset it"));
        }
        let value = self.coerce(value);
        if let Self::Git(git) = self {
            return git.set(config, value, trusted);
        }
        if let Self::Policy(_) = self {
            return Err(self.invalid("the Deployment Policy is not in the config"));
        }
        if self == Self::Image {
            // An empty Service takes an image as its source.
            let credentials = match &config.source {
                ServiceSource::Image { credentials, .. } => credentials.clone(),
                ServiceSource::Empty { .. } => ServiceImageCredentials::None,
                ServiceSource::Git { .. } => {
                    return Err(self.invalid("this Service builds from a repository"));
                }
            };
            config.source = image_source(self.decode(value)?, credentials)?;
            return Ok(());
        }
        if self == Self::Healthcheck {
            let value = self.healthcheck(value, &config.healthcheck)?;
            let value = self.validated(self.name(), value)?;
            return self.store(config, value);
        }
        let value = self.validated(self.name(), value)?;
        self.store(config, value)
    }

    /// A healthcheck's stored shape from `/path`, or `{"path", "timeoutSeconds"?}`
    /// keeping `current`'s timeout when on.
    fn healthcheck(self, value: Value, current: &ServiceHealthcheck) -> Result<Value, RpcError> {
        let timeout = match current {
            ServiceHealthcheck::Http {
                timeout_seconds, ..
            } => *timeout_seconds,
            ServiceHealthcheck::None => HEALTHCHECK_TIMEOUT_MAX,
        };
        let (path, timeout) = match value {
            Value::String(path) => (path, json!(timeout)),
            Value::Object(mut fields)
                if fields
                    .keys()
                    .all(|key| key == "path" || key == "timeoutSeconds") =>
            {
                let Some(Value::String(path)) = fields.remove("path") else {
                    return Err(self.invalid("expected a path starting with /"));
                };
                let timeout = fields.remove("timeoutSeconds").unwrap_or(json!(timeout));
                (path, self.coerce_number(timeout))
            }
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => {
                return Err(self.invalid("expected a path, or {\"path\", \"timeoutSeconds\"}"));
            }
        };
        Ok(json!({ "type": "http", "path": path, "timeoutSeconds": timeout }))
    }

    /// Return this Setting to its [`default`](Self::default).
    pub(crate) fn unset(self, config: &mut AuthoredServiceConfig) -> Result<(), RpcError> {
        if let Self::Git(git) = self {
            return git.unset(config);
        }
        if let Self::Policy(_) = self {
            return Err(self.invalid("the Deployment Policy is not in the config"));
        }
        match (self, &mut config.source) {
            // Disconnected, the Service is empty until it gets a source again.
            (Self::Image, ServiceSource::Image { .. }) => {
                config.source = ServiceSource::Empty {
                    version: 1,
                    root_dir: "/".to_owned(),
                };
                Ok(())
            }
            (Self::Image, _) => Err(self.invalid("this Service runs no image")),
            (Self::RegistryCredential, ServiceSource::Image { credentials, .. }) => {
                *credentials = ServiceImageCredentials::None;
                Ok(())
            }
            (Self::RegistryCredential, _) => Err(self.invalid("only an image Service has one")),
            (Self::Healthcheck, _) => {
                config.healthcheck = ServiceHealthcheck::None;
                Ok(())
            }
            _ => self.store(config, self.default()),
        }
    }

    fn store(self, config: &mut AuthoredServiceConfig, value: Value) -> Result<(), RpcError> {
        let mut document = serde_json::to_value(&*config).expect("service settings are JSON");
        if let Some(fields) = document.as_object_mut() {
            fields.insert(self.name().into(), value);
        }
        *config = self.decode(document)?;
        Ok(())
    }

    /// Text that spells a number becomes the number, for numeric Settings.
    fn coerce(self, value: Value) -> Value {
        let numeric = matches!(
            self.expected().get("type").and_then(Value::as_str),
            Some("integer" | "number")
        );
        match numeric {
            true => self.coerce_number(value),
            false => value,
        }
    }

    /// Text that spells a number, as the number.
    fn coerce_number(self, value: Value) -> Value {
        match value.as_str() {
            Some(number) => number
                .trim()
                .parse::<serde_json::Number>()
                .map_or(value, Value::Number),
            None => value,
        }
    }

    /// Validate through core's field rules, reporting this Setting's path, never the value.
    fn validated(self, field: &str, value: Value) -> Result<Value, RpcError> {
        parse_service_setting(json!({ "field": field, "value": value }))
            .map_err(|error| self.invalid(&error.message))
    }

    pub(crate) fn decode<T: serde::de::DeserializeOwned>(
        self,
        value: Value,
    ) -> Result<T, RpcError> {
        serde_json::from_value(value).map_err(|_| self.invalid("invalid value"))
    }

    /// A refused value: what this Setting expects and an example, never the value sent.
    pub(crate) fn invalid(self, message: &str) -> RpcError {
        error::invalid(
            format!("{}: {message}", self.name()),
            json!({
                "setting": self.name(),
                "expected": self.expected(),
                "example": self.examples().get(0),
            }),
        )
    }

    /// The Setting named `name`, or why none is: the closest name, with what it
    /// expects and an example, and every name there is.
    pub(crate) fn parse(name: &str) -> Result<Self, RpcError> {
        Self::ALL
            .into_iter()
            .find(|setting| setting.name() == name)
            .ok_or_else(|| {
                // `env` and `mounts` hold keyed children, so they are suggested too.
                let names = Self::ALL
                    .map(Self::name)
                    .into_iter()
                    .chain(["env", "mounts"]);
                let first = name.split('.').next().unwrap_or(name);
                let closest = error::did_you_mean(first, names.clone());
                let setting = Self::ALL
                    .into_iter()
                    .find(|setting| Some(setting.name()) == closest);
                error::invalid(
                    "Unknown Service Setting",
                    json!({
                        "did_you_mean": closest,
                        "expected": setting.map(Self::expected),
                        "example": setting.and_then(|setting| setting.examples().get(0).cloned()),
                        "valid_children": names.collect::<Vec<_>>(),
                    }),
                )
            })
    }
}

/// A core change row's value at `field` as reads show it: never a secret, a
/// credential or a repository ID. A variable shows its text or `{"secret": true}`.
pub(crate) fn shown(field: &str, value: Value) -> Value {
    if field.starts_with("env.") || field.starts_with("variables.") {
        return match value.get("kind").and_then(Value::as_str) {
            Some("secret") => json!({ "secret": true }),
            Some(_) => value.get("value").cloned().unwrap_or_default(),
            None => value,
        };
    }
    ServiceSetting::of_field(field).map_or(value.clone(), |setting| setting.shown(value))
}

/// An image Service's source, checked by core's field rules.
pub(crate) fn image_source(
    image: String,
    credentials: ServiceImageCredentials,
) -> Result<ServiceSource, RpcError> {
    let setting = ServiceSetting::Image;
    let source = ServiceSource::Image {
        version: 1,
        image,
        credentials,
    };
    let source = serde_json::to_value(source).expect("a Service source is JSON");
    setting.decode(setting.validated("source", source)?)
}

/// A node of an Environment by name: `SERVICE` for a Service, `volumes.VOLUME` for a
/// Volume.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub enum NodeName {
    Service(ServiceName),
    Volume(VolumeName),
}

impl NodeName {
    /// Parse `SERVICE` or `volumes.VOLUME`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for anything else, never echoing it.
    pub fn parse(name: &str) -> Result<Self, RpcError> {
        match name.strip_prefix("volumes.") {
            Some(volume) => Ok(Self::Volume(VolumeName::parse(volume)?)),
            None => ServiceName::parse(name).map(Self::Service).map_err(|_| {
                error::invalid(
                    "Expected a node name: SERVICE, or volumes.VOLUME for a Volume",
                    json!({ "example": "web" }),
                )
            }),
        }
    }
}

impl fmt::Display for NodeName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Service(service) => write!(formatter, "{service}"),
            Self::Volume(volume) => write!(formatter, "volumes.{volume}"),
        }
    }
}

impl TryFrom<String> for NodeName {
    type Error = RpcError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<NodeName> for String {
    fn from(value: NodeName) -> Self {
        value.to_string()
    }
}

/// What a request addresses in an Environment: `SERVICE` for a whole Service,
/// `SERVICE.SETTING` for one of its Settings, `SERVICE.env.KEY` for one of its
/// variables, `SERVICE.env.KEY.exported` for whether other Services see it,
/// `SERVICE.mounts.VOLUME` for where it mounts a Volume, or `volumes.VOLUME` for a
/// whole Volume, which has no Settings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct SettingPath(Addressed);

#[derive(Clone, Debug, Eq, PartialEq)]
enum Addressed {
    Service(ServiceName, Option<Target>),
    Volume(VolumeName),
}

/// What a path addresses inside its Service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Target {
    Setting(ServiceSetting),
    /// A variable's value.
    Variable(VariableKey),
    /// Whether a variable is exported.
    Exported(VariableKey),
    /// Where the Service mounts a Volume.
    Mount(VolumeName),
}

impl SettingPath {
    /// Parse `SERVICE`, `SERVICE.SETTING`, `SERVICE.env.KEY`,
    /// `SERVICE.env.KEY.exported`, `SERVICE.mounts.VOLUME` or `volumes.VOLUME`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a malformed path or an unknown Setting, never
    /// echoing the path.
    pub fn parse(path: &str) -> Result<Self, RpcError> {
        if path == "volumes" {
            return Err(error::invalid(
                "Name a Volume: volumes.VOLUME",
                json!({ "example": "volumes.data" }),
            ));
        }
        if let Some(volume) = path.strip_prefix("volumes.") {
            if volume.contains('.') {
                return Err(error::invalid(
                    "A Volume has no Settings: address it as volumes.VOLUME",
                    json!({ "example": "volumes.data" }),
                ));
            }
            return Ok(Self(Addressed::Volume(VolumeName::parse(volume)?)));
        }
        let (service, rest) = path.split_once('.').unzip();
        let service = ServiceName::parse(service.unwrap_or(path)).map_err(|_| {
            error::invalid(
                "Expected a path like SERVICE.SETTING",
                json!({ "example": "web.replicas" }),
            )
        })?;
        let target = match rest {
            None => None,
            Some("env") => {
                return Err(error::invalid(
                    "Name a variable: SERVICE.env.KEY",
                    json!({ "example": format!("{service}.env.DATABASE_URL") }),
                ));
            }
            Some("mounts") => {
                return Err(error::invalid(
                    "Name a Volume: SERVICE.mounts.VOLUME",
                    json!({ "example": format!("{service}.mounts.data") }),
                ));
            }
            Some(rest) if rest.starts_with("mounts.") => {
                let volume = rest.strip_prefix("mounts.").unwrap_or_default();
                Some(Target::Mount(VolumeName::parse(volume)?))
            }
            Some(rest) => Some(match rest.strip_prefix("env.") {
                None => Target::Setting(ServiceSetting::parse(rest)?),
                Some(variable) => match variable.split_once('.') {
                    None => Target::Variable(VariableKey::parse(variable)?),
                    Some((key, "exported")) => Target::Exported(VariableKey::parse(key)?),
                    Some(_) => {
                        return Err(error::invalid(
                            "Unknown variable field",
                            json!({ "valid_children": ["exported"] }),
                        ));
                    }
                },
            }),
        };
        Ok(Self(Addressed::Service(service, target)))
    }

    /// The node it is in.
    #[must_use]
    pub fn node(&self) -> NodeName {
        match &self.0 {
            Addressed::Service(service, _) => NodeName::Service(service.clone()),
            Addressed::Volume(volume) => NodeName::Volume(volume.clone()),
        }
    }

    /// The Service it is in; none for a Volume.
    #[must_use]
    pub const fn service(&self) -> Option<&ServiceName> {
        match &self.0 {
            Addressed::Service(service, _) => Some(service),
            Addressed::Volume(_) => None,
        }
    }

    /// The Service whose Settings it addresses, or why a Volume has none.
    pub(crate) fn settings_of(&self) -> Result<&ServiceName, RpcError> {
        self.service().ok_or_else(|| {
            error::invalid(
                "A Volume has no Settings: name a Service, SERVICE.SETTING",
                json!({ "example": "web.replicas" }),
            )
        })
    }

    /// What it addresses inside its Service; none for a whole Service or a Volume.
    pub(crate) const fn target(&self) -> Option<&Target> {
        match &self.0 {
            Addressed::Service(_, target) => target.as_ref(),
            Addressed::Volume(_) => None,
        }
    }

    /// The path of `service` as a whole.
    pub(crate) fn whole(service: &ServiceName) -> Self {
        Self(Addressed::Service(service.clone(), None))
    }

    /// The path of Volume `volume` as a whole.
    pub(crate) fn volume(volume: &VolumeName) -> Self {
        Self(Addressed::Volume(volume.clone()))
    }

    /// The path of one Setting of `service`.
    pub(crate) fn of(service: &ServiceName, setting: ServiceSetting) -> Self {
        Self::at(service, Target::Setting(setting))
    }

    pub(crate) fn at(service: &ServiceName, target: Target) -> Self {
        Self(Addressed::Service(service.clone(), Some(target)))
    }
}

impl fmt::Display for SettingPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Addressed::Service(service, target) = &self.0 else {
            return write!(formatter, "{}", self.node());
        };
        match target {
            Some(Target::Setting(setting)) => write!(formatter, "{service}.{}", setting.name()),
            Some(Target::Variable(key)) => write!(formatter, "{service}.env.{key}"),
            Some(Target::Exported(key)) => write!(formatter, "{service}.env.{key}.exported"),
            Some(Target::Mount(volume)) => write!(formatter, "{service}.mounts.{volume}"),
            None => write!(formatter, "{service}"),
        }
    }
}

impl TryFrom<String> for SettingPath {
    type Error = RpcError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<SettingPath> for String {
    fn from(value: SettingPath) -> Self {
        value.to_string()
    }
}

/// A path that stops at its node: name one of its Settings.
pub(crate) fn name_a_setting(node: impl std::fmt::Display) -> RpcError {
    error::invalid(
        "Name a Setting: SERVICE.SETTING",
        json!({
            "valid_children": ServiceSetting::ALL.map(ServiceSetting::name),
            "example": format!("{node}.replicas"),
        }),
    )
}
