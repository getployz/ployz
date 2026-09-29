//! The Settings a Service exposes, addressed as `SERVICE.SETTING`: the Service half
//! of the settings catalog. The stored document behind them is never addressed
//! directly. Each Setting carries what `get`, `explain`, `schema`, validation and
//! completion need; [`crate::catalog`] renders them as JSON Schema.

use std::fmt;

use ployz_core::config::{
    AuthoredServiceConfig, ServiceImageCredentials, ServiceSource, default_max_retries,
    default_replicas, parse_service_setting,
};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error;

/// Whether a change waits for a Deploy or takes effect at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
    Image,
    MaxRetries,
    MemLimit,
    PreDeployCommand,
    Replicas,
    RestartPolicy,
    StartCommand,
}

impl ServiceSetting {
    /// Every Setting, in the order `get` lists them.
    pub(crate) const ALL: [Self; 8] = [
        Self::CpuLimit,
        Self::Image,
        Self::MaxRetries,
        Self::MemLimit,
        Self::PreDeployCommand,
        Self::Replicas,
        Self::RestartPolicy,
        Self::StartCommand,
    ];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::CpuLimit => "cpuLimit",
            Self::Image => "image",
            Self::MaxRetries => "maxRetries",
            Self::MemLimit => "memLimit",
            Self::PreDeployCommand => "preDeployCommand",
            Self::Replicas => "replicas",
            Self::RestartPolicy => "restartPolicy",
            Self::StartCommand => "startCommand",
        }
    }

    /// The dashboard's field label.
    pub(crate) const fn label(self) -> &'static str {
        match self {
            Self::CpuLimit => "CPU limit",
            Self::Image => "Container image",
            Self::MaxRetries => "Max retries",
            Self::MemLimit => "Memory limit",
            Self::PreDeployCommand => "Pre-deploy command",
            Self::Replicas => "Replicas",
            Self::RestartPolicy => "Restart policy",
            Self::StartCommand => "Start command",
        }
    }

    pub(crate) const fn description(self) -> &'static str {
        match self {
            Self::CpuLimit => "Most vCPUs each replica may use. Unset means no limit.",
            Self::Image => "The container image each replica runs.",
            Self::MaxRetries => "How often an on-failure restart policy restarts a replica.",
            Self::MemLimit => "Most memory each replica may use, in GB. Unset means no limit.",
            Self::PreDeployCommand => {
                "Runs once in a new replica before a Deploy starts the Service."
            }
            Self::Replicas => "How many copies of the Service run.",
            Self::RestartPolicy => "When a stopped replica restarts.",
            Self::StartCommand => "Overrides the image's command. Unset runs the image's own.",
        }
    }

    /// The core config field this Setting writes, as change rows and restores name it.
    pub(crate) const fn field(self) -> &'static str {
        match self {
            Self::Image => "source.image",
            Self::CpuLimit
            | Self::MaxRetries
            | Self::MemLimit
            | Self::PreDeployCommand
            | Self::Replicas
            | Self::RestartPolicy
            | Self::StartCommand => self.name(),
        }
    }

    pub(crate) const fn apply(self) -> Apply {
        match self {
            Self::CpuLimit
            | Self::Image
            | Self::MaxRetries
            | Self::MemLimit
            | Self::PreDeployCommand
            | Self::Replicas
            | Self::RestartPolicy
            | Self::StartCommand => Apply::Staged,
        }
    }

    /// The value `unset` restores; `null` when the Setting has none.
    pub(crate) fn default(self) -> Value {
        match self {
            Self::CpuLimit
            | Self::Image
            | Self::MemLimit
            | Self::PreDeployCommand
            | Self::StartCommand => Value::Null,
            Self::MaxRetries => json!(default_max_retries()),
            Self::Replicas => json!(default_replicas()),
            Self::RestartPolicy => json!("unless-stopped"),
        }
    }

    /// The JSON Schema of a value: its type and allowed values or bounds.
    pub(crate) fn expected(self) -> Value {
        match self {
            Self::CpuLimit => json!({ "type": "number", "exclusiveMinimum": 0, "maximum": 64 }),
            Self::MemLimit => json!({ "type": "number", "exclusiveMinimum": 0, "maximum": 1024 }),
            Self::Image => json!({ "type": "string", "minLength": 1, "maxLength": 500 }),
            Self::MaxRetries => json!({ "type": "integer", "minimum": 0, "maximum": 100 }),
            Self::Replicas => json!({ "type": "integer", "minimum": 0, "maximum": 50 }),
            Self::PreDeployCommand | Self::StartCommand => {
                json!({ "type": "string", "minLength": 1, "maxLength": 2000 })
            }
            Self::RestartPolicy => json!({
                "type": "string",
                "enum": ["always", "no", "on-failure", "unless-stopped"],
            }),
        }
    }

    /// One to three values `set` accepts.
    pub(crate) fn examples(self) -> Value {
        match self {
            Self::CpuLimit => json!([0.5, 2]),
            Self::Image => json!(["nginx:1.27", "ghcr.io/acme/web:1.4.0"]),
            Self::MaxRetries => json!([3]),
            Self::MemLimit => json!([0.5, 4]),
            Self::PreDeployCommand => json!(["npm run migrate"]),
            Self::Replicas => json!([3]),
            Self::RestartPolicy => json!(["on-failure"]),
            Self::StartCommand => json!(["npm start"]),
        }
    }

    pub(crate) fn value(self, config: &AuthoredServiceConfig) -> Value {
        if self == Self::Image {
            return match &config.source {
                ServiceSource::Image { image, .. } => json!(image),
                ServiceSource::Empty { .. } | ServiceSource::Git { .. } => Value::Null,
            };
        }
        // Every other Setting is the stored field of the same name.
        serde_json::to_value(config)
            .expect("service settings are JSON")
            .get_mut(self.name())
            .map(Value::take)
            .unwrap_or_default()
    }

    /// Validate `value` against this Setting and write it. Text is accepted for any
    /// type, as `set PATH=VALUE` sends it.
    pub(crate) fn set(
        self,
        config: &mut AuthoredServiceConfig,
        value: Value,
    ) -> Result<(), RpcError> {
        if value.is_null() {
            return Err(self.invalid("null never clears a Setting; unset it"));
        }
        let value = self.coerce(value);
        if self == Self::Image {
            let ServiceSource::Image { credentials, .. } = &config.source else {
                return Err(self.invalid("this Service does not run an image"));
            };
            config.source = image_source(self.decode(value)?, credentials.clone())?;
            return Ok(());
        }
        let value = self.validated(self.name(), value)?;
        self.store(config, value)
    }

    /// Return this Setting to its [`default`](Self::default).
    pub(crate) fn unset(self, config: &mut AuthoredServiceConfig) -> Result<(), RpcError> {
        if self == Self::Image {
            return Err(self.invalid("an image Service needs an image; set another one"));
        }
        self.store(config, self.default())
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
        if let (true, Some(number)) = (numeric, value.as_str()) {
            return number
                .trim()
                .parse::<serde_json::Number>()
                .map_or(value, Value::Number);
        }
        value
    }

    /// Validate through core's field rules, reporting this Setting's path, never the value.
    fn validated(self, field: &str, value: Value) -> Result<Value, RpcError> {
        parse_service_setting(json!({ "field": field, "value": value }))
            .map_err(|error| self.invalid(&error.message))
    }

    fn decode<T: serde::de::DeserializeOwned>(self, value: Value) -> Result<T, RpcError> {
        serde_json::from_value(value).map_err(|_| self.invalid("invalid value"))
    }

    /// A refused value: what this Setting expects and an example, never the value sent.
    fn invalid(self, message: &str) -> RpcError {
        error::invalid(
            format!("{}: {message}", self.name()),
            json!({
                "setting": self.name(),
                "expected": self.expected(),
                "example": self.examples().get(0),
            }),
        )
    }

    pub(crate) fn parse(name: &str) -> Result<Self, RpcError> {
        Self::ALL
            .into_iter()
            .find(|setting| setting.name() == name)
            .ok_or_else(|| {
                let names = Self::ALL.map(Self::name);
                error::invalid(
                    "Unknown Service Setting",
                    json!({
                        "did_you_mean": error::did_you_mean(name, names),
                        "valid_children": names,
                    }),
                )
            })
    }
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

