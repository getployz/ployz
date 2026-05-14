use super::{CERT_VALIDITY_FALLBACK_SECS, LocalHttp01ChallengeReadiness};
use ployz_cert_acme_api::{
    AcmeAccountCoordinator, AcmeIssuer, AcmeIssuerFactory, Http01ChallengeReadiness,
    IssuanceAcquisition, IssuanceCoordinator, NoopAcmeAccountCoordinator, NoopIssuanceCoordinator,
};
use ployz_error::{CertificateError, Error, Result};
use ployz_model::{
    CertificateRecord, CertificateState, CertificateStateGoal, CertificateStateTransition,
    CertificateTransitionEvidence, CertificateVersion,
};
use ployz_store_api::{CertificateStore, StoreDriver};
use ployz_time::now_unix_secs;
use std::sync::Arc;
use uuid::Uuid;
use x509_parser::parse_x509_certificate;
use x509_parser::pem::parse_x509_pem;

pub fn spawn_certificate_finalization(
    store: StoreDriver,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
) {
    spawn_certificate_finalization_with_readiness(
        store,
        issuer_factory,
        Arc::new(LocalHttp01ChallengeReadiness),
    );
}

pub fn spawn_certificate_finalization_with_readiness(
    store: StoreDriver,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    readiness: Arc<dyn Http01ChallengeReadiness>,
) {
    spawn_certificate_finalization_with_coordination(
        store,
        issuer_factory,
        readiness,
        Arc::new(NoopAcmeAccountCoordinator),
        Arc::new(NoopIssuanceCoordinator),
    );
}

pub fn spawn_certificate_finalization_with_coordination(
    store: StoreDriver,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    readiness: Arc<dyn Http01ChallengeReadiness>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    issuance_coordinator: Arc<dyn IssuanceCoordinator>,
) {
    tokio::spawn(async move {
        let issuer = issuer_factory.create(readiness, account_coordinator);
        if let Err(error) =
            finalize_due_certificates(&store, issuer.as_ref(), issuance_coordinator.as_ref()).await
        {
            tracing::warn!(?error, "managed certificate finalization failed");
        }
    });
}

/// Background: for every certificate with an open order (`Issuing` + stored
/// `order_url`), resume the order and finalize. Runs after every apply and —
/// once a renewal trigger exists — on that schedule too.
pub async fn finalize_due_certificates<I, C>(
    store: &StoreDriver,
    issuer: &I,
    coordinator: &C,
) -> Result<()>
where
    I: AcmeIssuer + Sync + ?Sized,
    C: IssuanceCoordinator + ?Sized,
{
    let certificates = store.list_certificates().await?;
    for certificate in certificates {
        if certificate.state() != CertificateState::Issuing {
            continue;
        }
        let Some(order_url) = certificate.order_url().map(ToString::to_string) else {
            continue;
        };
        finalize_one(store, issuer, coordinator, certificate, &order_url).await?;
    }
    Ok(())
}

pub(super) async fn finalize_one<I, C>(
    store: &StoreDriver,
    issuer: &I,
    coordinator: &C,
    record: CertificateRecord,
    order_url: &str,
) -> Result<()>
where
    I: AcmeIssuer + Sync + ?Sized,
    C: IssuanceCoordinator + ?Sized,
{
    let hostname = record.hostname.clone();

    // Acquire the same hostname-scoped cluster lock `start_one` uses. Without
    // it, every daemon that sees the Issuing record races the same ACME order:
    // exactly one wins `finalize()`, but the losers' fast `Failed` writes
    // beat the winner's slow `poll_certificate`, dropping the issued cert
    // and burning duplicate-cert rate limit on every cycle. Holding the
    // lock across the entire ACME flow + persistence guarantees a single
    // finalizer per order.
    let hold = match coordinator.try_acquire(&hostname).await {
        IssuanceAcquisition::Allowed(hold) => hold,
        IssuanceAcquisition::VetoedByPeer(reason) => {
            tracing::info!(
                hostname = %hostname,
                reason = %reason,
                "ACME finalization deferred: another orchestrator holds the hostname lock"
            );
            return Ok(());
        }
        IssuanceAcquisition::CoordinationFailed(reason) => {
            return Err(CertificateError::CertificateLockAcquireFailed {
                phase: "finalization",
                hostname,
                message: reason,
            }
            .into());
        }
    };

    let outcome = finalize_one_under_lock(store, issuer, &hostname, order_url).await;

    // Release AFTER persistence (mirrors `start_one`): the lock must cover
    // both the external ACME side effects and the record update.
    hold.release().await;
    outcome
}

