use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_core::cert::{ActiveCertState, CustomCertBundle};
use ployz_core::ids::{CertId, OperationId};
use ployz_core::ops::{CertOperationFailure, FailureMessage, RouteHostname};
use ployz_core::subjects::INTENT_CHANGED;

use super::issuer::{AcmeIssueContext, AcmeIssuer, AcmeIssuerError, InstantAcmeIssuer};
use super::material::validate_and_read_validity;
use crate::core_store::CoreStore;
use crate::intent::certificate_intent::CertificateIntentStore;
use crate::operations::log::{CertOperationSubmission, OperationRepository};
use crate::tasks::TaskRegistry;

pub const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
const DEFAULT_DNS_TIMEOUT: Duration = Duration::from_secs(10);
const DEFAULT_ISSUE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateManagerConfig {
    pub directory_url: String,
    pub contact_email: Option<String>,
    pub state_dir: PathBuf,
    pub dns_timeout: Duration,
    pub issue_timeout: Duration,
}

impl CertificateManagerConfig {
    #[must_use]
    pub fn for_core_db(core_db_path: &Path) -> Self {
        let core_db_path = absolute_path(core_db_path);
        let state_dir = core_db_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("certificates");
        Self {
            directory_url: DEFAULT_ACME_DIRECTORY_URL.to_owned(),
            contact_email: None,
            state_dir,
            dns_timeout: DEFAULT_DNS_TIMEOUT,
            issue_timeout: DEFAULT_ISSUE_TIMEOUT,
        }
    }
}

#[derive(Clone)]
pub struct CertificateManager {
    store: CertificateIntentStore,
    repository: OperationRepository,
    client: async_nats::Client,
    issuer: Arc<dyn AcmeIssuer>,
    dns_timeout: Duration,
    issue_timeout: Duration,
    // ponytail: one cluster-local lock serializes rare certificate issuance;
    // use per-host locks only if unrelated domains measurably contend.
    issuance_lock: Arc<tokio::sync::Mutex<()>>,
    now_seconds: Arc<dyn Fn() -> u64 + Send + Sync>,
    tasks: TaskRegistry,
}

impl std::fmt::Debug for CertificateManager {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CertificateManager")
    }
}

impl CertificateManager {
    #[must_use]
    pub fn new(
        core_store: CoreStore,
        client: async_nats::Client,
        config: CertificateManagerConfig,
    ) -> Self {
        let store =
            CertificateIntentStore::new(core_store.clone(), absolute_path(&config.state_dir));
        let issuer = Arc::new(InstantAcmeIssuer::new(
            config.directory_url,
            config.contact_email,
            store.clone(),
        ));
        Self::new_with_issuer(
            core_store,
            client,
            store,
            issuer,
            config.dns_timeout,
            config.issue_timeout,
            Arc::new(system_now_seconds),
        )
    }

    #[must_use]
    pub fn with_issuer(
        core_store: CoreStore,
        client: async_nats::Client,
        config: CertificateManagerConfig,
        issuer: Arc<dyn AcmeIssuer>,
    ) -> Self {
        let store =
            CertificateIntentStore::new(core_store.clone(), absolute_path(&config.state_dir));
        Self::new_with_issuer(
            core_store,
            client,
            store,
            issuer,
            config.dns_timeout,
            config.issue_timeout,
            Arc::new(system_now_seconds),
        )
    }

    #[must_use]
    pub fn with_issuer_and_time(
        core_store: CoreStore,
        client: async_nats::Client,
        config: CertificateManagerConfig,
        issuer: Arc<dyn AcmeIssuer>,
        now_seconds: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        let store =
            CertificateIntentStore::new(core_store.clone(), absolute_path(&config.state_dir));
        Self::new_with_issuer(
            core_store,
            client,
            store,
            issuer,
            config.dns_timeout,
            config.issue_timeout,
            now_seconds,
        )
    }

    fn new_with_issuer(
        core_store: CoreStore,
        client: async_nats::Client,
        store: CertificateIntentStore,
        issuer: Arc<dyn AcmeIssuer>,
        dns_timeout: Duration,
        issue_timeout: Duration,
        now_seconds: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self {
            store,
            repository: OperationRepository::open(core_store, client.clone()),
            client,
            issuer,
            dns_timeout,
            issue_timeout,
            issuance_lock: Arc::new(tokio::sync::Mutex::new(())),
            now_seconds,
            tasks: TaskRegistry::default(),
        }
    }

    #[must_use]
    pub fn with_task_registry(mut self, tasks: TaskRegistry) -> Self {
        self.tasks = tasks;
        self
    }

