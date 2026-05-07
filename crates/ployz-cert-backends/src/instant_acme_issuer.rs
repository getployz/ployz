use async_trait::async_trait;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use ployz_orchestrator::certificates::{
    AccountAcquisition, AcmeAccountCoordinator, AcmeIssuer, AcmeIssuerFactory, CHALLENGE_TTL_SECS,
    CertificateManagerConfig, HTTP01_GATEWAY_SNAPSHOT_SETTLE, Http01ChallengeReadiness,
    IssuedCertificate, LocalHttp01ChallengeReadiness, NoopAcmeAccountCoordinator, StartedOrder,
    account_id_for_issuer_url,
};
use ployz_store_api::{CertificateStore, StoreDriver};
use ployz_types::error::{Error, Result};
use ployz_types::model::{AcmeAccountRecord, AcmeChallengeRecord};
use ployz_types::time::now_unix_secs;
use std::sync::Arc;

pub struct InstantAcmeIssuer {
    config: CertificateManagerConfig,
    readiness: Arc<dyn Http01ChallengeReadiness>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
}

impl InstantAcmeIssuer {
    #[must_use]
    pub fn new(config: CertificateManagerConfig) -> Self {
        Self::with_readiness(config, Arc::new(LocalHttp01ChallengeReadiness))
    }

    #[must_use]
    pub fn with_readiness(
        config: CertificateManagerConfig,
        readiness: Arc<dyn Http01ChallengeReadiness>,
    ) -> Self {
        Self::with_readiness_and_account_coordinator(
            config,
            readiness,
            Arc::new(NoopAcmeAccountCoordinator),
        )
    }

    #[must_use]
    pub fn with_readiness_and_account_coordinator(
        config: CertificateManagerConfig,
        readiness: Arc<dyn Http01ChallengeReadiness>,
        account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    ) -> Self {
        Self {
            config,
            readiness,
            account_coordinator,
        }
    }
}

#[async_trait]
impl AcmeIssuer for InstantAcmeIssuer {
    async fn start_order(&self, store: &StoreDriver, hostname: &str) -> Result<StartedOrder> {
        let account =
            load_or_create_account(store, &self.config, self.account_coordinator.as_ref()).await?;

        let identifiers = [Identifier::Dns(hostname.to_string())];
        let mut order = account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(acme_error("new_order"))?;
        let order_url = order.url().to_string();

        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authorization = result.map_err(acme_error("authorization"))?;
            match authorization.status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Invalid
                | AuthorizationStatus::Revoked
                | AuthorizationStatus::Expired
                | AuthorizationStatus::Deactivated => {
                    return Err(Error::operation(
                        "acme_authorization",
                        format!("authorization for {hostname} is {:?}", authorization.status),
                    ));
                }
            }
            let challenge = authorization
                .challenge(ChallengeType::Http01)
                .ok_or_else(|| Error::operation("acme_challenge", "no http-01 challenge found"))?;
            store
                .upsert_acme_challenge(&AcmeChallengeRecord {
                    hostname: hostname.to_string(),
                    token: challenge.token.clone(),
                    key_authorization: challenge.key_authorization().as_str().to_string(),
                    expires_at: now_unix_secs() + CHALLENGE_TTL_SECS,
                    created_at: now_unix_secs(),
                })
                .await?;
        }

        Ok(StartedOrder { order_url })
    }

    async fn finalize_order(
        &self,
        store: &StoreDriver,
        hostname: &str,
        order_url: &str,
    ) -> Result<IssuedCertificate> {
        // The stored order URL is opaque user-influenced data once a
        // certificate record has been written. Refuse to resume against any
        // origin that is not the configured ACME directory so a corrupted
        // record cannot redirect the finalize step at an attacker-controlled
        // host.
        ensure_order_url_matches_directory(&self.config.issuer_url, order_url)?;
        let account =
            load_or_create_account(store, &self.config, self.account_coordinator.as_ref()).await?;
        let mut order = account
            .order(order_url.to_string())
            .await
            .map_err(acme_error("resume_order"))?;

        // Tokens this order owns. Used after finalization to scope challenge
        // cleanup to *this* order — deleting by hostname alone would clobber
        // a newer in-flight order's tokens if a stale finalizer races a peer's
        // `start_one` for the same hostname.
        let mut order_tokens: Vec<String> = Vec::new();
        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authorization = result.map_err(acme_error("authorization"))?;
            let status = authorization.status;
            let mut challenge = authorization
                .challenge(ChallengeType::Http01)
                .ok_or_else(|| Error::operation("acme_challenge", "no http-01 challenge found"))?;
            order_tokens.push(challenge.token.clone());
            match status {
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Invalid
                | AuthorizationStatus::Revoked
                | AuthorizationStatus::Expired
                | AuthorizationStatus::Deactivated => {
                    return Err(Error::operation(
                        "acme_authorization",
                        format!("authorization for {hostname} is {status:?}"),
                    ));
                }
            }
            self.readiness
                .wait_ready(store, hostname, &challenge.token)
                .await?;
            tokio::time::sleep(HTTP01_GATEWAY_SNAPSHOT_SETTLE).await;
            challenge
                .set_ready()
                .await
                .map_err(acme_error("set_ready"))?;
        }
        drop(authorizations);

        let status = order
            .poll_ready(&RetryPolicy::default())
            .await
            .map_err(acme_error("poll_ready"))?;
        if status != OrderStatus::Ready {
            return Err(Error::operation(
                "acme_order",
                format!("order for {hostname} reached unexpected status {status:?}"),
            ));
        }

        let private_key_pem = order.finalize().await.map_err(acme_error("finalize"))?;
        let fullchain_pem = order
            .poll_certificate(&RetryPolicy::default())
            .await
            .map_err(acme_error("poll_certificate"))?;
        for token in &order_tokens {
            store.delete_acme_challenge(hostname, token).await?;
        }
        Ok(IssuedCertificate {
            fullchain_pem,
            private_key_pem,
        })
    }
}

