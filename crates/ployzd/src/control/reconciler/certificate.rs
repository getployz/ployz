use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_core::certificate::ActiveCertState;
use ployz_core::ingress::{ActiveCertificateMetadata, CertificateOwner};
use ployz_core::operation::{
    CertOperationFailure, CertificateProvisionFailure, FailureMessage, OperationStatus,
};
use ployz_core::roles::GatewayRole;

use crate::certificate::{
    CertificateManager, GatewayCertificateTarget, gateway_certificate_targets,
};
use crate::control::intent::ingress_intent::{PloyzDnsTargetAllocation, PloyzDnsTargetStore};
use crate::control::intent::machine_roster::MachineRosterStore;
use crate::control::operation_evidence::{OperationStatusStoreError, RecordCertTransitionError};
use crate::control::role_client::machine::{NatsMachineFactsReader, read_machine_placement_facts};
use crate::control::store::CoreStoreError;
use crate::lease::{BundleDownloadOutcome, LeaseClient, LeaseClientError};
use crate::tasks::TaskRegistry;

pub const CERTIFICATE_RENEWAL_TICK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const CERTIFICATE_SYNC_TICK_INTERVAL: Duration = Duration::from_secs(60);
const PLOYZ_WILDCARD_PENDING_INTERVAL: Duration = Duration::from_secs(60);
const CERTIFICATE_FAILURE_BACKOFF_BASE: Duration = Duration::from_secs(5);
const CERTIFICATE_RENEWAL_BACKOFF_CAP: Duration = Duration::from_secs(6 * 60 * 60);

#[must_use]
pub fn start_certificate_renewal_task(
    registry: &TaskRegistry,
    manager: CertificateManager,
    facts_reader: NatsMachineFactsReader,
    roster: MachineRosterStore,
    ployz_dns_target: PloyzDnsTargetStore,
    worker: LeaseClient,
    wake: tokio::sync::mpsc::Receiver<()>,
) -> CertificateRenewalHealth {
    let health = CertificateRenewalHealth::default();
    registry.spawn(run_loop(
        manager,
        facts_reader,
        roster,
        ployz_dns_target,
        worker,
        wake,
        health.clone(),
    ));
    health
}

