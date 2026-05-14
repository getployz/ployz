use super::STUCK_ISSUING_MAX_AGE_SECS;
use super::orders::start_pending_orders_checked;
use ployz_cert_acme_api::{AcmeIssuer, IssuanceCoordinator};
use ployz_error::{Error, Result};
use ployz_model::{
    CertificateState, CertificateStateGoal, CertificateStateTransition,
    CertificateTransitionEvidence,
};
use ployz_store_api::{CertificateStore, StoreDriver};
use ployz_time::now_unix_secs;
use std::future::Future;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

pub struct CertificateRenewalTask {
    cancel: CancellationToken,
    task: JoinHandle<()>,
    name: &'static str,
}

impl CertificateRenewalTask {
    pub fn spawn<F>(name: &'static str, run: impl FnOnce(CancellationToken) -> F) -> Self
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let task = tokio::spawn(run(task_cancel));
        Self { cancel, task, name }
    }

    pub async fn shutdown(self) {
        self.cancel.cancel();
        if let Err(error) = self.task.await {
            tracing::warn!(
                ?error,
                task = self.name,
                "certificate renewal task failed during shutdown"
            );
        }
    }
}

/// Process one certificate renewal job. This is the unit of work for NATS
/// `cert_jobs` delivery: the job names a hostname, this function re-reads the
/// authoritative certificate record, applies wall-clock state transitions, then
/// starts an ACME order if that record still needs one.
pub async fn process_renewal_job<I, C>(
    store: &StoreDriver,
    issuer: &I,
    coordinator: &C,
    hostname: &str,
) -> Result<()>
where
    I: AcmeIssuer + Sync + ?Sized,
    C: IssuanceCoordinator + ?Sized,
{
    let Some(mut record) = store.get_certificate(hostname).await? else {
        tracing::info!(hostname, "certificate renewal job skipped missing record");
        return Ok(());
    };
    let now = now_unix_secs();
    let due = match record.state() {
        CertificateState::Active => {
            let Some(threshold) = record.next_renewal_at() else {
                return Ok(());
            };
            if now < threshold {
                return Ok(());
            }
            record
                .apply_state_transition(CertificateStateTransition {
                    goal: CertificateStateGoal::MarkRenewalDue,
                    evidence: CertificateTransitionEvidence::RenewalScheduler {
                        hostname: hostname.to_string(),
                    },
                    at_unix_secs: now,
                })
                .map_err(|error| {
                    Error::operation(
                        "certificate_transition",
                        format!("mark renewal due {hostname}: {error}"),
                    )
                })?;
            store.upsert_certificate(&record).await?;
            true
        }
        CertificateState::Issuing => {
            if now.saturating_sub(record.updated_at) < STUCK_ISSUING_MAX_AGE_SECS {
                return Ok(());
            }
            record
                .apply_state_transition(CertificateStateTransition {
                    goal: CertificateStateGoal::ResetStalledIssuing {
                        error: "previous order stalled; re-ordering".into(),
                    },
                    evidence: CertificateTransitionEvidence::RenewalScheduler {
                        hostname: hostname.to_string(),
                    },
                    at_unix_secs: now,
                })
                .map_err(|error| {
                    Error::operation(
                        "certificate_transition",
                        format!("reset stalled issuing {hostname}: {error}"),
                    )
                })?;
            store.upsert_certificate(&record).await?;
            true
        }
        CertificateState::Pending | CertificateState::Failed | CertificateState::RenewalDue => true,
    };
    if !due {
        return Ok(());
    }
    for warning in
        start_pending_orders_checked(store, issuer, coordinator, &[record.hostname]).await?
    {
        tracing::warn!(%warning, "certificate renewal");
    }
    Ok(())
}