/// Factory that produces `InstantAcmeIssuer` instances bound to a particular
/// ACME directory. Implements the orchestrator-side `AcmeIssuerFactory` trait
/// so the orchestrator can stay free of `instant-acme` and `reqwest`
/// dependencies.
pub struct InstantAcmeIssuerFactory {
    config: CertificateManagerConfig,
}

impl InstantAcmeIssuerFactory {
    #[must_use]
    pub fn new(config: CertificateManagerConfig) -> Self {
        Self { config }
    }
}

impl AcmeIssuerFactory for InstantAcmeIssuerFactory {
    fn issuer_url(&self) -> &str {
        &self.config.issuer_url
    }

    fn create(
        &self,
        readiness: Arc<dyn Http01ChallengeReadiness>,
        account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    ) -> Arc<dyn AcmeIssuer> {
        Arc::new(InstantAcmeIssuer::with_readiness_and_account_coordinator(
            self.config.clone(),
            readiness,
            account_coordinator,
        ))
    }
}

async fn load_or_create_account(
    store: &StoreDriver,
    config: &CertificateManagerConfig,
    coordinator: &dyn AcmeAccountCoordinator,
) -> Result<Account> {
    if let Some(record) = store.get_acme_account(&config.issuer_url).await? {
        return account_from_record(config, &record).await;
    }

    let hold = match coordinator.try_acquire_account(&config.issuer_url).await {
        AccountAcquisition::Allowed(hold) => hold,
        AccountAcquisition::VetoedByPeer(reason) => {
            return Err(Error::operation(
                "acme_account_coordination",
                format!(
                    "ACME account creation deferred for {}: {reason}",
                    config.issuer_url
                ),
            ));
        }
        AccountAcquisition::CoordinationFailed(reason) => {
            return Err(Error::operation(
                "acme_account_coordination",
                format!(
                    "could not acquire ACME account lock for {}: {reason}",
                    config.issuer_url
                ),
            ));
        }
    };

    let result = load_or_create_account_under_lock(store, config).await;
    hold.release().await;
    result
}

async fn load_or_create_account_under_lock(
    store: &StoreDriver,
    config: &CertificateManagerConfig,
) -> Result<Account> {
    if let Some(record) = store.get_acme_account(&config.issuer_url).await? {
        return account_from_record(config, &record).await;
    }

    let contact = contact_uris(config);
    let contact_refs = contact.iter().map(String::as_str).collect::<Vec<_>>();
    let (account, credentials) = account_builder(config)?
        .create(
            &NewAccount {
                contact: &contact_refs,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            config.issuer_url.clone(),
            None,
        )
        .await
        .map_err(acme_error("account_create"))?;
    let now = now_unix_secs();
    store
        .upsert_acme_account(&AcmeAccountRecord {
            account_id: account_id_for_issuer_url(&config.issuer_url),
            issuer_url: config.issuer_url.clone(),
            contact_email: config.contact_email.clone(),
            account_credentials_json: serde_json::to_string(&credentials)
                .map_err(|error| Error::operation("acme_account_encode", error.to_string()))?,
            created_at: now,
            updated_at: now,
        })
        .await?;
    Ok(account)
}

async fn account_from_record(
    config: &CertificateManagerConfig,
    record: &AcmeAccountRecord,
) -> Result<Account> {
    let credentials: AccountCredentials = serde_json::from_str(&record.account_credentials_json)
        .map_err(|error| Error::operation("acme_account_decode", error.to_string()))?;
    account_builder(config)?
        .from_credentials(credentials)
        .await
        .map_err(acme_error("account_from_credentials"))
}

fn account_builder(config: &CertificateManagerConfig) -> Result<instant_acme::AccountBuilder> {
    match &config.root_ca_path {
        Some(path) => Account::builder_with_root(path).map_err(acme_error("account_builder")),
        None => Account::builder().map_err(acme_error("account_builder")),
    }
}

fn contact_uris(config: &CertificateManagerConfig) -> Vec<String> {
    let Some(contact_email) = config.contact_email.as_deref() else {
        return Vec::new();
    };
    if contact_email.starts_with("mailto:") {
        vec![contact_email.to_string()]
    } else {
        vec![format!("mailto:{contact_email}")]
    }
}

fn acme_error(
    operation: &'static str,
) -> impl FnOnce(instant_acme::Error) -> Error + Send + Sync + 'static {
    move |error| Error::operation(operation, error.to_string())
}