/// What a request addresses in an Environment: `SERVICE` for a whole Service, or
/// `SERVICE.SETTING` for one of its Settings.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SettingPath {
    service: ServiceName,
    setting: Option<ServiceSetting>,
}

impl SettingPath {
    /// Parse `SERVICE` or `SERVICE.SETTING`.
    ///
    /// # Errors
    /// Returns `invalid_argument` for a malformed path or an unknown Setting, never
    /// echoing the path.
    pub fn parse(path: &str) -> Result<Self, RpcError> {
        let (service, setting) = path.split_once('.').unzip();
        let service = ServiceName::parse(service.unwrap_or(path)).map_err(|_| {
            error::invalid(
                "Expected a path like SERVICE.SETTING",
                json!({ "example": "web.replicas" }),
            )
        })?;
        let setting = setting.map(ServiceSetting::parse).transpose()?;
        Ok(Self { service, setting })
    }

    /// The Service this path is in.
    #[must_use]
    pub const fn service(&self) -> &ServiceName {
        &self.service
    }

    pub(crate) const fn setting(&self) -> Option<ServiceSetting> {
        self.setting
    }

    /// The path of one Setting of `service`.
    pub(crate) fn of(service: &ServiceName, setting: ServiceSetting) -> Self {
        Self {
            service: service.clone(),
            setting: Some(setting),
        }
    }
}

impl fmt::Display for SettingPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.setting {
            Some(setting) => write!(formatter, "{}.{}", self.service, setting.name()),
            None => write!(formatter, "{}", self.service),
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
