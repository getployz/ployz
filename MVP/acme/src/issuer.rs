use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;

use async_trait::async_trait;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
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

pub struct InstantAcmeIssuer {
    config: AcmeIssuerConfig,
    account_coordinator: Box<dyn AcmeAccountCoordinator>,
}

impl InstantAcmeIssuer {
    #[must_use]
    pub fn new(config: AcmeIssuerConfig) -> Self {
        Self::with_account_coordinator(config, Box::new(NoopAcmeAccountCoordinator))
    }

    #[must_use]
    pub fn with_account_coordinator(
        config: AcmeIssuerConfig,
        account_coordinator: Box<dyn AcmeAccountCoordinator>,
    ) -> Self {
        Self {
            config,
            account_coordinator,
        }
    }
}

#[async_trait]
impl AcmeIssuer for InstantAcmeIssuer {
    async fn start_order(
        &self,
        account_store: &mut dyn AcmeAccountStore,
        challenge_publisher: &mut dyn AcmeHttp01ChallengePublisher,
        hostname: AcmeHostname,
        now_secs: u64,
    ) -> Result<StartedAcmeOrder, AcmeIssuanceError> {
        let account = load_or_create_account(
            account_store,
            &self.config,
            self.account_coordinator.as_ref(),
            now_secs,
        )
        .await?;
        let identifiers = [Identifier::Dns(hostname.as_str().to_string())];
        let mut order = account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(acme_operation("new_order"))?;
        let order_url = AcmeOrderUrl::parse(order.url().to_string())?;
        let mut published = Vec::new();
        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authorization = result.map_err(acme_operation("authorization"))?;
            match authorization.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Invalid
                | AuthorizationStatus::Revoked
                | AuthorizationStatus::Expired
                | AuthorizationStatus::Deactivated => {
                    return Err(AcmeIssuanceError::AuthorizationUnexpectedStatus {
                        hostname,
                        status: acme_authorization_status(authorization.status),
                    });
                }
            }
            let challenge = authorization
                .challenge(ChallengeType::Http01)
                .ok_or_else(|| AcmeIssuanceError::Http01ChallengeMissing {
                    hostname: hostname.clone(),
                })?;
            let token = AcmeChallengeToken::parse(challenge.token.clone()).map_err(|source| {
                AcmeIssuanceError::Operation {
                    operation: "http01_token",
                    message: source.to_string(),
                }
            })?;
            let key_authorization = AcmeKeyAuthorization::parse_for_token(
                &token,
                challenge.key_authorization().as_str(),
            )
            .map_err(|source| AcmeIssuanceError::Operation {
                operation: "http01_key_authorization",
                message: source.to_string(),
            })?;
            let challenge = AcmeHttp01OrderChallenge {
                hostname: hostname.clone(),
                token,
                key_authorization,
                expires_at_secs: now_secs + DEFAULT_HTTP01_CHALLENGE_TTL_SECS,
            };
            challenge_publisher
                .publish_http01(challenge.clone())
                .await?;
            published.push(challenge);
        }
        Ok(StartedAcmeOrder {
            hostname,
            order_url,
            challenges: published,
        })
    }

    async fn finalize_order(
        &self,
        account_store: &mut dyn AcmeAccountStore,
        readiness: &dyn AcmeHttp01ChallengeReadiness,
        challenge_publisher: &mut dyn AcmeHttp01ChallengePublisher,
        hostname: AcmeHostname,
        order_url: AcmeOrderUrl,
        now_secs: u64,
    ) -> Result<IssuedCertificate, AcmeIssuanceError> {
        order_url.ensure_same_origin_as_directory(&self.config.directory_url)?;
        let account = load_or_create_account(
            account_store,
            &self.config,
            self.account_coordinator.as_ref(),
            now_secs,
        )
        .await?;
        let mut order = account
            .order(order_url.as_str().to_string())
            .await
            .map_err(acme_operation("resume_order"))?;
        let mut order_tokens = Vec::new();
        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authorization = result.map_err(acme_operation("authorization"))?;
            let status = authorization.status;
            let mut challenge =
                authorization
                    .challenge(ChallengeType::Http01)
                    .ok_or_else(|| AcmeIssuanceError::Http01ChallengeMissing {
                        hostname: hostname.clone(),
                    })?;
            let token = AcmeChallengeToken::parse(challenge.token.clone()).map_err(|source| {
                AcmeIssuanceError::Operation {
                    operation: "http01_token",
                    message: source.to_string(),
                }
            })?;
            order_tokens.push(token.clone());
            match status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Invalid
                | AuthorizationStatus::Revoked
                | AuthorizationStatus::Expired
                | AuthorizationStatus::Deactivated => {
                    return Err(AcmeIssuanceError::AuthorizationUnexpectedStatus {
                        hostname,
                        status: acme_authorization_status(status),
                    });
                }
            }
            readiness.wait_ready(&hostname, &token).await?;
            challenge
                .set_ready()
                .await
                .map_err(acme_operation("set_ready"))?;
        }
        drop(authorizations);

        let status = order
            .poll_ready(&RetryPolicy::default())
            .await
            .map_err(acme_operation("poll_ready"))?;
        if status != OrderStatus::Ready {
            return Err(AcmeIssuanceError::OrderUnexpectedStatus {
                hostname,
                status: acme_order_status(status),
            });
        }

        let private_key_pem = order.finalize().await.map_err(acme_operation("finalize"))?;
        let fullchain_pem = order
            .poll_certificate(&RetryPolicy::default())
            .await
            .map_err(acme_operation("poll_certificate"))?;
        for token in &order_tokens {
            challenge_publisher.clear_http01(&hostname, token).await?;
        }
        Ok(IssuedCertificate {
            hostname,
            order_url,
            fullchain_pem,
            private_key_pem,
            issued_at_secs: now_secs,
            not_before_secs: None,
            not_after_secs: None,
        })
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

