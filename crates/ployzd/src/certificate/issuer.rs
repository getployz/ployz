use async_trait::async_trait;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use ployz_core::cert::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
};
use ployz_core::ids::{CertId, OperationId};
use ployz_core::ops::RouteHostname;
use ployz_core::subjects::INTENT_CHANGED;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::intent::certificate_intent::CertificateIntentStore;
use crate::operations::log::OperationRepository;

const CHALLENGE_TTL_SECONDS: u64 = 15 * 60;

#[derive(Debug, Clone)]
pub struct IssuedCertificate {
    pub certificate_chain_pem: String,
    pub private_key_pem: String,
}

#[async_trait]
pub trait AcmeIssuer: Send + Sync {
    async fn issue_http01(
        &self,
        context: &AcmeIssueContext,
        hostname: &RouteHostname,
    ) -> Result<IssuedCertificate, AcmeIssuerError>;
}

#[derive(Debug, Clone)]
pub struct AcmeIssueContext {
    store: CertificateIntentStore,
    repository: OperationRepository,
    client: async_nats::Client,
    operation_id: OperationId,
    cert_id: CertId,
    challenge_published: Arc<AtomicBool>,
}

impl AcmeIssueContext {
    pub(crate) fn new(
        store: CertificateIntentStore,
        repository: OperationRepository,
        client: async_nats::Client,
        operation_id: OperationId,
        cert_id: CertId,
    ) -> Self {
        Self {
            store,
            repository,
            client,
            operation_id,
            cert_id,
            challenge_published: Arc::new(AtomicBool::new(false)),
        }
    }

    pub async fn publish_challenge(
        &self,
        challenge: AcmeHttp01Challenge,
    ) -> Result<(), AcmeIssuerError> {
        self.store
            .store_challenge(challenge.clone())
            .await
            .map_err(challenge_publish_error)?;
        self.publish_intent_changed().await?;
        self.repository
            .record_cert_challenge(&self.operation_id, self.cert_id.clone(), challenge)
            .await
            .map_err(challenge_publish_error)?;
        self.challenge_published.store(true, Ordering::Release);
        Ok(())
    }

    pub async fn validation_started(&self) -> Result<(), AcmeIssuerError> {
        if !self.challenge_published.load(Ordering::Acquire) {
            return Err(AcmeIssuerError::Validation {
                message: "HTTP-01 validation cannot start before challenge intent is published"
                    .to_owned(),
            });
        }
        self.repository
            .record_cert_validation_started(&self.operation_id, self.cert_id.clone())
            .await
            .map_err(validation_error)?;
        Ok(())
    }

    pub(crate) async fn clear_challenges(
        &self,
        hostname: &RouteHostname,
    ) -> Result<(), AcmeIssuerError> {
        self.store
            .remove_challenges_for_hostname(hostname)
            .await
            .map_err(challenge_publish_error)?;
        self.publish_intent_changed().await
    }

