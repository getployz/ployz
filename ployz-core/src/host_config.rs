use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::ValueError;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "name", rename_all = "kebab-case")]
pub enum RestartPolicy {
    No,
    Always,
    #[default]
    UnlessStopped,
    OnFailure {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum_retry_count: Option<i64>,
    },
}

impl RestartPolicy {
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        let (name, retries) = value
            .split_once(':')
            .map_or((value, None), |(name, retries)| (name, Some(retries)));
        match (name, retries) {
            ("no", None) => Ok(Self::No),
            ("always", None) => Ok(Self::Always),
            ("unless-stopped", None) => Ok(Self::UnlessStopped),
            ("on-failure", None) => Ok(Self::OnFailure {
                maximum_retry_count: None,
            }),
            ("on-failure", Some(retries)) => Ok(Self::OnFailure {
                maximum_retry_count: Some(i64::from(retries.parse::<u32>().map_err(|_| {
                    ValueError::new(
                        "restart policy retry count",
                        retries,
                        "a non-negative integer",
                    )
                })?)),
            }),
            _ => Err(ValueError::new(
                "restart policy",
                value,
                "no, always, unless-stopped, or on-failure[:max]",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindRecursive {
    Disabled,
    Writable,
    Readonly,
}

impl FromStr for BindRecursive {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "disabled" => Ok(Self::Disabled),
            "writable" => Ok(Self::Writable),
            "readonly" => Ok(Self::Readonly),
            _ => Err(ValueError::new(
                "bind recursive mode",
                value,
                "disabled, writable, or readonly",
            )),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum PidMode {
    Host,
    Container(String),
}

impl FromStr for PidMode {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "host" {
            return Ok(Self::Host);
        }
        if let Some(id) = value.strip_prefix("container:")
            && !id.is_empty()
        {
            return Ok(Self::Container(id.to_owned()));
        }
        Err(ValueError::new(
            "PID mode",
            value,
            "'host' or 'container:<id>'",
        ))
    }
}

impl fmt::Display for PidMode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Host => formatter.write_str("host"),
            Self::Container(id) => write!(formatter, "container:{id}"),
        }
    }
}

impl From<PidMode> for String {
    fn from(value: PidMode) -> Self {
        value.to_string()
    }
}

impl TryFrom<String> for PidMode {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}
