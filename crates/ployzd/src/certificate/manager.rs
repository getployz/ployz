use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_core::certificate::{ActiveCertState, CustomCertBundle, ManagedCertBundle};
use ployz_core::ids::{CertId, MachineId, OperationId};
use ployz_core::ingress::{ActiveCertificateMetadata, CertificateOwner};
use ployz_core::operation::{
    CertOperationFailure, CertificateProvisionFailure, CertificateProvisionWarning, FailureMessage,
    RouteHostname,
};

use super::GatewayCertificateTarget;
use super::gateway::GatewayCertificateClient;
use super::issuer::{AcmeIssueContext, AcmeIssuer, AcmeIssuerError, InstantAcmeIssuer};
use super::material::{
    load_custom_certificate, prepare_custom_certificate, prepare_ployz_wildcard_certificate,
    validate_custom_certificate_for_activation, write_custom_certificate,
};
use crate::control::intent::certificate_intent::CertificateIntentStore;
use crate::control::operation_evidence::{CertOperationSubmission, OperationRepository};
use crate::control::store::CoreStore;
use crate::tasks::TaskSpawner;

pub const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
const DNS_TIMEOUT: Duration = Duration::from_secs(10);
const ISSUE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const CHALLENGE_READINESS_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
enum CertificateTaskOwner {
    /// Standalone managers leave bounded issuance owned by the current runtime.
    Runtime,
    /// The control process admits issuance to its bounded shutdown lifecycle.
    Control(TaskSpawner),
}

