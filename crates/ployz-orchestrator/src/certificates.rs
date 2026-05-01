use async_trait::async_trait;
use ployz_store_api::{CertificateStore, StoreDriver};
use ployz_types::error::{Error, Result};
use ployz_types::model::{CertificateRecord, CertificateState, CertificateVersion};
use ployz_types::time::now_unix_secs;
use rand::RngExt;
use std::collections::BTreeSet;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use x509_parser::parse_x509_certificate;
use x509_parser::pem::parse_x509_pem;

pub const DEFAULT_ACME_DIRECTORY_URL: &str = "https://acme-v02.api.letsencrypt.org/directory";
const CERT_VALIDITY_FALLBACK_SECS: u64 = 90 * 24 * 60 * 60;
pub const CHALLENGE_TTL_SECS: u64 = 15 * 60;
// Finalization runs in the background, so this can cover unusually slow store
// propagation before reachable peers must observe the HTTP-01 row.
pub const HTTP01_CHALLENGE_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const HTTP01_CHALLENGE_VISIBILITY_POLL: Duration = Duration::from_millis(100);
pub const HTTP01_GATEWAY_SNAPSHOT_SETTLE: Duration = Duration::from_secs(1);
const RENEWAL_TICK_DEFAULT_SECS: u64 = 60 * 60;
const RENEWAL_TICK_MIN_SECS: u64 = 60;
const STUCK_ISSUING_MAX_AGE_SECS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone)]
pub struct CertificateManagerConfig {
    pub issuer_url: String,
    pub contact_email: Option<String>,
    pub root_ca_path: Option<PathBuf>,
}

impl Default for CertificateManagerConfig {
    fn default() -> Self {
        Self {
            issuer_url: DEFAULT_ACME_DIRECTORY_URL.to_string(),
            contact_email: None,
            root_ca_path: None,
        }
    }
}

impl CertificateManagerConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let issuer_url = std::env::var("PLOYZ_ACME_DIRECTORY_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_ACME_DIRECTORY_URL.to_string());
        let contact_email = std::env::var("PLOYZ_ACME_CONTACT_EMAIL")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let root_ca_path = std::env::var_os("PLOYZ_ACME_ROOT_CA_PATH")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        Self {
            issuer_url,
            contact_email,
            root_ca_path,
        }
    }
}

#[must_use]
pub fn account_id_for_issuer_url(issuer_url: &str) -> String {
    issuer_url.to_string()
}

#[derive(Debug, Clone)]
pub struct StartedOrder {
    pub order_url: String,
}

#[derive(Debug, Clone)]
pub struct IssuedCertificate {
    pub fullchain_pem: String,
    pub private_key_pem: String,
}

#[async_trait]
pub trait AcmeIssuer: Send + Sync {
    async fn start_order(&self, store: &StoreDriver, hostname: &str) -> Result<StartedOrder>;
    async fn finalize_order(
        &self,
        store: &StoreDriver,
        hostname: &str,
        order_url: &str,
    ) -> Result<IssuedCertificate>;
}

#[async_trait]
pub trait Http01ChallengeReadiness: Send + Sync {
    async fn wait_ready(&self, store: &StoreDriver, hostname: &str, token: &str) -> Result<()>;
}

/// Issuer-scoped coordination for ACME account creation. Orders are tied to
/// the account key that created them, so concurrent first-use account creation
/// must be serialized per issuer URL before any order is opened.
#[async_trait]
pub trait AcmeAccountCoordinator: Send + Sync {
    async fn try_acquire_account(&self, issuer_url: &str) -> AccountAcquisition;
}

pub enum AccountAcquisition {
    Allowed(IssuanceHold),
    VetoedByPeer(String),
}

pub struct NoopAcmeAccountCoordinator;

#[async_trait]
impl AcmeAccountCoordinator for NoopAcmeAccountCoordinator {
    async fn try_acquire_account(&self, _issuer_url: &str) -> AccountAcquisition {
        AccountAcquisition::Allowed(IssuanceHold::noop())
    }
}

pub struct LocalHttp01ChallengeReadiness;

#[async_trait]
impl Http01ChallengeReadiness for LocalHttp01ChallengeReadiness {
    async fn wait_ready(&self, store: &StoreDriver, hostname: &str, token: &str) -> Result<()> {
        wait_for_http01_challenge_visible(store, hostname, token).await
    }
}

/// Cluster-wide coordination for ACME order placement. Implementations fan
/// out a connection-bound lock to peer machines before `start_order` runs;
/// explicit deny from any reachable peer vetoes this pass, unreachable peers
/// abstain. The guard is held until both the ACME order side effect and the
/// resulting certificate-row state transition have been persisted.
#[async_trait]
pub trait IssuanceCoordinator: Send + Sync {
    async fn try_acquire(&self, hostname: &str) -> IssuanceAcquisition;
}

pub enum IssuanceAcquisition {
    Allowed(IssuanceHold),
    VetoedByPeer(String),
}

pub struct IssuanceHold {
    #[allow(clippy::type_complexity)]
    releaser: Option<
        Box<dyn FnOnce() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> + Send>,
    >,
}

impl IssuanceHold {
    #[must_use]
    pub fn new<F, Fut>(release: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        Self {
            releaser: Some(Box::new(move || Box::pin(release()))),
        }
    }

    #[must_use]
    pub fn noop() -> Self {
        Self::new(|| async {})
    }

    pub async fn release(mut self) {
        if let Some(releaser) = self.releaser.take() {
            releaser().await;
        }
    }
}

impl Drop for IssuanceHold {
    fn drop(&mut self) {
        if let Some(releaser) = self.releaser.take() {
            tracing::warn!(
                "ACME issuance/account hold dropped without explicit release; releasing asynchronously"
            );
            match tokio::runtime::Handle::try_current() {
                Ok(handle) => {
                    handle.spawn(releaser());
                }
                Err(error) => {
                    tracing::warn!(
                        ?error,
                        "ACME issuance/account hold could not release because no Tokio runtime is active"
                    );
                }
            }
        }
    }
}

/// Coordinator that always allows. Used in memory-mode tests and in single-
/// machine setups where cluster fanout isn't needed.
pub struct NoopIssuanceCoordinator;

#[async_trait]
impl IssuanceCoordinator for NoopIssuanceCoordinator {
    async fn try_acquire(&self, _hostname: &str) -> IssuanceAcquisition {
        IssuanceAcquisition::Allowed(IssuanceHold::noop())
    }
}

/// Builds `AcmeIssuer` instances bound to a specific ACME directory. The
/// concrete implementation lives in `ployz-cert-backends`; orchestrator and
/// tests only ever see this trait, which keeps `instant-acme` and `reqwest`
/// out of the orchestrator dependency graph.
pub trait AcmeIssuerFactory: Send + Sync {
    fn issuer_url(&self) -> &str;
    fn create(
        &self,
        readiness: Arc<dyn Http01ChallengeReadiness>,
        account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    ) -> Arc<dyn AcmeIssuer>;
}

