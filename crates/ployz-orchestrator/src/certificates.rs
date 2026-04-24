use async_trait::async_trait;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use ployz_store_api::{CertificateStore, StoreDriver};
use ployz_types::error::{Error, Result};
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeRecord, CertificateRecord, CertificateState, CertificateVersion,
};
use ployz_types::time::now_unix_secs;
use uuid::Uuid;

pub const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
const DEFAULT_ACCOUNT_ID: &str = "letsencrypt-production";
const CERT_VALIDITY_FALLBACK_SECS: u64 = 90 * 24 * 60 * 60;
const RENEWAL_BEFORE_EXPIRY_SECS: u64 = 30 * 24 * 60 * 60;
const CHALLENGE_TTL_SECS: u64 = 15 * 60;

#[derive(Debug, Clone)]
pub struct CertificateManagerConfig {
    pub issuer_url: String,
    pub contact_email: Option<String>,
}

impl Default for CertificateManagerConfig {
    fn default() -> Self {
        Self {
            issuer_url: DEFAULT_ACME_DIRECTORY_URL.to_string(),
            contact_email: None,
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
        Self {
            issuer_url,
            contact_email,
        }
    }
}

#[derive(Debug, Clone)]
pub struct IssuedCertificate {
    pub fullchain_pem: String,
    pub private_key_pem: String,
}

#[async_trait]
pub trait AcmeIssuer {
    async fn issue_http01(&self, store: &StoreDriver, hostname: &str) -> Result<IssuedCertificate>;
}

pub fn spawn_certificate_issuance(store: StoreDriver, config: CertificateManagerConfig) {
    tokio::spawn(async move {
        if let Err(error) = issue_due_certificates(&store, &InstantAcmeIssuer::new(config)).await {
            tracing::warn!(?error, "managed certificate issuance failed");
        }
    });
}

pub async fn issue_due_certificates<I>(store: &StoreDriver, issuer: &I) -> Result<()>
where
    I: AcmeIssuer + Sync,
{
    let certificates = store.list_certificates().await?;
    for certificate in certificates {
        match certificate.state {
            CertificateState::Pending | CertificateState::RenewalDue => {
                issue_one(store, issuer, certificate).await?;
            }
            CertificateState::Issuing | CertificateState::Active | CertificateState::Failed => {}
        }
    }
    Ok(())
}

async fn issue_one<I>(store: &StoreDriver, issuer: &I, mut record: CertificateRecord) -> Result<()>
where
    I: AcmeIssuer + Sync,
{
    let previous_active_version_id = record.active_version_id.clone();
    record.state = CertificateState::Issuing;
    record.updated_at = now_unix_secs();
    record.last_error = None;
    store.upsert_certificate(&record).await?;

    match issuer.issue_http01(store, &record.hostname).await {
        Ok(issued) => {
            let now = now_unix_secs();
            let not_after = now + CERT_VALIDITY_FALLBACK_SECS;
            let next_renewal_at = not_after.saturating_sub(RENEWAL_BEFORE_EXPIRY_SECS);
            let version_id = Uuid::new_v4().to_string();
            record.versions.push(CertificateVersion {
                version_id: version_id.clone(),
                fullchain_pem: issued.fullchain_pem,
                private_key_pem: issued.private_key_pem,
                not_before: Some(now),
                not_after: Some(not_after),
                issued_at: now,
            });
            record.active_version_id = Some(version_id);
            record.state = CertificateState::Active;
            record.updated_at = now;
            record.next_renewal_at = Some(next_renewal_at);
            record.last_error = None;
        }
        Err(error) => {
            record.state = CertificateState::Failed;
            record.active_version_id = previous_active_version_id;
            record.updated_at = now_unix_secs();
            record.last_error = Some(error.to_string());
        }
    }

    store.upsert_certificate(&record).await
}

pub struct InstantAcmeIssuer {
    config: CertificateManagerConfig,
}

impl InstantAcmeIssuer {
    #[must_use]
    pub fn new(config: CertificateManagerConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl AcmeIssuer for InstantAcmeIssuer {
    async fn issue_http01(&self, store: &StoreDriver, hostname: &str) -> Result<IssuedCertificate> {
        let account = load_or_create_account(store, &self.config).await?;
        let identifiers = [Identifier::Dns(hostname.to_string())];
        let mut order = account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(acme_error("new_order"))?;

        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authorization = result.map_err(acme_error("authorization"))?;
            match authorization.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
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
            let mut challenge = authorization
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
        let challenges = store.list_acme_challenges().await?;
        for challenge in challenges
            .iter()
            .filter(|challenge| challenge.hostname == hostname)
        {
            store
                .delete_acme_challenge(&challenge.hostname, &challenge.token)
                .await?;
        }
        Ok(IssuedCertificate {
            fullchain_pem,
            private_key_pem,
        })
    }
}

async fn load_or_create_account(
    store: &StoreDriver,
    config: &CertificateManagerConfig,
) -> Result<Account> {
    if let Some(record) = store.get_acme_account(&config.issuer_url).await? {
        let credentials: AccountCredentials = serde_json::from_str(&record.account_key_pem)
            .map_err(|error| Error::operation("acme_account_decode", error.to_string()))?;
        return Account::builder()
            .map_err(acme_error("account_builder"))?
            .from_credentials(credentials)
            .await
            .map_err(acme_error("account_from_credentials"));
    }

    let contact = contact_uris(config);
    let contact_refs = contact.iter().map(String::as_str).collect::<Vec<_>>();
    let (account, credentials) = Account::builder()
        .map_err(acme_error("account_builder"))?
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
            account_id: DEFAULT_ACCOUNT_ID.to_string(),
            issuer_url: config.issuer_url.clone(),
            contact_email: config.contact_email.clone(),
            account_key_pem: serde_json::to_string(&credentials)
                .map_err(|error| Error::operation("acme_account_encode", error.to_string()))?,
            created_at: now,
            updated_at: now,
        })
        .await?;
    Ok(account)
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::CertificateRecord;