async fn load_or_create_account(
    account_store: &mut dyn AcmeAccountStore,
    config: &AcmeIssuerConfig,
    coordinator: &dyn AcmeAccountCoordinator,
    now_secs: u64,
) -> Result<Account, AcmeIssuanceError> {
    if let Some(record) = account_store.load_account(&config.directory_url).await? {
        return account_from_record(config, &record).await;
    }
    let hold = match coordinator.try_acquire_account(&config.directory_url).await {
        AccountAcquisition::Allowed(hold) => hold,
        AccountAcquisition::VetoedByPeer(message) => {
            return Err(AcmeIssuanceError::AccountCreationDeferred {
                directory_url: config.directory_url.clone(),
                message,
            });
        }
        AccountAcquisition::CoordinationFailed(message) => {
            return Err(AcmeIssuanceError::AccountLockFailed {
                directory_url: config.directory_url.clone(),
                message,
            });
        }
    };
    let result = load_or_create_account_under_lock(account_store, config, now_secs).await;
    hold.release().await;
    result
}

async fn load_or_create_account_under_lock(
    account_store: &mut dyn AcmeAccountStore,
    config: &AcmeIssuerConfig,
    now_secs: u64,
) -> Result<Account, AcmeIssuanceError> {
    if let Some(record) = account_store.load_account(&config.directory_url).await? {
        return account_from_record(config, &record).await;
    }
    let contact = contact_uris(config.contact_email.as_deref());
    let contact_refs = contact.iter().map(String::as_str).collect::<Vec<_>>();
    let (account, credentials) = account_builder(config)?
        .create(
            &NewAccount {
                contact: &contact_refs,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            config.directory_url.clone(),
            None,
        )
        .await
        .map_err(acme_operation("account_create"))?;
    account_store
        .save_account(AcmeAccountRecord {
            account_id: AcmeAccountId::for_directory_url(&config.directory_url),
            directory_url: config.directory_url.clone(),
            contact_email: config.contact_email.clone(),
            account_credentials_json: serde_json::to_string(&credentials).map_err(|source| {
                AcmeIssuanceError::Operation {
                    operation: "acme_account_encode",
                    message: source.to_string(),
                }
            })?,
            created_at_secs: now_secs,
            updated_at_secs: now_secs,
        })
        .await?;
    Ok(account)
}

async fn account_from_record(
    config: &AcmeIssuerConfig,
    record: &AcmeAccountRecord,
) -> Result<Account, AcmeIssuanceError> {
    let credentials: AccountCredentials = serde_json::from_str(&record.account_credentials_json)
        .map_err(|source| AcmeIssuanceError::InvalidAccountCredentials {
            directory_url: config.directory_url.clone(),
            message: source.to_string(),
        })?;
    account_builder(config)?
        .from_credentials(credentials)
        .await
        .map_err(acme_operation("account_from_credentials"))
}

fn account_builder(
    config: &AcmeIssuerConfig,
) -> Result<instant_acme::AccountBuilder, AcmeIssuanceError> {
    match &config.root_ca_path {
        Some(path) => Account::builder_with_root(path).map_err(acme_operation("account_builder")),
        None => Account::builder().map_err(acme_operation("account_builder")),
    }
}

fn acme_operation(
    operation: &'static str,
) -> impl FnOnce(instant_acme::Error) -> AcmeIssuanceError + Send + Sync + 'static {
    move |source| AcmeIssuanceError::Operation {
        operation,
        message: source.to_string(),
    }
}