/// Issuer that always errors. Returned by `NoopAcmeIssuerFactory` for tests
/// and single-machine setups that never trigger ACME issuance.
pub struct NoopAcmeIssuer;

#[async_trait]
impl AcmeIssuer for NoopAcmeIssuer {
    async fn start_order(&self, _store: &StoreDriver, _hostname: &str) -> Result<StartedOrder> {
        Err(Error::operation(
            "acme_disabled",
            "no ACME issuer is configured for this orchestrator",
        ))
    }

    async fn finalize_order(
        &self,
        _store: &StoreDriver,
        _hostname: &str,
        _order_url: &str,
    ) -> Result<IssuedCertificate> {
        Err(Error::operation(
            "acme_disabled",
            "no ACME issuer is configured for this orchestrator",
        ))
    }
}

/// Factory that returns `NoopAcmeIssuer` instances. Use in tests and in
/// single-machine setups that have no managed certificates configured.
pub struct NoopAcmeIssuerFactory {
    issuer_url: String,
}

impl NoopAcmeIssuerFactory {
    #[must_use]
    pub fn new(issuer_url: impl Into<String>) -> Self {
        Self {
            issuer_url: issuer_url.into(),
        }
    }
}

impl Default for NoopAcmeIssuerFactory {
    fn default() -> Self {
        Self::new(DEFAULT_ACME_DIRECTORY_URL)
    }
}

impl AcmeIssuerFactory for NoopAcmeIssuerFactory {
    fn issuer_url(&self) -> &str {
        &self.issuer_url
    }

    fn create(
        &self,
        _readiness: Arc<dyn Http01ChallengeReadiness>,
        _account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    ) -> Arc<dyn AcmeIssuer> {
        Arc::new(NoopAcmeIssuer)
    }
}

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

/// Hot-path: for every certificate that needs a new ACME order (`Pending`,
/// `Failed`, or `RenewalDue`), call `start_order`. Errors are returned as
/// human-readable strings so callers can surface them as deploy warnings —
/// they never fail the deploy.
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
    let hostnames = hostnames.iter().cloned().collect::<BTreeSet<_>>();
    if hostnames.is_empty() {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    let records = match store.list_certificates().await {
        Ok(records) => records,
        Err(error) => {
            warnings.push(format!(
                "Could not list managed certificates for ACME order: {error}"
            ));
            return warnings;
        }
    };
    for record in records {
        if !hostnames.contains(&record.hostname) {
            continue;
        }
        if !needs_start_order(&record) {
            continue;
        }
        if let Some(warning) = start_one(store, issuer, coordinator, record).await {
            warnings.push(warning);
        }
    }
    warnings
}

fn needs_start_order(record: &CertificateRecord) -> bool {
    match record.state {
        CertificateState::Pending | CertificateState::Failed | CertificateState::RenewalDue => true,
        CertificateState::Issuing | CertificateState::Active => false,
    }
}

/// Delete all challenge rows for a given hostname. Called immediately before
/// minting a new ACME order so retries from `Failed` don't accumulate dead
/// `(hostname, token)` rows in `acme_challenges`.
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

async fn start_one<I, C>(
    store: &StoreDriver,
    issuer: &I,
    coordinator: &C,
    record: CertificateRecord,
) -> Option<String>
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
            return None;
        }
    };

    // Re-read inside the lock. The row we were handed by `start_pending_orders`
    // may already be stale: another daemon could have raced ahead while we were
    // waiting on the cluster lock. The lock-bound re-read is what makes this
    // critical section publish-coherent.
    // Keep a single lock-release point, similar to Go's `defer`: everything in
    // this critical section exits through `'under_lock`, then we release.
    let warning =
        'under_lock: {
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

            // Prune stale challenge rows for this hostname before opening a new
            // order. Tokens are scoped to the order ACME issued them under, so rows
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
                    record.state = CertificateState::Issuing;
                    record.order_url = Some(started.order_url);
                    record.updated_at = now_unix_secs();
                    record.last_error = None;
                    store.upsert_certificate(&record).await.err().map(|error| {
                        format!("Could not persist ACME order for {hostname}: {error}")
                    })
                }
                Err(error) => {
                    let detail = error.to_string();
                    record.state = CertificateState::Failed;
                    record.last_error = Some(detail.clone());
                    record.order_url = None;
                    record.updated_at = now_unix_secs();
                    let _ = store.upsert_certificate(&record).await;
                    Some(format!("ACME order for {hostname} failed: {detail}"))
                }
            };
        };

    // Release AFTER persistence so the lock covers both the external order
    // creation and the row update. Releasing earlier opens a window where
    // another daemon's reconciler can read the still-Pending row, acquire
    // the (now-free) lock, and create a duplicate ACME order.
    hold.release().await;
    warning
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
        if certificate.state != CertificateState::Issuing {
            continue;
        }
        let Some(order_url) = certificate.order_url.clone() else {
            continue;
        };
        finalize_one(store, issuer, coordinator, certificate, &order_url).await?;
    }
    Ok(())
}

async fn finalize_one<I, C>(
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
    // it, every daemon that sees the Issuing row races the same ACME order:
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
    };

    let outcome = finalize_one_under_lock(store, issuer, &hostname, order_url).await;

    // Release AFTER persistence (mirrors `start_one`): the lock must cover
    // both the external ACME side effects and the row update.
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
    // the row to Active or to a newer order_url.
    let Some(pre) = store.get_certificate(hostname).await? else {
        tracing::warn!(
            hostname = %hostname,
            order_url,
            "skipping ACME finalization because certificate row disappeared"
        );
        return Ok(());
    };
    if !is_same_inflight_order(&pre, order_url) {
        tracing::info!(
            hostname = %hostname,
            order_url,
            current_state = %pre.state,
            current_order_url = ?pre.order_url,
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
            "skipping ACME finalization write because certificate row disappeared"
        );
        return Ok(());
    };
    if !is_same_inflight_order(&current, order_url) {
        tracing::info!(
            hostname = %hostname,
            order_url,
            current_state = %current.state,
            current_order_url = ?current.order_url,
            "skipping stale ACME finalization write"
        );
        return Ok(());
    }

    let previous_active_version_id = current.active_version_id.clone();
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
            current.active_version_id = Some(version_id);
            current.state = CertificateState::Active;
            current.updated_at = now;
            current.next_renewal_at = next_renewal_at;
            current.order_url = None;
            current.last_error = None;
        }
        Err(error) => {
            if is_retryable_challenge_visibility(&error) {
                current.state = CertificateState::Issuing;
            } else {
                current.state = CertificateState::Failed;
                current.active_version_id = previous_active_version_id;
                current.order_url = None;
            }
            current.updated_at = now_unix_secs();
            current.last_error = Some(error.to_string());
        }
    }

    store.upsert_certificate(&current).await
}

fn is_same_inflight_order(record: &CertificateRecord, order_url: &str) -> bool {
    record.state == CertificateState::Issuing && record.order_url.as_deref() == Some(order_url)
}

