use ployz_cert_api::{AcmeIssuer, IssuanceAcquisition, IssuanceCoordinator};
use ployz_error::{CertificateError, Result};
use ployz_model::{
    CertificateRecord, CertificateState, CertificateStateGoal, CertificateStateTransition,
    CertificateTransitionEvidence,
};
use ployz_store_api::{CertificateStore, StoreDriver};
use ployz_time::now_unix_secs;
use std::collections::BTreeSet;

pub async fn start_pending_orders<I, C>(
    store: &StoreDriver,
    issuer: &I,
    coordinator: &C,
    hostnames: &[String],
) -> Vec<String>
where
    I: AcmeIssuer + Sync + ?Sized,
    C: IssuanceCoordinator + ?Sized,
{
    match start_pending_orders_checked(store, issuer, coordinator, hostnames).await {
        Ok(warnings) => warnings,
        Err(error) => vec![format!(
            "Could not start managed certificate order: {error}"
        )],
    }
}

pub(super) async fn start_pending_orders_checked<I, C>(
    store: &StoreDriver,
    issuer: &I,
    coordinator: &C,
    hostnames: &[String],
) -> Result<Vec<String>>
where
    I: AcmeIssuer + Sync + ?Sized,
    C: IssuanceCoordinator + ?Sized,
{
    let hostnames = hostnames.iter().cloned().collect::<BTreeSet<_>>();
    if hostnames.is_empty() {
        return Ok(Vec::new());
    }
    let mut warnings = Vec::new();
    let records = match store.list_certificates().await {
        Ok(records) => records,
        Err(error) => {
            warnings.push(format!(
                "Could not list managed certificates for ACME order: {error}"
            ));
            return Ok(warnings);
        }
    };
    for record in records {
        if !hostnames.contains(&record.hostname) {
            continue;
        }
        if !needs_start_order(&record) {
            continue;
        }
        if let Some(warning) = start_one(store, issuer, coordinator, record).await? {
            warnings.push(warning);
        }
    }
    Ok(warnings)
}

pub(super) fn needs_start_order(record: &CertificateRecord) -> bool {
    match record.state() {
        CertificateState::Pending | CertificateState::Failed | CertificateState::RenewalDue => true,
        CertificateState::Issuing | CertificateState::Active => false,
    }
}

/// Delete all challenge records for a given hostname. Called immediately before
/// minting a new ACME order so retries from `Failed` don't accumulate dead
/// `(hostname, token)` entries in `acme_challenges`.
async fn prune_acme_challenges_for(store: &StoreDriver, hostname: &str) -> Result<()> {
    let challenges = store.list_acme_challenges().await?;
    for challenge in challenges
        .iter()
        .filter(|challenge| challenge.hostname == hostname)
    {
        store
            .delete_acme_challenge(&challenge.hostname, &challenge.token)
            .await?;
    }
    Ok(())
}

pub(super) async fn start_one<I, C>(
    store: &StoreDriver,
    issuer: &I,
    coordinator: &C,
    record: CertificateRecord,
) -> Result<Option<String>>
where
    I: AcmeIssuer + Sync + ?Sized,
    C: IssuanceCoordinator + ?Sized,
{
    let hostname = record.hostname.clone();
    let hold = match coordinator.try_acquire(&hostname).await {
        IssuanceAcquisition::Allowed(hold) => hold,
        IssuanceAcquisition::VetoedByPeer(reason) => {
            tracing::info!(
                hostname = %hostname,
                reason = %reason,
                "cert issuance deferred: another orchestrator holds the hostname lock"
            );
            return Ok(None);
        }
        IssuanceAcquisition::CoordinationFailed(reason) => {
            return Err(CertificateError::CertificateLockAcquireFailed {
                phase: "issuance",
                hostname,
                message: reason,
            }
            .into());
        }
    };

    // Re-read inside the lock. The record we were handed by `start_pending_orders`
    // may already be stale: another daemon could have raced ahead while we were
    // waiting on the cluster lock. The lock-bound read is what makes this
    // critical section publish-coherent.
    // Keep a single lock-release point, similar to Go's `defer`: everything in
    // this critical section exits through `'under_lock`, then we release.
    let warning = 'under_lock: {
        let current = match store.get_certificate(&hostname).await {
            Ok(Some(current)) if needs_start_order(&current) => current,
            Ok(_) => break 'under_lock None,
            Err(error) => {
                break 'under_lock Some(format!(
                    "Could not re-read certificate {hostname} before ACME order: {error}"
                ));
            }
        };
        let mut record = current;
        let hostname_for_transition = hostname.clone();

        // Prune stale challenge records for this hostname before opening a new
        // order. Tokens are scoped to the order ACME issued them under, so records
        // left over from a prior failed order can no longer be validated. The
        // success path of `finalize_order` deletes challenges per token, but
        // failure paths leave them behind — without this prune, repeated retries
        // would grow `acme_challenges` without bound, replicate the leak across
        // the cluster, and bloat every gateway snapshot rebuild. Done under the
        // cluster lock so a peer can't be mid-validation against a token we're
        // about to delete.
        if let Err(error) = prune_acme_challenges_for(store, &hostname).await {
            break 'under_lock Some(format!(
                "Could not prune stale ACME challenges for {hostname}: {error}"
            ));
        }

        let outcome = issuer.start_order(store, &hostname).await;
        break 'under_lock match outcome {
            Ok(started) => {
                let transition_result = record.apply_state_transition(CertificateStateTransition {
                    goal: CertificateStateGoal::StartIssuing {
                        order_url: started.order_url,
                    },
                    evidence: CertificateTransitionEvidence::AcmeOrderStart {
                        hostname: hostname_for_transition.clone(),
                    },
                    at_unix_secs: now_unix_secs(),
                });
                match transition_result {
                    Ok(_) => store.upsert_certificate(&record).await.err().map(|error| {
                        format!("Could not persist ACME order for {hostname}: {error}")
                    }),
                    Err(error) => Some(format!(
                        "Could not transition certificate {hostname} after ACME order: {error}"
                    )),
                }
            }
            Err(error) => {
                let detail = error.to_string();
                let _ = record.apply_state_transition(CertificateStateTransition {
                    goal: CertificateStateGoal::MarkOrderFailed {
                        error: detail.clone(),
                    },
                    evidence: CertificateTransitionEvidence::AcmeOrderStart {
                        hostname: hostname_for_transition,
                    },
                    at_unix_secs: now_unix_secs(),
                });
                let _ = store.upsert_certificate(&record).await;
                Some(format!("ACME order for {hostname} failed: {detail}"))
            }
        };
    };

    // Release AFTER persistence so the lock covers both the external order
    // creation and the record update. Releasing earlier opens a window where
    // another daemon can read the still-Pending record, acquire
    // the (now-free) lock, and create a duplicate ACME order.
    hold.release().await;
    Ok(warning)
}
