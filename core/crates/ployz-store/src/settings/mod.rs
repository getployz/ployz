//! The Settings a Service exposes, addressed as `SERVICE.SETTING`: the Service half
//! of the settings catalog. The stored document behind them is never addressed
//! directly. Each Setting carries what `get`, `explain`, `schema`, validation and
//! completion need; [`crate::catalog`] renders them as JSON Schema.

pub(crate) mod edit;
mod path;
pub(crate) mod query;
mod region;
mod secret;

pub use secret::{SecretCell, SecretInput};

pub(crate) use path::{
    ConfigField, NodeField, Target, config_name, domains_are_read_only, name_a_setting,
};
pub use path::{EnvironmentSetting, NodeName, PlainValue, SettingPath};
pub use region::refuse_unknown as refuse_unknown_region;

pub(crate) mod size;

use ployz_core::config::{
    AuthoredServiceConfig, COMMAND_MAX, CPU_LIMIT_MAX, HEALTHCHECK_PATH_MAX,
    HEALTHCHECK_TIMEOUT_DEFAULT, HEALTHCHECK_TIMEOUT_MAX, IMAGE_MAX, MAX_RETRIES_MAX,
    MEM_LIMIT_MAX, REPLICAS_MAX, RESTART_POLICIES, Replicas, STOP_GRACE_PERIOD_DEFAULT,
    STOP_GRACE_PERIOD_MAX, STOP_SIGNALS, ServiceBuildConfig, ServiceHealthcheck,
    ServiceImageCredentials, ServiceRestartPolicy, ServiceSource, Setting, UPDATE_ORDERS,
    default_max_retries, default_replicas, parse_service_setting,
};
use ployz_core::{Hint, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::error;
use crate::followup::{FollowUp, Place};
use crate::git::GitSetting;
use crate::policy::{Policy, PolicySetting};
use crate::scope::EnvironmentSummary;
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

/// What one Setting is: everything `get`, `explain`, the catalog and validation say
/// about it. Behavior that differs by Setting beyond its field (`set`, `unset`) stays in code.
#[derive(Clone, Copy)]
pub(crate) struct Spec {
    pub(crate) name: &'static str,
    /// The dashboard's field label.
    pub(crate) label: &'static str,
    pub(crate) description: &'static str,
    /// The core config field it writes, as change rows and restores name it.
    pub(crate) field: &'static str,
    /// Its config field, when it is one field `get`, `set`, `unset` and a review's
    /// restore move whole.
    pub(crate) access: Option<Access>,
    /// The core Setting a review row of it is, if any.
    pub(crate) core: Option<Setting>,
    pub(crate) apply: Apply,
    pub(crate) needs: Needs,
    /// What `unset` restores for the Service named, if one is; `null` for none.
    pub(crate) default: fn(Option<&ServiceName>) -> Value,
    /// The JSON Schema of a value: its type and allowed values or bounds.
    pub(crate) expected: fn() -> Value,
    /// One to three values `set` accepts.
    pub(crate) examples: fn() -> Value,
}

/// One config field, read and written as the JSON it serializes to.
#[derive(Clone, Copy)]
pub(crate) struct Access {
    pub(crate) read: fn(&AuthoredServiceConfig) -> Value,
    pub(crate) restore: fn(&mut AuthoredServiceConfig, Value) -> Result<(), serde_json::Error>,
}

/// The [`Access`] to the config field at `config.$field`.
macro_rules! access {
    ($($field:ident).+) => {
        $crate::settings::Access {
            read: |config| serde_json::json!(config.$($field).+),
            restore: |config, value| {
                config.$($field).+ = serde_json::from_value(value)?;
                Ok(())
            },
        }
    };
}
pub(crate) use access;

/// The sources a Setting means anything for.
#[derive(Clone, Copy)]
pub(crate) enum Needs {
    Any,
    Image,
    /// Anything built: a repository, or uploads to a Service without a source.
    Built,
    Git,
}

/// A command's schema.
pub(crate) fn command() -> Value {
    json!({ "type": "string", "pattern": "^[^\\u0000]*$", "minLength": 1, "maxLength": COMMAND_MAX })
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
    UpdateOrder,
    StopSignal,
    StopGracePeriod,
    /// A Git-backed Service's source and build.
    Git(GitSetting),
    /// A Git-backed Service's Deployment Policy; see [`crate::policy`].
    Policy(PolicySetting),
}

impl ServiceSetting {
    /// Every Setting, in the order `get` lists them.
    pub(crate) const ALL: [Self; 25] = [
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
        Self::StopGracePeriod,
        Self::StopSignal,
        Self::Template,
        Self::UpdateOrder,
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

    /// What this Setting is: one row for each.
    pub(crate) const fn spec(self) -> Spec {
        match self {
            Self::CpuLimit => Spec {
                name: "cpuLimit",
                label: "CPU limit",
                description: "Most vCPUs each replica may use. Unset means no limit.",
                field: "cpuLimit",
                access: Some(access!(cpu_limit)),
                core: Some(Setting::CpuLimit),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| Value::Null,
                expected: || json!({ "type": "number", "exclusiveMinimum": 0, "maximum": CPU_LIMIT_MAX }),
                examples: || json!([0.5, 2]),
            },
            Self::Healthcheck => Spec {
                name: "healthcheck",
                label: "Healthcheck",
                description: "A check a new replica must pass before it takes traffic: an HTTP path on the port the domains send traffic to (PORT with no domains, or when they differ) or a shell command, and how many seconds it may take (default 300). Text sets the path. Unset turns it off, along with the image's own check.",
                field: "healthcheck",
                access: Some(access!(healthcheck)),
                core: Some(Setting::Healthcheck),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| Value::Null,
                expected: || {
                    let path = json!({
                        "type": "string",
                        "pattern": "^/[^\\u0000-\\u001f\\u007f-\\u009f]*$",
                        "minLength": 1,
                        "maxLength": HEALTHCHECK_PATH_MAX,
                    });
                    let command = json!({
                        "type": "string",
                        "pattern": "^[^\\u0000]*$",
                        "minLength": 1,
                        "maxLength": COMMAND_MAX,
                    });
                    let timeout = json!({
                        "type": "integer",
                        "minimum": 1,
                        "maximum": HEALTHCHECK_TIMEOUT_MAX,
                        "default": HEALTHCHECK_TIMEOUT_DEFAULT,
                    });
                    // Each alternative names its own fields, so a path and a command together match neither.
                    let only = |field: &str, schema: &Value| {
                        json!({
                            "properties": { field: schema, "timeoutSeconds": timeout },
                            "required": [field],
                            "additionalProperties": false,
                        })
                    };
                    json!({
                        "type": "object",
                        "properties": { "path": path, "command": command, "timeoutSeconds": timeout },
                        "anyOf": [only("path", &path), only("command", &command)],
                        "additionalProperties": false,
                    })
                },
                examples: || {
                    json!([
                        { "path": "/health", "timeoutSeconds": 30 },
                        { "command": "pg_isready -h 127.0.0.1", "timeoutSeconds": 60 },
                    ])
                },
            },
            Self::Image => Spec {
                name: "image",
                label: "Container image",
                description: "The container image each replica runs.",
                field: "source.image",
                access: None,
                core: None,
                apply: Apply::Staged,
                needs: Needs::Image,
                default: |_| Value::Null,
                expected: || json!({ "type": "string", "pattern": "^[^\\u0000]*$", "minLength": 1, "maxLength": IMAGE_MAX }),
                examples: || json!(["nginx:1.27", "ghcr.io/acme/web:1.4.0"]),
            },
            Self::MaxRetries => Spec {
                name: "maxRetries",
                label: "Max retries",
                description: "How often an on-failure restart policy restarts a replica.",
                field: "maxRetries",
                access: Some(access!(max_retries)),
                core: Some(Setting::MaxRetries),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| json!(default_max_retries()),
                expected: || json!({ "type": "integer", "minimum": 0, "maximum": MAX_RETRIES_MAX }),
                examples: || json!([3]),
            },
            Self::MemLimit => Spec {
                name: "memLimit",
                label: "Memory limit",
                description: "Most memory each replica may use. A number is GB; a string takes a unit. Unset means no limit.",
                field: "memLimit",
                access: Some(access!(mem_limit)),
                core: Some(Setting::MemLimit),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| Value::Null,
                expected: || {
                    json!({
                        "anyOf": [
                            { "type": "number", "minimum": size::MEM_LIMIT_MIN, "maximum": MEM_LIMIT_MAX },
                            { "type": "string", "pattern": size::PATTERN },
                        ],
                    })
                },
                examples: || json!(["1GiB", "512MiB", 0.5]),
            },
            Self::PreDeployCommand => Spec {
                name: "preDeployCommand",
                label: "Pre-deploy command",
                description: "Runs once in a new replica before a Deploy starts the Service.",
                field: "preDeployCommand",
                access: Some(access!(pre_deploy_command)),
                core: Some(Setting::PreDeployCommand),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| Value::Null,
                expected: command,
                examples: || json!(["npm run migrate"]),
            },
            Self::PrivateDns => Spec {
                name: "privateDns",
                label: "Private DNS",
                description: "The name other Services in the Environment reach it at, as NAME.internal. Renaming the Service keeps it. Unset returns it to the Service's name.",
                field: "privateDns",
                access: Some(access!(private_dns)),
                core: Some(Setting::PrivateDns),
                apply: Apply::Staged,
                needs: Needs::Any,
                // The Service's current name, so none until a Service is named.
                default: |service| service.map_or(Value::Null, |name| json!(name)),
                expected: || json!({ "type": "string", "pattern": crate::catalog::NODE_NAME, "maxLength": 63 }),
                examples: || json!(["api", "api-v2"]),
            },
            Self::RegistryCredential => Spec {
                name: "registryCredential",
                label: "Registry credentials",
                description: "The username and secret a private image is pulled with. A new secret replaces the stored one at once; turning credentials on or off is staged, and unset keeps the stored secret for {\"secret\": true} to turn back on. Reads show {\"secret\": true}, and sending that back keeps it.",
                field: "source.credentials",
                access: None,
                core: None,
                apply: Apply::Immediate,
                needs: Needs::Image,
                default: |_| Value::Null,
                expected: || {
                    json!({
                        "type": "object",
                        "properties": {
                            "username": { "type": "string", "minLength": 1, "maxLength": 255 },
                            "secret": { "anyOf": [{ "type": "string", "minLength": 1 }, { "const": true }] },
                        },
                        "required": ["secret"],
                        "additionalProperties": false,
                    })
                },
                examples: || json!([SecretInput::Keep.to_value()]),
            },
            Self::Replicas => Spec {
                name: "replicas",
                label: "Replicas",
                description: "How many copies of the Service run. A number, or a count for each region like {\"fsn\":2,\"syd\":1}.",
                field: "replicas",
                access: Some(access!(replicas)),
                core: Some(Setting::Replicas),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| json!(default_replicas()),
                expected: || {
                    let count = json!({ "type": "integer", "minimum": 1, "maximum": REPLICAS_MAX });
                    json!({ "anyOf": [count, { "type": "object", "additionalProperties": count }] })
                },
                examples: || json!([3, { "fsn": 2, "syd": 1 }]),
            },
            Self::RestartPolicy => Spec {
                name: "restartPolicy",
                label: "Restart policy",
                description: "When a stopped replica restarts.",
                field: "restartPolicy",
                access: Some(access!(restart_policy)),
                core: Some(Setting::RestartPolicy),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| json!(ServiceRestartPolicy::default()),
                expected: || json!({ "type": "string", "enum": RESTART_POLICIES }),
                examples: || json!(["on-failure"]),
            },
            Self::StartCommand => Spec {
                name: "startCommand",
                label: "Start command",
                description: "Overrides the image's command. Unset runs the image's own.",
                field: "startCommand",
                access: Some(access!(start_command)),
                core: Some(Setting::StartCommand),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| Value::Null,
                expected: command,
                examples: || json!(["npm start"]),
            },
            Self::Template => Spec {
                name: "template",
                label: "Template",
                description: "The Service Template it was created from, and its version. It changes nothing that runs. Unset forgets it.",
                field: "template",
                access: Some(access!(template)),
                core: None,
                // ponytail: a tag, not runtime config; nothing lowers it, so a Deploy never ships it.
                apply: Apply::Immediate,
                needs: Needs::Any,
                default: |_| Value::Null,
                expected: || {
                    json!({
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "pattern": crate::catalog::NODE_NAME, "maxLength": 63 },
                            "version": { "type": "integer", "minimum": 1 },
                        },
                        "required": ["id", "version"],
                        "additionalProperties": false,
                    })
                },
                examples: || json!([{ "id": "postgres", "version": 1 }]),
            },
            Self::UpdateOrder => Spec {
                name: "updateOrder",
                label: "Update order",
                description: "How a Deploy replaces replicas; rolling stops one Server's at a time. Unset is stop-first with a Volume, start-first without.",
                field: "updateOrder",
                access: Some(access!(update_order)),
                core: Some(Setting::UpdateOrder),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| Value::Null,
                expected: || json!({ "type": "string", "enum": UPDATE_ORDERS }),
                examples: || json!(["rolling"]),
            },
            Self::StopSignal => Spec {
                name: "stopSignal",
                label: "Stop signal",
                description: "Auto: SIGTERM with a start command, otherwise the image's.",
                field: "stopSignal",
                access: Some(access!(stop_signal)),
                core: Some(Setting::StopSignal),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| Value::Null,
                expected: || json!({ "type": "string", "enum": STOP_SIGNALS }),
                examples: || json!(["SIGTERM", "SIGQUIT"]),
            },
            Self::StopGracePeriod => Spec {
                name: "stopGracePeriod",
                label: "Stop grace period",
                description: "Seconds a replica gets to stop before it's killed.",
                field: "stopGracePeriod",
                // The default is stored as unset, so a document that names it reads as one that doesn't.
                access: Some(Access {
                    read: |config| {
                        json!(
                            config
                                .stop_grace_period
                                .unwrap_or(STOP_GRACE_PERIOD_DEFAULT)
                        )
                    },
                    restore: |config, value| {
                        let seconds: Option<u16> = serde_json::from_value(value)?;
                        config.stop_grace_period =
                            seconds.filter(|seconds| *seconds != STOP_GRACE_PERIOD_DEFAULT);
                        Ok(())
                    },
                }),
                core: Some(Setting::StopGracePeriod),
                apply: Apply::Staged,
                needs: Needs::Any,
                default: |_| json!(STOP_GRACE_PERIOD_DEFAULT),
                expected: || json!({ "type": "integer", "minimum": 1, "maximum": STOP_GRACE_PERIOD_MAX }),
                examples: || json!([60]),
            },
            Self::Git(git) => git.spec(),
            Self::Policy(policy) => policy.spec(),
        }
    }

    /// The Setting a core row's `setting` is, if `set` has one.
    pub(crate) fn of(setting: Setting) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|one| one.spec().core == Some(setting))
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
        self.spec().name
    }

    pub(crate) const fn label(self) -> &'static str {
        self.spec().label
    }

    pub(crate) const fn description(self) -> &'static str {
        self.spec().description
    }

    /// A caveat on this Setting for a Service with `config`: why its value may not act.
    pub(crate) fn note(self, config: &AuthoredServiceConfig) -> Option<String> {
        match self {
            Self::Policy(policy) => policy.note(config),
            Self::CpuLimit
            | Self::Healthcheck
            | Self::Image
            | Self::MaxRetries
            | Self::MemLimit
            | Self::PreDeployCommand
            | Self::PrivateDns
            | Self::RegistryCredential
            | Self::Replicas
            | Self::RestartPolicy
            | Self::StartCommand
            | Self::Template
            | Self::UpdateOrder
            | Self::StopSignal
            | Self::StopGracePeriod
            | Self::Git(_) => None,
        }
    }

    pub(crate) const fn field(self) -> &'static str {
        self.spec().field
    }

    pub(crate) const fn apply(self) -> Apply {
        self.spec().apply
    }

    /// The value `unset` restores for `service`; `null` when the Setting has none.
    pub(crate) fn default(self, service: Option<&ServiceName>) -> Value {
        (self.spec().default)(service)
    }

    pub(crate) fn expected(self) -> Value {
        (self.spec().expected)()
    }

    pub(crate) fn examples(self) -> Value {
        (self.spec().examples)()
    }

    /// Whether this Setting means anything for a Service with `config`'s source.
    pub(crate) const fn applies(self, config: &AuthoredServiceConfig) -> bool {
        match self.spec().needs {
            Needs::Any => true,
            Needs::Image => matches!(config.source, ServiceSource::Image { .. }),
            Needs::Built => !matches!(config.source, ServiceSource::Image { .. }),
            Needs::Git => matches!(config.source, ServiceSource::Git { .. }),
        }
    }

    /// Refuse this Setting for `service`, whose source it does not [apply](Self::applies) to.
    pub(crate) fn not_for(self, service: &ServiceName, source: &ServiceSource) -> RpcError {
        let needs = match self.spec().needs {
            Needs::Image => "an image source",
            Needs::Any | Needs::Built | Needs::Git => "a repository source",
        };
        let has = match source {
            ServiceSource::Image { .. } => "runs an image",
            ServiceSource::Git { .. } => "builds a repository",
            ServiceSource::Empty { .. } => "has no source",
        };
        self.invalid(&format!("needs {needs}, and {service} {has}"))
    }

    /// Its value for a Service with `config` and Deployment Policy `policy`.
    pub(crate) fn value(self, config: &AuthoredServiceConfig, policy: &Policy) -> Value {
        if let Some(access) = self.spec().access {
            return self.shown((access.read)(config));
        }
        match (self, &config.source) {
            (Self::Git(git), _) => git.value(config),
            (Self::Policy(setting), _) => setting.value(policy),
            (Self::Image, ServiceSource::Image { image, .. }) => json!(image),
            (Self::RegistryCredential, ServiceSource::Image { credentials, .. }) => {
                self.shown(json!(credentials))
            }
            (
                Self::Image
                | Self::RegistryCredential
                | Self::CpuLimit
                | Self::Healthcheck
                | Self::MaxRetries
                | Self::MemLimit
                | Self::PreDeployCommand
                | Self::PrivateDns
                | Self::Replicas
                | Self::RestartPolicy
                | Self::StartCommand
                | Self::Template
                | Self::UpdateOrder
                | Self::StopSignal
                | Self::StopGracePeriod,
                _,
            ) => Value::Null,
        }
    }

    /// A change row's value as `get` shows it, never the stored shape.
    pub(crate) fn shown(self, value: Value) -> Value {
        match self {
            Self::Git(git) => git.shown(value),
            // Never the stored credential reference: only whether one is on.
            Self::RegistryCredential => match value.get("type").and_then(Value::as_str) {
                Some("configured") => SecretCell::Set.to_value(),
                _ => Value::Null,
            },
            Self::Healthcheck => match value.get("type").and_then(Value::as_str) {
                Some("http") => json!({
                    "path": value.get("path"),
                    "timeoutSeconds": value.get("timeoutSeconds"),
                }),
                Some("command") => json!({
                    "command": value.get("command"),
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
            | Self::Template
            | Self::UpdateOrder
            | Self::StopSignal
            | Self::StopGracePeriod => value,
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
        let value = if matches!(self, Self::MemLimit) {
            self.mem_limit(value)?
        } else {
            self.coerce(value)
        };
        if let Self::Git(git) = self {
            return git.set(config, value, trusted);
        }
        if let Self::Policy(_) = self {
            return Err(self.invalid("the Deployment Policy is not in the config"));
        }
        if self == Self::Image {
            // An empty or repository Service takes an image as its source. Build settings mean
            // nothing to an image, so they return to their defaults rather than resurface later.
            let credentials = match &config.source {
                ServiceSource::Image { credentials, .. } => credentials.clone(),
                ServiceSource::Empty { .. } | ServiceSource::Git { .. } => {
                    config.build = ServiceBuildConfig::default();
                    ServiceImageCredentials::None
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
        if let (Self::Replicas, Some(regions)) = (self, &trusted.regions) {
            let replicas: Replicas = self.decode(value.clone())?;
            for region in replicas.regions() {
                region::refuse_unknown(region, regions)?;
            }
        }
        self.store(config, value)
    }

    fn healthcheck(self, value: Value, current: &ServiceHealthcheck) -> Result<Value, RpcError> {
        const EXPECTED: &str =
            "expected a path, {\"path\", \"timeoutSeconds\"} or {\"command\", \"timeoutSeconds\"}";
        let timeout = match current {
            ServiceHealthcheck::Http {
                timeout_seconds, ..
            }
            | ServiceHealthcheck::Command {
                timeout_seconds, ..
            } => *timeout_seconds,
            ServiceHealthcheck::None => HEALTHCHECK_TIMEOUT_DEFAULT,
        };
        let healthcheck = match value {
            Value::String(path) => ServiceHealthcheck::Http {
                path,
                timeout_seconds: timeout,
            },
            Value::Object(mut fields)
                if fields
                    .keys()
                    .all(|key| matches!(key.as_str(), "path" | "command" | "timeoutSeconds")) =>
            {
                let timeout_seconds = match fields.remove("timeoutSeconds") {
                    None => timeout,
                    Some(given) => serde_json::from_value(self.coerce_number(given))
                        .map_err(|_| self.invalid("timeoutSeconds: expected whole seconds"))?,
                };
                match (fields.remove("path"), fields.remove("command")) {
                    (Some(Value::String(path)), None) => ServiceHealthcheck::Http {
                        path,
                        timeout_seconds,
                    },
                    (None, Some(Value::String(command))) => ServiceHealthcheck::Command {
                        command,
                        timeout_seconds,
                    },
                    _ => return Err(self.invalid(EXPECTED)),
                }
            }
            Value::Null
            | Value::Bool(_)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => return Err(self.invalid(EXPECTED)),
        };
        Ok(serde_json::to_value(healthcheck).expect("a healthcheck is JSON"))
    }

    /// A memory limit in GB from a number, text spelling one, or a size with a unit,
    /// rounded to whole bytes. A number of bytes is refused with its size, and one under
    /// [`size::MEM_LIMIT_MIN`] as too small to run.
    fn mem_limit(self, value: Value) -> Result<Value, RpcError> {
        let value = match value.as_str().and_then(|text| size::bytes(text.trim())) {
            Some(bytes) => json!(bytes / 1e9),
            None => self.coerce_number(value),
        };
        if value.as_f64().is_some_and(|gb| gb < size::MEM_LIMIT_MIN) {
            return Err(self.invalid(&format!(
                "expected at least {} GB (10 MB): Docker refuses less than 6 MiB",
                size::MEM_LIMIT_MIN
            )));
        }
        if value.is_string() {
            return Err(self.invalid(
                "expected GB from 0.01 to 1024, or a size with a unit such as \"512MiB\" or \"1GiB\"",
            ));
        }
        if let Some((size, text)) = value.as_f64().and_then(size::looks_like_bytes) {
            return Err(error::invalid(
                format!(
                    "{}: {value} is above the most, {MEM_LIMIT_MAX} GB. It looks like bytes: {size} is \"{text}\".",
                    self.name()
                ),
                json!({ "setting": self.name(), "expected": self.expected(), "example": text }),
            ));
        }
        Ok(value)
    }

    /// Whether `value` validates as this Setting's type, as [`Self::set`] checks it, and that
    /// type is a number, a choice, a Service's name or an image: none that holds a secret.
    pub(crate) fn plain(self, value: Value) -> bool {
        let checked = match self {
            Self::MemLimit => self.mem_limit(value),
            Self::CpuLimit | Self::MaxRetries | Self::StopGracePeriod => {
                Ok(self.coerce_number(value))
            }
            Self::Replicas => Ok(self.coerce(value)),
            Self::StopSignal => Ok(self.coerce(value)),
            Self::PrivateDns | Self::RestartPolicy | Self::UpdateOrder | Self::Image => Ok(value),
            Self::Healthcheck
            | Self::PreDeployCommand
            | Self::RegistryCredential
            | Self::StartCommand
            | Self::Template
            | Self::Git(_)
            | Self::Policy(_) => return false,
        };
        let Ok(value) = checked else {
            return false;
        };
        let field = if self == Self::Image {
            "imageReference"
        } else {
            self.name()
        };
        let of_its_kind = match (self, &value) {
            (Self::PrivateDns, Value::String(name)) => ServiceName::parse(name.as_str()).is_ok(),
            // A count for each region: the regions by name, each count a number.
            (Self::Replicas, Value::Object(counts)) => counts.iter().all(|(region, count)| {
                crate::region::RegionName::parse(region.as_str()).is_ok() && count.is_number()
            }),
            (
                Self::RestartPolicy | Self::UpdateOrder | Self::StopSignal | Self::Image,
                Value::String(_),
            )
            | (
                Self::CpuLimit
                | Self::MaxRetries
                | Self::MemLimit
                | Self::Replicas
                | Self::StopGracePeriod,
                Value::Number(_),
            ) => true,
            _ => false,
        };
        of_its_kind && self.validated(field, value).is_ok()
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
                config.source = ServiceSource::empty();
                Ok(())
            }
            (Self::RegistryCredential, ServiceSource::Image { credentials, .. }) => {
                *credentials = ServiceImageCredentials::None;
                Ok(())
            }
            // A Setting the source does not have is already unset.
            (Self::Image | Self::RegistryCredential, _) => Ok(()),
            (Self::Healthcheck, _) => {
                config.healthcheck = ServiceHealthcheck::None;
                Ok(())
            }
            _ => self.store(config, self.default(None)),
        }
    }

    fn store(self, config: &mut AuthoredServiceConfig, value: Value) -> Result<(), RpcError> {
        let access = self
            .spec()
            .access
            .ok_or_else(|| self.invalid("is not one config field"))?;
        (access.restore)(config, value).map_err(|error| self.invalid(&error.to_string()))
    }

    /// Text that spells a number becomes the number, for numeric Settings, and text
    /// that spells an object becomes the object, for object Settings.
    pub(crate) fn coerce(self, value: Value) -> Value {
        if self == Self::Replicas {
            return Self::coerce_object(self.coerce_number(value));
        }
        if self == Self::StopSignal {
            return Self::signal_name(value);
        }
        match self.expected().get("type").and_then(Value::as_str) {
            Some("integer" | "number") => self.coerce_number(value),
            Some("object") => Self::coerce_object(value),
            _ => value,
        }
    }

    /// A signal however it is spelled, `term` or `sigterm`, as its name, `SIGTERM`.
    fn signal_name(value: Value) -> Value {
        match value.as_str().map(|text| text.trim().to_ascii_uppercase()) {
            Some(name) if name.starts_with("SIG") => Value::String(name),
            Some(name) => Value::String(format!("SIG{name}")),
            None => value,
        }
    }

    fn coerce_object(value: Value) -> Value {
        match value.as_str().map(serde_json::from_str::<Value>) {
            Some(Ok(object @ Value::Object(_))) => object,
            _ => value,
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
        ployz_core::decode::decode(&value).map_err(|error| self.invalid(&error.to_string()))
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
                    .chain(["env", "mounts", "domains"]);
                let first = name.split('.').next().unwrap_or(name);
                let closest = error::did_you_mean(first, names.clone());
                let mut details = serde_json::Map::new();
                if let Some(setting) = Self::ALL
                    .into_iter()
                    .find(|setting| Some(setting.name()) == closest)
                {
                    details.insert("expected".into(), setting.expected());
                    if let Some(example) = setting.examples().get(0) {
                        details.insert("example".into(), example.clone());
                    }
                }
                let mut details = Value::Object(details);
                let hints: Vec<_> = closest
                    .map(|closest| Hint::Closest(closest.into()))
                    .into_iter()
                    .chain([Hint::valid(names)])
                    .collect();
                Hint::into_details(&hints, &mut details);
                error::invalid("Unknown Service Setting", details)
            })
    }
}

/// Whether `value` is text that names no HTTP path and spells no object.
pub(crate) fn is_healthcheck_text(value: &Value) -> bool {
    matches!(ServiceSetting::Healthcheck.coerce(value.clone()), Value::String(text) if !text.starts_with('/'))
}

/// Healthcheck text that is not a path: a command or a timeout takes an object.
pub(crate) fn healthcheck_text(service: &ServiceName, summary: &EnvironmentSummary) -> RpcError {
    let setting = ServiceSetting::Healthcheck;
    error::invalid(
        format!(
            "{}: text sets an HTTP path like /health; a command takes an object like {{\"command\":\"pg_isready\",\"timeoutSeconds\":60}}, and an HTTP check's timeout one like {{\"path\":\"/health\",\"timeoutSeconds\":60}}",
            setting.name()
        ),
        FollowUp::PatchHealthcheck {
            service: service.to_string(),
            place: Place::of(summary),
        }
        .within(json!({
            "setting": setting.name(),
            "expected": setting.expected(),
            "example": setting.examples().get(0),
        })),
    )
}

/// A core change row's value at `field` as reads show it: never a secret, a
/// credential or a repository ID. A variable shows its text, `{"secret": true}`, or
/// `{"secret": false}` for a secret still without a value.
pub(crate) fn shown(field: &str, value: Value) -> Value {
    if field.starts_with("env.") || field.starts_with("variables.") {
        return match value.get("kind").and_then(Value::as_str) {
            Some("secret") => {
                SecretCell::from(value.get("valued") != Some(&json!(false))).to_value()
            }
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
