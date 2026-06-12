//! Certificate state and ACME challenge models.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::ids::CertId;
use crate::ops::RouteHostname;
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

pub const CERT_STATE_PREFIX: &str = "certs";
pub const ACME_LOCK_PREFIX: &str = "acme";
pub const ACME_CHALLENGE_PREFIX: &str = "acme.challenges";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveCertState {
    pub cert_id: CertId,
    pub hostname: RouteHostname,
    pub bundle_ref: CertBundleRef,
    pub validity: CertValidityWindow,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertStateKey(String);

impl CertStateKey {
    #[must_use]
    pub fn from_cert_id(cert_id: &CertId) -> Self {
        Self(format!("{CERT_STATE_PREFIX}.{}", cert_id.as_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeLockKey(String);

impl AcmeLockKey {
    #[must_use]
    pub fn from_cert_id(cert_id: &CertId) -> Self {
        Self(format!("{ACME_LOCK_PREFIX}.{}", cert_id.as_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeChallengeStateKey(String);

impl AcmeChallengeStateKey {
    #[must_use]
    pub fn from_cert_id(cert_id: &CertId) -> Self {
        Self(format!("{ACME_CHALLENGE_PREFIX}.{}", cert_id.as_str()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(try_from = "AcmeHttp01ChallengeWire", into = "AcmeHttp01ChallengeWire")]
pub struct AcmeHttp01Challenge {
    hostname: RouteHostname,
    token: AcmeChallengeToken,
    value: AcmeChallengeValue,
    ttl_seconds: AcmeChallengeTtlSeconds,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AcmeHttp01ChallengeWire {
    hostname: RouteHostname,
    token: AcmeChallengeToken,
    value: AcmeChallengeValue,
    ttl_seconds: AcmeChallengeTtlSeconds,
}

impl AcmeHttp01Challenge {
    pub fn try_new(
        hostname: RouteHostname,
        token: AcmeChallengeToken,
        value: AcmeChallengeValue,
        ttl_seconds: AcmeChallengeTtlSeconds,
    ) -> Result<Self, AcmeChallengeError> {
        if !value.matches_token(&token) {
            return Err(AcmeChallengeError::TokenValueMismatch { token, value });
        }

        Ok(Self {
            hostname,
            token,
            value,
            ttl_seconds,
        })
    }

    #[must_use]
    pub const fn hostname(&self) -> &RouteHostname {
        &self.hostname
    }

    #[must_use]
    pub const fn token(&self) -> &AcmeChallengeToken {
        &self.token
    }

    #[must_use]
    pub const fn value(&self) -> &AcmeChallengeValue {
        &self.value
    }

    #[must_use]
    pub const fn ttl_seconds(&self) -> AcmeChallengeTtlSeconds {
        self.ttl_seconds
    }
}

impl TryFrom<AcmeHttp01ChallengeWire> for AcmeHttp01Challenge {
    type Error = AcmeChallengeError;

    fn try_from(value: AcmeHttp01ChallengeWire) -> Result<Self, Self::Error> {
        Self::try_new(value.hostname, value.token, value.value, value.ttl_seconds)
    }
}

impl From<AcmeHttp01Challenge> for AcmeHttp01ChallengeWire {
    fn from(value: AcmeHttp01Challenge) -> Self {
        Self {
            hostname: value.hostname,
            token: value.token,
            value: value.value,
            ttl_seconds: value.ttl_seconds,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcmeChallengeError {
    TokenValueMismatch {
        token: AcmeChallengeToken,
        value: AcmeChallengeValue,
    },
}

impl fmt::Display for AcmeChallengeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TokenValueMismatch { .. } => {
                formatter.write_str("ACME challenge value does not start with its token")
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct CertValidityWindow {
    pub not_before: CertValidAt,
    pub not_after: CertValidAt,
}

impl CertValidityWindow {
    pub fn try_new(
        not_before: CertValidAt,
        not_after: CertValidAt,
    ) -> Result<Self, CertValidityError> {
        if not_before >= not_after {
            return Err(CertValidityError::EmptyOrInverted {
                not_before,
                not_after,
            });
        }

        Ok(Self {
            not_before,
            not_after,
        })
    }
}

positive_u64_wire_newtype! {
    pub struct CertValidAt;
    ts_brand: "Brand<string, \"CertValidAt\">";
    accessor: unix_seconds;
    error: CertValidAtError;
}

positive_u64_wire_error! {
    pub enum CertValidAtError;
    noun: "certificate validity timestamp";
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertValidityError {
    EmptyOrInverted {
        not_before: CertValidAt,
        not_after: CertValidAt,
    },
}

impl fmt::Display for CertValidityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOrInverted { .. } => {
                formatter.write_str("certificate validity window must end after it starts")
            }
        }
    }
}

positive_u64_wire_newtype! {
    pub struct AcmeChallengeTtlSeconds;
    ts_brand: "Brand<string, \"AcmeChallengeTtlSeconds\">";
    accessor: get;
    error: AcmeChallengeTtlError;
}

positive_u64_wire_error! {
    pub enum AcmeChallengeTtlError;
    noun: "ACME challenge TTL";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"AcmeChallengeToken\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct AcmeChallengeToken(String);

impl AcmeChallengeToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CertTextError> {
        let value = value.into();
        if value.is_empty() {
            return Err(CertTextError::Empty {
                field: "ACME challenge token",
            });
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(CertTextError::InvalidAcmeToken { value });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AcmeChallengeToken {
    type Error = CertTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<AcmeChallengeToken> for String {
    fn from(value: AcmeChallengeToken) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"AcmeChallengeValue\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct AcmeChallengeValue(String);

impl AcmeChallengeValue {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CertTextError> {
        let value = validated_visible_ascii(value.into(), "ACME challenge value")?;
        validate_acme_key_authorization(&value)?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn matches_token(&self, token: &AcmeChallengeToken) -> bool {
        self.0
            .strip_prefix(token.as_str())
            .is_some_and(|tail| tail.starts_with('.'))
    }
}

impl TryFrom<String> for AcmeChallengeValue {
    type Error = CertTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<AcmeChallengeValue> for String {
    fn from(value: AcmeChallengeValue) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"CertBundleRef\">"))]
#[serde(try_from = "String", into = "String")]
pub struct CertBundleRef(String);

impl CertBundleRef {
    pub fn try_new(value: impl Into<String>) -> Result<Self, CertTextError> {
        let value = validated_visible_ascii(value.into(), "cert bundle reference")?;
        validate_object_store_ref(&value)?;

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CertBundleRef {
    type Error = CertTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<CertBundleRef> for String {
    fn from(value: CertBundleRef) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertTextError {
    Empty { field: &'static str },
    ContainsWhitespace { field: &'static str, value: String },
    ContainsNonVisibleAscii { field: &'static str, value: String },
    InvalidAcmeToken { value: String },
    InvalidAcmeChallengeValue { value: String },
    InvalidBundleRef { value: String },
}

impl fmt::Display for CertTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty { field } => write!(formatter, "{field} is empty"),
            Self::ContainsWhitespace { field, value } => {
                write!(formatter, "{field} contains whitespace: {value}")
            }
            Self::ContainsNonVisibleAscii { field, value } => {
                write!(formatter, "{field} contains non-visible ASCII: {value}")
            }
            Self::InvalidAcmeToken { value } => {
                write!(formatter, "ACME challenge token is invalid: {value}")
            }
            Self::InvalidAcmeChallengeValue { value } => {
                write!(formatter, "ACME challenge value is invalid: {value}")
            }
            Self::InvalidBundleRef { value } => {
                write!(formatter, "cert bundle reference is invalid: {value}")
            }
        }
    }
}

fn validated_visible_ascii(value: String, field: &'static str) -> Result<String, CertTextError> {
    if value.trim().is_empty() {
        return Err(CertTextError::Empty { field });
    }

    if value.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(CertTextError::ContainsWhitespace { field, value });
    }

    if !value.bytes().all(|byte| byte.is_ascii_graphic()) {
        return Err(CertTextError::ContainsNonVisibleAscii { field, value });
    }

    Ok(value)
}

fn validate_acme_key_authorization(value: &str) -> Result<(), CertTextError> {
    let Some((token, thumbprint)) = value.split_once('.') else {
        return Err(CertTextError::InvalidAcmeChallengeValue {
            value: value.to_owned(),
        });
    };

    if token.is_empty()
        || thumbprint.is_empty()
        || thumbprint.contains('.')
        || !token.bytes().all(is_base64_url_byte)
        || !thumbprint.bytes().all(is_base64_url_byte)
    {
        return Err(CertTextError::InvalidAcmeChallengeValue {
            value: value.to_owned(),
        });
    }

    Ok(())
}

fn validate_object_store_ref(value: &str) -> Result<(), CertTextError> {
    let Some(path) = value.strip_prefix("obj://") else {
        return Err(CertTextError::InvalidBundleRef {
            value: value.to_owned(),
        });
    };
    let Some((bucket, object)) = path.split_once('/') else {
        return Err(CertTextError::InvalidBundleRef {
            value: value.to_owned(),
        });
    };

    if bucket.is_empty() || object.is_empty() || object.starts_with('/') || object.contains("//") {
        return Err(CertTextError::InvalidBundleRef {
            value: value.to_owned(),
        });
    }

    Ok(())
}

fn is_base64_url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}
