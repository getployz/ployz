//! Certificate material, issuance, managed lease, and gateway contract models.

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::install::InstallSha256Digest;
use crate::operation::RouteHostname;
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

pub const MANAGED_LEASE_DOMAIN_SUFFIX: &str = "up.ployz.app";
pub const DEFAULT_LEASE_WORKER_URL: &str = "https://dns.ployz.app";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseAcquireRequest {
    pub acquisition_id: ManagedLeaseAcquisitionId,
    pub token: LeaseBearerToken,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseRenewRequest {
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
}

/// Canonical public gateway addresses applied to a managed lease.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseAddressSet {
    ipv4: BTreeSet<Ipv4Addr>,
    ipv6: BTreeSet<Ipv6Addr>,
}

impl ManagedLeaseAddressSet {
    #[must_use]
    pub fn new(ipv4: Vec<Ipv4Addr>, ipv6: Vec<Ipv6Addr>) -> Self {
        Self {
            ipv4: ipv4.into_iter().collect(),
            ipv6: ipv6.into_iter().collect(),
        }
    }

    #[must_use]
    pub const fn ipv4(&self) -> &BTreeSet<Ipv4Addr> {
        &self.ipv4
    }

    #[must_use]
    pub const fn ipv6(&self) -> &BTreeSet<Ipv6Addr> {
        &self.ipv6
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ManagedCertificateIssuanceFailureKind {
    RateLimit,
    #[serde(rename = "provider_5xx")]
    Provider5xx,
    ValidationTimeout,
    Caa,
    DnsTxtMissing,
    ProviderError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseAcquired {
    pub lease: ManagedLeaseRecord,
    pub bundle: Option<ManagedCertBundle>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseRenewed {
    pub lease: ManagedLeaseRecord,
    pub bundle: Option<ManagedCertBundle>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(try_from = "ManagedLeaseRecordWire", into = "ManagedLeaseRecordWire")]
pub struct ManagedLeaseRecord {
    pub name: ManagedLeaseName,
    pub token: LeaseBearerToken,
    pub issued_at: LeaseIssuedAt,
    pub expires_at: LeaseExpiresAt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedLeaseRecordWire {
    name: ManagedLeaseName,
    token: LeaseBearerToken,
    issued_at: LeaseIssuedAt,
    expires_at: LeaseExpiresAt,
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

    #[must_use]
    pub const fn is_valid_at(&self, now_seconds: u64) -> bool {
        self.issued_at.unix_seconds() <= now_seconds && now_seconds < self.expires_at.unix_seconds()
    }
}

impl TryFrom<ManagedLeaseRecordWire> for ManagedLeaseRecord {
    type Error = ManagedLeaseError;

    fn try_from(value: ManagedLeaseRecordWire) -> Result<Self, Self::Error> {
        let ManagedLeaseRecordWire {
            name,
            token,
            issued_at,
            expires_at,
        } = value;
        Self::try_new(name, token, issued_at, expires_at)
    }
}

impl From<ManagedLeaseRecord> for ManagedLeaseRecordWire {
    fn from(value: ManagedLeaseRecord) -> Self {
        let ManagedLeaseRecord {
            name,
            token,
            issued_at,
            expires_at,
        } = value;
        Self {
            name,
            token,
            issued_at,
            expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"ManagedLeaseName\">"))]
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(type = "Brand<string, \"ManagedLeaseAcquisitionId\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct ManagedLeaseAcquisitionId(String);

impl ManagedLeaseAcquisitionId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ManagedLeaseError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ManagedLeaseError::EmptyAcquisitionId);
        }
        if !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(ManagedLeaseError::InvalidAcquisitionId { value });
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ManagedLeaseAcquisitionId {
    type Error = ManagedLeaseError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<ManagedLeaseAcquisitionId> for String {
    fn from(value: ManagedLeaseAcquisitionId) -> Self {
        value.0
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"LeaseBearerToken\">"))]
#[serde(try_from = "String", into = "String")]
pub struct LeaseBearerToken(String);

impl std::fmt::Debug for LeaseBearerToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("LeaseBearerToken")
            .field(&"[REDACTED]")
            .finish()
    }
}

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
            return Err(ManagedLeaseError::InvalidBearerToken);
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(try_from = "ManagedCertBundleWire", into = "ManagedCertBundleWire")]
pub struct ManagedCertBundle {
    pub lease: ManagedLeaseName,
    pub dns_names: [String; 2],
    pub certificate_chain_pem: String,
    pub private_key_pem: String,
    pub issued_at: LeaseIssuedAt,
    pub expires_at: LeaseExpiresAt,
    pub digest: InstallSha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManagedCertBundleWire {
    lease: ManagedLeaseName,
    dns_names: [String; 2],
    certificate_chain_pem: String,
    private_key_pem: String,
    issued_at: LeaseIssuedAt,
    expires_at: LeaseExpiresAt,
    digest: InstallSha256Digest,
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
        if issued_at.unix_seconds() >= expires_at.unix_seconds() {
            return Err(ManagedLeaseError::EmptyOrInvertedLease {
                issued_at,
                expires_at,
            });
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

    #[must_use]
    pub const fn is_valid_at(&self, now_seconds: u64) -> bool {
        self.issued_at.unix_seconds() <= now_seconds && now_seconds < self.expires_at.unix_seconds()
    }
}

impl TryFrom<ManagedCertBundleWire> for ManagedCertBundle {
    type Error = ManagedLeaseError;

    fn try_from(value: ManagedCertBundleWire) -> Result<Self, Self::Error> {
        let ManagedCertBundleWire {
            lease,
            dns_names,
            certificate_chain_pem,
            private_key_pem,
            issued_at,
            expires_at,
            digest: expected_digest,
        } = value;
        let bundle = Self::try_new(
            lease,
            dns_names,
            certificate_chain_pem,
            private_key_pem,
            issued_at,
            expires_at,
        )?;
        if bundle.digest != expected_digest {
            return Err(ManagedLeaseError::BundleDigestMismatch);
        }
        Ok(bundle)
    }
}

impl From<ManagedCertBundle> for ManagedCertBundleWire {
    fn from(value: ManagedCertBundle) -> Self {
        let ManagedCertBundle {
            lease,
            dns_names,
            certificate_chain_pem,
            private_key_pem,
            issued_at,
            expires_at,
            digest,
        } = value;
        Self {
            lease,
            dns_names,
            certificate_chain_pem,
            private_key_pem,
            issued_at,
            expires_at,
            digest,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManagedLeaseError {
    #[error("managed lease name is empty")]
    EmptyLeaseName,
    #[error("managed lease name is invalid: {value}")]
    InvalidLeaseName { value: String },
    #[error("managed lease acquisition id is empty")]
    EmptyAcquisitionId,
    #[error("managed lease acquisition id is malformed: {value}")]
    InvalidAcquisitionId { value: String },
    #[error("lease bearer token is empty")]
    EmptyBearerToken,
    #[error("lease bearer token is malformed")]
    InvalidBearerToken,
    #[error("managed lease expiry must be after issue time")]
    EmptyOrInvertedLease {
        issued_at: LeaseIssuedAt,
        expires_at: LeaseExpiresAt,
    },
    #[error("managed cert bundle DNS names must be wildcard and apex for its lease")]
    BundleDnsNamesInvalid { dns_names: [String; 2] },
    #[error("managed cert bundle digest does not match its certificate and private key")]
    BundleDigestMismatch,
    #[error("managed cert bundle digest is invalid: {0}")]
    Digest(#[from] crate::install::InstallContractError),
}

/// ACME HTTP-01 challenge evidence value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"AcmeChallengeToken\">"))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"AcmeChallengeValue\">"))]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn managed_lease_address_set_sorts_and_deduplicates_each_family() {
        let addresses = ManagedLeaseAddressSet::new(
            vec![
                "203.0.113.8".parse().expect("IPv4"),
                "198.51.100.2".parse().expect("IPv4"),
                "203.0.113.8".parse().expect("IPv4"),
            ],
            vec![
                "2001:db8::8".parse().expect("IPv6"),
                "2001:db8::2".parse().expect("IPv6"),
                "2001:db8::8".parse().expect("IPv6"),
            ],
        );

        assert_eq!(
            addresses.ipv4().iter().copied().collect::<Vec<_>>(),
            [
                "198.51.100.2".parse::<Ipv4Addr>().expect("IPv4"),
                "203.0.113.8".parse::<Ipv4Addr>().expect("IPv4"),
            ]
        );
        assert_eq!(
            addresses.ipv6().iter().copied().collect::<Vec<_>>(),
            [
                "2001:db8::2".parse::<Ipv6Addr>().expect("IPv6"),
                "2001:db8::8".parse::<Ipv6Addr>().expect("IPv6"),
            ]
        );
    }
}