async fn finalize_one_under_lock<I>(
    store: &StoreDriver,
    issuer: &I,
    hostname: &str,
    order_url: &str,
) -> Result<()>
where
    I: AcmeIssuer + Sync + ?Sized,
{
    // Re-read inside the lock. The snapshot from `finalize_due_certificates`
    // may be stale: a peer holding the lock before us could have rotated
    // the record to Active or to a newer order_url.
    let Some(pre) = store.get_certificate(hostname).await? else {
        tracing::warn!(
            hostname = %hostname,
            order_url,
            "skipping ACME finalization because certificate record disappeared"
        );
        return Ok(());
    };
    if !is_same_inflight_order(&pre, order_url) {
        tracing::info!(
            hostname = %hostname,
            order_url,
            current_state = %pre.state(),
            current_order_url = ?pre.order_url(),
            "skipping stale ACME finalization"
        );
        return Ok(());
    }

    let outcome = issuer.finalize_order(store, hostname, order_url).await;

    // Defense-in-depth post-check. The cluster lock prevents peer rotations
    // while we ran ACME, but `NoopIssuanceCoordinator` (single-machine /
    // tests) provides no real serialization, so a sibling task in the same
    // process could have moved on. The cluster-locked path makes this guard
    // unreachable; the noop path makes it load-bearing.
    let Some(mut current) = store.get_certificate(hostname).await? else {
        tracing::warn!(
            hostname = %hostname,
            order_url,
            "skipping ACME finalization write because certificate record disappeared"
        );
        return Ok(());
    };
    if !is_same_inflight_order(&current, order_url) {
        tracing::info!(
            hostname = %hostname,
            order_url,
            current_state = %current.state(),
            current_order_url = ?current.order_url(),
            "skipping stale ACME finalization write"
        );
        return Ok(());
    }

    let previous_active_version_id = current.active_version_id().map(ToString::to_string);
    match outcome {
        Ok(issued) => {
            let now = now_unix_secs();
            let (not_before, not_after) = leaf_validity(&issued.fullchain_pem)
                .unwrap_or((Some(now), Some(now + CERT_VALIDITY_FALLBACK_SECS)));
            let next_renewal_at = renewal_threshold(not_before, not_after);
            let version_id = Uuid::new_v4().to_string();
            current.versions.push(CertificateVersion {
                version_id: version_id.clone(),
                fullchain_pem: issued.fullchain_pem,
                private_key_pem: issued.private_key_pem,
                not_before,
                not_after,
                issued_at: now,
            });
            current
                .apply_state_transition(CertificateStateTransition {
                    goal: CertificateStateGoal::FinalizeActive {
                        active_version_id: version_id,
                        next_renewal_at,
                    },
                    evidence: CertificateTransitionEvidence::AcmeFinalize {
                        hostname: hostname.to_string(),
                    },
                    at_unix_secs: now,
                })
                .map_err(|error| {
                    Error::operation(
                        "certificate_transition",
                        format!("finalize active {hostname}: {error}"),
                    )
                })?;
        }
        Err(error) => {
            let detail = error.to_string();
            if is_retryable_challenge_visibility(&error) {
                current
                    .apply_state_transition(CertificateStateTransition {
                        goal: CertificateStateGoal::KeepIssuingAfterRetryableFailure {
                            error: detail,
                        },
                        evidence: CertificateTransitionEvidence::AcmeFinalize {
                            hostname: hostname.to_string(),
                        },
                        at_unix_secs: now_unix_secs(),
                    })
                    .map_err(|error| {
                        Error::operation(
                            "certificate_transition",
                            format!("retryable finalize failure {hostname}: {error}"),
                        )
                    })?;
            } else {
                current
                    .apply_state_transition(CertificateStateTransition {
                        goal: CertificateStateGoal::MarkFinalizeFailed {
                            error: detail,
                            previous_active_version_id,
                        },
                        evidence: CertificateTransitionEvidence::AcmeFinalize {
                            hostname: hostname.to_string(),
                        },
                        at_unix_secs: now_unix_secs(),
                    })
                    .map_err(|error| {
                        Error::operation(
                            "certificate_transition",
                            format!("finalize failure {hostname}: {error}"),
                        )
                    })?;
            }
        }
    }

    store.upsert_certificate(&current).await
}

fn is_same_inflight_order(record: &CertificateRecord, order_url: &str) -> bool {
    record.state() == CertificateState::Issuing && record.order_url() == Some(order_url)
}

fn is_retryable_challenge_visibility(error: &Error) -> bool {
    matches!(
        error,
        Error::Certificate(
            CertificateError::Http01LocalChallengeNotVisible { .. }
                | CertificateError::Http01NoEligibleGateway { .. }
                | CertificateError::Http01HostnameNotAdvertised { .. }
                | CertificateError::Http01MissingReadiness { .. }
                | CertificateError::Http01InvalidServiceRevision { .. }
        ) | Error::Operation {
            operation: "acme_challenge_visibility",
            ..
        }
    )
}

fn leaf_validity(fullchain_pem: &str) -> Option<(Option<u64>, Option<u64>)> {
    let (_, pem) = parse_x509_pem(fullchain_pem.as_bytes()).ok()?;
    let (_, leaf) = parse_x509_certificate(&pem.contents).ok()?;
    let validity = leaf.validity();
    let not_before = i64::try_from(validity.not_before.timestamp())
        .ok()
        .and_then(|secs| u64::try_from(secs).ok());
    let not_after = i64::try_from(validity.not_after.timestamp())
        .ok()
        .and_then(|secs| u64::try_from(secs).ok());
    Some((not_before, not_after))
}

/// Renewal threshold = `not_before + 2 * lifetime / 3`.
/// Works for both 90-day (→ renew with 30 days remaining) and 6-day
/// (→ renew with 2 days remaining) certs without any hard-coded "30 days".
#[must_use]
pub fn renewal_threshold(not_before: Option<u64>, not_after: Option<u64>) -> Option<u64> {
    let (Some(not_before), Some(not_after)) = (not_before, not_after) else {
        return None;
    };
    let lifetime = not_after.saturating_sub(not_before);
    Some(not_before + (lifetime.saturating_mul(2) / 3))
}

// ---------------------------------------------------------------------------
// Renewal work
// ---------------------------------------------------------------------------