    pub async fn ensure(
        &self,
        hostname: &RouteHostname,
        expected_gateway_ips: &[IpAddr],
    ) -> Result<ActiveCertState, CertificateManagerError> {
        if let Some(active) = self
            .store
            .active_for_hostname(hostname)
            .await
            .map_err(active_commit_error)?
            && active.active_cert.validity.not_after.unix_seconds() > (self.now_seconds)()
        {
            return Ok(active.active_cert);
        }
        self.spawn_issue(
            hostname.clone(),
            expected_gateway_ips.to_vec(),
            IssueRequest::Ensure,
        )
        .await
    }

    pub(crate) async fn renew(
        &self,
        active: CustomCertBundle,
        expected_gateway_ips: &[IpAddr],
    ) -> Result<ActiveCertState, CertificateManagerError> {
        self.spawn_issue(
            active.active_cert.hostname.clone(),
            expected_gateway_ips.to_vec(),
            IssueRequest::Renew(active),
        )
        .await
    }

    pub(crate) const fn store(&self) -> &CertificateIntentStore {
        &self.store
    }

    pub(crate) const fn repository(&self) -> &OperationRepository {
        &self.repository
    }

    pub(crate) async fn clear_all_challenges(&self) -> Result<(), CertificateManagerError> {
        self.store.remove_all_challenges().await.map_err(|error| {
            CertificateManagerError::ChallengePublish {
                message: failure_message(error.to_string()),
            }
        })?;
        self.client
            .publish(INTENT_CHANGED, Vec::new().into())
            .await
            .map_err(|error| CertificateManagerError::ChallengePublish {
                message: failure_message(error.to_string()),
            })
    }

    async fn spawn_issue(
        &self,
        hostname: RouteHostname,
        expected_gateway_ips: Vec<IpAddr>,
        request: IssueRequest,
    ) -> Result<ActiveCertState, CertificateManagerError> {
        let manager = self.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.tasks.spawn(async move {
            let _guard = manager.issuance_lock.lock().await;
            let current = manager
                .store
                .active_for_hostname(&hostname)
                .await
                .map_err(active_commit_error);
            let result = match (current, request) {
                (Err(error), _) => Err(error),
                (Ok(Some(current)), IssueRequest::Ensure)
                    if current.active_cert.validity.not_after.unix_seconds()
                        > (manager.now_seconds)() =>
                {
                    Ok(current.active_cert)
                }
                (Ok(current), IssueRequest::Ensure) => {
                    manager
                        .issue_inner(&hostname, &expected_gateway_ips, current)
                        .await
                }
                (Ok(Some(current)), IssueRequest::Renew(expected))
                    if current.active_cert != expected.active_cert =>
                {
                    Ok(current.active_cert)
                }
                (Ok(_), IssueRequest::Renew(expected)) => {
                    manager
                        .issue_inner(&hostname, &expected_gateway_ips, Some(expected))
                        .await
                }
            };
            let _ = result_tx.send(result);
        });
        result_rx.await.map_err(active_commit_error)?
    }

