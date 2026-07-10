use ployz_core::cert::{AcmeHttp01Challenge, ActiveCertState};
use ployz_core::ids::CertId;
use ployz_core::ops::RouteHostname;
use rusqlite::{OptionalExtension, Transaction, params};

use crate::core_store::{CoreStore, CoreStoreError, query_json, query_json_list, to_json};

#[derive(Debug, Clone)]
pub struct CertificateIntentStore {
    store: CoreStore,
}

impl CertificateIntentStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub async fn active_for_hostname(
        &self,
        hostname: &RouteHostname,
    ) -> Result<Option<ActiveCertState>, CertificateIntentStoreError> {
        let hostname = hostname.as_str().to_owned();
        self.store
            .call(move |conn| {
                query_json(
                    conn,
                    "SELECT json FROM custom_certificate_intent WHERE hostname = ?1",
                    &hostname,
                )
            })
            .await
            .map_err(store_error)
    }

    pub async fn active_for_cert_id(
        &self,
        cert_id: &CertId,
    ) -> Result<Option<ActiveCertState>, CertificateIntentStoreError> {
        let cert_id = cert_id.clone();
        self.store
            .call(move |conn| {
                let active_certificates: Vec<ActiveCertState> = query_json_list(
                    conn,
                    "SELECT json FROM custom_certificate_intent ORDER BY hostname",
                )?;
                Ok(active_certificates
                    .into_iter()
                    .find(|active| active.cert_id == cert_id))
            })
            .await
            .map_err(store_error)
    }

    pub async fn active_certificates(
        &self,
    ) -> Result<Vec<ActiveCertState>, CertificateIntentStoreError> {
        self.store
            .call(|conn| {
                query_json_list(
                    conn,
                    "SELECT json FROM custom_certificate_intent ORDER BY hostname",
                )
            })
            .await
            .map_err(store_error)
    }

    pub(crate) async fn seed_active_metadata(
        &self,
        active_cert: ActiveCertState,
    ) -> Result<(), CertificateIntentStoreError> {
        self.store
            .call(move |conn| {
                let transaction = conn.transaction()?;
                upsert_active_metadata(&transaction, &active_cert)?;
                transaction.commit()?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn store_challenge(
        &self,
        challenge: AcmeHttp01Challenge,
    ) -> Result<(), CertificateIntentStoreError> {
        self.store
            .call(move |conn| {
                conn.execute(
                    "INSERT INTO acme_http01_challenges (hostname, token, json)
                     VALUES (?1, ?2, ?3)
                     ON CONFLICT(hostname, token) DO UPDATE SET json = excluded.json",
                    params![
                        challenge.hostname().as_str(),
                        challenge.token().as_str(),
                        to_json(&challenge)?,
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn challenges(
        &self,
    ) -> Result<Vec<AcmeHttp01Challenge>, CertificateIntentStoreError> {
        self.store
            .call(|conn| {
                query_json_list(
                    conn,
                    "SELECT json FROM acme_http01_challenges ORDER BY hostname, token",
                )
            })
            .await
            .map_err(store_error)
    }

    pub async fn remove_challenges_for_hostname(
        &self,
        hostname: &RouteHostname,
    ) -> Result<(), CertificateIntentStoreError> {
        let hostname = hostname.as_str().to_owned();
        self.store
            .call(move |conn| {
                conn.execute(
                    "DELETE FROM acme_http01_challenges WHERE hostname = ?1",
                    [hostname],
                )?;
                Ok(())
            })
            .await
            .map_err(store_error)
    }

    pub async fn remove_all_challenges(&self) -> Result<(), CertificateIntentStoreError> {
        self.store
            .call(|conn| {
                conn.execute("DELETE FROM acme_http01_challenges", [])?;
                Ok(())
            })
            .await
            .map_err(store_error)
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

pub(crate) fn upsert_active_metadata(
    transaction: &Transaction<'_>,
    active_cert: &ActiveCertState,
) -> Result<(), rusqlite::Error> {
    transaction.execute(
        "INSERT INTO custom_certificate_intent (hostname, json) VALUES (?1, ?2)
         ON CONFLICT(hostname) DO UPDATE SET json = excluded.json",
        params![active_cert.hostname.as_str(), to_json(active_cert)?],
    )?;
    Ok(())
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
    use ployz_core::cert::{
        AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
        CertBundleRef, CertValidAt, CertValidityWindow,
    };
    use ployz_test_support::ids::{cert_id, route_hostname};

    use super::*;

    #[tokio::test]
    async fn active_metadata_round_trips_without_material_configuration() {
        let store =
            CertificateIntentStore::new(CoreStore::open_in_memory().await.expect("core store"));
        let active = active_certificate();
        store
            .seed_active_metadata(active.clone())
            .await
            .expect("store metadata");

        assert_eq!(
            store
                .active_for_hostname(&active.hostname)
                .await
                .expect("load metadata"),
            Some(active)
        );
    }

    #[tokio::test]
    async fn challenge_removal_clears_the_published_challenge() {
        let store =
            CertificateIntentStore::new(CoreStore::open_in_memory().await.expect("core store"));
        let hostname = route_hostname("app.example.com");
        store
            .store_challenge(challenge(hostname.clone()))
            .await
            .expect("publish challenge");

        store
            .remove_challenges_for_hostname(&hostname)
            .await
            .expect("remove challenge");

        assert!(
            store
                .challenges()
                .await
                .expect("list challenges")
                .is_empty()
        );
    }

    fn challenge(hostname: RouteHostname) -> AcmeHttp01Challenge {
        AcmeHttp01Challenge::try_new(
            hostname,
            AcmeChallengeToken::try_new("token").expect("token"),
            AcmeChallengeValue::try_new("token.account-thumbprint").expect("value"),
            AcmeChallengeTtlSeconds::try_new(900).expect("ttl"),
        )
        .expect("challenge")
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