fn ensure_order_url_matches_directory(directory_url: &str, order_url: &str) -> Result<()> {
    let directory = reqwest::Url::parse(directory_url)
        .map_err(|error| Error::operation("acme_directory_url", error.to_string()))?;
    let order = reqwest::Url::parse(order_url)
        .map_err(|error| Error::operation("acme_order_url", error.to_string()))?;
    let same_origin = directory.scheme() == order.scheme()
        && directory.host_str() == order.host_str()
        && directory.port_or_known_default() == order.port_or_known_default();
    if !same_origin {
        return Err(Error::operation(
            "acme_order_url_origin",
            format!("ACME order URL {order} does not share an origin with directory {directory}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use ployz_orchestrator::certificates::{
        AccountAcquisition, AcmeAccountCoordinator, CertificateManagerConfig,
    };
    use ployz_store_api::StoreDriver;
    use ployz_types::error::Error;
    use ployz_types::model::AcmeAccountRecord;

    struct VetoAccountCoordinator;

    #[async_trait]
    impl AcmeAccountCoordinator for VetoAccountCoordinator {
        async fn try_acquire_account(&self, issuer_url: &str) -> AccountAcquisition {
            AccountAcquisition::VetoedByPeer(format!("peer holds {issuer_url}"))
        }
    }

    struct FailingAccountCoordinator;

    #[async_trait]
    impl AcmeAccountCoordinator for FailingAccountCoordinator {
        async fn try_acquire_account(&self, issuer_url: &str) -> AccountAcquisition {
            AccountAcquisition::CoordinationFailed(format!("lock backend failed for {issuer_url}"))
        }
    }

    fn config_with_issuer(issuer_url: &str) -> CertificateManagerConfig {
        CertificateManagerConfig {
            issuer_url: issuer_url.into(),
            contact_email: None,
            root_ca_path: None,
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
            Error::Operation {
                operation: "acme_order_url_origin",
                ..
            }
        ));

        let mismatched_scheme = ensure_order_url_matches_directory(
            directory,
            "http://acme-v02.api.letsencrypt.org/acme/order/123/456",
        )
        .expect_err("mismatched scheme should be rejected");
        assert!(matches!(
            mismatched_scheme,
            Error::Operation {
                operation: "acme_order_url_origin",
                ..
            }
        ));

        let bad_order = ensure_order_url_matches_directory(directory, "not a url")
            .expect_err("unparseable order URL should be rejected");
        assert!(matches!(
            bad_order,
            Error::Operation {
                operation: "acme_order_url",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn account_creation_respects_issuer_scoped_coordination_veto() {
        let store = StoreDriver::memory();
        let config = config_with_issuer("https://acme.test/dir");

        let error = match load_or_create_account(&store, &config, &VetoAccountCoordinator).await {
            Ok(_) => {
                panic!("missing account plus coordination veto should fail before ACME create")
            }
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("ACME account creation deferred"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn account_creation_surfaces_coordination_backend_failure() {
        let store = StoreDriver::memory();
        let config = config_with_issuer("https://acme.test/dir");

        let error = match load_or_create_account(&store, &config, &FailingAccountCoordinator).await
        {
            Ok(_) => {
                panic!(
                    "missing account plus coordination backend failure should fail before ACME create"
                )
            }
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("could not acquire ACME account lock")
        );
        assert!(error.to_string().contains("lock backend failed"));
    }

    #[tokio::test]
    async fn account_from_record_rejects_malformed_json() {
        let config = config_with_issuer("https://acme.test/dir");
        let record = AcmeAccountRecord {
            account_id: account_id_for_issuer_url(&config.issuer_url),
            issuer_url: config.issuer_url.clone(),
            contact_email: None,
            account_credentials_json: "{not json".into(),
            created_at: 1,
            updated_at: 1,
        };
        match account_from_record(&config, &record).await {
            Ok(_) => panic!("malformed credentials JSON should fail to rehydrate"),
            Err(Error::Operation { operation, .. }) => assert_eq!(operation, "acme_account_decode"),
        }
    }

    #[tokio::test]
    async fn account_from_record_rejects_wrong_shape_json() {
        let config = config_with_issuer("https://acme.test/dir");
        let record = AcmeAccountRecord {
            account_id: account_id_for_issuer_url(&config.issuer_url),
            issuer_url: config.issuer_url.clone(),
            contact_email: None,
            account_credentials_json: r#"{"foo":"bar"}"#.into(),
            created_at: 1,
            updated_at: 1,
        };
        match account_from_record(&config, &record).await {
            Ok(_) => panic!("JSON missing AccountCredentials fields should fail to rehydrate"),
            Err(Error::Operation { operation, .. }) => assert_eq!(operation, "acme_account_decode"),
        }
    }
}
