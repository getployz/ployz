use serde::{Deserialize, Serialize};
use std::num::NonZeroU16;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RouteTarget {
    pub hostname: RouteHostname,
}

impl RouteTarget {
    #[must_use]
    pub fn new(hostname: RouteHostname) -> Self {
        Self { hostname }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"RouteHostname\">"))]
#[serde(try_from = "String", into = "String")]
pub struct RouteHostname(String);

impl RouteHostname {
    pub fn try_new(value: impl Into<String>) -> Result<Self, RouteHostnameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RouteHostnameError::Empty);
        }

        if value.split('.').any(|label| {
            label.is_empty() || label.len() > 63 || label.starts_with('-') || label.ends_with('-')
        }) || value.len() > 253
        {
            return Err(RouteHostnameError::Invalid { value });
        }

        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
        {
            return Err(RouteHostnameError::Invalid { value });
        }

        Ok(Self(value.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for RouteHostname {
    type Error = RouteHostnameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<RouteHostname> for String {
    fn from(value: RouteHostname) -> Self {
        value.0
    }
}

impl std::fmt::Display for RouteHostname {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl std::str::FromStr for RouteHostname {
    type Err = RouteHostnameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RouteHostnameError {
    #[error("route hostname is empty")]
    Empty,
    #[error("route hostname is invalid: {value}")]
    Invalid { value: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "SafeInteger<\"RoutePort\">"))]
#[serde(try_from = "u16", into = "u16")]
pub struct RoutePort(NonZeroU16);

impl RoutePort {
    pub fn try_new(value: u16) -> Result<Self, RoutePortError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(RoutePortError::Zero);
        };

        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for RoutePort {
    type Error = RoutePortError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<RoutePort> for u16 {
    fn from(value: RoutePort) -> Self {
        value.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RoutePortError {
    #[error("route port must be greater than zero")]
    Zero,
}
