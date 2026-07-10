use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_core::cert::{ActiveCertState, CustomCertBundle};
use ployz_core::ids::{CertId, MachineId, OperationId};
use ployz_core::ops::{
    CertOperationFailure, CertificateProvisionFailure, FailureMessage, RouteHostname,
};
use ployz_core::subjects::INTENT_CHANGED;

use super::GatewayCertificateTarget;
use super::gateway::GatewayCertificateClient;
use super::issuer::{AcmeIssueContext, AcmeIssuer, AcmeIssuerError, InstantAcmeIssuer};
use super::material::{
    load_custom_certificate, prepare_custom_certificate,
    validate_custom_certificate_for_activation, write_custom_certificate,
};
use crate::core_store::CoreStore;
use crate::intent::certificate_intent::CertificateIntentStore;
use crate::operations::log::{CertOperationSubmission, OperationRepository};
use crate::tasks::TaskRegistry;

pub const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
const DNS_TIMEOUT: Duration = Duration::from_secs(10);
const ISSUE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CHALLENGE_READINESS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateManagerConfig {
    pub directory_url: String,
    pub state_dir: PathBuf,
}

impl CertificateManagerConfig {
    #[must_use]
    pub fn for_core_db(core_db_path: &Path) -> Self {
        let core_db_path = std::path::absolute(core_db_path)
            .unwrap_or_else(|_| Path::new("/var/lib/ployz").join(core_db_path));
        let state_dir = core_db_path
            .parent()
            .unwrap_or_else(|| Path::new("/var/lib/ployz"))
            .join("certificates");
        Self {
            directory_url: DEFAULT_ACME_DIRECTORY_URL.to_owned(),
            state_dir,
        }
    }
}