    struct FakeIssuer {
        result: Result<IssuedCertificate>,
    }

    #[async_trait]
    impl AcmeIssuer for FakeIssuer {
        async fn issue_http01(
            &self,
            _store: &StoreDriver,
            _hostname: &str,
        ) -> Result<IssuedCertificate> {
            self.result.clone()
        }
    }

    #[tokio::test]
    async fn issuance_writes_active_certificate() {
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("pending cert should persist");

        issue_due_certificates(
            &store,
            &FakeIssuer {
                result: Ok(IssuedCertificate {
                    fullchain_pem: "fullchain".into(),
                    private_key_pem: "key".into(),
                }),
            },
        )
        .await
        .expect("issuance should run");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Active);
        assert!(record.active_version_id.is_some());
        assert_eq!(record.versions.len(), 1);
        assert_eq!(record.versions[0].fullchain_pem, "fullchain");
    }

    #[tokio::test]
    async fn failed_renewal_keeps_previous_active_version() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        record.state = CertificateState::RenewalDue;
        record.active_version_id = Some("old".into());
        record.versions.push(CertificateVersion {
            version_id: "old".into(),
            fullchain_pem: "old-chain".into(),
            private_key_pem: "old-key".into(),
            not_before: Some(1),
            not_after: Some(2),
            issued_at: 1,
        });
        store
            .upsert_certificate(&record)
            .await
            .expect("renewal cert should persist");

        issue_due_certificates(
            &store,
            &FakeIssuer {
                result: Err(Error::operation("fake_acme", "failed")),
            },
        )
        .await
        .expect("issuance errors are recorded per certificate");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Failed);
        assert_eq!(record.active_version_id.as_deref(), Some("old"));
        assert_eq!(record.versions.len(), 1);
    }

    fn pending_record(hostname: &str) -> CertificateRecord {
        CertificateRecord {
            hostname: hostname.into(),
            issuer_url: DEFAULT_ACME_DIRECTORY_URL.into(),
            account_id: DEFAULT_ACCOUNT_ID.into(),
            state: CertificateState::Pending,
            active_version_id: None,
            versions: Vec::new(),
            last_error: None,
            requested_at: 1,
            updated_at: 1,
            next_renewal_at: None,
        }
    }
}
