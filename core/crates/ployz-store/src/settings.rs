//! The Settings a Service exposes, addressed as `SERVICE.SETTING`. The stored
//! document behind them is never addressed directly.

use std::fmt;

use ployz_core::config::{
    AuthoredServiceConfig, ServiceImageCredentials, ServiceSource, default_replicas,
    parse_service_setting,
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

/// One Setting of one Service.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ServiceSetting {
    Command,
    Image,
    Replicas,
}

impl ServiceSetting {
    /// Every Setting, in the order `get` lists them.
    pub(crate) const ALL: [Self; 3] = [Self::Command, Self::Image, Self::Replicas];

    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Image => "image",
            Self::Replicas => "replicas",
        }
    }

    pub(crate) const fn apply(self) -> Apply {
        match self {
            Self::Command | Self::Image | Self::Replicas => Apply::Staged,
        }
    }

    /// The value `unset` restores; `null` when the Setting has none.
    pub(crate) fn default(self) -> Value {
        match self {
            Self::Command | Self::Image => Value::Null,
            Self::Replicas => json!(default_replicas()),
        }
    }

    pub(crate) fn value(self, config: &AuthoredServiceConfig) -> Value {
        match self {
            Self::Command => json!(config.start_command),
            Self::Image => match &config.source {
                ServiceSource::Image { image, .. } => json!(image),
                ServiceSource::Empty { .. } | ServiceSource::Git { .. } => Value::Null,
            },
            Self::Replicas => json!(config.replicas),
        }
    }

    /// Validate `value` against this Setting and write it. Text is accepted for any
    /// type, as `set PATH=VALUE` sends it.
    pub(crate) fn set(
        self,
        config: &mut AuthoredServiceConfig,
        value: Value,
    ) -> Result<(), RpcError> {
        if value.is_null() {
            return Err(error::invalid(
                format!("{}: null never clears a Setting; unset it", self.name()),
                json!({ "setting": self.name() }),
            ));
        }
        match self {
            Self::Command => {
                config.start_command = Some(self.decode(self.validated("startCommand", value)?)?);
            }
            Self::Image => {
                let ServiceSource::Image { credentials, .. } = &config.source else {
                    return Err(error::invalid(
                        "image: this Service does not run an image",
                        json!({ "setting": self.name() }),
                    ));
                };
                config.source = image_source(self.decode(value)?, credentials.clone())?;
            }
            Self::Replicas => {
                let value = value
                    .as_str()
                    .and_then(|text| text.trim().parse::<u8>().ok())
                    .map_or(value, |number| json!(number));
                config.replicas = self.decode(self.validated("replicas", value)?)?;
            }
        }
        Ok(())
    }

    /// Return this Setting to its [`default`](Self::default).
    pub(crate) fn unset(self, config: &mut AuthoredServiceConfig) -> Result<(), RpcError> {
        match self {
            Self::Command => config.start_command = self.decode(self.default())?,
            Self::Image => {
                return Err(error::invalid(
                    "image: an image Service needs an image; set another one",
                    json!({ "setting": self.name() }),
                ));
            }
            Self::Replicas => config.replicas = self.decode(self.default())?,
        }
        Ok(())
    }

    /// Validate through core's field rules, reporting this Setting's path, never the value.
    fn validated(self, field: &str, value: Value) -> Result<Value, RpcError> {
        parse_service_setting(json!({ "field": field, "value": value })).map_err(|error| {
            error::invalid(
                format!("{}: {}", self.name(), error.message),
                json!({ "setting": self.name() }),
            )
        })
    }

    fn decode<T: serde::de::DeserializeOwned>(self, value: Value) -> Result<T, RpcError> {
        serde_json::from_value(value).map_err(|_| {
            error::invalid(
                format!("{}: invalid value", self.name()),
                json!({ "setting": self.name() }),
            )
        })
    }

    fn parse(name: &str) -> Result<Self, RpcError> {
        Self::ALL
            .into_iter()
            .find(|setting| setting.name() == name)
            .ok_or_else(|| {
                error::invalid(
                    "Unknown Service Setting",
                    json!({ "settings": Self::ALL.map(Self::name) }),
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
        let (service, setting) = match path.split_once('.') {
            Some((service, setting)) => (service, Some(ServiceSetting::parse(setting)?)),
            None => (path, None),
        };
        let service = ServiceName::parse(service).map_err(|_| {
            error::invalid(
                "Expected a path like SERVICE.SETTING",
                json!({ "example": "web.replicas" }),
            )
        })?;
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

    /// The path of one Setting of a stored Service.
    pub(crate) fn of(service: &str, setting: ServiceSetting) -> Result<Self, RpcError> {
        Ok(Self {
            service: ServiceName::parse(service).map_err(|_| error::corrupt("Service name"))?,
            setting: Some(setting),
        })
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