impl CertificateTaskOwner {
    fn spawn<Build, Future>(&self, build: Build) -> Result<(), crate::tasks::TaskAdmissionError>
    where
        Build: FnOnce() -> Future,
        Future: std::future::Future<Output = ()> + Send + 'static,
    {
        match self {
            Self::Runtime => {
                tokio::spawn(build());
                Ok(())
            }
            Self::Control(tasks) => tasks.spawn(build),
        }
    }
}

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
    issuance_tasks: CertificateTaskOwner,
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
            issuance_tasks: CertificateTaskOwner::Runtime,
        }
    }

    #[must_use]
    pub fn with_task_spawner(mut self, tasks: TaskSpawner) -> Self {
        self.issuance_tasks = CertificateTaskOwner::Control(tasks);
        self
    }

    pub async fn ensure(
        &self,
        owner_operation_id: &OperationId,
        owner: CertificateOwner,
        hostname: &RouteHostname,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let preflight = self.dns_preflight(hostname, targets).await?;
        self.spawn_issue(
            hostname.clone(),
            owner,
            targets.to_vec(),
            Some(preflight),
            IssueRequest::Ensure {
                owner_operation_id: owner_operation_id.clone(),
            },
        )
        .await
    }

    pub async fn ensure_ployz_wildcard(
        &self,
        owner_operation_id: &OperationId,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let certificate = self
            .store
            .active_for_owner(&CertificateOwner::PloyzAutomaticNamespace)
            .await
            .map_err(active_commit_without_attempt)?
            .ok_or_else(|| CertificateProvisionFailure::AcmeValidation {
                message: failure_message("Ployz automatic wildcard certificate is not active"),
            })?;
        validate_custom_certificate_for_activation(&certificate.active, (self.now_seconds)())
            .map_err(acme_validation_failure)?;
        let bundle = load_custom_certificate(&self.state_dir, &certificate.active)
            .map_err(acme_validation_failure)?;
        self.push_bundle(owner_operation_id, targets, &bundle)
            .await?;
        Ok(certificate.active)
    }

    pub(crate) async fn renew(
        &self,
        certificate: ActiveCertificateMetadata,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let CertificateOwner::RouteBinding { .. } = &certificate.owner else {
            return Err(CertificateProvisionFailure::AcmeValidation {
                message: failure_message("only exact route certificates can be renewed"),
            });
        };
        let hostname = certificate.active.hostname.clone();
        let owner = certificate.owner.clone();
        self.spawn_issue(
            hostname,
            owner,
            targets.to_vec(),
            None,
            IssueRequest::Renew(certificate),
        )
        .await
    }

    pub async fn install_ployz_wildcard(
        &self,
        worker_bundle: ManagedCertBundle,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let _guard = self.issuance_lock.lock().await;
        let retained = self
            .store
            .active_for_owner(&CertificateOwner::PloyzAutomaticNamespace)
            .await
            .map_err(active_commit_without_attempt)?;
        let cert_id = CertId::try_new(format!("cert_ployz_{}", worker_bundle.lease.as_str()))
            .map_err(active_commit_without_attempt)?;
        let operation_id = cert_operation_id()?;
        self.submit_operation(&operation_id, cert_id.clone())
            .await?;
        let activation = prepare_ployz_wildcard_certificate(&self.state_dir, worker_bundle)
            .map(|(metadata, bundle)| CertificateActivation::InstallNew { metadata, bundle })
            .map_err(acme_validation_failure);
        self.activate_or_record_failure(
            &operation_id,
            &cert_id,
            retained.as_ref().map(|metadata| &metadata.active),
            targets,
            activation,
        )
        .await
    }

    pub(crate) async fn synchronize(
        &self,
        certificate: ActiveCertificateMetadata,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let _guard = self.issuance_lock.lock().await;
        let active = certificate.active.clone();
        let operation_id = cert_operation_id()?;
        self.submit_operation(&operation_id, active.cert_id.clone())
            .await?;
        let activation = load_custom_certificate(&self.state_dir, &active)
            .map(|bundle| CertificateActivation::SynchronizeStored {
                metadata: certificate,
                bundle,
            })
            .map_err(acme_validation_failure);
        self.activate_or_record_failure(
            &operation_id,
            &active.cert_id,
            Some(&active),
            targets,
            activation,
        )
        .await
    }

    pub(crate) async fn artifact_status(
        &self,
        certificates: &[ActiveCertificateMetadata],
        targets: &[GatewayCertificateTarget],
    ) -> Vec<(MachineId, Result<Vec<CertId>, FailureMessage>)> {
        let desired = certificates
            .iter()
            .map(|certificate| certificate.active.clone())
            .collect::<Vec<_>>();
        let machine_ids = targets
            .iter()
            .map(|target| target.machine_id.clone())
            .collect::<Vec<_>>();
        self.gateway_client
            .artifact_status(&machine_ids, &desired)
            .await
    }

    pub(crate) const fn store(&self) -> &CertificateIntentStore {
        &self.store
    }

    pub(crate) const fn repository(&self) -> &OperationRepository {
        &self.repository
    }

    async fn spawn_issue(
        &self,
        hostname: RouteHostname,
        owner: CertificateOwner,
        targets: Vec<GatewayCertificateTarget>,
        preflight: Option<DnsPreflightResult>,
        request: IssueRequest,
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let manager = self.clone();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        self.issuance_tasks
            .spawn(|| async move {
                let _guard = manager.issuance_lock.lock().await;
                let current = manager
                    .store
                    .active_for_owner(&owner)
                    .await
                    .map_err(active_commit_without_attempt);
                let result = match (current, request) {
                    (Err(error), _) => Err(error),
                    (Ok(current), IssueRequest::Ensure { owner_operation_id }) => {
                        let reusable = current.as_ref().and_then(|metadata| {
                            let active = &metadata.active;
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
                                    .issue_inner(owner, &hostname, &targets, preflight, current)
                                    .await
                            }
                        }
                    }
                    (Ok(Some(current)), IssueRequest::Renew(expected)) if current != expected => {
                        Ok(current.active)
                    }
                    (Ok(current), IssueRequest::Renew(expected)) => {
                        manager
                            .issue_inner(
                                owner,
                                &hostname,
                                &targets,
                                preflight,
                                current.or(Some(expected)),
                            )
                            .await
                    }
                };
                let _ = result_tx.send(result);
            })
            .map_err(active_commit_without_attempt)?;
        result_rx.await.map_err(active_commit_without_attempt)?
    }

    async fn issue_inner(
        &self,
        owner: CertificateOwner,
        hostname: &RouteHostname,
        targets: &[GatewayCertificateTarget],
        preflight: Option<DnsPreflightResult>,
        retained: Option<ActiveCertificateMetadata>,
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let cert_id = cert_id_for_hostname(hostname);
        let operation_id = cert_operation_id()?;
        self.submit_operation(&operation_id, cert_id.clone())
            .await?;
        let activation = self
            .issue_activation(owner, hostname, targets, preflight, &operation_id, &cert_id)
            .await;
        self.activate_or_record_failure(
            &operation_id,
            &cert_id,
            retained.as_ref().map(|metadata| &metadata.active),
            targets,
            activation,
        )
        .await
    }

    async fn issue_activation(
        &self,
        owner: CertificateOwner,
        hostname: &RouteHostname,
        targets: &[GatewayCertificateTarget],
        preflight: Option<DnsPreflightResult>,
        operation_id: &OperationId,
        cert_id: &CertId,
    ) -> Result<CertificateActivation, CertificateProvisionFailure> {
        let preflight = match preflight {
            Some(preflight) => preflight,
            None => self.dns_preflight(hostname, targets).await?,
        };
        if let Some(warning) = preflight.warning {
            self.repository
                .record_cert_warning(operation_id, cert_id.clone(), warning)
                .await
                .map_err(operation_evidence_failure)?;
        }
        let challenge_machine_ids = preflight.machine_ids;

        let context = AcmeIssueContext::new(
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
        let cleanup_missing_machine_ids = context.clear_challenges(hostname).await;
        if !cleanup_missing_machine_ids.is_empty() {
            self.repository
                .record_cert_warning(
                    operation_id,
                    cert_id.clone(),
                    CertificateProvisionWarning::ChallengeCleanupIncomplete {
                        missing_machine_ids: cleanup_missing_machine_ids,
                    },
                )
                .await
                .map_err(operation_evidence_failure)?;
        }
        let issued = issued?;

        let bundle = prepare_custom_certificate(
            &self.state_dir,
            cert_id.clone(),
            hostname.clone(),
            issued.certificate_chain_pem,
            issued.private_key_pem,
        )
        .map_err(acme_validation_failure)?;
        let active_cert = bundle.active_cert().clone();
        Ok(CertificateActivation::InstallNew {
            metadata: ActiveCertificateMetadata {
                owner,
                active: active_cert,
            },
            bundle,
        })
    }

    async fn activate_or_record_failure(
        &self,
        operation_id: &OperationId,
        cert_id: &CertId,
        retained_active_cert: Option<&ActiveCertState>,
        targets: &[GatewayCertificateTarget],
        activation: Result<CertificateActivation, CertificateProvisionFailure>,
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let result = match activation {
            Ok(activation) => self.activate(operation_id, targets, activation).await,
            Err(failure) => Err(failure),
        };
        match result {
            Ok(active) => Ok(active),
            Err(failure) => Err(self
                .record_failure(operation_id, cert_id, failure, retained_active_cert)
                .await),
        }
    }

    async fn activate(
        &self,
        operation_id: &OperationId,
        targets: &[GatewayCertificateTarget],
        activation: CertificateActivation,
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        let (metadata, bundle) = match &activation {
            CertificateActivation::InstallNew { metadata, bundle }
            | CertificateActivation::SynchronizeStored { metadata, bundle } => (metadata, bundle),
        };
        validate_custom_certificate_for_activation(bundle.active_cert(), (self.now_seconds)())
            .map_err(acme_validation_failure)?;
        if let CertificateActivation::InstallNew { bundle, .. } = &activation {
            write_custom_certificate(&self.state_dir, bundle)
                .map_err(|error| active_commit_failure(bundle.active_cert().clone(), error))?;
        }
        self.push_bundle(operation_id, targets, bundle).await?;
        let active = metadata.active.clone();
        self.repository
            .activate_cert(operation_id, metadata.clone())
            .await
            .map_err(|error| active_commit_failure(active.clone(), error))?;
        if matches!(activation, CertificateActivation::InstallNew { .. }) {
            crate::control::operations::publish_intent_changed(
                &self.client,
                "certificate-activation",
            )
            .await;
        }
        Ok(active)
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
    ) -> Result<DnsPreflightResult, CertificateProvisionFailure> {
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
            let mut machine_ids = targets
                .iter()
                .map(|target| target.machine_id.clone())
                .collect::<Vec<_>>();
            machine_ids.sort_by(|left, right| left.as_str().cmp(right.as_str()));
            machine_ids.dedup();
            return Ok(DnsPreflightResult {
                machine_ids,
                warning: Some(CertificateProvisionWarning::DnsPreflightMismatch {
                    message: failure_message(format!(
                        "{} resolves to {resolved_ips:?}, which is not a non-empty subset of known gateway IPs {expected_gateway_ips:?}",
                        hostname.as_str()
                    )),
                }),
            });
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
        Ok(DnsPreflightResult {
            machine_ids: addressed,
            warning: None,
        })
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
    Renew(ActiveCertificateMetadata),
}

enum CertificateActivation {
    InstallNew {
        metadata: ActiveCertificateMetadata,
        bundle: CustomCertBundle,
    },
    SynchronizeStored {
        metadata: ActiveCertificateMetadata,
        bundle: CustomCertBundle,
    },
}

struct DnsPreflightResult {
    machine_ids: Vec<MachineId>,
    warning: Option<CertificateProvisionWarning>,
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

fn acme_validation_failure(error: impl std::fmt::Display) -> CertificateProvisionFailure {
    CertificateProvisionFailure::AcmeValidation {
        message: failure_message(error.to_string()),
    }
}

fn operation_evidence_failure(error: impl std::fmt::Debug) -> CertificateProvisionFailure {
    CertificateProvisionFailure::OperationEvidenceWrite {
        message: failure_message(format!("{error:?}")),
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
