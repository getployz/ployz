use std::net::IpAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ployz_core::ops::{CertOperationFailure, FailureMessage, OperationStatus};

use super::CertificateManager;
use crate::intent::machine_roster::{MachineRosterStore, MachineRosterStoreError};
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
) {
    registry.spawn(run_loop(manager, facts_reader, roster));
}

async fn run_loop(
    manager: CertificateManager,
    facts_reader: NatsMachineFactsReader,
    roster: MachineRosterStore,
) {
    if let Err(error) = recover_unfinished_operations(&manager).await {
        eprintln!("ployzd certificate recovery warning: {error}");
    }
    let mut consecutive_failures = 0_u32;
    loop {
        let outcome =
            run_once_with_roster_at(&manager, &facts_reader, &roster, now_seconds()).await;
        let delay = match outcome {
            Ok(CertificateRenewalOutcome::NoAction)
            | Ok(CertificateRenewalOutcome::Attempted { failed: 0, .. }) => {
                consecutive_failures = 0;
                CERTIFICATE_RENEWAL_TICK_INTERVAL
            }
            Ok(CertificateRenewalOutcome::Attempted { failed, .. }) => {
                consecutive_failures = consecutive_failures.saturating_add(failed as u32);
                failure_delay(consecutive_failures)
            }
            Err(error) => {
                eprintln!("ployzd certificate renewal warning: {error}");
                consecutive_failures = consecutive_failures.saturating_add(1);
                failure_delay(consecutive_failures)
            }
        };
        tokio::time::sleep(delay).await;
    }
}

pub async fn run_once_at(
    manager: &CertificateManager,
    expected_gateway_ips: &[IpAddr],
    now_seconds: u64,
) -> Result<CertificateRenewalOutcome, CertificateRenewalTaskError> {
    let due = manager
        .store()
        .active_certificates()
        .await?
        .into_iter()
        .filter(|bundle| bundle.active_cert.needs_renewal(now_seconds))
        .collect::<Vec<_>>();
    if due.is_empty() {
        return Ok(CertificateRenewalOutcome::NoAction);
    }
    let attempted = due.len();
    let mut failed = 0;
    for active in due {
        if manager.renew(active, expected_gateway_ips).await.is_err() {
            failed += 1;
        }
    }
    Ok(CertificateRenewalOutcome::Attempted { attempted, failed })
}

async fn run_once_with_roster_at(
    manager: &CertificateManager,
    facts_reader: &NatsMachineFactsReader,
    roster: &MachineRosterStore,
    now_seconds: u64,
) -> Result<CertificateRenewalOutcome, CertificateRenewalTaskError> {
    let gateway_machines = roster
        .active_machines()
        .await?
        .into_iter()
        .filter(|machine| {
            matches!(
                machine.roles.gateway,
                ployz_core::roles::GatewayRole::Install
            )
        })
        .map(|machine| (machine.machine_id, machine.lifecycle))
        .collect::<Vec<_>>();
    let mut expected_gateway_ips = read_machine_placement_facts(facts_reader, gateway_machines)
        .await
        .into_iter()
        .filter_map(|facts| facts.endpoints)
        .flat_map(|observation| observation.control_endpoints)
        .collect::<Vec<_>>();
    expected_gateway_ips.sort_unstable();
    expected_gateway_ips.dedup();
    run_once_at(manager, &expected_gateway_ips, now_seconds).await
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
        let retained_active_cert = manager
            .store()
            .active_for_cert_id(&cert_id)
            .await?
            .map(|bundle| bundle.active_cert);
        let failure = match &cleanup_error {
            Some(error) => CertOperationFailure::ChallengePublishFailed {
                cert_id,
                message: error.failure_message(),
            },
            None => CertOperationFailure::AcmeValidationFailed {
                cert_id,
                message: FailureMessage::try_new(
                    "certificate task restarted before terminal evidence",
                )
                .expect("static certificate recovery failure is non-empty"),
                retained_active_cert,
            },
        };
        manager
            .repository()
            .record_cert_failed(&id, failure)
            .await?;
    }
    Ok(())
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateRenewalOutcome {
    NoAction,
    Attempted { attempted: usize, failed: usize },
}

#[derive(Debug, thiserror::Error)]
pub enum CertificateRenewalTaskError {
    #[error("certificate intent: {0}")]
    Intent(#[from] crate::intent::certificate_intent::CertificateIntentStoreError),
    #[error("certificate machine roster: {0}")]
    Roster(#[from] MachineRosterStoreError),
    #[error("certificate operation status: {0}")]
    OperationStatus(#[from] OperationStatusStoreError),
    #[error("certificate operation evidence: {0}")]
    OperationEvidence(#[from] RecordCertTransitionError),
}
