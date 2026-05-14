use async_trait::async_trait;
use ployz_error::{CertificateError, Result};
use ployz_store_api::StoreDriver;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const CHALLENGE_TTL_SECS: u64 = 15 * 60;
pub const HTTP01_GATEWAY_SNAPSHOT_SETTLE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone)]
pub struct CertificateManagerConfig {
    pub issuer_url: String,
    pub contact_email: Option<String>,
    pub root_ca_path: Option<PathBuf>,
}

impl Default for CertificateManagerConfig {
    fn default() -> Self {
        Self {
            issuer_url: DEFAULT_ACME_DIRECTORY_URL.to_string(),
            contact_email: None,
            root_ca_path: None,
        }
    }
}

impl CertificateManagerConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let issuer_url = std::env::var("PLOYZ_ACME_DIRECTORY_URL")
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
            issuer_url,
            contact_email,
            root_ca_path,
        }
    }
}

#[must_use]
pub fn account_id_for_issuer_url(issuer_url: &str) -> String {
    issuer_url.to_string()
}

#[derive(Debug, Clone)]
pub struct StartedOrder {
    pub order_url: String,
}

#[derive(Debug, Clone)]
pub struct IssuedCertificate {
    pub fullchain_pem: String,
    pub private_key_pem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CertificateIssuerKind {
    Acme,
    Static,
    Imported,
    Other(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateIssuerInfo {
    pub id: String,
    pub kind: CertificateIssuerKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateIssueRequest {
    pub hostname: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateRenewRequest {
    pub hostname: String,
    pub existing_version_id: Option<String>,
}

#[async_trait]
pub trait CertificateIssuer: Send + Sync {
    fn info(&self) -> CertificateIssuerInfo;

    async fn issue(
        &self,
        store: &StoreDriver,
        request: CertificateIssueRequest,
    ) -> Result<IssuedCertificate>;

    async fn renew(
        &self,
        store: &StoreDriver,
        request: CertificateRenewRequest,
    ) -> Result<IssuedCertificate>;
}

pub struct NoopCertificateIssuer {
    info: CertificateIssuerInfo,
}

impl NoopCertificateIssuer {
    #[must_use]
    pub fn new(id: impl Into<String>, kind: CertificateIssuerKind) -> Self {
        Self {
            info: CertificateIssuerInfo {
                id: id.into(),
                kind,
            },
        }
    }
}

impl Default for NoopCertificateIssuer {
    fn default() -> Self {
        Self::new("disabled", CertificateIssuerKind::Other("disabled".into()))
    }
}

#[async_trait]
impl CertificateIssuer for NoopCertificateIssuer {
    fn info(&self) -> CertificateIssuerInfo {
        self.info.clone()
    }

    async fn issue(
        &self,
        _store: &StoreDriver,
        _request: CertificateIssueRequest,
    ) -> Result<IssuedCertificate> {
        Err(CertificateError::IssuerDisabled {
            issuer: self.info.id.clone(),
        }
        .into())
    }

    async fn renew(
        &self,
        _store: &StoreDriver,
        _request: CertificateRenewRequest,
    ) -> Result<IssuedCertificate> {
        Err(CertificateError::IssuerDisabled {
            issuer: self.info.id.clone(),
        }
        .into())
    }
}

#[async_trait]
pub trait AcmeIssuer: Send + Sync {
    async fn start_order(&self, store: &StoreDriver, hostname: &str) -> Result<StartedOrder>;
    async fn finalize_order(
        &self,
        store: &StoreDriver,
        hostname: &str,
        order_url: &str,
    ) -> Result<IssuedCertificate>;
}

#[async_trait]
pub trait Http01ChallengeReadiness: Send + Sync {
    async fn wait_ready(&self, store: &StoreDriver, hostname: &str, token: &str) -> Result<()>;
}

#[async_trait]
pub trait AcmeAccountCoordinator: Send + Sync {
    async fn try_acquire_account(&self, issuer_url: &str) -> AccountAcquisition;
}

pub enum AccountAcquisition {
    Allowed(IssuanceHold),
    VetoedByPeer(String),
    CoordinationFailed(String),
}

pub struct NoopAcmeAccountCoordinator;

#[async_trait]
impl AcmeAccountCoordinator for NoopAcmeAccountCoordinator {
    async fn try_acquire_account(&self, _issuer_url: &str) -> AccountAcquisition {
        AccountAcquisition::Allowed(IssuanceHold::noop())
    }
}

pub struct NoopHttp01ChallengeReadiness;

#[async_trait]
impl Http01ChallengeReadiness for NoopHttp01ChallengeReadiness {
    async fn wait_ready(&self, store: &StoreDriver, hostname: &str, token: &str) -> Result<()> {
        let _ = (store, hostname, token);
        Ok(())
    }
}

#[async_trait]
pub trait IssuanceCoordinator: Send + Sync {
    async fn try_acquire(&self, hostname: &str) -> IssuanceAcquisition;
}

pub enum IssuanceAcquisition {
    Allowed(IssuanceHold),
    VetoedByPeer(String),
    CoordinationFailed(String),
}

pub struct IssuanceHold {
    #[allow(clippy::type_complexity)]
    releaser: Option<
        Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>,
    >,
}

impl IssuanceHold {
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

impl Drop for IssuanceHold {
    fn drop(&mut self) {
        if let Some(releaser) = self.releaser.take() {
            tracing::warn!(
                "ACME issuance/account hold dropped without explicit release; releasing asynchronously"
            );
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(releaser());
                }
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        "ACME issuance/account hold could not release because no Tokio runtime is active"
                    );
                }
            }
        }
    }
}

pub struct NoopIssuanceCoordinator;

#[async_trait]
impl IssuanceCoordinator for NoopIssuanceCoordinator {
    async fn try_acquire(&self, _hostname: &str) -> IssuanceAcquisition {
        IssuanceAcquisition::Allowed(IssuanceHold::noop())
    }
}

pub trait AcmeIssuerFactory: Send + Sync {
    fn issuer_url(&self) -> &str;
    fn create(
        &self,
        readiness: Arc<dyn Http01ChallengeReadiness>,
        account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    ) -> Arc<dyn AcmeIssuer>;
}

pub struct NoopAcmeIssuer;

#[async_trait]
impl AcmeIssuer for NoopAcmeIssuer {
    async fn start_order(&self, _store: &StoreDriver, _hostname: &str) -> Result<StartedOrder> {
        Err(CertificateError::AcmeDisabled.into())
    }

    async fn finalize_order(
        &self,
        _store: &StoreDriver,
        _hostname: &str,
        _order_url: &str,
    ) -> Result<IssuedCertificate> {
        Err(CertificateError::AcmeDisabled.into())
    }
}

pub struct NoopAcmeIssuerFactory {
    issuer_url: String,
}

impl NoopAcmeIssuerFactory {
    #[must_use]
    pub fn new(issuer_url: impl Into<String>) -> Self {
        Self {
            issuer_url: issuer_url.into(),
        }
    }
}

impl Default for NoopAcmeIssuerFactory {
    fn default() -> Self {
        Self::new(DEFAULT_ACME_DIRECTORY_URL)
    }
}

impl AcmeIssuerFactory for NoopAcmeIssuerFactory {
    fn issuer_url(&self) -> &str {
        &self.issuer_url
    }

    fn create(
        &self,
        _readiness: Arc<dyn Http01ChallengeReadiness>,
        _account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    ) -> Arc<dyn AcmeIssuer> {
        Arc::new(NoopAcmeIssuer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generic_issuer_info_does_not_expose_acme_terms() {
        let issuer = NoopCertificateIssuer::new("static-dev", CertificateIssuerKind::Static);

        assert_eq!(
            issuer.info(),
            CertificateIssuerInfo {
                id: "static-dev".to_string(),
                kind: CertificateIssuerKind::Static,
            }
        );
    }

    #[test]
    fn disabled_generic_issuer_error_is_not_acme_specific() {
        let error = CertificateError::IssuerDisabled {
            issuer: "disabled".to_string(),
        };

        assert_eq!(
            error.to_string(),
            "certificate issuer 'disabled' is disabled"
        );
    }
}