    async fn publish_intent_changed(&self) -> Result<(), AcmeIssuerError> {
        self.client
            .publish(INTENT_CHANGED, Vec::new().into())
            .await
            .map_err(challenge_publish_error)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcmeIssuerError {
    #[error("ACME challenge publication failed: {message}")]
    ChallengePublish { message: String },
    #[error("ACME validation failed: {message}")]
    Validation { message: String },
}

fn challenge_publish_error(error: impl std::fmt::Display) -> AcmeIssuerError {
    AcmeIssuerError::ChallengePublish {
        message: error.to_string(),
    }
}

fn validation_error(error: impl std::fmt::Display) -> AcmeIssuerError {
    AcmeIssuerError::Validation {
        message: error.to_string(),
    }
}

pub(super) struct InstantAcmeIssuer {
    directory_url: String,
    contact_email: Option<String>,
    store: CertificateIntentStore,
}

impl InstantAcmeIssuer {
    pub(super) fn new(
        directory_url: String,
        contact_email: Option<String>,
        store: CertificateIntentStore,
    ) -> Self {
        Self {
            directory_url,
            contact_email,
            store,
        }
    }
}

#[async_trait]
impl AcmeIssuer for InstantAcmeIssuer {
    async fn issue_http01(
        &self,
        context: &AcmeIssueContext,
        hostname: &RouteHostname,
    ) -> Result<IssuedCertificate, AcmeIssuerError> {
        let account = load_or_create_account(self).await?;
        let identifiers = [Identifier::Dns(hostname.as_str().to_owned())];
        let mut order = account
            .new_order(&NewOrder::new(&identifiers))
            .await
            .map_err(validation_error)?;

        let mut authorizations = order.authorizations();
        while let Some(result) = authorizations.next().await {
            let mut authorization = result.map_err(validation_error)?;
            match authorization.status {
                AuthorizationStatus::Pending => {}
                AuthorizationStatus::Valid => continue,
                AuthorizationStatus::Invalid
                | AuthorizationStatus::Revoked
                | AuthorizationStatus::Expired
                | AuthorizationStatus::Deactivated => {
                    return Err(AcmeIssuerError::Validation {
                        message: format!(
                            "authorization for {} is {:?}",
                            hostname.as_str(),
                            authorization.status
                        ),
                    });
                }
            }
            let mut challenge =
                authorization
                    .challenge(ChallengeType::Http01)
                    .ok_or_else(|| AcmeIssuerError::Validation {
                        message: "authorization has no HTTP-01 challenge".to_owned(),
                    })?;
            let challenge_state = AcmeHttp01Challenge::try_new(
                hostname.clone(),
                AcmeChallengeToken::try_new(challenge.token.clone()).map_err(validation_error)?,
                AcmeChallengeValue::try_new(challenge.key_authorization().as_str())
                    .map_err(validation_error)?,
                AcmeChallengeTtlSeconds::try_new(CHALLENGE_TTL_SECONDS)
                    .map_err(validation_error)?,
            )
            .map_err(validation_error)?;
            context.publish_challenge(challenge_state).await?;
            context.validation_started().await?;
            challenge.set_ready().await.map_err(validation_error)?;
        }
        drop(authorizations);

        let status = order
            .poll_ready(&RetryPolicy::default())
            .await
            .map_err(validation_error)?;
        if status != OrderStatus::Ready {
            return Err(AcmeIssuerError::Validation {
                message: format!(
                    "order for {} reached unexpected status {status:?}",
                    hostname.as_str()
                ),
            });
        }

        let private_key_pem = order.finalize().await.map_err(validation_error)?;
        let certificate_chain_pem = order
            .poll_certificate(&RetryPolicy::default())
            .await
            .map_err(validation_error)?;
        Ok(IssuedCertificate {
            certificate_chain_pem,
            private_key_pem,
        })
    }
}

async fn load_or_create_account(issuer: &InstantAcmeIssuer) -> Result<Account, AcmeIssuerError> {
    if let Some(credentials) = issuer
        .store
        .account_credentials(&issuer.directory_url)
        .await
        .map_err(validation_error)?
    {
        let credentials: AccountCredentials =
            serde_json::from_str(&credentials).map_err(validation_error)?;
        return Account::builder()
            .map_err(validation_error)?
            .from_credentials(credentials)
            .await
            .map_err(validation_error);
    }

    let contacts = issuer
        .contact_email
        .as_deref()
        .map(|email| {
            if email.starts_with("mailto:") {
                email.to_owned()
            } else {
                format!("mailto:{email}")
            }
        })
        .into_iter()
        .collect::<Vec<_>>();
    let contact_refs = contacts.iter().map(String::as_str).collect::<Vec<_>>();
    let (account, credentials) = Account::builder()
        .map_err(validation_error)?
        .create(
            &NewAccount {
                contact: &contact_refs,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            issuer.directory_url.clone(),
            None,
        )
        .await
        .map_err(validation_error)?;
    let credentials = serde_json::to_string(&credentials).map_err(validation_error)?;
    issuer
        .store
        .store_account_credentials(issuer.directory_url.clone(), credentials)
        .await
        .map_err(validation_error)?;
    Ok(account)
}