async fn run_loop(
    manager: CertificateManager,
    facts_reader: NatsMachineFactsReader,
    roster: MachineRosterStore,
    ployz_dns_target: PloyzDnsTargetStore,
    worker: LeaseClient,
    mut wake: tokio::sync::mpsc::Receiver<()>,
    health: CertificateRenewalHealth,
) {
    if let Err(error) = recover_unfinished_operations(&manager).await {
        eprintln!("ployzd certificate recovery warning: {error}");
        record_renewal_attempt(&health, &Err(error));
    }
    loop {
        let outcome = run_once_with_roster_at(
            &manager,
            &facts_reader,
            &roster,
            &ployz_dns_target,
            &worker,
            now_seconds(),
        )
        .await;
        if let Err(error) = &outcome {
            eprintln!("ployzd certificate renewal warning: {error}");
        }
        let delay = record_renewal_attempt(&health, &outcome);
        tokio::select! {
            _ = tokio::time::sleep(delay) => {}
            received = wake.recv() => {
                if received.is_none() {
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
}

fn record_renewal_attempt(
    health: &CertificateRenewalHealth,
    outcome: &Result<CertificateRenewalOutcome, CertificateRenewalTaskError>,
) -> Duration {
    let mut state = health
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match outcome {
        Ok(outcome @ CertificateRenewalOutcome::AwaitingPloyzWildcard) => {
            state.last_attempt = Some(CertificateRenewalAttempt::Completed { outcome: *outcome });
            state.consecutive_failures = 0;
            PLOYZ_WILDCARD_PENDING_INTERVAL
        }
        Ok(outcome @ CertificateRenewalOutcome::NoAction) => {
            state.last_attempt = Some(CertificateRenewalAttempt::Completed { outcome: *outcome });
            state.consecutive_failures = 0;
            CERTIFICATE_RENEWAL_TICK_INTERVAL
        }
        Ok(outcome @ CertificateRenewalOutcome::Attempted { failed: 0, .. }) => {
            state.last_attempt = Some(CertificateRenewalAttempt::Completed { outcome: *outcome });
            state.consecutive_failures = 0;
            CERTIFICATE_SYNC_TICK_INTERVAL
        }
        Ok(outcome @ CertificateRenewalOutcome::Attempted { failed, .. }) => {
            state.last_attempt = Some(CertificateRenewalAttempt::Completed { outcome: *outcome });
            state.consecutive_failures = state
                .consecutive_failures
                .saturating_add(u64::try_from(*failed).unwrap_or(u64::MAX));
            failure_delay(u32::try_from(state.consecutive_failures).unwrap_or(u32::MAX))
        }
        Err(error) => {
            state.last_attempt = Some(CertificateRenewalAttempt::Failed {
                failure: CertificateRenewalHealthFailure::from(error),
            });
            state.consecutive_failures = state.consecutive_failures.saturating_add(1);
            failure_delay(u32::try_from(state.consecutive_failures).unwrap_or(u32::MAX))
        }
    }
}

#[cfg(test)]
pub async fn run_once_at(
    manager: &CertificateManager,
    targets: &[GatewayCertificateTarget],
    now_seconds: u64,
) -> Result<CertificateRenewalOutcome, CertificateRenewalTaskError> {
    let active = manager.store().active_certificates().await?;
    if active.is_empty() {
        return Ok(CertificateRenewalOutcome::NoAction);
    }
    Ok(run_certificate_work(manager, active, None, targets, now_seconds).await)
}

async fn run_certificate_work(
    manager: &CertificateManager,
    active: Vec<ActiveCertificateMetadata>,
    wildcard_bundle: Option<BundleDownloadOutcome>,
    targets: &[GatewayCertificateTarget],
    now_seconds: u64,
) -> CertificateRenewalOutcome {
    let mut attempted = 0;
    let mut failed = 0;
    let mut wildcard_handled = false;
    let mut wildcard_pending = false;
    match wildcard_bundle {
        Some(BundleDownloadOutcome::Ready(bundle)) => {
            let already_active = active.iter().any(|certificate| {
                matches!(certificate.owner, CertificateOwner::PloyzAutomaticNamespace)
                    && certificate
                        .active
                        .bundle_ref
                        .artifact_parts()
                        .is_ok_and(|(digest, _)| digest == bundle.digest)
            });
            if !already_active {
                attempted += 1;
                wildcard_handled = true;
                if manager
                    .install_ployz_wildcard(bundle, targets)
                    .await
                    .is_err()
                {
                    failed += 1;
                }
            }
        }
        Some(BundleDownloadOutcome::Pending { .. }) => wildcard_pending = true,
        None => {}
    }
    for certificate in active {
        if wildcard_handled
            && matches!(certificate.owner, CertificateOwner::PloyzAutomaticNamespace)
        {
            continue;
        }
        attempted += 1;
        let result = if certificate.active.needs_renewal(now_seconds)
            && matches!(certificate.owner, CertificateOwner::RouteBinding { .. })
        {
            manager.renew(certificate, targets).await
        } else {
            manager.synchronize(certificate, targets).await
        };
        if result.is_err() {
            failed += 1;
        }
    }
    if attempted == 0 && wildcard_pending {
        CertificateRenewalOutcome::AwaitingPloyzWildcard
    } else if attempted == 0 {
        CertificateRenewalOutcome::NoAction
    } else {
        CertificateRenewalOutcome::Attempted { attempted, failed }
    }
}

async fn run_once_with_roster_at(
    manager: &CertificateManager,
    facts_reader: &NatsMachineFactsReader,
    roster: &MachineRosterStore,
    ployz_dns_target: &PloyzDnsTargetStore,
    worker: &LeaseClient,
    now_seconds: u64,
) -> Result<CertificateRenewalOutcome, CertificateRenewalTaskError> {
    let active = manager.store().active_certificates().await?;
    let allocation = ployz_dns_target.load_allocation().await?;
    if active.is_empty() && allocation.is_none() {
        return Ok(CertificateRenewalOutcome::NoAction);
    }
    let active_machines = match roster.active_machines().await {
        Ok(active_machines) => active_machines,
        Err(error) => {
            return record_roster_failure(manager, active, error.to_string()).await;
        }
    };
    let gateway_machines = active_machines
        .iter()
        .filter(|machine| matches!(machine.roles.gateway, GatewayRole::Install))
        .map(|machine| (machine.machine_id.clone(), machine.lifecycle))
        .collect::<Vec<_>>();
    let placement_facts = read_machine_placement_facts(facts_reader, gateway_machines).await;
    let targets = gateway_certificate_targets(&active_machines, &placement_facts);
    let wildcard_bundle = match allocation {
        Some(PloyzDnsTargetAllocation::Allocated { lease }) => Some(
            worker
                .download_bundle(lease.name, lease.token)
                .await
                .map_err(CertificateRenewalTaskError::Worker)?,
        ),
        Some(PloyzDnsTargetAllocation::Unacquired { .. }) | None => None,
    };
    Ok(run_certificate_work(manager, active, wildcard_bundle, &targets, now_seconds).await)
}

async fn record_roster_failure(
    manager: &CertificateManager,
    active: Vec<ActiveCertificateMetadata>,
    message: String,
) -> Result<CertificateRenewalOutcome, CertificateRenewalTaskError> {
    let attempted = active.len();
    let failure = CertificateProvisionFailure::DnsPreflight {
        message: FailureMessage::try_new(format!(
            "gateway roster unavailable before certificate renewal: {message}"
        ))
        .expect("generated roster failure is non-empty"),
    };
    let mut evidence_error = None;
    for active in active {
        if let Err(error) = manager
            .record_renewal_failure(active.active, failure.clone())
            .await
        {
            evidence_error.get_or_insert(error);
        }
    }
    if let Some(error) = evidence_error {
        return Err(CertificateRenewalTaskError::RenewalEvidence(error));
    }
    Ok(CertificateRenewalOutcome::Attempted {
        attempted,
        failed: attempted,
    })
}

pub async fn recover_unfinished_operations(
    manager: &CertificateManager,
) -> Result<(), CertificateRenewalTaskError> {
    let statuses = manager.repository().unfinished_cert_operations().await?;
    if statuses.is_empty() {
        return Ok(());
    }
    for status in statuses {
        let OperationStatus::Cert { id, cert_id, .. } = status else {
            continue;
        };
        let retained_active_cert = manager.store().active_for_cert_id(&cert_id).await?;
        let failure = CertificateProvisionFailure::AcmeValidation {
            message: FailureMessage::try_new("certificate task restarted before terminal evidence")
                .expect("static certificate recovery failure is non-empty"),
        };
        let failure = recovery_failure(
            cert_id,
            failure,
            retained_active_cert.map(|metadata| metadata.active),
        );
        manager
            .repository()
            .record_cert_failed(&id, failure)
            .await?;
    }
    Ok(())
}

fn recovery_failure(
    cert_id: ployz_core::ids::CertId,
    failure: CertificateProvisionFailure,
    retained_active_cert: Option<ActiveCertState>,
) -> CertOperationFailure {
    CertOperationFailure::try_new(cert_id, failure, retained_active_cert)
        .expect("recovered active certificate identity matches its operation")
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn failure_delay(consecutive_failures: u32) -> Duration {
    CERTIFICATE_FAILURE_BACKOFF_BASE
        .saturating_mul(2_u32.saturating_pow(consecutive_failures.min(8)))
        .min(CERTIFICATE_RENEWAL_BACKOFF_CAP)
}

#[derive(Debug, Clone, Default)]
pub struct CertificateRenewalHealth {
    state: Arc<Mutex<CertificateRenewalHealthState>>,
}

impl CertificateRenewalHealth {
    #[must_use]
    #[cfg(test)]
    pub fn snapshot(&self) -> CertificateRenewalHealthState {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CertificateRenewalHealthState {
    pub last_attempt: Option<CertificateRenewalAttempt>,
    pub consecutive_failures: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateRenewalAttempt {
    Completed {
        outcome: CertificateRenewalOutcome,
    },
    Failed {
        failure: CertificateRenewalHealthFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateRenewalHealthFailure {
    IntentStore {
        message: String,
    },
    RenewalEvidence {
        failure: CertificateProvisionFailure,
    },
    OperationStatus {
        message: String,
    },
    OperationEvidence {
        message: String,
    },
    PloyzDnsTarget {
        message: String,
    },
    Worker {
        message: String,
    },
}

impl From<&CertificateRenewalTaskError> for CertificateRenewalHealthFailure {
    fn from(error: &CertificateRenewalTaskError) -> Self {
        match error {
            CertificateRenewalTaskError::Intent(error) => Self::IntentStore {
                message: error.to_string(),
            },
            CertificateRenewalTaskError::RenewalEvidence(failure) => Self::RenewalEvidence {
                failure: failure.clone(),
            },
            CertificateRenewalTaskError::OperationStatus(error) => Self::OperationStatus {
                message: error.to_string(),
            },
            CertificateRenewalTaskError::OperationEvidence(error) => Self::OperationEvidence {
                message: error.to_string(),
            },
            CertificateRenewalTaskError::PloyzDnsTarget(error) => Self::PloyzDnsTarget {
                message: error.to_string(),
            },
            CertificateRenewalTaskError::Worker(error) => Self::Worker {
                message: error.to_string(),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateRenewalOutcome {
    NoAction,
    AwaitingPloyzWildcard,
    Attempted { attempted: usize, failed: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum CertificateRenewalTaskError {
    #[error("certificate intent: {0}")]
    Intent(#[from] crate::control::intent::certificate_intent::CertificateIntentStoreError),
    #[error("Ployz DNS target store: {0}")]
    PloyzDnsTarget(#[from] CoreStoreError),
    #[error("Ployz wildcard issuer adapter: {0}")]
    Worker(LeaseClientError),
    #[error("certificate renewal evidence: {0:?}")]
    RenewalEvidence(CertificateProvisionFailure),
    #[error("certificate operation status: {0}")]
    OperationStatus(#[from] OperationStatusStoreError),
    #[error("certificate operation evidence: {0}")]
    OperationEvidence(#[from] RecordCertTransitionError),
}

#[cfg(test)]
mod tests {
    use super::{
        CERTIFICATE_RENEWAL_TICK_INTERVAL, CertificateRenewalAttempt, CertificateRenewalHealth,
        CertificateRenewalHealthFailure, CertificateRenewalOutcome, CertificateRenewalTaskError,
        record_renewal_attempt, recovery_failure,
    };
    use ployz_core::certificate::{
        ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow,
    };
    use ployz_core::ids::CertId;
    use ployz_core::ingress::{ActiveCertificateMetadata, CertificateOwner};
    use ployz_core::operation::{CertificateProvisionFailure, FailureMessage, RouteHostname};

    #[test]
    fn renewal_becomes_due_at_two_thirds_of_validity() {
        let active = active_certificate(3, 9);
        let metadata = ActiveCertificateMetadata {
            owner: CertificateOwner::PloyzAutomaticNamespace,
            active,
        };

        assert!(!metadata.active.needs_renewal(6));
        assert!(metadata.active.needs_renewal(7));
    }

    #[test]
    fn recovery_failure_keeps_matching_active_metadata() {
        let active = active_certificate(3, 9);
        let failure = CertificateProvisionFailure::AcmeValidation {
            message: FailureMessage::try_new("control process restarted")
                .expect("valid failure message"),
        };

        let recovered = recovery_failure(
            active.cert_id.clone(),
            failure.clone(),
            Some(active.clone()),
        );

        assert_eq!(recovered.failure(), &failure);
        assert_eq!(recovered.retained_active_cert(), Some(&active));
    }

    #[test]
    fn renewal_health_records_success_degradation_and_store_scan_failure() {
        let health = CertificateRenewalHealth::default();
        let cloned = health.clone();

        assert_eq!(
            record_renewal_attempt(&health, &Ok(CertificateRenewalOutcome::NoAction)),
            CERTIFICATE_RENEWAL_TICK_INTERVAL
        );
        assert_eq!(
            cloned.snapshot().last_attempt,
            Some(CertificateRenewalAttempt::Completed {
                outcome: CertificateRenewalOutcome::NoAction,
            })
        );
        assert_eq!(cloned.snapshot().consecutive_failures, 0);

        record_renewal_attempt(
            &health,
            &Ok(CertificateRenewalOutcome::Attempted {
                attempted: 3,
                failed: 2,
            }),
        );
        assert_eq!(cloned.snapshot().consecutive_failures, 2);

        record_renewal_attempt(
            &health,
            &Err(CertificateRenewalTaskError::Intent(
                crate::control::intent::certificate_intent::CertificateIntentStoreError::Store {
                    message: "database unavailable".to_owned(),
                },
            )),
        );
        assert_eq!(cloned.snapshot().consecutive_failures, 3);
        assert!(matches!(
            cloned.snapshot().last_attempt,
            Some(CertificateRenewalAttempt::Failed {
                failure: CertificateRenewalHealthFailure::IntentStore { message },
            }) if message.contains("database unavailable")
        ));
    }

    fn active_certificate(not_before: u64, not_after: u64) -> ActiveCertState {
        ActiveCertState {
            cert_id: CertId::try_new("cert_api").expect("valid cert id"),
            hostname: RouteHostname::try_new("api.example.com").expect("valid hostname"),
            bundle_ref: CertBundleRef::try_new(format!(
                "sha256:{}:/var/lib/ployz/certificates/cert_api.bundle",
                "a".repeat(64)
            ))
            .expect("valid bundle ref"),
            validity: CertValidityWindow::try_new(
                CertValidAt::try_new(not_before).expect("valid not-before"),
                CertValidAt::try_new(not_after).expect("valid not-after"),
            )
            .expect("valid validity"),
        }
    }
}
