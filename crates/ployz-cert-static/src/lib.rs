use async_trait::async_trait;
use ployz_cert_api::{
    CertificateIssueRequest, CertificateIssuer, CertificateIssuerInfo, CertificateIssuerKind,
    CertificateRenewRequest, IssuedCertificate,
};
use ployz_error::Result;
use ployz_store_api::StoreDriver;

#[derive(Debug, Clone)]
pub struct StaticCertificateIssuer {
    info: CertificateIssuerInfo,
    material: IssuedCertificate,
}

impl StaticCertificateIssuer {
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        fullchain_pem: impl Into<String>,
        private_key_pem: impl Into<String>,
    ) -> Self {
        Self {
            info: CertificateIssuerInfo {
                id: id.into(),
                kind: CertificateIssuerKind::Static,
            },
            material: IssuedCertificate {
                fullchain_pem: fullchain_pem.into(),
                private_key_pem: private_key_pem.into(),
            },
        }
    }

    #[must_use]
    pub fn material(&self) -> IssuedCertificate {
        self.material.clone()
    }
}

#[async_trait]
impl CertificateIssuer for StaticCertificateIssuer {
    fn info(&self) -> CertificateIssuerInfo {
        self.info.clone()
    }

    async fn issue(
        &self,
        _store: &StoreDriver,
        _request: CertificateIssueRequest,
    ) -> Result<IssuedCertificate> {
        Ok(self.material())
    }

    async fn renew(
        &self,
        _store: &StoreDriver,
        _request: CertificateRenewRequest,
    ) -> Result<IssuedCertificate> {
        Ok(self.material())
    }
}

#[cfg(test)]
mod tests {
    use ployz_cert_api::CertificateIssuer;
    use ployz_store_api::StoreDriver;
    use ployz_store_memory::StoreDriverMemoryExt;

    use super::*;

    #[test]
    fn exposes_static_issuer_info() {
        let issuer = StaticCertificateIssuer::new("dev", "chain", "key");

        assert_eq!(
            issuer.info(),
            CertificateIssuerInfo {
                id: "dev".to_string(),
                kind: CertificateIssuerKind::Static,
            }
        );
    }

    #[tokio::test]
    async fn issue_returns_configured_material() {
        let store = StoreDriver::memory();
        let issuer = StaticCertificateIssuer::new("dev", "chain", "key");

        let issued = issuer
            .issue(
                &store,
                CertificateIssueRequest {
                    hostname: "example.test".to_string(),
                },
            )
            .await
            .expect("issue static certificate");

        assert_eq!(issued.fullchain_pem, "chain");
        assert_eq!(issued.private_key_pem, "key");
    }
}
