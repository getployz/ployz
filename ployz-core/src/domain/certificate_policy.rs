//! Certificate Policy: cluster-state knobs for issuance, with honest defaults.

use std::time::Duration;

use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD},
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::ValueError;

/// Cluster table key for the Certificate Policy row.
pub const CERTIFICATE_POLICY_CLUSTER_KEY: &str = "certificate_policy";

/// Two thirds of a certificate's own lifetime. Used when the row omits the fraction.
pub const DEFAULT_RENEW_AT_LIFETIME_FRACTION: f64 = 2.0 / 3.0;

/// Retry interval when the row omits backoff.base. Matches the current issuance loop.
pub const DEFAULT_BACKOFF_BASE: Duration = Duration::from_secs(60);

/// Upper bound when the row omits backoff.cap.
pub const DEFAULT_BACKOFF_CAP: Duration = Duration::from_secs(60 * 60);

/// HTTP-01 probe wait when the row omits probe_timeout.
pub const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_secs(30);

const HTTP_01: &str = "http-01";

crate::value::open_string_enum!(CertificateKeyType, Unrecognized {
    EcdsaP256 => "ecdsa-p256",
    EcdsaP384 => "ecdsa-p384",
    Rsa2048 => "rsa-2048",
});

/// External Account Binding credentials for a certificate authority that requires them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalAccountBinding {
    kid: String,
    hmac_key: String,
}

impl ExternalAccountBinding {
    /// Both `kid` and `hmac_key` must be non-empty.
    #[must_use]
    pub fn new(kid: impl Into<String>, hmac_key: impl Into<String>) -> Option<Self> {
        let kid = kid.into();
        let hmac_key = hmac_key.into();
        (!kid.is_empty() && !hmac_key.is_empty()).then_some(Self { kid, hmac_key })
    }

    #[must_use]
    pub fn kid(&self) -> &str {
        &self.kid
    }

    #[must_use]
    pub fn hmac_key(&self) -> &str {
        &self.hmac_key
    }

    /// Decode the HMAC key from standard or URL-safe base64.
    ///
    /// # Errors
    ///
    /// Returns when the key is not base64.
    pub fn to_hmac_key_bytes(&self) -> Result<Vec<u8>, ValueError> {
        URL_SAFE_NO_PAD
            .decode(&self.hmac_key)
            .or_else(|_| URL_SAFE.decode(&self.hmac_key))
            .or_else(|_| STANDARD.decode(&self.hmac_key))
            .map_err(|_| {
                ValueError::new(
                    "EAB hmac_key",
                    &self.hmac_key,
                    "standard or URL-safe base64",
                )
            })
    }
}

/// Values the daemon uses after applying built-in defaults to a Certificate Policy row.
#[derive(Clone, Debug, PartialEq)]
pub struct CertificatePolicy {
    directory_url: Option<String>,
    eab: Option<ExternalAccountBinding>,
    key_type: CertificateKeyType,
    renew_at_lifetime_fraction: f64,
    backoff_base: Duration,
    backoff_cap: Duration,
    probe_timeout: Duration,
}

impl CertificatePolicy {
    /// Built-in defaults. `directory_url` is the no-policy directory (env or Let's Encrypt).
    #[must_use]
    pub fn built_in(directory_url: Option<String>) -> Self {
        Self {
            directory_url,
            eab: None,
            key_type: CertificateKeyType::EcdsaP256,
            renew_at_lifetime_fraction: DEFAULT_RENEW_AT_LIFETIME_FRACTION,
            backoff_base: DEFAULT_BACKOFF_BASE,
            backoff_cap: DEFAULT_BACKOFF_CAP,
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
        }
    }

    #[must_use]
    pub fn directory_url(&self) -> Option<&str> {
        self.directory_url.as_deref()
    }

    #[must_use]
    pub fn eab(&self) -> Option<&ExternalAccountBinding> {
        self.eab.as_ref()
    }

    #[must_use]
    pub fn key_type(&self) -> &CertificateKeyType {
        &self.key_type
    }

    #[must_use]
    pub fn renew_at_lifetime_fraction(&self) -> f64 {
        self.renew_at_lifetime_fraction
    }

    #[must_use]
    pub fn backoff_base(&self) -> Duration {
        self.backoff_base
    }

    #[must_use]
    pub fn backoff_cap(&self) -> Duration {
        self.backoff_cap
    }

    #[must_use]
    pub fn probe_timeout(&self) -> Duration {
        self.probe_timeout
    }
}

/// Why a Certificate Policy was refused instead of applied.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{reason}")]
pub struct CertificatePolicyRefusal {
    reason: String,
}

