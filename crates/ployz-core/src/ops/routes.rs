use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::NonZeroU16;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct RouteTarget {
    pub hostname: RouteHostname,
    pub port: RoutePort,
}

impl RouteTarget {
    #[must_use]
    pub fn try_new(hostname: RouteHostname, port: RoutePort) -> Self {
        Self { hostname, port }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"RouteHostname\">"))]
#[serde(try_from = "String", into = "String")]
pub struct RouteHostname(String);

impl RouteHostname {
    pub fn try_new(value: impl Into<String>) -> Result<Self, RouteHostnameError> {
        let value = value.into();
        if value.is_empty() {
            return Err(RouteHostnameError::Empty);
        }

        if value
            .split('.')
            .any(|label| label.is_empty() || label.starts_with('-') || label.ends_with('-'))
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteHostnameError {
    Empty,
    Invalid { value: String },
}

impl fmt::Display for RouteHostnameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("route hostname is empty"),
            Self::Invalid { value } => write!(formatter, "route hostname is invalid: {value}"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "SafeInteger<\"RoutePort\">"))]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutePortError {
    Zero,
}

impl fmt::Display for RoutePortError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("route port must be greater than zero"),
        }
    }
}
