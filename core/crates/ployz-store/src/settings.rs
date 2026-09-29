//! The Settings a Service exposes, addressed as `SERVICE.SETTING`. The stored
//! document behind them is never addressed directly.

use ployz_core::config::{AuthoredServiceConfig, ServiceSource, parse_service_setting};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::error;

/// Whether a change waits for a Deploy or takes effect at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Apply {
    Staged,
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
            Self::Replicas => json!(1),
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
                let source = json!({
                    "type": "image",
                    "version": 1,
                    "image": value,
                    "credentials": credentials,
                });
                config.source = self.decode(self.validated("source", source)?)?;
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

    /// Return this Setting to its default.
    pub(crate) fn unset(self, config: &mut AuthoredServiceConfig) -> Result<(), RpcError> {
        match self {
            Self::Command => config.start_command = None,
            Self::Image => {
                return Err(error::invalid(
                    "image: an image Service needs an image; set another one",
                    json!({ "setting": self.name() }),
                ));
            }
            Self::Replicas => config.replicas = 1,
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

/// A parsed `SERVICE[.SETTING]` path.
#[derive(Clone, Debug)]
pub(crate) struct SettingPath {
    pub(crate) service: ServiceName,
    pub(crate) setting: Option<ServiceSetting>,
}

impl SettingPath {
    pub(crate) fn parse(path: &str) -> Result<Self, RpcError> {
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

    /// The path of one Setting, as results name it.
    pub(crate) fn of(service: &str, setting: ServiceSetting) -> String {
        format!("{service}.{}", setting.name())
    }
}
