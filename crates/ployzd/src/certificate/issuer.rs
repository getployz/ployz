//! Transport-neutral ACME HTTP-01 issuance mechanics.

use std::time::Duration;

use async_trait::async_trait;
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use ployz_core::certificate::{
    AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue, AcmeHttp01Challenge,
};
use ployz_core::operation::RouteHostname;

pub const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const DEFAULT_ACME_ISSUE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
pub const DEFAULT_ACME_CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);
const CHALLENGE_TTL_SECONDS: u64 = 15 * 60;

/// Machine-local persistence for one gateway's ACME account credentials.
#[async_trait]
pub trait AcmeAccountStore: Send + Sync {
    async fn load_account_credentials(&self, directory_url: &str)
    -> Result<Option<String>, String>;

    async fn store_account_credentials(
        &self,
        directory_url: &str,
        credentials: String,
    ) -> Result<(), String>;
}

/// Publishes HTTP-01 material and confirms it is ready before ACME validation.
/// Removal must be idempotent so cleanup can run after every issuance outcome.
#[async_trait]
pub trait Http01ChallengePublisher: Send + Sync {
    async fn publish_challenge(&self, challenge: &AcmeHttp01Challenge) -> Result<(), String>;

    async fn remove_challenge(&self, challenge: &AcmeHttp01Challenge) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuedCertificate {
    pub certificate_chain_pem: String,
    pub private_key_pem: String,
}

#[derive(Debug)]
pub struct InstantAcmeIssuer<S> {
    directory_url: String,
    account_store: S,
    issue_timeout: Duration,
    cleanup_timeout: Duration,
}

impl<S> InstantAcmeIssuer<S> {
    #[must_use]
    pub fn new(directory_url: impl Into<String>, account_store: S) -> Self {
        Self {
            directory_url: directory_url.into(),
            account_store,
            issue_timeout: DEFAULT_ACME_ISSUE_TIMEOUT,
            cleanup_timeout: DEFAULT_ACME_CLEANUP_TIMEOUT,
        }
    }

    #[must_use]
    pub const fn with_timeouts(
        mut self,
        issue_timeout: Duration,
        cleanup_timeout: Duration,
    ) -> Self {
        self.issue_timeout = issue_timeout;
        self.cleanup_timeout = cleanup_timeout;
        self
    }
}

impl<S: AcmeAccountStore> InstantAcmeIssuer<S> {
    pub async fn issue_http01<P: Http01ChallengePublisher>(
        &self,
        publisher: &P,
        hostname: &RouteHostname,
    ) -> Result<IssuedCertificate, AcmeIssuerError> {
        let mut presented = Vec::new();
        let issuance = tokio::time::timeout(
            self.issue_timeout,
            self.issue_inner(publisher, hostname, &mut presented),
        )
        .await
        .map_err(|_| AcmeIssuerError::TimedOut {
            phase: AcmeTimeoutPhase::Issue,
            timeout: self.issue_timeout,
        })
        .and_then(|result| result);
        let cleanup = tokio::time::timeout(
            self.cleanup_timeout,
            cleanup_presented_challenges(publisher, &presented),
        )
        .await
        .map_err(|_| AcmeIssuerError::TimedOut {
            phase: AcmeTimeoutPhase::Cleanup,
            timeout: self.cleanup_timeout,
        })
        .and_then(|result| result);
        combine_issuance_and_cleanup(issuance, cleanup)
    }

