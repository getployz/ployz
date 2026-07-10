use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_core::cert::ActiveCertState;
use ployz_core::ops::{
    CertOperationFailure, CertificateProvisionFailure, FailureMessage, OperationStatus,
};
use ployz_core::roles::GatewayRole;

use super::{CertificateManager, GatewayCertificateTarget, gateway_certificate_targets};
use crate::intent::machine_roster::MachineRosterStore;
use crate::operations::log::{OperationStatusStoreError, RecordCertTransitionError};
use crate::roles::machine::client::{NatsMachineFactsReader, read_machine_placement_facts};
use crate::tasks::TaskRegistry;

pub const CERTIFICATE_RENEWAL_TICK_INTERVAL: Duration = Duration::from_secs(60 * 60);
const CERTIFICATE_RENEWAL_BACKOFF_CAP: Duration = Duration::from_secs(6 * 60 * 60);

pub fn start_certificate_renewal_task(
    registry: &TaskRegistry,
    manager: CertificateManager,
    facts_reader: NatsMachineFactsReader,
    roster: MachineRosterStore,
) -> CertificateRenewalHealth {
    let health = CertificateRenewalHealth::default();
    registry.spawn(run_loop(manager, facts_reader, roster, health.clone()));
    health
}

async fn run_loop(
    manager: CertificateManager,
    facts_reader: NatsMachineFactsReader,
    roster: MachineRosterStore,
    health: CertificateRenewalHealth,
) {
    if let Err(error) = recover_unfinished_operations(&manager).await {
        eprintln!("ployzd certificate recovery warning: {error}");
        record_renewal_attempt(&health, &Err(error));
    }
    loop {
        let outcome =
            run_once_with_roster_at(&manager, &facts_reader, &roster, now_seconds()).await;
        if let Err(error) = &outcome {
            eprintln!("ployzd certificate renewal warning: {error}");
        }
        let delay = record_renewal_attempt(&health, &outcome);
        tokio::time::sleep(delay).await;
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
        Ok(outcome @ CertificateRenewalOutcome::NoAction)
        | Ok(outcome @ CertificateRenewalOutcome::Attempted { failed: 0, .. }) => {
            state.last_attempt = Some(CertificateRenewalAttempt::Completed { outcome: *outcome });
            state.consecutive_failures = 0;
            CERTIFICATE_RENEWAL_TICK_INTERVAL
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

pub async fn run_once_at(
    manager: &CertificateManager,
    targets: &[GatewayCertificateTarget],
    now_seconds: u64,
) -> Result<CertificateRenewalOutcome, CertificateRenewalTaskError> {
    let due = due_certificates(manager.store().active_certificates().await?, now_seconds);
    if due.is_empty() {
        return Ok(CertificateRenewalOutcome::NoAction);
    }
    Ok(run_due_renewals(manager, due, targets).await)
}

async fn run_due_renewals(
    manager: &CertificateManager,
    due: Vec<ActiveCertState>,
    targets: &[GatewayCertificateTarget],
) -> CertificateRenewalOutcome {
    let attempted = due.len();
    let mut failed = 0;
    for active in due {
        if manager.renew(active, targets).await.is_err() {
            failed += 1;
        }
    }
    CertificateRenewalOutcome::Attempted { attempted, failed }
}

async fn run_once_with_roster_at(
    manager: &CertificateManager,
    facts_reader: &NatsMachineFactsReader,
    roster: &MachineRosterStore,
    now_seconds: u64,
) -> Result<CertificateRenewalOutcome, CertificateRenewalTaskError> {
    let due = due_certificates(manager.store().active_certificates().await?, now_seconds);
    if due.is_empty() {
        return Ok(CertificateRenewalOutcome::NoAction);
    }
    let active_machines = match roster.active_machines().await {
        Ok(active_machines) => active_machines,
        Err(error) => {
            return record_roster_failure(manager, due, error.to_string()).await;
        }
    };
    let gateway_machines = active_machines
        .iter()
        .filter(|machine| matches!(machine.roles.gateway, GatewayRole::Install))
        .map(|machine| (machine.machine_id.clone(), machine.lifecycle))
        .collect::<Vec<_>>();
    let placement_facts = read_machine_placement_facts(facts_reader, gateway_machines).await;
    let targets = gateway_certificate_targets(&active_machines, &placement_facts);
    Ok(run_due_renewals(manager, due, &targets).await)
}

async fn record_roster_failure(
    manager: &CertificateManager,
    due: Vec<ActiveCertState>,
    message: String,
) -> Result<CertificateRenewalOutcome, CertificateRenewalTaskError> {
    let attempted = due.len();
    let failure = CertificateProvisionFailure::DnsPreflight {
        message: FailureMessage::try_new(format!(
            "gateway roster unavailable before certificate renewal: {message}"
        ))
        .expect("generated roster failure is non-empty"),
    };
    let mut evidence_error = None;
    for active in due {
        if let Err(error) = manager
            .record_renewal_failure(active, failure.clone())
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

fn due_certificates(
    active_certificates: Vec<ActiveCertState>,
    now_seconds: u64,
) -> Vec<ActiveCertState> {
    active_certificates
        .into_iter()
        .filter(|active| active.needs_renewal(now_seconds))
        .collect()
}

pub async fn recover_unfinished_operations(
    manager: &CertificateManager,
) -> Result<(), CertificateRenewalTaskError> {
    let statuses = manager.repository().unfinished_cert_operations().await?;
    if statuses.is_empty() {
        return Ok(());
    }
    let cleanup_error = manager.clear_all_challenges().await.err();
    for status in statuses {
        let OperationStatus::Cert { id, cert_id, .. } = status else {
            continue;
        };
        let retained_active_cert = manager.store().active_for_cert_id(&cert_id).await?;
        let failure = match &cleanup_error {
            Some(error) => error.clone(),
            None => CertificateProvisionFailure::AcmeValidation {
                message: FailureMessage::try_new(
                    "certificate task restarted before terminal evidence",
                )
                .expect("static certificate recovery failure is non-empty"),
            },
        };
        let failure = recovery_failure(cert_id, failure, retained_active_cert);
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
    CERTIFICATE_RENEWAL_TICK_INTERVAL
        .saturating_mul(2_u32.saturating_pow(consecutive_failures.min(8)))
        .min(CERTIFICATE_RENEWAL_BACKOFF_CAP)
}

#[derive(Debug, Clone, Default)]
pub struct CertificateRenewalHealth {
    state: Arc<Mutex<CertificateRenewalHealthState>>,
}

impl CertificateRenewalHealth {
    #[must_use]
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
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateRenewalOutcome {
    NoAction,
    Attempted { attempted: usize, failed: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum CertificateRenewalTaskError {
    #[error("certificate intent: {0}")]
    Intent(#[from] crate::intent::certificate_intent::CertificateIntentStoreError),
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
        due_certificates, record_renewal_attempt, recovery_failure,
    };
    use ployz_core::cert::{ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow};
    use ployz_core::ids::CertId;
    use ployz_core::ops::{CertificateProvisionFailure, FailureMessage, RouteHostname};

    #[test]
    fn renewal_becomes_due_at_two_thirds_of_validity() {
        let active = active_certificate(3, 9);

        assert!(due_certificates(vec![active.clone()], 6).is_empty());
        assert_eq!(due_certificates(vec![active.clone()], 7), [active]);
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
                crate::intent::certificate_intent::CertificateIntentStoreError::Store {
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