impl CertificatePolicyRefusal {
    fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

#[derive(Deserialize)]
struct PolicyBody {
    #[serde(default)]
    directory_url: Option<String>,
    #[serde(default)]
    eab: Option<EabBody>,
    #[serde(default)]
    key_type: Option<CertificateKeyType>,
    #[serde(default)]
    renew_at_lifetime_fraction: Option<f64>,
    #[serde(default)]
    backoff: Option<BackoffBody>,
    #[serde(default)]
    probe_timeout: Option<u64>,
    #[serde(default)]
    challenge_kind: Option<String>,
}

#[derive(Deserialize)]
struct EabBody {
    #[serde(default)]
    kid: String,
    #[serde(default)]
    hmac_key: String,
}

#[derive(Deserialize)]
struct BackoffBody {
    #[serde(default)]
    base: Option<u64>,
    #[serde(default)]
    cap: Option<u64>,
}

/// Apply a Certificate Policy row over built-in defaults.
///
/// Absent or empty input yields `defaults`. Unknown fields are ignored.
/// A challenge kind other than HTTP-01 is refused.
///
/// # Errors
///
/// Returns [`CertificatePolicyRefusal`] when the row is not JSON, names a
/// challenge kind or key type this daemon cannot perform, or sets a knob to
/// an unusable value.
pub fn resolve_certificate_policy(
    raw: Option<&str>,
    defaults: &CertificatePolicy,
) -> Result<CertificatePolicy, CertificatePolicyRefusal> {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Ok(defaults.clone());
    };
    let body: PolicyBody = serde_json::from_str(raw).map_err(|error| {
        CertificatePolicyRefusal::new(format!("certificate policy is not valid JSON: {error}"))
    })?;
    if let Some(kind) = body
        .challenge_kind
        .as_deref()
        .filter(|kind| !kind.is_empty())
        && kind != HTTP_01
    {
        return Err(CertificatePolicyRefusal::new(format!(
            "certificate policy names challenge kind {kind} which this daemon cannot perform"
        )));
    }
    let mut policy = defaults.clone();
    if let Some(directory_url) = body.directory_url {
        policy.directory_url = (!directory_url.is_empty()).then_some(directory_url);
    }
    if let Some(eab) = body.eab {
        let binding = ExternalAccountBinding::new(eab.kid, eab.hmac_key).ok_or_else(|| {
            CertificatePolicyRefusal::new("certificate policy eab is missing kid or hmac_key")
        })?;
        binding.to_hmac_key_bytes()?;
        policy.eab = Some(binding);
    }
    if let Some(key_type) = body.key_type {
        if let CertificateKeyType::Unrecognized(kind) = &key_type {
            return Err(CertificatePolicyRefusal::new(format!(
                "certificate policy names key type {kind} which this daemon cannot perform"
            )));
        }
        policy.key_type = key_type;
    }
    if let Some(fraction) = body.renew_at_lifetime_fraction {
        if !fraction.is_finite() || !(0.0 < fraction && fraction < 1.0) {
            return Err(CertificatePolicyRefusal::new(
                "certificate policy renew_at_lifetime_fraction must be between 0 and 1 exclusive",
            ));
        }
        policy.renew_at_lifetime_fraction = fraction;
    }
    if let Some(backoff) = body.backoff {
        if let Some(base) = backoff.base {
            policy.backoff_base = Duration::from_secs(base);
        }
        if let Some(cap) = backoff.cap {
            policy.backoff_cap = Duration::from_secs(cap);
        }
        if policy.backoff_base.is_zero() || policy.backoff_cap.is_zero() {
            return Err(CertificatePolicyRefusal::new(
                "certificate policy backoff bounds must be greater than zero",
            ));
        }
        if policy.backoff_base > policy.backoff_cap {
            return Err(CertificatePolicyRefusal::new(
                "certificate policy backoff.base must not exceed backoff.cap",
            ));
        }
    }
    if let Some(timeout) = body.probe_timeout {
        if timeout == 0 {
            return Err(CertificatePolicyRefusal::new(
                "certificate policy probe_timeout must be greater than zero",
            ));
        }
        policy.probe_timeout = Duration::from_secs(timeout);
    }
    Ok(policy)
}