    async fn issue_inner<P: Http01ChallengePublisher>(
        &self,
        publisher: &P,
        hostname: &RouteHostname,
        presented: &mut Vec<AcmeHttp01Challenge>,
    ) -> Result<IssuedCertificate, AcmeIssuerError> {
        let account = load_or_create_account(&self.directory_url, &self.account_store).await?;
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
            publisher
                .publish_challenge(&challenge_state)
                .await
                .map_err(|message| AcmeIssuerError::ChallengePublication { message })?;
            presented.push(challenge_state);
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

async fn load_or_create_account<S: AcmeAccountStore>(
    directory_url: &str,
    store: &S,
) -> Result<Account, AcmeIssuerError> {
    if let Some(credentials) = store
        .load_account_credentials(directory_url)
        .await
        .map_err(|message| AcmeIssuerError::AccountPersistence { message })?
    {
        let credentials: AccountCredentials =
            serde_json::from_str(&credentials).map_err(account_credentials_error)?;
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
            directory_url.to_owned(),
            None,
        )
        .await
        .map_err(validation_error)?;
    let credentials = serde_json::to_string(&credentials).map_err(account_credentials_error)?;
    store
        .store_account_credentials(directory_url, credentials)
        .await
        .map_err(|message| AcmeIssuerError::AccountPersistence { message })?;
    Ok(account)
}

async fn cleanup_presented_challenges<P: Http01ChallengePublisher>(
    publisher: &P,
    challenges: &[AcmeHttp01Challenge],
) -> Result<(), AcmeIssuerError> {
    let mut failures = Vec::new();
    for challenge in challenges {
        if let Err(message) = publisher.remove_challenge(challenge).await {
            failures.push(format!("{}: {message}", challenge.hostname().as_str()));
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(AcmeIssuerError::ChallengeCleanup {
            message: failures.join("; "),
        })
    }
}

fn combine_issuance_and_cleanup(
    issuance: Result<IssuedCertificate, AcmeIssuerError>,
    cleanup: Result<(), AcmeIssuerError>,
) -> Result<IssuedCertificate, AcmeIssuerError> {
    match (issuance, cleanup) {
        (Ok(certificate), Ok(())) => Ok(certificate),
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(issuance), Err(cleanup)) => Err(AcmeIssuerError::IssuanceAndChallengeCleanup {
            issuance_message: issuance.to_string(),
            cleanup_message: cleanup.to_string(),
        }),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcmeTimeoutPhase {
    Issue,
    Cleanup,
}

impl std::fmt::Display for AcmeTimeoutPhase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Issue => formatter.write_str("issuance"),
            Self::Cleanup => formatter.write_str("challenge cleanup"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AcmeIssuerError {
    #[error("ACME account persistence failed: {message}")]
    AccountPersistence { message: String },
    #[error("ACME account credentials are invalid: {message}")]
    AccountCredentials { message: String },
    #[error("HTTP-01 challenge publication failed: {message}")]
    ChallengePublication { message: String },
    #[error("HTTP-01 challenge cleanup failed: {message}")]
    ChallengeCleanup { message: String },
    #[error(
        "ACME issuance failed: {issuance_message}; HTTP-01 cleanup also failed: {cleanup_message}"
    )]
    IssuanceAndChallengeCleanup {
        issuance_message: String,
        cleanup_message: String,
    },
    #[error("ACME {phase} timed out after {timeout:?}")]
    TimedOut {
        phase: AcmeTimeoutPhase,
        timeout: Duration,
    },
    #[error("ACME validation failed: {message}")]
    Validation { message: String },
}

fn validation_error(error: impl std::fmt::Display) -> AcmeIssuerError {
    AcmeIssuerError::Validation {
        message: error.to_string(),
    }
}

fn account_credentials_error(error: impl std::fmt::Display) -> AcmeIssuerError {
    AcmeIssuerError::AccountCredentials {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::certificate::{
        AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue,
    };
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingPublisher {
        removed: Mutex<Vec<String>>,
    }

    #[async_trait]
    impl Http01ChallengePublisher for RecordingPublisher {
        async fn publish_challenge(&self, _challenge: &AcmeHttp01Challenge) -> Result<(), String> {
            Ok(())
        }

        async fn remove_challenge(&self, challenge: &AcmeHttp01Challenge) -> Result<(), String> {
            self.removed
                .lock()
                .expect("recording lock")
                .push(challenge.hostname().as_str().to_owned());
            (challenge.hostname().as_str() != "bad.example.com")
                .then_some(())
                .ok_or_else(|| "row unavailable".to_owned())
        }
    }

    #[tokio::test]
    async fn cleanup_attempts_every_presented_challenge() {
        let publisher = RecordingPublisher::default();
        let challenges = [challenge("bad.example.com"), challenge("good.example.com")];

        let error = cleanup_presented_challenges(&publisher, &challenges)
            .await
            .expect_err("one cleanup fails");

        assert!(matches!(error, AcmeIssuerError::ChallengeCleanup { .. }));
        assert_eq!(
            *publisher.removed.lock().expect("recording lock"),
            ["bad.example.com", "good.example.com"]
        );
    }

    #[test]
    fn cleanup_failure_preserves_the_issuance_failure() {
        let issuance = AcmeIssuerError::Validation {
            message: "authorization rejected".to_owned(),
        };
        let cleanup = AcmeIssuerError::TimedOut {
            phase: AcmeTimeoutPhase::Cleanup,
            timeout: Duration::from_secs(30),
        };

        let error = combine_issuance_and_cleanup(Err(issuance), Err(cleanup))
            .expect_err("both phases fail");

        assert!(matches!(
            error,
            AcmeIssuerError::IssuanceAndChallengeCleanup {
                issuance_message,
                ..
            } if issuance_message.contains("authorization rejected")
        ));
    }

    fn challenge(hostname: &str) -> AcmeHttp01Challenge {
        let token = "token123";
        AcmeHttp01Challenge::try_new(
            RouteHostname::try_new(hostname).expect("hostname"),
            AcmeChallengeToken::try_new(token).expect("challenge token"),
            AcmeChallengeValue::try_new(format!("{token}.thumbprint")).expect("challenge value"),
            AcmeChallengeTtlSeconds::try_new(CHALLENGE_TTL_SECONDS).expect("challenge ttl"),
        )
        .expect("challenge")
    }
}
