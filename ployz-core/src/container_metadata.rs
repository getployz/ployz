//! Validated native container metadata shared by requested and resolved specs.

use std::{collections::BTreeMap, fmt, net::IpAddr, str::FromStr};

use serde::{Deserialize, Serialize};

use crate::ValueError;

/// Docker label namespace reserved for Ployz management metadata.
pub const MANAGEMENT_LABEL_PREFIX: &str = "ployz.";

fn validate_container_label_key(key: &str) -> Result<(), ValueError> {
    if key.starts_with(MANAGEMENT_LABEL_PREFIX) {
        return Err(ValueError::new(
            "container label key",
            key,
            "outside the reserved 'ployz.*' management namespace",
        ));
    }
    Ok(())
}

/// User-supplied Docker labels outside Ployz's management namespace.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(
    try_from = "BTreeMap<String, String>",
    into = "BTreeMap<String, String>"
)]
pub struct ContainerLabels(BTreeMap<String, String>);

impl ContainerLabels {
    /// Validate user-supplied Docker labels.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when a key is in the reserved `ployz.*` namespace.
    pub fn parse(labels: BTreeMap<String, String>) -> Result<Self, ValueError> {
        for key in labels.keys() {
            validate_container_label_key(key)?;
        }
        Ok(Self(labels))
    }

    /// Borrow the validated label map.
    #[must_use]
    pub fn as_map(&self) -> &BTreeMap<String, String> {
        &self.0
    }

    /// Consume these labels and return their map.
    #[must_use]
    pub fn into_map(self) -> BTreeMap<String, String> {
        self.0
    }
}

impl TryFrom<BTreeMap<String, String>> for ContainerLabels {
    type Error = ValueError;

    fn try_from(labels: BTreeMap<String, String>) -> Result<Self, Self::Error> {
        Self::parse(labels)
    }
}

impl From<ContainerLabels> for BTreeMap<String, String> {
    fn from(labels: ContainerLabels) -> Self {
        labels.0
    }
}

fn is_rfc1123_label(value: &str) -> bool {
    let bytes = value.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'-')
}

fn is_rfc1123_hostname(value: &str) -> bool {
    (1..=64).contains(&value.len()) && value.split('.').all(is_rfc1123_label)
}

/// A container's UTS hostname. It has no Service, DNS, ingress, placement, or Machine meaning.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ContainerHostname(String);

impl ContainerHostname {
    /// Parse an RFC 1123 hostname.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the hostname is invalid.
    pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        if is_rfc1123_hostname(&value) {
            Ok(Self(value))
        } else {
            Err(ValueError::new(
                "container hostname",
                value,
                "a 1-64 character RFC 1123 hostname",
            ))
        }
    }

    /// Borrow the validated hostname.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ContainerHostname {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ContainerHostname {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for ContainerHostname {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ContainerHostname> for String {
    fn from(value: ContainerHostname) -> Self {
        value.0
    }
}

/// One container-local Docker `/etc/hosts` entry.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ExtraHost(String);

impl ExtraHost {
    /// Build an entry from a non-empty host and an IP address or `host-gateway`.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when either part is invalid.
    pub fn from_parts(host: &str, address: &str) -> Result<Self, ValueError> {
        let address = address
            .strip_prefix('[')
            .and_then(|address| address.strip_suffix(']'))
            .unwrap_or(address);
        if host.is_empty()
            || host
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || matches!(byte, b':' | b'='))
            || address.is_empty()
            || (address != "host-gateway" && address.parse::<IpAddr>().is_err())
        {
            return Err(extra_host_error(format!("{host}:{address}")));
        }
        Ok(Self(format!("{host}:{address}")))
    }

    /// Parse Docker's canonical `host:address` entry form.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] when the host is empty or the address is neither
    /// an IP address nor `host-gateway`.
    pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        let Some((host, address)) = value.split_once(':') else {
            return Err(extra_host_error(value));
        };
        Self::from_parts(host, address).map_err(|_| extra_host_error(value))
    }

    /// Borrow Docker's canonical `host:address` entry.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn extra_host_error(value: impl Into<String>) -> ValueError {
    ValueError::new(
        "extra host",
        value,
        "a non-empty host and an IP address or 'host-gateway'",
    )
}

impl fmt::Display for ExtraHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl FromStr for ExtraHost {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for ExtraHost {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<ExtraHost> for String {
    fn from(value: ExtraHost) -> Self {
        value.0
    }
}