    async fn issue_inner(
        &self,
        hostname: &RouteHostname,
        expected_gateway_ips: &[IpAddr],
        retained: Option<CustomCertBundle>,
    ) -> Result<ActiveCertState, CertificateManagerError> {
        let cert_id = cert_id_for_hostname(hostname);
        let operation_id = OperationId::try_new(format!("op_cert_{}", nuid::next()))
            .map_err(active_commit_error)?;
        self.repository
            .submit_cert(CertOperationSubmission {
                operation_id: operation_id.clone(),
                cert_id: cert_id.clone(),
            })
            .await
            .map_err(|error| active_commit_error(format!("{error:?}")))?;

        if let Err(error) = self.dns_preflight(hostname, expected_gateway_ips).await {
            self.record_failure(&operation_id, &cert_id, &error, retained.as_ref(), None)
                .await;
            return Err(error);
        }

        let context = AcmeIssueContext::new(
            self.store.clone(),
            self.repository.clone(),
            self.client.clone(),
            operation_id.clone(),
            cert_id.clone(),
        );
        let issued = tokio::time::timeout(
            self.issue_timeout,
            self.issuer.issue_http01(&context, hostname),
        )
        .await
        .map_err(|_| CertificateManagerError::AcmeValidation {
            message: failure_message(format!(
                "ACME issuance timed out after {}ms",
                self.issue_timeout.as_millis()
            )),
        })
        .and_then(|result| result.map_err(CertificateManagerError::from));
        let cleanup = context
            .clear_challenges(hostname)
            .await
            .map_err(CertificateManagerError::from);
        let issued = match (issued, cleanup) {
            (_, Err(error)) => {
                self.record_failure(&operation_id, &cert_id, &error, retained.as_ref(), None)
                    .await;
                return Err(error);
            }
            (Err(error), Ok(())) => {
                self.record_failure(&operation_id, &cert_id, &error, retained.as_ref(), None)
                    .await;
                return Err(error);
            }
            (Ok(issued), Ok(())) => issued,
        };

        let validity = match validate_and_read_validity(
            &issued.certificate_chain_pem,
            &issued.private_key_pem,
            hostname,
        )
        .map_err(|error| CertificateManagerError::AcmeValidation {
            message: failure_message(error.to_string()),
        }) {
            Ok(validity) => validity,
            Err(error) => {
                self.record_failure(&operation_id, &cert_id, &error, retained.as_ref(), None)
                    .await;
                return Err(error);
            }
        };
        let bundle = match self
            .store
            .prepare_active(
                cert_id.clone(),
                hostname.clone(),
                validity,
                issued.certificate_chain_pem,
                issued.private_key_pem,
            )
            .map_err(|error| CertificateManagerError::AcmeValidation {
                message: failure_message(error.to_string()),
            }) {
            Ok(bundle) => bundle,
            Err(error) => {
                self.record_failure(&operation_id, &cert_id, &error, retained.as_ref(), None)
                    .await;
                return Err(error);
            }
        };
        if let Err(error) = self.store.commit_prepared(bundle.clone()).await {
            let error = active_commit_error(error);
            self.record_failure(
                &operation_id,
                &cert_id,
                &error,
                retained.as_ref(),
                Some(&bundle),
            )
            .await;
            return Err(error);
        }
        if let Err(error) = self.client.publish(INTENT_CHANGED, Vec::new().into()).await {
            let error = active_commit_error(error);
            self.record_failure(
                &operation_id,
                &cert_id,
                &error,
                retained.as_ref(),
                Some(&bundle),
            )
            .await;
            return Err(error);
        }
        if let Err(error) = self
            .repository
            .record_cert_completed(&operation_id, bundle.active_cert.clone())
            .await
        {
            let error = active_commit_error(error);
            self.record_failure(
                &operation_id,
                &cert_id,
                &error,
                retained.as_ref(),
                Some(&bundle),
            )
            .await;
            return Err(error);
        }
        Ok(bundle.active_cert)
    }

    async fn dns_preflight(
        &self,
        hostname: &RouteHostname,
        expected_gateway_ips: &[IpAddr],
    ) -> Result<(), CertificateManagerError> {
        if expected_gateway_ips.is_empty() {
            return Err(CertificateManagerError::DnsPreflight {
                guidance: DnsPreflightGuidance::NoExpectedGatewayIps {
                    hostname: hostname.clone(),
                },
            });
        }
        let resolved = tokio::time::timeout(
            self.dns_timeout,
            tokio::net::lookup_host((hostname.as_str(), 0)),
        )
        .await
        .map_err(|_| CertificateManagerError::DnsPreflight {
            guidance: DnsPreflightGuidance::ResolutionFailed {
                hostname: hostname.clone(),
                message: format!(
                    "DNS resolution timed out after {}ms",
                    self.dns_timeout.as_millis()
                ),
            },
        })?
        .map_err(|error| CertificateManagerError::DnsPreflight {
            guidance: DnsPreflightGuidance::ResolutionFailed {
                hostname: hostname.clone(),
                message: error.to_string(),
            },
        })?;
        let mut resolved_ips = resolved.map(|address| address.ip()).collect::<Vec<_>>();
        resolved_ips.sort_unstable();
        resolved_ips.dedup();
        if dns_answers_are_gateway_subset(&resolved_ips, expected_gateway_ips) {
            return Ok(());
        }
        let mut expected_gateway_ips = expected_gateway_ips.to_vec();
        expected_gateway_ips.sort_unstable();
        expected_gateway_ips.dedup();
        Err(CertificateManagerError::DnsPreflight {
            guidance: DnsPreflightGuidance::NoMatchingGatewayIp {
                hostname: hostname.clone(),
                resolved_ips,
                expected_gateway_ips,
            },
        })
    }

