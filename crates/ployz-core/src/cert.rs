//! Certificate state and ACME challenge models.

use serde::{Deserialize, Serialize};

use crate::ids::CertId;
use crate::install::{AbsoluteInstallPath, InstallSha256Digest};
use crate::ops::RouteHostname;
use crate::state_key::id_prefixed_state_key;
use sha2::{Digest, Sha256};

use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

pub const CERT_STATE_PREFIX: &str = "certs";
pub const ACME_LOCK_PREFIX: &str = "acme";
pub const ACME_CHALLENGE_PREFIX: &str = "acme.challenges";
pub const MANAGED_LEASE_DOMAIN_SUFFIX: &str = "up.ployz.app";
pub const DEFAULT_MANAGED_LEASE_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

/// Active certificate intent/evidence value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ActiveCertState {
    pub cert_id: CertId,
    pub hostname: RouteHostname,
    pub bundle_ref: CertBundleRef,
    pub validity: CertValidityWindow,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseAcquireRequest {
    pub cluster_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseAcquired {
    pub lease: ManagedLeaseRecord,
    pub bundle: ManagedCertBundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseRenewed {
    pub lease: ManagedLeaseRecord,
    pub bundle: ManagedCertBundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseRecord {
    pub name: ManagedLeaseName,
    pub token: LeaseBearerToken,
    pub issued_at: LeaseIssuedAt,
    pub expires_at: LeaseExpiresAt,
}

impl ManagedLeaseRecord {
    pub fn try_new(
        name: ManagedLeaseName,
        token: LeaseBearerToken,
        issued_at: LeaseIssuedAt,
        expires_at: LeaseExpiresAt,
    ) -> Result<Self, ManagedLeaseError> {
        if issued_at.unix_seconds() >= expires_at.unix_seconds() {
            return Err(ManagedLeaseError::EmptyOrInvertedLease {
                issued_at,
                expires_at,
            });
        }

        Ok(Self {
            name,
            token,
            issued_at,
            expires_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"ManagedLeaseName\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct ManagedLeaseName(String);

impl ManagedLeaseName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ManagedLeaseError> {
        let value = value.into().to_ascii_lowercase();
        if value.is_empty() {
            return Err(ManagedLeaseError::EmptyLeaseName);
        }

        if value.starts_with('-')
            || value.ends_with('-')
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(ManagedLeaseError::InvalidLeaseName { value });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn hostname_suffix(&self) -> String {
        format!("{}.{}", self.0, MANAGED_LEASE_DOMAIN_SUFFIX)
    }

    #[must_use]
    pub fn wildcard_and_apex(&self) -> [String; 2] {
        let apex = self.hostname_suffix();
        [format!("*.{apex}"), apex]
    }
}

impl TryFrom<String> for ManagedLeaseName {
    type Error = ManagedLeaseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ManagedLeaseName> for String {
    fn from(value: ManagedLeaseName) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"LeaseBearerToken\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct LeaseBearerToken(String);

impl LeaseBearerToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ManagedLeaseError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ManagedLeaseError::EmptyBearerToken);
        }

        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(ManagedLeaseError::InvalidBearerToken { value });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LeaseBearerToken {
    type Error = ManagedLeaseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<LeaseBearerToken> for String {
    fn from(value: LeaseBearerToken) -> Self {
        value.0
    }
}

positive_u64_wire_newtype! {
    pub struct LeaseIssuedAt;
    ts_brand: "Brand<string, \"LeaseIssuedAt\">";
    accessor: unix_seconds;
    error: LeaseTimestampError;
}

positive_u64_wire_newtype! {
    pub struct LeaseExpiresAt;
    ts_brand: "Brand<string, \"LeaseExpiresAt\">";
    accessor: unix_seconds;
    error: LeaseTimestampError;
}

positive_u64_wire_error! {
    pub enum LeaseTimestampError;
    noun: "managed lease timestamp";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedCertBundle {
    pub lease: ManagedLeaseName,
    pub dns_names: [String; 2],
    pub certificate_chain_pem: String,
    pub private_key_pem: String,
    pub issued_at: LeaseIssuedAt,
    pub expires_at: LeaseExpiresAt,
    pub digest: InstallSha256Digest,
}

impl ManagedCertBundle {
    pub fn try_new(
        name: ManagedLeaseName,
        dns_names: [String; 2],
        certificate_chain_pem: String,
        private_key_pem: String,
        issued_at: LeaseIssuedAt,
        expires_at: LeaseExpiresAt,
    ) -> Result<Self, ManagedLeaseError> {
        if dns_names != name.wildcard_and_apex() {
            return Err(ManagedLeaseError::BundleDnsNamesInvalid { dns_names });
        }
        let digest = bundle_digest(&certificate_chain_pem, &private_key_pem)?;

        Ok(Self {
            lease: name,
            dns_names,
            certificate_chain_pem,
            private_key_pem,
            issued_at,
            expires_at,
            digest,
        })
    }

    #[must_use]
    pub fn dns_names(&self) -> [&str; 2] {
        [self.dns_names[0].as_str(), self.dns_names[1].as_str()]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedLeaseError {
    #[error("managed lease name is empty")]
    EmptyLeaseName,
    #[error("managed lease name is invalid: {value}")]
    InvalidLeaseName { value: String },
    #[error("lease bearer token is empty")]
    EmptyBearerToken,
    #[error("lease bearer token is malformed")]
    InvalidBearerToken { value: String },
    #[error("managed lease expiry must be after issue time")]
    EmptyOrInvertedLease {
        issued_at: LeaseIssuedAt,
        expires_at: LeaseExpiresAt,
    },
    #[error("managed cert bundle DNS names must be wildcard and apex for its lease")]
    BundleDnsNamesInvalid { dns_names: [String; 2] },
    #[error("managed cert bundle digest is invalid: {0}")]
    Digest(#[from] crate::install::InstallContractError),
}

id_prefixed_state_key! { pub struct CertStateKey; prefix: CERT_STATE_PREFIX; fn from_cert_id(&CertId); }
id_prefixed_state_key! { pub struct AcmeLockKey; prefix: ACME_LOCK_PREFIX; fn from_cert_id(&CertId); }
id_prefixed_state_key! { pub struct AcmeChallengeStateKey; prefix: ACME_CHALLENGE_PREFIX; fn from_cert_id(&CertId); }

/// ACME HTTP-01 challenge evidence value.
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcmeChallengeError {
    #[error("ACME challenge value does not start with its token")]
    TokenValueMismatch {
        token: AcmeChallengeToken,
        value: AcmeChallengeValue,
    },
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CertValidityError {
    #[error("certificate validity window must end after it starts")]
    EmptyOrInverted {
        not_before: CertValidAt,
        not_after: CertValidAt,
    },
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
        validate_artifact_ref(&value)?;

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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CertTextError {
    #[error("{field} is empty")]
    Empty { field: &'static str },
    #[error("{field} contains whitespace: {value}")]
    ContainsWhitespace { field: &'static str, value: String },
    #[error("{field} contains non-visible ASCII: {value}")]
    ContainsNonVisibleAscii { field: &'static str, value: String },
    #[error("ACME challenge token is invalid: {value}")]
    InvalidAcmeToken { value: String },
    #[error("ACME challenge value is invalid: {value}")]
    InvalidAcmeChallengeValue { value: String },
    #[error("cert bundle reference is invalid: {value}")]
    InvalidBundleRef { value: String },
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

fn validate_artifact_ref(value: &str) -> Result<(), CertTextError> {
    let Some(rest) = value.strip_prefix("sha256:") else {
        return Err(CertTextError::InvalidBundleRef {
            value: value.to_owned(),
        });
    };
    let Some((digest, path)) = rest.split_once(':') else {
        return Err(CertTextError::InvalidBundleRef {
            value: value.to_owned(),
        });
    };

    if InstallSha256Digest::try_new(digest).is_err() || AbsoluteInstallPath::try_new(path).is_err()
    {
        return Err(CertTextError::InvalidBundleRef {
            value: value.to_owned(),
        });
    }

    Ok(())
}

fn bundle_digest(
    certificate_chain_pem: &str,
    private_key_pem: &str,
) -> Result<InstallSha256Digest, crate::install::InstallContractError> {
    let mut hasher = Sha256::new();
    hasher.update(certificate_chain_pem.as_bytes());
    hasher.update(b"\0");
    hasher.update(private_key_pem.as_bytes());
    let digest = hasher.finalize();
    InstallSha256Digest::try_new(format!("{digest:x}"))
}

fn is_base64_url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}