fn is_retryable_challenge_visibility(error: &Error) -> bool {
    matches!(
        error,
        Error::Operation {
            operation: "acme_challenge_visibility",
            ..
        }
    )
}

async fn wait_for_http01_challenge_visible(
    store: &StoreDriver,
    hostname: &str,
    token: &str,
) -> Result<()> {
    let start = tokio::time::Instant::now();
    loop {
        let visible = store
            .list_acme_challenges()
            .await?
            .iter()
            .any(|challenge| challenge.hostname == hostname && challenge.token == token);
        if visible {
            return Ok(());
        }
        if start.elapsed() >= HTTP01_CHALLENGE_VISIBILITY_TIMEOUT {
            return Err(Error::operation(
                "acme_challenge_visibility",
                format!(
                    "HTTP-01 challenge for {hostname} was not visible in store within {:?}",
                    HTTP01_CHALLENGE_VISIBILITY_TIMEOUT
                ),
            ));
        }
        tokio::time::sleep(HTTP01_CHALLENGE_VISIBILITY_POLL).await;
    }
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
// Renewal reconciliation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RenewalConfig {
    pub interval: Duration,
}

impl Default for RenewalConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(RENEWAL_TICK_DEFAULT_SECS),
        }
    }
}

impl RenewalConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let interval_secs = std::env::var("PLOYZ_CERT_RENEWAL_INTERVAL_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .map(|secs| secs.max(RENEWAL_TICK_MIN_SECS))
            .unwrap_or(RENEWAL_TICK_DEFAULT_SECS);
        Self {
            interval: Duration::from_secs(interval_secs),
        }
    }
}

/// Cancellable owner for the certificate renewal ticker.
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

/// Spawn a background ticker that runs `reconcile_renewals` immediately and
/// then on an hourly-ish jittered interval. The ticker only flips state and
/// fires `start_pending_orders` — it never waits on ACME itself.
pub fn spawn_certificate_renewal_ticker(
    store: StoreDriver,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    renewal_config: RenewalConfig,
    coordinator: Arc<dyn IssuanceCoordinator>,
    readiness: Arc<dyn Http01ChallengeReadiness>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
) -> CertificateRenewalTask {
    CertificateRenewalTask::spawn("certificate renewal ticker", |task_cancel| async move {
        let issuer = issuer_factory.create(
            Arc::new(LocalHttp01ChallengeReadiness),
            account_coordinator.clone(),
        );
        loop {
            if let Err(error) =
                reconcile_renewals(&store, issuer.as_ref(), coordinator.as_ref()).await
            {
                tracing::warn!(?error, "certificate renewal reconcile failed");
            }
            // Finalize any Issuing rows out-of-band. `finalize_order` blocks
            // on LE for seconds-to-minutes, so spawning prevents a single
            // slow cert from stalling the ticker.
            spawn_certificate_finalization_with_coordination(
                store.clone(),
                issuer_factory.clone(),
                readiness.clone(),
                account_coordinator.clone(),
                coordinator.clone(),
            );
            tokio::select! {
                () = task_cancel.cancelled() => break,
                () = tokio::time::sleep(jittered(renewal_config.interval)) => {}
            }
        }
    })
}

/// Walk certificates and process each hostname that currently needs renewal
/// consideration. Does NOT call `finalize_order` — that's the caller's job,
/// spawned separately by the ticker today and by the NATS job worker later.
pub async fn reconcile_renewals<I, C>(
    store: &StoreDriver,
    issuer: &I,
    coordinator: &C,
) -> Result<()>
where
    I: AcmeIssuer + Sync + ?Sized,
    C: IssuanceCoordinator + ?Sized,
{
    let records = store.list_certificates().await?;
    for record in records {
        process_renewal_job(store, issuer, coordinator, &record.hostname).await?;
    }
    Ok(())
}

/// Process one certificate renewal job. This is the unit of work for NATS
/// `cert_jobs` delivery: the job names a hostname, this function re-reads the
/// authoritative certificate row, applies wall-clock state transitions, then
/// starts an ACME order if that row still needs one.
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
        tracing::info!(hostname, "certificate renewal job skipped missing row");
        return Ok(());
    };
    let now = now_unix_secs();
    let due = match record.state {
        CertificateState::Active => {
            let Some(threshold) = record.next_renewal_at else {
                return Ok(());
            };
            if now < threshold {
                return Ok(());
            }
            record.state = CertificateState::RenewalDue;
            record.updated_at = now;
            store.upsert_certificate(&record).await?;
            true
        }
        CertificateState::Issuing => {
            if now.saturating_sub(record.updated_at) < STUCK_ISSUING_MAX_AGE_SECS {
                return Ok(());
            }
            record.state = CertificateState::Pending;
            record.order_url = None;
            record.last_error = Some("previous order stalled; re-ordering".into());
            record.updated_at = now;
            store.upsert_certificate(&record).await?;
            true
        }
        CertificateState::Pending | CertificateState::Failed | CertificateState::RenewalDue => true,
    };
    if !due {
        return Ok(());
    }
    for warning in start_pending_orders(store, issuer, coordinator, &[record.hostname]).await {
        tracing::warn!(%warning, "certificate renewal");
    }
    Ok(())
}