#[derive(Clone)]
pub struct CertificateManager {
    store: CertificateIntentStore,
    state_dir: PathBuf,
    repository: OperationRepository,
    client: async_nats::Client,
    gateway_client: GatewayCertificateClient,
    issuer: Arc<dyn AcmeIssuer>,
    issuance_lock: Arc<tokio::sync::Mutex<()>>,
    now_seconds: Arc<dyn Fn() -> u64 + Send + Sync>,
    issuance_tasks: TaskRegistry,
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
        let state_dir = std::path::absolute(&config.state_dir)
            .unwrap_or_else(|_| Path::new("/var/lib/ployz").join(&config.state_dir));
        let store = CertificateIntentStore::new(core_store.clone());
        let issuer = Arc::new(InstantAcmeIssuer::new(config.directory_url, store.clone()));
        Self::new_with_issuer(
            core_store,
            client,
            store,
            state_dir,
            issuer,
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
        Self::with_issuer_and_time(
            core_store,
            client,
            config,
            issuer,
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
        let state_dir = std::path::absolute(&config.state_dir)
            .unwrap_or_else(|_| Path::new("/var/lib/ployz").join(&config.state_dir));
        let store = CertificateIntentStore::new(core_store.clone());
        Self::new_with_issuer(core_store, client, store, state_dir, issuer, now_seconds)
    }

    fn new_with_issuer(
        core_store: CoreStore,
        client: async_nats::Client,
        store: CertificateIntentStore,
        state_dir: PathBuf,
        issuer: Arc<dyn AcmeIssuer>,
        now_seconds: Arc<dyn Fn() -> u64 + Send + Sync>,
    ) -> Self {
        Self {
            store,
            state_dir,
            repository: OperationRepository::open(core_store, client.clone()),
            gateway_client: GatewayCertificateClient::new(client.clone()),
            client,
            issuer,
            // ponytail: global issuance lock; use per-hostname locks if issuance throughput matters.
            issuance_lock: Arc::new(tokio::sync::Mutex::new(())),
            now_seconds,
            issuance_tasks: TaskRegistry::default(),
        }
    }

    #[must_use]
    pub fn with_task_registry(mut self, tasks: TaskRegistry) -> Self {
        self.issuance_tasks = tasks;
        self
    }

    pub async fn ensure(
        &self,
        owner_operation_id: &OperationId,
        hostname: &RouteHostname,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let challenge_machine_ids = self.dns_preflight(hostname, targets).await?;
        self.spawn_issue(
            hostname.clone(),
            targets.to_vec(),
            Some(challenge_machine_ids),
            IssueRequest::Ensure {
                owner_operation_id: owner_operation_id.clone(),
            },
        )
        .await
    }

    pub(crate) async fn renew(
        &self,
        active: ActiveCertState,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let hostname = active.hostname.clone();
        self.spawn_issue(
            hostname,
            targets.to_vec(),
            None,
            IssueRequest::Renew(active),
        )
        .await
    }

    pub async fn record_renewal_failure(
        &self,
        active: ActiveCertState,
        failure: CertificateProvisionFailure,
    ) -> Result<(), CertificateProvisionFailure> {
        let operation_id = cert_operation_id()?;
        self.submit_operation(&operation_id, active.cert_id.clone())
            .await?;
        let recorded = self
            .record_failure(&operation_id, &active.cert_id, failure, Some(&active))
            .await;
        if matches!(
            &recorded,
            CertificateProvisionFailure::OperationEvidenceWrite { .. }
        ) {
            return Err(recorded);
        }
        Ok(())
    }

    pub(crate) const fn store(&self) -> &CertificateIntentStore {
        &self.store
    }

    pub(crate) const fn repository(&self) -> &OperationRepository {
        &self.repository
    }

    pub(crate) async fn clear_all_challenges(&self) -> Result<(), CertificateProvisionFailure> {
        self.store.remove_all_challenges().await.map_err(|error| {
            CertificateProvisionFailure::ChallengePublish {
                message: failure_message(error.to_string()),
            }
        })?;
        let _ = self.client.publish(INTENT_CHANGED, Vec::new().into()).await;
        Ok(())
    }

    async fn spawn_issue(
        &self,
        hostname: RouteHostname,
        targets: Vec<GatewayCertificateTarget>,
        challenge_machine_ids: Option<Vec<MachineId>>,
        request: IssueRequest,
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let manager = self.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.issuance_tasks.spawn(async move {
            let _guard = manager.issuance_lock.lock().await;
            let current = manager
                .store
                .active_for_hostname(&hostname)
                .await
                .map_err(active_commit_without_attempt);
            let result = match (current, request) {
                (Err(error), _) => Err(error),
                (Ok(current), IssueRequest::Ensure { owner_operation_id }) => {
                    let reusable = current.as_ref().and_then(|active| {
                        if !active.is_usable_at((manager.now_seconds)()) {
                            return None;
                        }
                        load_custom_certificate(&manager.state_dir, active)
                            .ok()
                            .map(|bundle| (active.clone(), bundle))
                    });
                    match reusable {
                        Some((active, bundle)) => manager
                            .push_bundle(&owner_operation_id, &targets, &bundle)
                            .await
                            .map(|()| active),
                        None => {
                            manager
                                .issue_inner(&hostname, &targets, challenge_machine_ids, current)
                                .await
                        }
                    }
                }
                (Ok(Some(current)), IssueRequest::Renew(expected)) if current != expected => {
                    Ok(current)
                }
                (Ok(current), IssueRequest::Renew(expected)) => {
                    manager
                        .issue_inner(
                            &hostname,
                            &targets,
                            challenge_machine_ids,
                            current.or(Some(expected)),
                        )
                        .await
                }
            };
            let _ = result_tx.send(result);
        });
        result_rx.await.map_err(active_commit_without_attempt)?
    }

    async fn issue_inner(
        &self,
        hostname: &RouteHostname,
        targets: &[GatewayCertificateTarget],
        challenge_machine_ids: Option<Vec<MachineId>>,
        retained: Option<ActiveCertState>,
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let cert_id = cert_id_for_hostname(hostname);
        let operation_id = cert_operation_id()?;
        self.submit_operation(&operation_id, cert_id.clone())
            .await?;
        let challenge_machine_ids = match challenge_machine_ids {
            Some(machine_ids) => machine_ids,
            None => match self.dns_preflight(hostname, targets).await {
                Ok(machine_ids) => machine_ids,
                Err(failure) => {
                    return Err(self
                        .record_failure(&operation_id, &cert_id, failure, retained.as_ref())
                        .await);
                }
            },
        };

        let context = AcmeIssueContext::new(
            self.store.clone(),
            self.repository.clone(),
            self.client.clone(),
            operation_id.clone(),
            cert_id.clone(),
            challenge_machine_ids,
            CHALLENGE_READINESS_TIMEOUT,
        );
        let issued =
            tokio::time::timeout(ISSUE_TIMEOUT, self.issuer.issue_http01(&context, hostname))
                .await
                .map_err(|_| CertificateProvisionFailure::AcmeValidation {
                    message: failure_message(format!(
                        "ACME issuance timed out after {}ms",
                        ISSUE_TIMEOUT.as_millis()
                    )),
                })
                .and_then(|result| result.map_err(provision_failure_from_issuer));
        let cleanup = context
            .clear_challenges(hostname)
            .await
            .map_err(provision_failure_from_issuer);
        let issued = match (issued, cleanup) {
            (_, Err(failure)) | (Err(failure), Ok(())) => {
                return Err(self
                    .record_failure(&operation_id, &cert_id, failure, retained.as_ref())
                    .await);
            }
            (Ok(issued), Ok(())) => issued,
        };

        let bundle = match prepare_custom_certificate(
            &self.state_dir,
            cert_id.clone(),
            hostname.clone(),
            issued.certificate_chain_pem,
            issued.private_key_pem,
        ) {
            Ok(bundle) => bundle,
            Err(error) => {
                let failure = CertificateProvisionFailure::AcmeValidation {
                    message: failure_message(error.to_string()),
                };
                return Err(self
                    .record_failure(&operation_id, &cert_id, failure, retained.as_ref())
                    .await);
            }
        };
        if let Err(error) =
            validate_custom_certificate_for_activation(bundle.active_cert(), (self.now_seconds)())
        {
            let failure = CertificateProvisionFailure::AcmeValidation {
                message: failure_message(error.to_string()),
            };
            return Err(self
                .record_failure(&operation_id, &cert_id, failure, retained.as_ref())
                .await);
        }
        if let Err(error) = write_custom_certificate(&self.state_dir, &bundle) {
            let failure = active_commit_failure(bundle.active_cert().clone(), error);
            return Err(self
                .record_failure(&operation_id, &cert_id, failure, retained.as_ref())
                .await);
        }
        if let Err(failure) = self.push_bundle(&operation_id, targets, &bundle).await {
            return Err(self
                .record_failure(&operation_id, &cert_id, failure, retained.as_ref())
                .await);
        }
        let active_cert = bundle.active_cert().clone();
        if let Err(error) = self
            .repository
            .activate_cert(&operation_id, active_cert.clone())
            .await
        {
            let failure = active_commit_failure(active_cert.clone(), error);
            return Err(self
                .record_failure(&operation_id, &cert_id, failure, retained.as_ref())
                .await);
        }
        let _ = self.client.publish(INTENT_CHANGED, Vec::new().into()).await;
        Ok(active_cert)
    }

    async fn submit_operation(
        &self,
        operation_id: &OperationId,
        cert_id: CertId,
    ) -> Result<(), CertificateProvisionFailure> {
        self.repository
            .submit_cert(CertOperationSubmission {
                operation_id: operation_id.clone(),
                cert_id,
            })
            .await
            .map_err(
                |error| CertificateProvisionFailure::OperationEvidenceWrite {
                    message: failure_message(format!("{error:?}")),
                },
            )
    }

    async fn push_bundle(
        &self,
        operation_id: &OperationId,
        targets: &[GatewayCertificateTarget],
        bundle: &CustomCertBundle,
    ) -> Result<(), CertificateProvisionFailure> {
        let machine_ids = targets
            .iter()
            .map(|target| target.machine_id.clone())
            .collect::<Vec<_>>();
        self.gateway_client
            .push_bundle(operation_id, &machine_ids, bundle)
            .await
    }

    async fn dns_preflight(
        &self,
        hostname: &RouteHostname,
        targets: &[GatewayCertificateTarget],
    ) -> Result<Vec<MachineId>, CertificateProvisionFailure> {
        let expected_gateway_ips = targets
            .iter()
            .flat_map(|target| target.public_ips.iter().copied())
            .collect::<Vec<_>>();
        if expected_gateway_ips.is_empty() {
            return Err(dns_failure(format!(
                "{} cannot be checked because no expected gateway IPs are known",
                hostname.as_str()
            )));
        }
        let resolved = tokio::time::timeout(
            DNS_TIMEOUT,
            tokio::net::lookup_host((hostname.as_str(), 0)),
        )
        .await
        .map_err(|_| {
            dns_failure(format!(
                "failed to resolve A/AAAA records for {}: DNS resolution timed out after {}ms",
                hostname.as_str(),
                DNS_TIMEOUT.as_millis()
            ))
        })?
        .map_err(|error| {
            dns_failure(format!(
                "failed to resolve A/AAAA records for {}: {error}",
                hostname.as_str()
            ))
        })?;
        let mut resolved_ips = resolved.map(|address| address.ip()).collect::<Vec<_>>();
        resolved_ips.sort_unstable();
        resolved_ips.dedup();
        if !dns_answers_are_gateway_subset(&resolved_ips, &expected_gateway_ips) {
            let mut expected_gateway_ips = expected_gateway_ips;
            expected_gateway_ips.sort_unstable();
            expected_gateway_ips.dedup();
            return Err(dns_failure(format!(
                "{} resolves to {resolved_ips:?}, which is not a non-empty subset of known gateway IPs {expected_gateway_ips:?}",
                hostname.as_str()
            )));
        }
        let mut addressed = targets
            .iter()
            .filter(|target| {
                target
                    .public_ips
                    .iter()
                    .any(|address| resolved_ips.contains(address))
            })
            .map(|target| target.machine_id.clone())
            .collect::<Vec<_>>();
        addressed.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        addressed.dedup();
        Ok(addressed)
    }

    async fn record_failure(
        &self,
        operation_id: &OperationId,
        cert_id: &CertId,
        failure: CertificateProvisionFailure,
        retained_active_cert: Option<&ActiveCertState>,
    ) -> CertificateProvisionFailure {
        let evidence = match CertOperationFailure::try_new(
            cert_id.clone(),
            failure.clone(),
            retained_active_cert.cloned(),
        ) {
            Ok(evidence) => evidence,
            Err(error) => {
                return CertificateProvisionFailure::OperationEvidenceWrite {
                    message: failure_message(error.to_string()),
                };
            }
        };
        match self
            .repository
            .record_cert_failed(operation_id, evidence)
            .await
        {
            Ok(_) => failure,
            Err(error) => CertificateProvisionFailure::OperationEvidenceWrite {
                message: failure_message(error.to_string()),
            },
        }
    }
}

fn cert_id_for_hostname(hostname: &RouteHostname) -> CertId {
    CertId::try_new(format!("cert_{}", hostname.as_str().replace('.', "_")))
        .expect("validated route hostnames render as certificate subject tokens")
}

fn cert_operation_id() -> Result<OperationId, CertificateProvisionFailure> {
    OperationId::try_new(format!("op_cert_{}", nuid::next())).map_err(|error| {
        CertificateProvisionFailure::OperationEvidenceWrite {
            message: failure_message(error.to_string()),
        }
    })
}

enum IssueRequest {
    Ensure { owner_operation_id: OperationId },
    Renew(ActiveCertState),
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

fn provision_failure_from_issuer(error: AcmeIssuerError) -> CertificateProvisionFailure {
    match error {
        AcmeIssuerError::OperationEvidenceWrite { message } => {
            CertificateProvisionFailure::OperationEvidenceWrite {
                message: failure_message(message),
            }
        }
        AcmeIssuerError::ChallengePublish { message } => {
            CertificateProvisionFailure::ChallengePublish {
                message: failure_message(message),
            }
        }
        AcmeIssuerError::ChallengeReadiness {
            missing_machine_ids,
        } => CertificateProvisionFailure::ChallengeReadiness {
            missing_machine_ids,
        },
        AcmeIssuerError::Validation { message } => CertificateProvisionFailure::AcmeValidation {
            message: failure_message(message),
        },
    }
}

fn dns_failure(message: impl Into<String>) -> CertificateProvisionFailure {
    CertificateProvisionFailure::DnsPreflight {
        message: failure_message(message),
    }
}

fn active_commit_failure(
    attempted_active_cert: ActiveCertState,
    error: impl std::fmt::Display,
) -> CertificateProvisionFailure {
    CertificateProvisionFailure::ActiveCertCommit {
        attempted_active_cert,
        message: failure_message(error.to_string()),
    }
}

fn active_commit_without_attempt(error: impl std::fmt::Display) -> CertificateProvisionFailure {
    CertificateProvisionFailure::OperationEvidenceWrite {
        message: failure_message(error.to_string()),
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message.into()).expect("generated certificate failure is non-empty")
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