fn acme_authorization_status(status: AuthorizationStatus) -> AcmeAuthorizationStatus {
    match status {
        AuthorizationStatus::Pending => AcmeAuthorizationStatus::Pending,
        AuthorizationStatus::Valid => AcmeAuthorizationStatus::Valid,
        AuthorizationStatus::Invalid => AcmeAuthorizationStatus::Invalid,
        AuthorizationStatus::Revoked => AcmeAuthorizationStatus::Revoked,
        AuthorizationStatus::Expired => AcmeAuthorizationStatus::Expired,
        AuthorizationStatus::Deactivated => AcmeAuthorizationStatus::Deactivated,
    }
}

fn acme_order_status(status: OrderStatus) -> AcmeOrderStatus {
    match status {
        OrderStatus::Pending => AcmeOrderStatus::Pending,
        OrderStatus::Ready => AcmeOrderStatus::Ready,
        OrderStatus::Processing => AcmeOrderStatus::Processing,
        OrderStatus::Valid => AcmeOrderStatus::Valid,
        OrderStatus::Invalid => AcmeOrderStatus::Invalid,
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

    struct VetoAccountCoordinator;

    #[async_trait]
    impl AcmeAccountCoordinator for VetoAccountCoordinator {
        async fn try_acquire_account(&self, directory_url: &str) -> AccountAcquisition {
            AccountAcquisition::VetoedByPeer(format!("peer holds {directory_url}"))
        }
    }

    struct FailingAccountCoordinator;

    #[async_trait]
    impl AcmeAccountCoordinator for FailingAccountCoordinator {
        async fn try_acquire_account(&self, directory_url: &str) -> AccountAcquisition {
            AccountAcquisition::CoordinationFailed(format!(
                "lock backend failed for {directory_url}"
            ))
        }
    }

    #[derive(Default)]
    struct MemoryAccountStore {
        record: Option<AcmeAccountRecord>,
    }

    #[async_trait]
    impl AcmeAccountStore for MemoryAccountStore {
        async fn load_account(
            &self,
            _directory_url: &str,
        ) -> Result<Option<AcmeAccountRecord>, AcmeIssuanceError> {
            Ok(self.record.clone())
        }

        async fn save_account(
            &mut self,
            record: AcmeAccountRecord,
        ) -> Result<(), AcmeIssuanceError> {
            self.record = Some(record);
            Ok(())
        }
    }

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

    #[tokio::test]
    async fn account_creation_respects_directory_scoped_coordination_veto() {
        let config = config_with_directory("https://acme.test/dir");
        let mut store = MemoryAccountStore::default();
        let error =
            match load_or_create_account(&mut store, &config, &VetoAccountCoordinator, 1).await {
                Ok(_) => panic!("coordination veto should stop before ACME create"),
                Err(error) => error,
            };

        assert!(matches!(
            error,
            AcmeIssuanceError::AccountCreationDeferred { .. }
        ));
    }

    #[tokio::test]
    async fn account_creation_surfaces_coordination_backend_failure() {
        let config = config_with_directory("https://acme.test/dir");
        let mut store = MemoryAccountStore::default();
        let error = match load_or_create_account(&mut store, &config, &FailingAccountCoordinator, 1)
            .await
        {
            Ok(_) => panic!("coordination backend failure should stop before ACME create"),
            Err(error) => error,
        };

        assert!(matches!(error, AcmeIssuanceError::AccountLockFailed { .. }));
    }

    #[tokio::test]
    async fn account_from_record_rejects_malformed_credentials_json() {
        let config = config_with_directory("https://acme.test/dir");
        let record = AcmeAccountRecord {
            account_id: AcmeAccountId::for_directory_url(&config.directory_url),
            directory_url: config.directory_url.clone(),
            contact_email: None,
            account_credentials_json: "{not json".to_string(),
            created_at_secs: 1,
            updated_at_secs: 1,
        };

        let error = match account_from_record(&config, &record).await {
            Ok(_) => panic!("malformed credentials should fail before account rehydrate"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            AcmeIssuanceError::InvalidAccountCredentials { .. }
        ));
    }

    fn config_with_directory(directory_url: &str) -> AcmeIssuerConfig {
        AcmeIssuerConfig {
            directory_url: directory_url.to_string(),
            contact_email: None,
            root_ca_path: None,
        }
    }
}
