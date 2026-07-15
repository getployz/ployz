use ployz_core::ingress::{ActiveCertificateMetadata, CertificateOwner};
use rusqlite::params;

use crate::core_store::{CoreStore, CoreStoreError, query_json, query_json_list, to_json};

#[derive(Debug, Clone)]
pub struct ActiveCertificateMetadataStore {
    store: CoreStore,
}

impl ActiveCertificateMetadataStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub async fn active_for_owner(
        &self,
        owner: &CertificateOwner,
    ) -> Result<Option<ActiveCertificateMetadata>, CoreStoreError> {
        let owner_key = certificate_owner_key(owner);
        self.store
            .call(move |conn| {
                query_json::<ActiveCertificateMetadata>(
                    conn,
                    "SELECT json FROM active_certificate_metadata WHERE owner_key = ?1",
                    &owner_key,
                )
            })
            .await
    }

    pub async fn active_certificates(
        &self,
    ) -> Result<Vec<ActiveCertificateMetadata>, CoreStoreError> {
        self.store
            .call(|conn| {
                query_json_list(
                    conn,
                    "SELECT json FROM active_certificate_metadata ORDER BY owner_key",
                )
            })
            .await
    }

    pub async fn replace(
        &self,
        certificate: ActiveCertificateMetadata,
    ) -> Result<(), CoreStoreError> {
        let owner_key = certificate_owner_key(&certificate.owner);
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO active_certificate_metadata (owner_key, json) VALUES (?1, ?2)
                     ON CONFLICT(owner_key) DO UPDATE SET json = excluded.json",
                    params![owner_key, to_json(&certificate)?],
                )?;
                Ok(())
            })
            .await
    }

    pub async fn remove(&self, owner: &CertificateOwner) -> Result<(), CoreStoreError> {
        let owner_key = certificate_owner_key(owner);
        self.store
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM active_certificate_metadata WHERE owner_key = ?1",
                    [owner_key],
                )?;
                Ok(())
            })
            .await
    }
}

fn certificate_owner_key(owner: &CertificateOwner) -> String {
    match owner {
        CertificateOwner::PloyzAutomaticNamespace => "ployz-automatic-namespace".to_owned(),
        CertificateOwner::RouteBinding { route_binding_id } => {
            format!("route-binding:{}", route_binding_id.as_str())
        }
    }
}

pub(crate) fn upsert_active_certificate_metadata(
    transaction: &rusqlite::Transaction<'_>,
    certificate: &ActiveCertificateMetadata,
) -> Result<(), rusqlite::Error> {
    let owner_key = certificate_owner_key(&certificate.owner);
    transaction.execute(
        "INSERT INTO active_certificate_metadata (owner_key, json) VALUES (?1, ?2)
         ON CONFLICT(owner_key) DO UPDATE SET json = excluded.json",
        params![owner_key, to_json(certificate)?],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::cert::{CertBundleRef, CertValidAt, CertValidityWindow};
    use ployz_core::ids::RouteBindingId;
    use ployz_test_support::ids::{cert_id, route_hostname};

    fn active_certificate_metadata() -> ActiveCertificateMetadata {
        ActiveCertificateMetadata {
            owner: CertificateOwner::RouteBinding {
                route_binding_id: RouteBindingId::try_new("route_1").expect("route binding id"),
            },
            active: ployz_core::cert::ActiveCertState {
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
            },
        }
    }

    #[test]
    fn certificate_owner_keys_keep_namespace_and_route_owners_distinct() {
        let route = CertificateOwner::RouteBinding {
            route_binding_id: RouteBindingId::try_new("route_1").expect("route binding id"),
        };

        assert_ne!(
            certificate_owner_key(&CertificateOwner::PloyzAutomaticNamespace),
            certificate_owner_key(&route)
        );
    }

    #[tokio::test]
    async fn active_certificate_metadata_round_trips_with_its_owner() {
        let store =
            ActiveCertificateMetadataStore::new(CoreStore::open_in_memory().await.expect("store"));
        let metadata = active_certificate_metadata();

        store
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
}
