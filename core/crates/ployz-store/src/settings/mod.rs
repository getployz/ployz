//! The Settings a Service exposes, addressed as `SERVICE.SETTING`: the Service half
//! of the settings catalog. The stored document behind them is never addressed
//! directly. Each Setting carries what `get`, `explain`, `schema`, validation and
//! completion need; [`crate::catalog`] renders them as JSON Schema.

pub(crate) mod edit;
mod path;
pub(crate) mod query;

pub use path::{NodeName, SettingPath};
pub(crate) use path::{Target, VolumeField, name_a_setting};

use ployz_core::RpcError;
use ployz_core::config::{
    AuthoredServiceConfig, COMMAND_MAX, CPU_LIMIT_MAX, HEALTHCHECK_PATH_MAX,
    HEALTHCHECK_TIMEOUT_DEFAULT, HEALTHCHECK_TIMEOUT_MAX, IMAGE_MAX, MAX_RETRIES_MAX,
    MEM_LIMIT_MAX, REPLICAS_MAX, RESTART_POLICIES, ServiceHealthcheck, ServiceImageCredentials,
    ServiceSource, default_max_retries, default_replicas, parse_service_setting,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::error;
use crate::git::GitSetting;
use crate::policy::{Policy, PolicySetting};
use crate::trusted::Trusted;

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
    /// The Service Template it was created from.
    Template,
    /// A Git-backed Service's source and build.
    Git(GitSetting),
    /// A Git-backed Service's Deployment Policy; see [`crate::policy`].
    Policy(PolicySetting),
}

