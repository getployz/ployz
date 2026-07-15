use ployz_core::ids::CertId;
use ployz_core::ingress::{ActiveCertificateMetadata, CertificateOwner};
use rusqlite::{OptionalExtension, params};

use crate::control::intent::ingress_intent::ActiveCertificateMetadataStore;
use crate::control::store::{CoreStore, CoreStoreError};

#[derive(Debug, Clone)]
pub struct CertificateIntentStore {
    store: CoreStore,
    active: ActiveCertificateMetadataStore,
}

impl CertificateIntentStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self {
            active: ActiveCertificateMetadataStore::new(store.clone()),
            store,
        }
    }

    pub async fn active_for_owner(
        &self,
        owner: &CertificateOwner,
    ) -> Result<Option<ActiveCertificateMetadata>, CertificateIntentStoreError> {
        self.active
            .active_for_owner(owner)
            .await
            .map_err(store_error)
    }

    pub async fn active_for_cert_id(
        &self,
        cert_id: &CertId,
    ) -> Result<Option<ActiveCertificateMetadata>, CertificateIntentStoreError> {
        let active = self
            .active
            .active_certificates()
            .await
            .map_err(store_error)?;
        Ok(active
            .into_iter()
            .find(|metadata| metadata.active.cert_id == *cert_id))
    }

    pub async fn active_certificates(
        &self,
    ) -> Result<Vec<ActiveCertificateMetadata>, CertificateIntentStoreError> {
        self.active.active_certificates().await.map_err(store_error)
    }

    pub async fn account_credentials(
        &self,
        directory_url: &str,
    ) -> Result<Option<String>, CertificateIntentStoreError> {
        let directory_url = directory_url.to_owned();
        self.store
            .call(move |conn| {
                conn.query_row(
                    "SELECT credentials_json FROM acme_accounts WHERE directory_url = ?1",
                    [directory_url],
                    |row| row.get(0),
                )
                .optional()
            })
            .await
            .map_err(store_error)
    }

    pub async fn store_account_credentials(
        &self,
        directory_url: String,
        credentials_json: String,
    ) -> Result<(), CertificateIntentStoreError> {
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO acme_accounts (directory_url, credentials_json) VALUES (?1, ?2)
                     ON CONFLICT(directory_url) DO UPDATE SET credentials_json = excluded.credentials_json",
                    params![directory_url, credentials_json],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CertificateIntentStoreError {
    #[error("certificate intent store: {message}")]
    Store { message: String },
}

fn store_error(error: CoreStoreError) -> CertificateIntentStoreError {
    CertificateIntentStoreError::Store {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::certificate::{
        ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow,
    };
    use ployz_core::ids::RouteBindingId;
    use ployz_test_support::ids::{cert_id, route_hostname};

    use super::*;

    #[tokio::test]
    async fn active_metadata_round_trips_without_material_configuration() {
        let core = CoreStore::open_in_memory().await.expect("core store");
        let store = CertificateIntentStore::new(core.clone());
        let metadata = ActiveCertificateMetadata {
            owner: CertificateOwner::RouteBinding {
                route_binding_id: RouteBindingId::try_new("route_app").expect("route id"),
            },
            active: active_certificate(),
        };
        ActiveCertificateMetadataStore::new(core)
            .replace(metadata.clone())
            .await
            .expect("store metadata");

        assert_eq!(
            store
                .active_for_owner(&metadata.owner)
                .await
                .expect("load metadata"),
            Some(metadata)
        );
    }

    fn active_certificate() -> ActiveCertState {
        ActiveCertState {
            cert_id: cert_id("cert_app_example_com"),
            hostname: route_hostname("app.example.com"),
            bundle_ref: CertBundleRef::try_new(format!(
                "sha256:{}:/var/lib/ployz/certificates/cert_app_example_com.bundle",
                "a".repeat(64)
            ))
            .expect("bundle ref"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(1).expect("not before"),
                CertValidAt::try_new(2).expect("not after"),
            )
            .expect("validity"),
        }
    }
}