fn jittered(base: Duration) -> Duration {
    let mut rng = rand::rng();
    let millis = base.as_millis().try_into().unwrap_or(u64::MAX);
    let jitter_ms = millis / 10;
    if jitter_ms == 0 {
        return base;
    }
    let delta = rng.random_range(0..=jitter_ms.saturating_mul(2));
    base.saturating_sub(Duration::from_millis(jitter_ms))
        .saturating_add(Duration::from_millis(delta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{AcmeChallengeRecord, CertificateRecord};
    use std::sync::Mutex;

    struct FakeIssuer {
        start_result: Mutex<Option<Result<StartedOrder>>>,
        finalize_result: Mutex<Option<Result<IssuedCertificate>>>,
    }

    enum FinalizeMutation {
        Activate {
            active_version_id: String,
            fullchain_pem: String,
            private_key_pem: String,
        },
        ReplaceOrder {
            order_url: String,
        },
    }

    struct MutatingFinalizeIssuer {
        mutation: FinalizeMutation,
        result: Mutex<Option<Result<IssuedCertificate>>>,
    }

    impl MutatingFinalizeIssuer {
        fn new(mutation: FinalizeMutation, result: Result<IssuedCertificate>) -> Self {
            Self {
                mutation,
                result: Mutex::new(Some(result)),
            }
        }
    }

    impl FakeIssuer {
        fn new(
            start_result: Result<StartedOrder>,
            finalize_result: Result<IssuedCertificate>,
        ) -> Self {
            Self {
                start_result: Mutex::new(Some(start_result)),
                finalize_result: Mutex::new(Some(finalize_result)),
            }
        }

        fn start_only(start_result: Result<StartedOrder>) -> Self {
            Self {
                start_result: Mutex::new(Some(start_result)),
                finalize_result: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl AcmeIssuer for FakeIssuer {
        async fn start_order(&self, _store: &StoreDriver, _hostname: &str) -> Result<StartedOrder> {
            self.start_result
                .lock()
                .expect("start_result lock")
                .take()
                .unwrap_or_else(|| Err(Error::operation("fake_start_order", "exhausted")))
        }

        async fn finalize_order(
            &self,
            _store: &StoreDriver,
            _hostname: &str,
            _order_url: &str,
        ) -> Result<IssuedCertificate> {
            self.finalize_result
                .lock()
                .expect("finalize_result lock")
                .take()
                .unwrap_or_else(|| Err(Error::operation("fake_finalize_order", "exhausted")))
        }
    }

    #[async_trait]
    impl AcmeIssuer for MutatingFinalizeIssuer {
        async fn start_order(&self, _store: &StoreDriver, _hostname: &str) -> Result<StartedOrder> {
            Err(Error::operation("fake_start_order", "unused"))
        }

        async fn finalize_order(
            &self,
            store: &StoreDriver,
            _hostname: &str,
            _order_url: &str,
        ) -> Result<IssuedCertificate> {
            let mut current = store
                .get_certificate("example.com")
                .await?
                .ok_or_else(|| Error::operation("fake_finalize_order", "missing cert"))?;
            match &self.mutation {
                FinalizeMutation::Activate {
                    active_version_id,
                    fullchain_pem,
                    private_key_pem,
                } => {
                    current.state = CertificateState::Active;
                    current.order_url = None;
                    current.active_version_id = Some(active_version_id.clone());
                    current.versions.push(CertificateVersion {
                        version_id: active_version_id.clone(),
                        fullchain_pem: fullchain_pem.clone(),
                        private_key_pem: private_key_pem.clone(),
                        not_before: Some(1),
                        not_after: Some(2),
                        issued_at: 1,
                    });
                }
                FinalizeMutation::ReplaceOrder { order_url } => {
                    current.order_url = Some(order_url.clone());
                    current.updated_at = now_unix_secs();
                }
            }
            store.upsert_certificate(&current).await?;
            self.result
                .lock()
                .expect("finalize_result lock")
                .take()
                .unwrap_or_else(|| Err(Error::operation("fake_finalize_order", "exhausted")))
        }
    }

    // -------------------------------------------------------------------
    // start_one — multi-daemon order-creation safety
    //
    // These tests pin the contract that protects against duplicate ACME
    // orders in a clustered deployment:
    //
    //   1. The cluster lock must cover both the external `start_order`
    //      side effect AND the row update — otherwise a peer's
    //      reconciler can read the still-Pending row in the gap, acquire
    //      the (released) lock, and create a duplicate order.
    //
    //   2. After acquiring the lock, the row must be re-read, because the
    //      snapshot handed in by `start_pending_orders` may already be stale.
    //      A row that is no longer Pending/Failed/RenewalDue must not
    //      trigger a new ACME order.
    // -------------------------------------------------------------------

    /// Issuer that aborts the test if `start_order` is invoked. Used to
    /// assert "we never asked ACME for a new order in this scenario."
    struct PanickingIssuer;

    #[async_trait]
    impl AcmeIssuer for PanickingIssuer {
        async fn start_order(&self, _: &StoreDriver, _: &str) -> Result<StartedOrder> {
            panic!("start_order should not be invoked");
        }
        async fn finalize_order(
            &self,
            _: &StoreDriver,
            _: &str,
            _: &str,
        ) -> Result<IssuedCertificate> {
            Err(Error::operation("panicking_issuer", "unused"))
        }
    }

    /// Coordinator that captures the certificate row's state at the moment
    /// `IssuanceHold::release` runs. Lets the test assert that the lock is
    /// still held when the row was upserted with `Issuing` + `order_url`.
    struct CaptureOnReleaseCoordinator {
        store: StoreDriver,
        captured: std::sync::Arc<Mutex<Option<CertificateRecord>>>,
    }

    #[async_trait]
    impl IssuanceCoordinator for CaptureOnReleaseCoordinator {
        async fn try_acquire(&self, hostname: &str) -> IssuanceAcquisition {
            let store = self.store.clone();
            let captured = std::sync::Arc::clone(&self.captured);
            let hostname = hostname.to_string();
            IssuanceAcquisition::Allowed(IssuanceHold::new(move || async move {
                if let Ok(Some(row)) = store.get_certificate(&hostname).await {
                    *captured.lock().expect("captured lock") = Some(row);
                }
            }))
        }
    }

    #[tokio::test]
    async fn start_one_skips_when_row_already_issuing_after_lock_acquire() {
        // Simulate: peer A held the lock, ran start_order, persisted Issuing
        // with an order_url, released. Peer B's `start_pending_orders` had
        // already read the row as Pending before A's write replicated, so
        // it hands a stale snapshot to start_one. The lock-bound re-read
        // must catch this and skip — otherwise B would mint a duplicate
        // ACME order for the same hostname.
        let store = StoreDriver::memory();
        let mut already_issuing = pending_record("example.com");
        already_issuing.state = CertificateState::Issuing;
        already_issuing.order_url = Some("https://acme/orders/41".into());
        store
            .upsert_certificate(&already_issuing)
            .await
            .expect("already-issuing cert should persist");

        // Stale snapshot of the same row, as start_pending_orders would have
        // observed it before peer A's write reached this daemon.
        let stale = pending_record("example.com");

        let warning = start_one(&store, &PanickingIssuer, &NoopIssuanceCoordinator, stale).await;
        assert!(warning.is_none(), "stale start should be skipped silently");

        // Row is unchanged: A's order_url and Issuing state are intact.
        let row = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert row should exist");
        assert_eq!(row.state, CertificateState::Issuing);
        assert_eq!(row.order_url.as_deref(), Some("https://acme/orders/41"));
    }

    #[tokio::test]
    async fn start_one_holds_lock_until_after_upsert() {
        // The cluster lock must cover the row write, not just `start_order`.
        // We assert this by capturing the row's state at the exact moment
        // `IssuanceHold::release` runs: if the lock covers the upsert, the
        // captured row already has Issuing + the new order_url.
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("pending cert should persist");

        let captured = std::sync::Arc::new(Mutex::new(None));
        let coordinator = CaptureOnReleaseCoordinator {
            store: store.clone(),
            captured: std::sync::Arc::clone(&captured),
        };

        let stale = store
            .get_certificate("example.com")
            .await
            .expect("read snapshot")
            .expect("snapshot row");

        let warning = start_one(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/42".into(),
            })),
            &coordinator,
            stale,
        )
        .await;
        assert!(warning.is_none(), "happy-path start should not warn");

        let snapshot_at_release = captured
            .lock()
            .expect("captured lock")
            .clone()
            .expect("release should have captured the row");
        // If the lock had been released before the upsert, this would still
        // be Pending with no order_url.
        assert_eq!(snapshot_at_release.state, CertificateState::Issuing);
        assert_eq!(
            snapshot_at_release.order_url.as_deref(),
            Some("https://acme/orders/42")
        );
    }

    #[tokio::test]
    async fn start_one_prunes_stale_challenge_rows_for_same_hostname() {
        // Failed-then-retry scenario: a previous order left stale challenge
        // rows behind because finalize_order's success path is the only
        // place that deletes them. The next `start_one` must prune those
        // before minting a new order — otherwise `acme_challenges` grows
        // unbounded across repeated failures, replicates the leak across
        // the cluster, and bloats every gateway snapshot rebuild.
        let store = StoreDriver::memory();
        let mut failed = pending_record("example.com");
        failed.state = CertificateState::Failed;
        failed.last_error = Some("previous order failed".into());
        store
            .upsert_certificate(&failed)
            .await
            .expect("failed cert should persist");

        // Two stale tokens for the failing hostname plus an unrelated
        // hostname's token that must NOT be pruned.
        for (hostname, token) in [
            ("example.com", "stale-tok-A"),
            ("example.com", "stale-tok-B"),
            ("other.example.com", "keep-tok"),
        ] {
            store
                .upsert_acme_challenge(&AcmeChallengeRecord {
                    hostname: hostname.into(),
                    token: token.into(),
                    key_authorization: format!("{token}.keyauth"),
                    expires_at: now_unix_secs() + 60,
                    created_at: now_unix_secs(),
                })
                .await
                .expect("challenge upsert should persist");
        }

        let warning = start_one(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/42".into(),
            })),
            &NoopIssuanceCoordinator,
            failed,
        )
        .await;
        assert!(warning.is_none(), "happy retry should not warn");

        let remaining = store
            .list_acme_challenges()
            .await
            .expect("list should work");
        // FakeIssuer::start_only doesn't write any new challenge rows; we
        // only assert pruning here, so the surviving row is the unrelated
        // hostname's challenge.
        assert_eq!(remaining.len(), 1, "stale rows should be pruned");
        assert_eq!(remaining[0].hostname, "other.example.com");
        assert_eq!(remaining[0].token, "keep-tok");
    }

    #[tokio::test]
    async fn start_pending_transitions_to_issuing_with_order_url() {
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("pending cert should persist");

        let warnings = start_pending_orders(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/42".into(),
            })),
            &NoopIssuanceCoordinator,
            &["example.com".into()],
        )
        .await;
        assert!(warnings.is_empty(), "healthy start should not warn");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Issuing);
        assert_eq!(record.order_url.as_deref(), Some("https://acme/orders/42"));
        assert!(record.last_error.is_none());
    }

    #[tokio::test]
    async fn start_pending_surfaces_rate_limit_as_warning() {
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("pending cert should persist");

        let warnings = start_pending_orders(
            &store,
            &FakeIssuer::start_only(Err(Error::operation(
                "new_order",
                "urn:ietf:params:acme:error:rateLimited: too many",
            ))),
            &NoopIssuanceCoordinator,
            &["example.com".into()],
        )
        .await;
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("example.com"));
        assert!(warnings[0].contains("rateLimited"));

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Failed);
        assert!(
            record
                .last_error
                .as_deref()
                .unwrap()
                .contains("rateLimited")
        );
        assert!(record.order_url.is_none());
    }

    #[tokio::test]
    async fn start_pending_only_touches_requested_hostnames() {
        let store = StoreDriver::memory();
        store
            .upsert_certificate(&pending_record("example.com"))
            .await
            .expect("target cert should persist");
        store
            .upsert_certificate(&pending_record("unrelated.example.com"))
            .await
            .expect("unrelated cert should persist");

        let warnings = start_pending_orders(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/42".into(),
            })),
            &NoopIssuanceCoordinator,
            &["example.com".into()],
        )
        .await;
        assert!(warnings.is_empty(), "healthy start should not warn");

        let target = store
            .get_certificate("example.com")
            .await
            .expect("target cert lookup should work")
            .expect("target cert should exist");
        let unrelated = store
            .get_certificate("unrelated.example.com")
            .await
            .expect("unrelated cert lookup should work")
            .expect("unrelated cert should exist");
        assert_eq!(target.state, CertificateState::Issuing);
        assert_eq!(unrelated.state, CertificateState::Pending);
        assert!(unrelated.order_url.is_none());
    }

    #[tokio::test]
    async fn finalize_due_writes_active_certificate() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Issuing;
        record.order_url = Some("https://acme/orders/42".into());
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        finalize_due_certificates(
            &store,
            &FakeIssuer::new(
                Err(Error::operation("unused", "start not called")),
                Ok(IssuedCertificate {
                    fullchain_pem: "fullchain".into(),
                    private_key_pem: "key".into(),
                }),
            ),
            &NoopIssuanceCoordinator,
        )
        .await
        .expect("finalization should run");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Active);
        assert!(record.active_version_id.is_some());
        assert_eq!(record.versions.len(), 1);
        assert_eq!(record.versions[0].fullchain_pem, "fullchain");
        assert!(record.order_url.is_none());
    }

    #[tokio::test]
    async fn finalize_failure_keeps_previous_active_version() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Issuing;
        record.order_url = Some("https://acme/orders/42".into());
        record.active_version_id = Some("old".into());
        record.versions.push(CertificateVersion {
            version_id: "old".into(),
            fullchain_pem: "old-chain".into(),
            private_key_pem: "old-key".into(),
            not_before: Some(1),
            not_after: Some(2),
            issued_at: 1,
        });
        store
            .upsert_certificate(&record)
            .await
            .expect("renewal cert should persist");

        finalize_due_certificates(
            &store,
            &FakeIssuer::new(
                Err(Error::operation("unused", "start not called")),
                Err(Error::operation("fake_acme", "failed")),
            ),
            &NoopIssuanceCoordinator,
        )
        .await
        .expect("finalization errors are recorded per certificate");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Failed);
        assert_eq!(record.active_version_id.as_deref(), Some("old"));
        assert_eq!(record.versions.len(), 1);
        assert!(record.order_url.is_none());
    }

    #[tokio::test]
    async fn stale_finalize_failure_does_not_overwrite_active_certificate() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Issuing;
        record.order_url = Some("https://acme/orders/42".into());
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        finalize_one(
            &store,
            &MutatingFinalizeIssuer::new(
                FinalizeMutation::Activate {
                    active_version_id: "new".into(),
                    fullchain_pem: "new-chain".into(),
                    private_key_pem: "new-key".into(),
                },
                Err(Error::operation("fake_acme", "late failure")),
            ),
            &NoopIssuanceCoordinator,
            record,
            "https://acme/orders/42",
        )
        .await
        .expect("stale finalization should be skipped");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Active);
        assert_eq!(record.active_version_id.as_deref(), Some("new"));
        assert_eq!(record.versions.len(), 1);
        assert!(record.order_url.is_none());
        assert!(record.last_error.is_none());
    }

    #[tokio::test]
    async fn stale_finalize_success_does_not_overwrite_new_order() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Issuing;
        record.order_url = Some("https://acme/orders/42".into());
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        finalize_one(
            &store,
            &MutatingFinalizeIssuer::new(
                FinalizeMutation::ReplaceOrder {
                    order_url: "https://acme/orders/43".into(),
                },
                Ok(IssuedCertificate {
                    fullchain_pem: "stale-chain".into(),
                    private_key_pem: "stale-key".into(),
                }),
            ),
            &NoopIssuanceCoordinator,
            record,
            "https://acme/orders/42",
        )
        .await
        .expect("stale finalization should be skipped");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Issuing);
        assert_eq!(record.order_url.as_deref(), Some("https://acme/orders/43"));
        assert!(record.active_version_id.is_none());
        assert!(record.versions.is_empty());
    }

    /// Issuer that records each `finalize_order` call. Lets the pre-call
    /// guard test assert the ACME side-effect path was not entered when
    /// `finalize_one` is handed a row that's already been rotated to a
    /// newer order.
    struct RecordingFinalizeIssuer {
        finalize_calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl RecordingFinalizeIssuer {
        fn new() -> Self {
            Self {
                finalize_calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn finalize_call_count(&self) -> usize {
            self.finalize_calls
                .load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl AcmeIssuer for RecordingFinalizeIssuer {
        async fn start_order(&self, _: &StoreDriver, _: &str) -> Result<StartedOrder> {
            Err(Error::operation("recording_issuer", "start_order unused"))
        }

        async fn finalize_order(
            &self,
            _: &StoreDriver,
            _: &str,
            _: &str,
        ) -> Result<IssuedCertificate> {
            self.finalize_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(IssuedCertificate {
                fullchain_pem: "should-not-be-used".into(),
                private_key_pem: "should-not-be-used".into(),
            })
        }
    }

    #[tokio::test]
    async fn finalize_one_skips_acme_when_stored_row_already_advanced_past_order() {
        // A peer's `start_one` has already rotated the row to a newer order
        // (order/43). This daemon's reconciler still holds a stale snapshot
        // that points at order/42. `finalize_one` must short-circuit before
        // calling the ACME finalize step — running it would delete or
        // disturb challenge state for the in-flight order/43 (the original
        // bug: stale finalizers ran ACME side effects unconditionally).
        let store = StoreDriver::memory();
        let mut current = pending_record("example.com");
        current.state = CertificateState::Issuing;
        current.order_url = Some("https://acme/orders/43".into());
        store
            .upsert_certificate(&current)
            .await
            .expect("issuing cert should persist");

        let stale_snapshot = {
            let mut record = pending_record("example.com");
            record.state = CertificateState::Issuing;
            record.order_url = Some("https://acme/orders/42".into());
            record
        };

        let issuer = RecordingFinalizeIssuer::new();
        finalize_one(
            &store,
            &issuer,
            &NoopIssuanceCoordinator,
            stale_snapshot,
            "https://acme/orders/42",
        )
        .await
        .expect("stale finalization should be skipped without error");

        assert_eq!(
            issuer.finalize_call_count(),
            0,
            "ACME finalize_order must not run when the stored row already points at a newer order"
        );

        // The newer order's row is untouched.
        let row = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(row.state, CertificateState::Issuing);
        assert_eq!(row.order_url.as_deref(), Some("https://acme/orders/43"));
    }

    // -------------------------------------------------------------------
    // finalize_one — multi-daemon order-finalization safety
    //
    // The pre-fix race: every daemon independently finalizes the same
    // Issuing row. Exactly one wins `finalize()` at LE; the losers' fast
    // `Failed` writes beat the winner's slow `poll_certificate` and the
    // winner's post-check then sees `Failed` and drops the issued cert.
    // On a 300-node cluster this fires every cycle and burns LE's
    // duplicate-cert rate limit.
    //
    // The fix: hold the same hostname-scoped cluster lock `start_one`
    // uses for the entire ACME flow + persistence. These tests pin that
    // contract.
    // -------------------------------------------------------------------

    /// Coordinator that always vetoes. Models a peer already holding the
    /// hostname lock.
    struct AlwaysVetoCoordinator;

    #[async_trait]
    impl IssuanceCoordinator for AlwaysVetoCoordinator {
        async fn try_acquire(&self, _hostname: &str) -> IssuanceAcquisition {
            IssuanceAcquisition::VetoedByPeer("peer holds lock".into())
        }
    }

    #[tokio::test]
    async fn finalize_one_skips_acme_when_coordinator_vetoes() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Issuing;
        record.order_url = Some("https://acme/orders/42".into());
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        let issuer = RecordingFinalizeIssuer::new();
        finalize_one(
            &store,
            &issuer,
            &AlwaysVetoCoordinator,
            record,
            "https://acme/orders/42",
        )
        .await
        .expect("vetoed finalize should be a no-op, not an error");

        assert_eq!(
            issuer.finalize_call_count(),
            0,
            "ACME finalize_order must not run when a peer holds the lock"
        );

        let row = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(row.state, CertificateState::Issuing);
        assert_eq!(row.order_url.as_deref(), Some("https://acme/orders/42"));
        assert!(row.last_error.is_none());
    }

    #[tokio::test]
    async fn finalize_one_holds_lock_until_after_persist() {
        // Mirrors `start_one_holds_lock_until_after_upsert`. The lock must
        // cover the row write so a peer can't read the still-Issuing row
        // between `finalize_order` returning and the persist landing,
        // grab the (released) lock, and start a duplicate fresh order.
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Issuing;
        record.order_url = Some("https://acme/orders/42".into());
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        let captured = std::sync::Arc::new(Mutex::new(None));
        let coordinator = CaptureOnReleaseCoordinator {
            store: store.clone(),
            captured: std::sync::Arc::clone(&captured),
        };
        let issuer = FakeIssuer::new(
            Err(Error::operation("unused", "start not called")),
            Ok(IssuedCertificate {
                fullchain_pem: "fullchain".into(),
                private_key_pem: "key".into(),
            }),
        );

        finalize_one(
            &store,
            &issuer,
            &coordinator,
            record,
            "https://acme/orders/42",
        )
        .await
        .expect("finalization should succeed");

        let row_at_release = captured
            .lock()
            .expect("captured lock")
            .clone()
            .expect("release callback should observe a row");
        assert_eq!(row_at_release.state, CertificateState::Active);
        assert!(row_at_release.active_version_id.is_some());
        assert!(row_at_release.order_url.is_none());
        let [version] = row_at_release.versions.as_slice() else {
            panic!(
                "expected exactly one issued version, got {:?}",
                row_at_release.versions
            );
        };
        assert_eq!(version.fullchain_pem, "fullchain");
    }

    /// Coordinator backed by a `tokio::sync::Mutex`. Concurrent acquires
    /// return `VetoedByPeer` synchronously rather than waiting, mirroring
    /// the production reservation semantics where peers either hold the
    /// reservation or get a synchronous deny.
    struct SerializingCoordinator {
        held: std::sync::Arc<tokio::sync::Mutex<()>>,
    }

    impl Default for SerializingCoordinator {
        fn default() -> Self {
            Self {
                held: std::sync::Arc::new(tokio::sync::Mutex::new(())),
            }
        }
    }

    #[async_trait]
    impl IssuanceCoordinator for SerializingCoordinator {
        async fn try_acquire(&self, _hostname: &str) -> IssuanceAcquisition {
            match self.held.clone().try_lock_owned() {
                Ok(guard) => IssuanceAcquisition::Allowed(IssuanceHold::new(move || async move {
                    drop(guard);
                })),
                Err(_) => IssuanceAcquisition::VetoedByPeer("peer holds lock".into()),
            }
        }
    }

    /// Issuer whose `finalize_order` sleeps before returning, modelling
    /// LE's poll_ready + finalize + poll_certificate round-trip. Forces
    /// concurrent finalizers to overlap when running under a multi-thread
    /// runtime so the lock's serialization is actually exercised.
    struct SlowFinalizeIssuer {
        delay: Duration,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        result: std::sync::Mutex<Option<Result<IssuedCertificate>>>,
    }

    impl SlowFinalizeIssuer {
        fn new(delay: Duration, result: Result<IssuedCertificate>) -> Self {
            Self {
                delay,
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                result: std::sync::Mutex::new(Some(result)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl AcmeIssuer for SlowFinalizeIssuer {
        async fn start_order(&self, _: &StoreDriver, _: &str) -> Result<StartedOrder> {
            Err(Error::operation("slow_issuer", "start unused"))
        }

        async fn finalize_order(
            &self,
            _: &StoreDriver,
            _: &str,
            _: &str,
        ) -> Result<IssuedCertificate> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            self.result
                .lock()
                .expect("slow_issuer result lock")
                .take()
                .unwrap_or_else(|| Err(Error::operation("slow_issuer", "exhausted")))
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_finalize_one_calls_serialize_through_lock() {
        // Eight concurrent finalize_one tasks against the same Issuing row.
        // Exactly one acquires the cluster lock and runs ACME; the seven
        // losers see VetoedByPeer and return early without touching the
        // row. The issued cert lands on the row — never `Failed`. This is
        // the test that would have caught the original bug: pre-fix, the
        // losers would have raced past the pre-check, run ACME, hit the
        // already-finalized order at LE, and written `Failed` before the
        // winner's `poll_certificate` returned.
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Issuing;
        record.order_url = Some("https://acme/orders/42".into());
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        let coordinator = std::sync::Arc::new(SerializingCoordinator::default());
        let issuer = std::sync::Arc::new(SlowFinalizeIssuer::new(
            Duration::from_millis(150),
            Ok(IssuedCertificate {
                fullchain_pem: "winner-chain".into(),
                private_key_pem: "winner-key".into(),
            }),
        ));

        let runs: Vec<_> = (0..8)
            .map(|_| {
                let store = store.clone();
                let coord = std::sync::Arc::clone(&coordinator);
                let issuer = std::sync::Arc::clone(&issuer);
                let record = record.clone();
                tokio::spawn(async move {
                    finalize_one(
                        &store,
                        issuer.as_ref(),
                        coord.as_ref(),
                        record,
                        "https://acme/orders/42",
                    )
                    .await
                })
            })
            .collect();

        for run in runs {
            run.await
                .expect("task join")
                .expect("finalize_one should not error");
        }

        assert_eq!(
            issuer.calls(),
            1,
            "exactly one daemon should have run ACME finalize"
        );

        let row = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(
            row.state,
            CertificateState::Active,
            "issued cert must land on the row, not Failed"
        );
        let [version] = row.versions.as_slice() else {
            panic!(
                "expected exactly one issued version, got {:?}",
                row.versions
            );
        };
        assert_eq!(version.fullchain_pem, "winner-chain");
        assert!(row.last_error.is_none());
        assert!(row.order_url.is_none());
    }

    #[tokio::test]
    async fn challenge_visibility_failure_keeps_order_issuing_for_retry() {
        let store = StoreDriver::memory();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Issuing;
        record.order_url = Some("https://acme/orders/42".into());
        store
            .upsert_certificate(&record)
            .await
            .expect("issuing cert should persist");

        finalize_due_certificates(
            &store,
            &FakeIssuer::new(
                Err(Error::operation("unused", "start not called")),
                Err(Error::operation(
                    "acme_challenge_visibility",
                    "peer did not see challenge yet",
                )),
            ),
            &NoopIssuanceCoordinator,
        )
        .await
        .expect("retryable visibility errors are recorded per certificate");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Issuing);
        assert_eq!(record.order_url.as_deref(), Some("https://acme/orders/42"));
        assert!(
            record
                .last_error
                .as_deref()
                .is_some_and(|error| error.contains("peer did not see challenge yet"))
        );
    }

    #[tokio::test]
    async fn http01_visibility_wait_observes_store_challenge() {
        let store = StoreDriver::memory();
        store
            .upsert_acme_challenge(&AcmeChallengeRecord {
                hostname: "example.com".into(),
                token: "token-1".into(),
                key_authorization: "key-auth".into(),
                expires_at: now_unix_secs() + CHALLENGE_TTL_SECS,
                created_at: now_unix_secs(),
            })
            .await
            .expect("challenge should persist");

        wait_for_http01_challenge_visible(&store, "example.com", "token-1")
            .await
            .expect("stored challenge should be visible");
    }

    #[test]
    fn renewal_threshold_is_two_thirds_of_lifetime() {
        // 90-day cert → renew with 30 days remaining
        let ninety_days: u64 = 90 * 24 * 60 * 60;
        let threshold =
            renewal_threshold(Some(0), Some(ninety_days)).expect("threshold computable");
        assert_eq!(threshold, ninety_days * 2 / 3);
        // 6-day cert → renew with 2 days remaining
        let six_days: u64 = 6 * 24 * 60 * 60;
        let threshold = renewal_threshold(Some(0), Some(six_days)).expect("threshold computable");
        assert_eq!(threshold, six_days * 2 / 3);
    }

    #[test]
    fn account_id_tracks_issuer_url() {
        let issuer_url = "https://acme-staging-v02.api.letsencrypt.org/directory";
        assert_eq!(account_id_for_issuer_url(issuer_url), issuer_url);
    }

    #[tokio::test]
    async fn reconcile_flips_active_past_threshold_to_renewal_due() {
        let store = StoreDriver::memory();
        let now = now_unix_secs();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Active;
        record.active_version_id = Some("v1".into());
        record.next_renewal_at = Some(now.saturating_sub(10));
        store
            .upsert_certificate(&record)
            .await
            .expect("active cert should persist");

        reconcile_renewals(
            &store,
            &FakeIssuer::new(
                Err(Error::operation("fake_start_order", "no work expected")),
                Err(Error::operation("fake_finalize_order", "no work expected")),
            ),
            &NoopIssuanceCoordinator,
        )
        .await
        .expect("reconcile should run");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        // RenewalDue was picked up by start_pending_orders in the same pass.
        // FakeIssuer returned Err → state is Failed, last_error captured.
        assert_eq!(record.state, CertificateState::Failed);
        assert!(record.last_error.is_some());
    }

    #[tokio::test]
    async fn reconcile_resets_stuck_issuing_to_pending() {
        let store = StoreDriver::memory();
        let now = now_unix_secs();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Issuing;
        record.order_url = Some("https://acme/orders/stale".into());
        record.updated_at = now.saturating_sub(STUCK_ISSUING_MAX_AGE_SECS + 1);
        store
            .upsert_certificate(&record)
            .await
            .expect("stuck cert should persist");

        reconcile_renewals(
            &store,
            &FakeIssuer::new(
                Ok(StartedOrder {
                    order_url: "https://acme/orders/new".into(),
                }),
                Err(Error::operation("fake_finalize_order", "no work expected")),
            ),
            &NoopIssuanceCoordinator,
        )
        .await
        .expect("reconcile should run");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        // Stuck Issuing flipped to Pending, then start_pending_orders opened
        // a new order and moved it to Issuing with the fresh URL.
        assert_eq!(record.state, CertificateState::Issuing);
        assert_eq!(record.order_url.as_deref(), Some("https://acme/orders/new"));
    }

    #[tokio::test]
    async fn renewal_job_skips_active_before_threshold() {
        let store = StoreDriver::memory();
        let now = now_unix_secs();
        let mut record = pending_record("example.com");
        record.state = CertificateState::Active;
        record.active_version_id = Some("v1".into());
        record.next_renewal_at = Some(now.saturating_add(3600));
        store
            .upsert_certificate(&record)
            .await
            .expect("active cert should persist");

        process_renewal_job(
            &store,
            &FakeIssuer::start_only(Err(Error::operation(
                "fake_start_order",
                "should not be called",
            ))),
            &NoopIssuanceCoordinator,
            "example.com",
        )
        .await
        .expect("renewal job should run");

        let record = store
            .get_certificate("example.com")
            .await
            .expect("cert lookup should work")
            .expect("cert record should exist");
        assert_eq!(record.state, CertificateState::Active);
        assert!(record.last_error.is_none());
    }

    #[tokio::test]
    async fn renewal_job_processes_one_due_hostname() {
        let store = StoreDriver::memory();
        let now = now_unix_secs();
        let mut due = pending_record("due.example.com");
        due.state = CertificateState::Active;
        due.active_version_id = Some("v1".into());
        due.next_renewal_at = Some(now.saturating_sub(1));
        let mut other = pending_record("other.example.com");
        other.state = CertificateState::Active;
        other.active_version_id = Some("v1".into());
        other.next_renewal_at = Some(now.saturating_sub(1));
        store
            .upsert_certificate(&due)
            .await
            .expect("due cert should persist");
        store
            .upsert_certificate(&other)
            .await
            .expect("other cert should persist");

        process_renewal_job(
            &store,
            &FakeIssuer::start_only(Ok(StartedOrder {
                order_url: "https://acme/orders/due".into(),
            })),
            &NoopIssuanceCoordinator,
            "due.example.com",
        )
        .await
        .expect("renewal job should run");

        let due = store
            .get_certificate("due.example.com")
            .await
            .expect("cert lookup should work")
            .expect("due cert should exist");
        let other = store
            .get_certificate("other.example.com")
            .await
            .expect("cert lookup should work")
            .expect("other cert should exist");
        assert_eq!(due.state, CertificateState::Issuing);
        assert_eq!(due.order_url.as_deref(), Some("https://acme/orders/due"));
        assert_eq!(other.state, CertificateState::Active);
        assert!(other.order_url.is_none());
    }

    #[tokio::test]
    async fn renewal_job_skips_missing_hostname() {
        let store = StoreDriver::memory();

        process_renewal_job(
            &store,
            &FakeIssuer::start_only(Err(Error::operation(
                "fake_start_order",
                "should not be called",
            ))),
            &NoopIssuanceCoordinator,
            "missing.example.com",
        )
        .await
        .expect("missing row should be a no-op");
    }

    #[tokio::test(start_paused = true)]
    async fn http01_challenge_visibility_returns_immediately_when_already_present() {
        let store = StoreDriver::memory();
        store
            .upsert_acme_challenge(&AcmeChallengeRecord {
                hostname: "example.com".into(),
                token: "tok-A".into(),
                key_authorization: "tok-A.keyauth".into(),
                expires_at: now_unix_secs() + CHALLENGE_TTL_SECS,
                created_at: now_unix_secs(),
            })
            .await
            .expect("seed challenge");

        let started = tokio::time::Instant::now();
        wait_for_http01_challenge_visible(&store, "example.com", "tok-A")
            .await
            .expect("already-visible challenge should return Ok");
        // The function only sleeps after a miss, so the hit case should not
        // advance virtual time at all.
        assert_eq!(started.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn http01_challenge_visibility_succeeds_after_late_write() {
        let store = StoreDriver::memory();
        let writer_store = store.clone();
        let writer = tokio::spawn(async move {
            // Make the visibility loop poll a few times before the row appears.
            tokio::time::sleep(HTTP01_CHALLENGE_VISIBILITY_POLL * 50).await;
            writer_store
                .upsert_acme_challenge(&AcmeChallengeRecord {
                    hostname: "example.com".into(),
                    token: "tok-B".into(),
                    key_authorization: "tok-B.keyauth".into(),
                    expires_at: now_unix_secs() + CHALLENGE_TTL_SECS,
                    created_at: now_unix_secs(),
                })
                .await
                .expect("delayed challenge upsert");
        });

        wait_for_http01_challenge_visible(&store, "example.com", "tok-B")
            .await
            .expect("late-written challenge should be observed");
        writer.await.expect("writer should not panic");
    }

    #[tokio::test(start_paused = true)]
    async fn http01_challenge_visibility_times_out() {
        let store = StoreDriver::memory();

        let error = wait_for_http01_challenge_visible(&store, "example.com", "tok-missing")
            .await
            .expect_err("missing challenge should time out");

        assert!(
            matches!(
                &error,
                Error::Operation {
                    operation: "acme_challenge_visibility",
                    ..
                }
            ),
            "expected acme_challenge_visibility tag, got: {error:?}"
        );
        assert!(
            error.to_string().contains("example.com"),
            "expected hostname in error, got: {error}"
        );
    }

    fn pending_record(hostname: &str) -> CertificateRecord {
        CertificateRecord {
            hostname: hostname.into(),
            issuer_url: DEFAULT_ACME_DIRECTORY_URL.into(),
            account_id: account_id_for_issuer_url(DEFAULT_ACME_DIRECTORY_URL),
            state: CertificateState::Pending,
            active_version_id: None,
            versions: Vec::new(),
            order_url: None,
            last_error: None,
            requested_at: 1,
            updated_at: 1,
            next_renewal_at: None,
        }
    }
}