impl From<ValueError> for CertificatePolicyRefusal {
    fn from(error: ValueError) -> Self {
        Self::new(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        CERTIFICATE_POLICY_CLUSTER_KEY, CertificateKeyType, CertificatePolicy,
        DEFAULT_BACKOFF_BASE, DEFAULT_BACKOFF_CAP, DEFAULT_PROBE_TIMEOUT,
        DEFAULT_RENEW_AT_LIFETIME_FRACTION, ExternalAccountBinding, resolve_certificate_policy,
    };

    #[test]
    fn cluster_key_is_stable() {
        assert_eq!(CERTIFICATE_POLICY_CLUSTER_KEY, "certificate_policy");
    }

    #[test]
    fn absent_policy_is_the_built_in_defaults() {
        let defaults = CertificatePolicy::built_in(Some(
            "https://acme-v02.api.letsencrypt.org/directory".into(),
        ));
        assert_eq!(
            resolve_certificate_policy(None, &defaults).unwrap(),
            defaults
        );
        assert_eq!(
            resolve_certificate_policy(Some(""), &defaults).unwrap(),
            defaults
        );
        assert_eq!(
            resolve_certificate_policy(Some("{}"), &defaults).unwrap(),
            defaults
        );
        assert_eq!(
            defaults.directory_url(),
            Some("https://acme-v02.api.letsencrypt.org/directory")
        );
        assert_eq!(defaults.eab(), None);
        assert_eq!(defaults.key_type(), &CertificateKeyType::EcdsaP256);
        assert_eq!(
            defaults.renew_at_lifetime_fraction(),
            DEFAULT_RENEW_AT_LIFETIME_FRACTION
        );
        assert_eq!(defaults.backoff_base(), DEFAULT_BACKOFF_BASE);
        assert_eq!(defaults.backoff_cap(), DEFAULT_BACKOFF_CAP);
        assert_eq!(defaults.probe_timeout(), DEFAULT_PROBE_TIMEOUT);
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let defaults = CertificatePolicy::built_in(Some("https://ca.test/directory".into()));
        let policy = resolve_certificate_policy(
            Some(r#"{"future_knob":true,"profiles":["shortlived"]}"#),
            &defaults,
        )
        .unwrap();
        assert_eq!(policy, defaults);
    }

    #[test]
    fn policy_overrides_every_knob() {
        let defaults = CertificatePolicy::built_in(Some("https://ca.test/directory".into()));
        let policy = resolve_certificate_policy(
            Some(
                r#"{
                    "directory_url":"http://127.0.0.1:9/directory",
                    "eab":{"kid":"kid-1","hmac_key":"dGVzdA"},
                    "key_type":"ecdsa-p384",
                    "renew_at_lifetime_fraction":0.5,
                    "backoff":{"base":10,"cap":120},
                    "probe_timeout":5,
                    "challenge_kind":"http-01"
                }"#,
            ),
            &defaults,
        )
        .unwrap();
        assert_eq!(policy.directory_url(), Some("http://127.0.0.1:9/directory"));
        let eab = policy.eab().unwrap();
        assert_eq!(eab.kid(), "kid-1");
        assert_eq!(eab.hmac_key(), "dGVzdA");
        assert_eq!(eab.to_hmac_key_bytes().unwrap(), b"test");
        assert_eq!(policy.key_type(), &CertificateKeyType::EcdsaP384);
        assert_eq!(policy.renew_at_lifetime_fraction(), 0.5);
        assert_eq!(policy.backoff_base(), Duration::from_secs(10));
        assert_eq!(policy.backoff_cap(), Duration::from_secs(120));
        assert_eq!(policy.probe_timeout(), Duration::from_secs(5));
    }

    #[test]
    fn omitted_knobs_keep_defaults() {
        let defaults = CertificatePolicy::built_in(Some("https://ca.test/directory".into()));
        let policy =
            resolve_certificate_policy(Some(r#"{"probe_timeout":15}"#), &defaults).unwrap();
        assert_eq!(policy.directory_url(), defaults.directory_url());
        assert_eq!(policy.probe_timeout(), Duration::from_secs(15));
        assert_eq!(policy.key_type(), defaults.key_type());
        assert_eq!(
            policy.renew_at_lifetime_fraction(),
            defaults.renew_at_lifetime_fraction()
        );
    }

    #[test]
    fn unsupported_challenge_kind_is_refused() {
        let defaults = CertificatePolicy::built_in(None);
        let error = resolve_certificate_policy(Some(r#"{"challenge_kind":"dns-01"}"#), &defaults)
            .unwrap_err();
        assert_eq!(
            error.reason(),
            "certificate policy names challenge kind dns-01 which this daemon cannot perform"
        );
    }

    #[test]
    fn unsupported_key_type_is_refused() {
        let defaults = CertificatePolicy::built_in(None);
        let error =
            resolve_certificate_policy(Some(r#"{"key_type":"ed25519"}"#), &defaults).unwrap_err();
        assert_eq!(
            error.reason(),
            "certificate policy names key type ed25519 which this daemon cannot perform"
        );
    }

    #[test]
    fn malformed_json_is_refused() {
        let defaults = CertificatePolicy::built_in(None);
        let error = resolve_certificate_policy(Some("{"), &defaults).unwrap_err();
        assert!(
            error
                .reason()
                .starts_with("certificate policy is not valid JSON:"),
            "{}",
            error.reason()
        );
    }

    #[test]
    fn empty_eab_is_refused() {
        let defaults = CertificatePolicy::built_in(None);
        let error = resolve_certificate_policy(
            Some(r#"{"eab":{"kid":"","hmac_key":"dGVzdA"}}"#),
            &defaults,
        )
        .unwrap_err();
        assert_eq!(
            error.reason(),
            "certificate policy eab is missing kid or hmac_key"
        );
    }

    #[test]
    fn eab_hmac_must_be_base64() {
        let defaults = CertificatePolicy::built_in(None);
        let error = resolve_certificate_policy(
            Some(r#"{"eab":{"kid":"kid","hmac_key":"***"}}"#),
            &defaults,
        )
        .unwrap_err();
        assert!(error.reason().contains("base64"), "{}", error.reason());
    }

    #[test]
    fn empty_eab_constructor_is_absent() {
        assert_eq!(ExternalAccountBinding::new("", "dGVzdA"), None);
        assert_eq!(ExternalAccountBinding::new("kid", ""), None);
    }
}
