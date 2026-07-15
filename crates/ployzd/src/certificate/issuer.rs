use async_trait::async_trait;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use ployz_core::cert::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
};
use ployz_core::ids::{CertId, MachineId, OperationId};
use ployz_core::ops::RouteHostname;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::intent::certificate_intent::CertificateIntentStore;
use crate::operations::log::OperationRepository;

use super::gateway::GatewayCertificateClient;

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
    repository: OperationRepository,
    operation_id: OperationId,
    cert_id: CertId,
    gateway_client: GatewayCertificateClient,
    challenge_machine_ids: Vec<MachineId>,
    challenge_published: Arc<AtomicBool>,
    applied_challenges: Arc<Mutex<Vec<AcmeHttp01Challenge>>>,
}

impl AcmeIssueContext {
    pub(crate) fn new(
        repository: OperationRepository,
        client: async_nats::Client,
        operation_id: OperationId,
        cert_id: CertId,
        challenge_machine_ids: Vec<MachineId>,
        _challenge_readiness_timeout: std::time::Duration,
    ) -> Self {
        Self {
            repository,
            operation_id,
            cert_id,
            gateway_client: GatewayCertificateClient::new(client),
            challenge_machine_ids,
            challenge_published: Arc::new(AtomicBool::new(false)),
            applied_challenges: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub async fn publish_challenge(
        &self,
        challenge: AcmeHttp01Challenge,
    ) -> Result<(), AcmeIssuerError> {
        self.repository
            .record_cert_challenge(&self.operation_id, self.cert_id.clone(), challenge.clone())
            .await
            .map_err(operation_evidence_error)?;
        self.gateway_client
            .apply_challenge(&self.operation_id, &challenge, &self.challenge_machine_ids)
            .await
            .map_err(|error| AcmeIssuerError::ChallengeReadiness {
                missing_machine_ids: error.missing_machine_ids,
            })?;
        self.applied_challenges
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(challenge);
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
            .map_err(operation_evidence_error)?;
        Ok(())
    }

    pub(crate) async fn clear_challenges(&self, _hostname: &RouteHostname) -> Vec<MachineId> {
        let challenges = std::mem::take(
            &mut *self
                .applied_challenges
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        let mut missing = Vec::new();
        for challenge in challenges {
            missing.extend(
                self.gateway_client
                    .remove_challenge(&self.operation_id, &challenge, &self.challenge_machine_ids)
                    .await,
            );
        }
        missing.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        missing.dedup();
        missing
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcmeIssuerError {
    #[error("certificate operation evidence write failed: {message}")]
    OperationEvidenceWrite { message: String },
    #[error("ACME challenge publication failed: {message}")]
    ChallengePublish { message: String },
    #[error("HTTP-01 challenge is not applied on gateways {missing_machine_ids:?}")]
    ChallengeReadiness { missing_machine_ids: Vec<MachineId> },
    #[error("ACME validation failed: {message}")]
    Validation { message: String },
}

fn validation_error(error: impl std::fmt::Display) -> AcmeIssuerError {
    AcmeIssuerError::Validation {
        message: error.to_string(),
    }
}

fn operation_evidence_error(error: impl std::fmt::Display) -> AcmeIssuerError {
    AcmeIssuerError::OperationEvidenceWrite {
        message: error.to_string(),
    }
}

pub(super) struct InstantAcmeIssuer {
    directory_url: String,
    store: CertificateIntentStore,
}

impl InstantAcmeIssuer {
    pub(super) fn new(directory_url: String, store: CertificateIntentStore) -> Self {
        Self {
            directory_url,
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

    let (account, credentials) = Account::builder()
        .map_err(validation_error)?
        .create(
            &NewAccount {
                contact: &[],
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
