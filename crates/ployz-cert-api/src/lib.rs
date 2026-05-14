use async_trait::async_trait;
use ployz_error::{CertificateError, Result};
use ployz_store_api::StoreDriver;
use serde::{Deserialize, Serialize};

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