    async fn record_failure(
        &self,
        operation_id: &OperationId,
        cert_id: &CertId,
        error: &CertificateManagerError,
        retained: Option<&CustomCertBundle>,
        attempted: Option<&CustomCertBundle>,
    ) {
        let retained_active_cert = retained.map(|bundle| bundle.active_cert.clone());
        let failure = match error {
            CertificateManagerError::DnsPreflight { .. } => {
                CertOperationFailure::DnsPreflightFailed {
                    cert_id: cert_id.clone(),
                    message: error.failure_message(),
                    retained_active_cert,
                }
            }
            CertificateManagerError::ChallengePublish { .. } => {
                CertOperationFailure::ChallengePublishFailed {
                    cert_id: cert_id.clone(),
                    message: error.failure_message(),
                }
            }
            CertificateManagerError::AcmeValidation { .. } => {
                CertOperationFailure::AcmeValidationFailed {
                    cert_id: cert_id.clone(),
                    message: error.failure_message(),
                    retained_active_cert,
                }
            }
            CertificateManagerError::ActiveCertCommit { .. } => {
                let Some(attempted) = attempted else {
                    eprintln!(
                        "ployzd certificate operation warning: operation={} failure={} was not recorded because no attempted bundle exists",
                        operation_id.as_str(),
                        error
                    );
                    return;
                };
                CertOperationFailure::ActiveCertCommitFailed {
                    cert_id: cert_id.clone(),
                    bundle_ref: attempted.active_cert.bundle_ref.clone(),
                    validity: attempted.active_cert.validity,
                    message: error.failure_message(),
                    retained_active_cert,
                }
            }
        };
        if let Err(record_error) = self
            .repository
            .record_cert_failed(operation_id, failure)
            .await
        {
            eprintln!(
                "ployzd certificate operation warning: operation={} record_failure={record_error}",
                operation_id.as_str()
            );
        }
    }
}

fn cert_id_for_hostname(hostname: &RouteHostname) -> CertId {
    CertId::try_new(format!("cert_{}", hostname.as_str().replace('.', "_")))
        .expect("validated route hostnames render as certificate subject tokens")
}

enum IssueRequest {
    Ensure,
    Renew(CustomCertBundle),
}

fn system_now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn dns_answers_are_gateway_subset(resolved: &[IpAddr], expected_gateway_ips: &[IpAddr]) -> bool {
    !resolved.is_empty()
        && resolved
            .iter()
            .all(|address| expected_gateway_ips.contains(address))
}

fn absolute_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir()
        .map(|current| current.join(path))
        .unwrap_or_else(|_| Path::new("/var/lib/ployz").join(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_core_database_gets_an_absolute_certificate_state_directory() {
        assert!(
            CertificateManagerConfig::for_core_db(Path::new("ployz-core.db"))
                .state_dir
                .is_absolute()
        );
    }

    #[test]
    fn dns_answers_reject_a_mixed_gateway_and_foreign_set() {
        let gateway = IpAddr::from([192, 0, 2, 10]);
        let foreign = IpAddr::from([198, 51, 100, 20]);

        assert!(!dns_answers_are_gateway_subset(
            &[gateway, foreign],
            &[gateway]
        ));
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("generated certificate failure is non-empty")
}

fn active_commit_error(error: impl std::fmt::Display) -> CertificateManagerError {
    CertificateManagerError::ActiveCertCommit {
        message: failure_message(error.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CertificateManagerError {
    #[error("certificate DNS preflight failed: {guidance}")]
    DnsPreflight { guidance: DnsPreflightGuidance },
    #[error("certificate challenge publication failed: {message}")]
    ChallengePublish { message: FailureMessage },
    #[error("certificate ACME validation failed: {message}")]
    AcmeValidation { message: FailureMessage },
    #[error("active certificate commit failed: {message}")]
    ActiveCertCommit { message: FailureMessage },
}

impl CertificateManagerError {
    #[must_use]
    pub fn failure_message(&self) -> FailureMessage {
        match self {
            Self::DnsPreflight { guidance } => failure_message(guidance.to_string()),
            Self::ChallengePublish { message }
            | Self::AcmeValidation { message }
            | Self::ActiveCertCommit { message } => message.clone(),
        }
    }
}

impl From<AcmeIssuerError> for CertificateManagerError {
    fn from(error: AcmeIssuerError) -> Self {
        match error {
            AcmeIssuerError::ChallengePublish { message } => Self::ChallengePublish {
                message: failure_message(message),
            },
            AcmeIssuerError::Validation { message } => Self::AcmeValidation {
                message: failure_message(message),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DnsPreflightGuidance {
    #[error(
        "{} cannot be checked because no expected gateway IPs are known",
        hostname.as_str()
    )]
    NoExpectedGatewayIps { hostname: RouteHostname },
    #[error(
        "failed to resolve A/AAAA records for {}: {message}",
        hostname.as_str()
    )]
    ResolutionFailed {
        hostname: RouteHostname,
        message: String,
    },
    #[error(
        "{} resolves to {resolved_ips:?}, which is not a non-empty subset of known gateway IPs {expected_gateway_ips:?}",
        hostname.as_str()
    )]
    NoMatchingGatewayIp {
        hostname: RouteHostname,
        resolved_ips: Vec<IpAddr>,
        expected_gateway_ips: Vec<IpAddr>,
    },
}
