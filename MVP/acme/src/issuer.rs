use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

use crate::{AcmeChallengeToken, AcmeHostname, AcmeKeyAuthorization};

pub const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const DEFAULT_HTTP01_CHALLENGE_TTL_SECS: u64 = 15 * 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeIssuerConfig {
    pub directory_url: String,
    pub contact_email: Option<String>,
    pub root_ca_path: Option<PathBuf>,
}

impl Default for AcmeIssuerConfig {
    fn default() -> Self {
        Self {
            directory_url: DEFAULT_ACME_DIRECTORY_URL.to_string(),
            contact_email: None,
            root_ca_path: None,
        }
    }
}

impl AcmeIssuerConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let directory_url = std::env::var("PLOYZ_ACME_DIRECTORY_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_ACME_DIRECTORY_URL.to_string());
        let contact_email = std::env::var("PLOYZ_ACME_CONTACT_EMAIL")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let root_ca_path = std::env::var_os("PLOYZ_ACME_ROOT_CA_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Self {
            directory_url,
            contact_email,
            root_ca_path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeAccountRecord {
    pub account_id: AcmeAccountId,
    pub directory_url: String,
    pub contact_email: Option<String>,
    pub account_credentials_json: String,
    pub created_at_secs: u64,
    pub updated_at_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AcmeAccountId(String);

impl AcmeAccountId {
    #[must_use]
    pub fn for_directory_url(directory_url: &str) -> Self {
        Self(directory_url.to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for AcmeAccountId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartedAcmeOrder {
    pub hostname: AcmeHostname,
    pub order_url: AcmeOrderUrl,
    pub challenges: Vec<AcmeHttp01OrderChallenge>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AcmeOrderUrl(String);

impl AcmeOrderUrl {
    pub fn parse(value: impl Into<String>) -> Result<Self, AcmeIssuanceError> {
        let value = value.into();
        Url::parse(&value).map_err(|source| AcmeIssuanceError::InvalidOrderUrl {
            order_url: value.clone(),
            message: source.to_string(),
        })?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn ensure_same_origin_as_directory(
        &self,
        directory_url: &str,
    ) -> Result<(), AcmeIssuanceError> {
        ensure_order_url_matches_directory(directory_url, self.as_str())
    }
}

impl Display for AcmeOrderUrl {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeHttp01OrderChallenge {
    pub hostname: AcmeHostname,
    pub token: AcmeChallengeToken,
    pub key_authorization: AcmeKeyAuthorization,
    pub expires_at_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssuedCertificate {
    pub hostname: AcmeHostname,
    pub order_url: AcmeOrderUrl,
    pub fullchain_pem: String,
    pub private_key_pem: String,
    pub issued_at_secs: u64,
    pub not_before_secs: Option<u64>,
    pub not_after_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcmeAuthorizationStatus {
    Pending,
    Valid,
    Invalid,
    Revoked,
    Expired,
    Deactivated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcmeOrderStatus {
    Pending,
    Ready,
    Processing,
    Valid,
    Invalid,
}

#[derive(Debug, Error)]
pub enum AcmeIssuanceError {
    #[error("ACME issuer is disabled")]
    Disabled,
    #[error("ACME account creation for {directory_url} was deferred: {message}")]
    AccountCreationDeferred {
        directory_url: String,
        message: String,
    },
    #[error("ACME account lock for {directory_url} failed: {message}")]
    AccountLockFailed {
        directory_url: String,
        message: String,
    },
    #[error("ACME account credentials for {directory_url} are invalid: {message}")]
    InvalidAccountCredentials {
        directory_url: String,
        message: String,
    },
    #[error("ACME order URL is invalid: {order_url}: {message}")]
    InvalidOrderUrl { order_url: String, message: String },
    #[error(
        "ACME order URL origin does not match directory: directory={directory_url}, order={order_url}"
    )]
    OrderUrlOriginMismatch {
        directory_url: String,
        order_url: String,
    },
    #[error("ACME HTTP-01 challenge is missing for {hostname}")]
    Http01ChallengeMissing { hostname: AcmeHostname },
    #[error("ACME authorization for {hostname} has unexpected status {status:?}")]
    AuthorizationUnexpectedStatus {
        hostname: AcmeHostname,
        status: AcmeAuthorizationStatus,
    },
    #[error("ACME order for {hostname} has unexpected status {status:?}")]
    OrderUnexpectedStatus {
        hostname: AcmeHostname,
        status: AcmeOrderStatus,
    },
    #[error(
        "ACME HTTP-01 challenge for {hostname} token {token} was not visible before validation"
    )]
    Http01ChallengeNotVisible {
        hostname: AcmeHostname,
        token: AcmeChallengeToken,
    },
    #[error("ACME operation {operation} failed: {message}")]
    Operation {
        operation: &'static str,
        message: String,
    },
}

#[async_trait]
pub trait AcmeIssuer: Send + Sync {
    async fn start_order(
        &self,
        account_store: &mut dyn AcmeAccountStore,
        challenge_publisher: &mut dyn AcmeHttp01ChallengePublisher,
        hostname: AcmeHostname,
        now_secs: u64,
    ) -> Result<StartedAcmeOrder, AcmeIssuanceError>;

    async fn finalize_order(
        &self,
        account_store: &mut dyn AcmeAccountStore,
        readiness: &dyn AcmeHttp01ChallengeReadiness,
        challenge_publisher: &mut dyn AcmeHttp01ChallengePublisher,
        hostname: AcmeHostname,
        order_url: AcmeOrderUrl,
        now_secs: u64,
    ) -> Result<IssuedCertificate, AcmeIssuanceError>;
}

#[async_trait]
pub trait AcmeAccountStore: Send {
    async fn load_account(
        &self,
        directory_url: &str,
    ) -> Result<Option<AcmeAccountRecord>, AcmeIssuanceError>;

    async fn save_account(&mut self, record: AcmeAccountRecord) -> Result<(), AcmeIssuanceError>;
}

#[async_trait]
pub trait AcmeHttp01ChallengePublisher: Send {
    async fn publish_http01(
        &mut self,
        challenge: AcmeHttp01OrderChallenge,
    ) -> Result<(), AcmeIssuanceError>;

    async fn clear_http01(
        &mut self,
        hostname: &AcmeHostname,
        token: &AcmeChallengeToken,
    ) -> Result<(), AcmeIssuanceError>;
}

#[async_trait]
pub trait AcmeHttp01ChallengeReadiness: Send + Sync {
    async fn wait_ready(
        &self,
        hostname: &AcmeHostname,
        token: &AcmeChallengeToken,
    ) -> Result<(), AcmeIssuanceError>;
}

pub enum AccountAcquisition {
    Allowed(AcmeAccountHold),
    VetoedByPeer(String),
    CoordinationFailed(String),
}

#[async_trait]
pub trait AcmeAccountCoordinator: Send + Sync {
    async fn try_acquire_account(&self, directory_url: &str) -> AccountAcquisition;
}

pub struct AcmeAccountHold {
    #[allow(clippy::type_complexity)]
    releaser: Option<
        Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>,
    >,
}

impl AcmeAccountHold {
    #[must_use]
    pub fn new<F, Fut>(release: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        Self {
            releaser: Some(Box::new(move || Box::pin(release()))),
        }
    }

    #[must_use]
    pub fn noop() -> Self {
        Self::new(|| async {})
    }

    pub async fn release(mut self) {
        if let Some(releaser) = self.releaser.take() {
            releaser().await;
        }
    }
}

impl Drop for AcmeAccountHold {
    fn drop(&mut self) {
        // Holds must be released explicitly so callers know the external ACME
        // side effect and the durable state write are covered by the same
        // critical section. Dropping without release intentionally leaks no
        // implicit async work from this domain crate.
        let _ = self.releaser.take();
    }
}

pub struct NoopAcmeAccountCoordinator;

#[async_trait]
impl AcmeAccountCoordinator for NoopAcmeAccountCoordinator {
    async fn try_acquire_account(&self, _directory_url: &str) -> AccountAcquisition {
        AccountAcquisition::Allowed(AcmeAccountHold::noop())
    }
}

pub struct DisabledAcmeIssuer;

#[async_trait]
impl AcmeIssuer for DisabledAcmeIssuer {
    async fn start_order(
        &self,
        _account_store: &mut dyn AcmeAccountStore,
        _challenge_publisher: &mut dyn AcmeHttp01ChallengePublisher,
        _hostname: AcmeHostname,
        _now_secs: u64,
    ) -> Result<StartedAcmeOrder, AcmeIssuanceError> {
        Err(AcmeIssuanceError::Disabled)
    }

    async fn finalize_order(
        &self,
        _account_store: &mut dyn AcmeAccountStore,
        _readiness: &dyn AcmeHttp01ChallengeReadiness,
        _challenge_publisher: &mut dyn AcmeHttp01ChallengePublisher,
        _hostname: AcmeHostname,
        _order_url: AcmeOrderUrl,
        _now_secs: u64,
    ) -> Result<IssuedCertificate, AcmeIssuanceError> {
        Err(AcmeIssuanceError::Disabled)
    }
}

pub fn contact_uris(contact_email: Option<&str>) -> Vec<String> {
    let Some(contact_email) = contact_email else {
        return Vec::new();
    };
    if contact_email.starts_with("mailto:") {
        vec![contact_email.to_string()]
    } else {
        vec![format!("mailto:{contact_email}")]
    }
}

pub fn ensure_order_url_matches_directory(
    directory_url: &str,
    order_url: &str,
) -> Result<(), AcmeIssuanceError> {
    let directory = Url::parse(directory_url).map_err(|source| AcmeIssuanceError::Operation {
        operation: "acme_directory_url",
        message: source.to_string(),
    })?;
    let order = Url::parse(order_url).map_err(|source| AcmeIssuanceError::InvalidOrderUrl {
        order_url: order_url.to_string(),
        message: source.to_string(),
    })?;
    let same_origin = directory.scheme() == order.scheme()
        && directory.host_str() == order.host_str()
        && directory.port_or_known_default() == order.port_or_known_default();
    if same_origin {
        return Ok(());
    }
    Err(AcmeIssuanceError::OrderUrlOriginMismatch {
        directory_url: directory.to_string(),
        order_url: order.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_url_origin_must_match_directory() {
        let directory = "https://acme-v02.api.letsencrypt.org/directory";
        ensure_order_url_matches_directory(
            directory,
            "https://acme-v02.api.letsencrypt.org/acme/order/123/456",
        )
        .expect("matching origin should be accepted");

        let mismatched_host = ensure_order_url_matches_directory(
            directory,
            "https://attacker.example/acme/order/123/456",
        )
        .expect_err("mismatched host should be rejected");
        assert!(matches!(
            mismatched_host,
            AcmeIssuanceError::OrderUrlOriginMismatch { .. }
        ));

        let mismatched_scheme = ensure_order_url_matches_directory(
            directory,
            "http://acme-v02.api.letsencrypt.org/acme/order/123/456",
        )
        .expect_err("mismatched scheme should be rejected");
        assert!(matches!(
            mismatched_scheme,
            AcmeIssuanceError::OrderUrlOriginMismatch { .. }
        ));

        let bad_order = ensure_order_url_matches_directory(directory, "not a url")
            .expect_err("unparseable order URL should be rejected");
        assert!(matches!(
            bad_order,
            AcmeIssuanceError::InvalidOrderUrl { .. }
        ));
    }

    #[test]
    fn contact_email_is_encoded_as_mailto_uri() {
        assert_eq!(contact_uris(None), Vec::<String>::new());
        assert_eq!(
            contact_uris(Some("ops@example.test")),
            vec!["mailto:ops@example.test".to_string()]
        );
        assert_eq!(
            contact_uris(Some("mailto:ops@example.test")),
            vec!["mailto:ops@example.test".to_string()]
        );
    }

    #[test]
    fn order_url_parser_rejects_non_urls() {
        let error = AcmeOrderUrl::parse("not a url").expect_err("bad URL rejected");
        assert!(matches!(error, AcmeIssuanceError::InvalidOrderUrl { .. }));
    }
}