impl ServiceSetting {
    /// Every Setting, in the order `get` lists them.
    pub(crate) const ALL: [Self; 22] = [
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
        Self::Template,
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

    /// The Setting a core row's `setting` is, if `set` has one.
    pub(crate) const fn of(setting: ployz_core::config::Setting) -> Option<Self> {
        use ployz_core::config::Setting as Core;
        Some(match setting {
            Core::Branch => Self::Git(GitSetting::Branch),
            Core::PrivateDns => Self::PrivateDns,
            Core::PreDeployCommand => Self::PreDeployCommand,
            Core::StartCommand => Self::StartCommand,
            Core::Healthcheck => Self::Healthcheck,
            Core::RestartPolicy => Self::RestartPolicy,
            Core::MaxRetries => Self::MaxRetries,
            Core::Replicas => Self::Replicas,
            Core::CpuLimit => Self::CpuLimit,
            Core::MemLimit => Self::MemLimit,
            Core::BuildMethod => Self::Git(GitSetting::BuildMethod),
            Core::DockerfilePath => Self::Git(GitSetting::DockerfilePath),
            Core::BuildCommand => Self::Git(GitSetting::BuildCommand),
            Core::Source | Core::ManagedHostnames | Core::Routes => return None,
        })
    }

    /// What discards it: the source holds each of its parts as one change row.
    pub(crate) const fn covering(self) -> Target {
        if matches!(
            self,
            Self::Image
                | Self::RegistryCredential
                | Self::Git(GitSetting::Repository | GitSetting::RootDir)
        ) {
            Target::Source
        } else {
            Target::Setting(self)
        }
    }

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
            Self::Template => "template",
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
            Self::Template => "Template",
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
            Self::Template => {
                "The Service Template it was created from, and its version. It changes nothing that runs. Unset forgets it."
            }
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
            | Self::StartCommand
            | Self::Template => self.name(),
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
            // ponytail: a tag, not runtime config; nothing lowers it, so a Deploy never ships it.
            Self::Template | Self::RegistryCredential | Self::Policy(_) => Apply::Immediate,
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
            | Self::StartCommand
            | Self::Template => Value::Null,
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
                        "pattern": "^/[^\\u0000-\\u001f\\u007f-\\u009f]*$",
                        "minLength": 1,
                        "maxLength": HEALTHCHECK_PATH_MAX,
                    },
                    "timeoutSeconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": HEALTHCHECK_TIMEOUT_MAX,
                        "default": HEALTHCHECK_TIMEOUT_DEFAULT,
                    },
                },
                "required": ["path"],
                "additionalProperties": false,
            }),
            Self::Image => {
                json!({ "type": "string", "pattern": "^[^\\u0000]*$", "minLength": 1, "maxLength": IMAGE_MAX })
            }
            Self::MaxRetries => {
                json!({ "type": "integer", "minimum": 0, "maximum": MAX_RETRIES_MAX })
            }
            Self::Replicas => json!({ "type": "integer", "minimum": 1, "maximum": REPLICAS_MAX }),
            Self::PreDeployCommand | Self::StartCommand => {
                json!({ "type": "string", "pattern": "^[^\\u0000]*$", "minLength": 1, "maxLength": COMMAND_MAX })
            }
            Self::RestartPolicy => json!({ "type": "string", "enum": RESTART_POLICIES }),
            Self::Template => json!({
                "type": "object",
                "properties": {
                    "id": { "type": "string", "pattern": crate::catalog::NODE_NAME, "maxLength": 63 },
                    "version": { "type": "integer", "minimum": 1 },
                },
                "required": ["id", "version"],
                "additionalProperties": false,
            }),
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
            Self::Template => json!([{ "id": "postgres", "version": 1 }]),
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
            | Self::StartCommand
            | Self::Template => true,
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
            | Self::StartCommand
            | Self::Template => value,
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
            ServiceHealthcheck::None => HEALTHCHECK_TIMEOUT_DEFAULT,
        };
        let (path, timeout) = match value {
            Value::String(path) => (path, timeout),
            Value::Object(mut fields)
                if fields
                    .keys()
                    .all(|key| key == "path" || key == "timeoutSeconds") =>
            {
                let Some(Value::String(path)) = fields.remove("path") else {
                    return Err(self.invalid("expected a path starting with /"));
                };
                let timeout = match fields.remove("timeoutSeconds") {
                    None => timeout,
                    Some(given) => serde_json::from_value(self.coerce_number(given))
                        .map_err(|_| self.invalid("timeoutSeconds: expected whole seconds"))?,
                };
                (path, timeout)
            }
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => {
                return Err(self.invalid("expected a path, or {\"path\", \"timeoutSeconds\"}"));
            }
        };
        let healthcheck = ServiceHealthcheck::Http {
            path,
            timeout_seconds: timeout,
        };
        Ok(serde_json::to_value(healthcheck).expect("a healthcheck is JSON"))
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
                let mut details = serde_json::Map::new();
                details.insert("valid_children".into(), names.collect::<Vec<_>>().into());
                if let Some(closest) = closest {
                    details.insert("did_you_mean".into(), closest.into());
                }
                if let Some(setting) = Self::ALL
                    .into_iter()
                    .find(|setting| Some(setting.name()) == closest)
                {
                    details.insert("expected".into(), setting.expected());
                    if let Some(example) = setting.examples().get(0) {
                        details.insert("example".into(), example.clone());
                    }
                }
                error::invalid("Unknown Service Setting", details.into())
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
    // The source: never its repository's ID or authority, nor its stored version.
    if field == "source" {
        let mut value = value;
        if let Some(fields) = value.as_object_mut() {
            for hidden in ["repositoryId", "access", "version"] {
                fields.remove(hidden);
            }
        }
        return value;
    }
    ServiceSetting::of_field(field).map_or(value.clone(), |setting| setting.shown(value))
}

/// An image Service's source, checked by core's field rules.
pub(crate) fn image_source(
    image: String,
    credentials: ServiceImageCredentials,
) -> Result<ServiceSource, RpcError> {
    let setting = ServiceSetting::Image;
    // Checked where an image is written, not where Saved State is read, so an Environment holding one from
    // before this check still opens.
    setting.validated("imageReference", json!(image))?;
    let source = ServiceSource::Image {
        version: 1,
        image,
        credentials,
    };
    let source = serde_json::to_value(source).expect("a Service source is JSON");
    setting.decode(setting.validated("source", source)?)
}
