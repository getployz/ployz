use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use crate::routes::{GatewaySnapshot, project, project_acme_challenges, project_certificates};
use ployz_types::model::{
    AcmeChallengeEvent, AcmeChallengeRecord, CertificateEvent, CertificateRecord,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::GatewayError;
use crate::snapshot::SharedSnapshot;

const REFRESH_DEBOUNCE: Duration = Duration::from_millis(100);

/// Paranoia caps on the in-memory mirror of the certificate / ACME challenge
/// tables. These are not capacity targets — they're a safety belt: if a buggy
/// upstream loops on challenge issuance or projection feedback ever pushes
/// records in faster than we expect, the gateway logs and degrades instead of
/// growing the cache without bound. The current ployz scale runs well under
/// these limits.
const MAX_CACHED_CERTIFICATES: usize = 10_000;
const MAX_CACHED_CHALLENGES: usize = 10_000;

// ---------------------------------------------------------------------------
// RoutingSnapshotReader trait — consumer contract
// ---------------------------------------------------------------------------

pub trait RoutingSnapshotReader: Send + Sync {
    fn load_routing_state(
        &self,
    ) -> impl Future<Output = Result<ployz_types::model::RoutingState, GatewayError>> + Send + '_;
    fn subscribe_routing_invalidations(
        &self,
    ) -> impl Future<Output = Result<mpsc::Receiver<()>, GatewayError>> + Send + '_;
    fn list_certificates(
        &self,
    ) -> impl Future<Output = Result<Vec<CertificateRecord>, GatewayError>> + Send + '_;
    fn subscribe_certificates(
        &self,
    ) -> impl Future<
        Output = Result<(Vec<CertificateRecord>, mpsc::Receiver<CertificateEvent>), GatewayError>,
    > + Send
    + '_;
    fn list_acme_challenges(
        &self,
    ) -> impl Future<Output = Result<Vec<AcmeChallengeRecord>, GatewayError>> + Send + '_;
    fn subscribe_acme_challenges(
        &self,
    ) -> impl Future<
        Output = Result<
            (Vec<AcmeChallengeRecord>, mpsc::Receiver<AcmeChallengeEvent>),
            GatewayError,
        >,
    > + Send
    + '_;
}

// ---------------------------------------------------------------------------
// Managed-TLS state cache
// ---------------------------------------------------------------------------

/// Authoritative in-memory mirror of the `certificates` and `acme_challenges`
/// tables. Populated from a normal query at startup, then kept current by the
/// per-row event stream. Every snapshot rebuild projects from these maps — no
/// full-table pull per invalidation.
#[derive(Default)]
struct ManagedTlsCache {
    certificates: HashMap<String, CertificateRecord>,
    challenges: HashMap<(String, String), AcmeChallengeRecord>,
}

impl ManagedTlsCache {
    fn insert_certificate(&mut self, record: CertificateRecord) {
        if !self.certificates.contains_key(&record.hostname)
            && self.certificates.len() >= MAX_CACHED_CERTIFICATES
        {
            warn!(
                hostname = %record.hostname,
                cached = self.certificates.len(),
                cap = MAX_CACHED_CERTIFICATES,
                "managed-TLS cache is at the certificate cap; dropping new record"
            );
            return;
        }
        self.certificates.insert(record.hostname.clone(), record);
    }

    fn insert_challenge(&mut self, record: AcmeChallengeRecord) {
        let key = (record.hostname.clone(), record.token.clone());
        if !self.challenges.contains_key(&key) && self.challenges.len() >= MAX_CACHED_CHALLENGES {
            warn!(
                hostname = %record.hostname,
                token = %record.token,
                cached = self.challenges.len(),
                cap = MAX_CACHED_CHALLENGES,
                "managed-TLS cache is at the challenge cap; dropping new record"
            );
            return;
        }
        self.challenges.insert(key, record);
    }

    fn apply_certificate(&mut self, event: CertificateEvent) {
        match event {
            CertificateEvent::Added(record) | CertificateEvent::Updated(record) => {
                self.insert_certificate(record);
            }
            CertificateEvent::Removed(record) => {
                self.certificates.remove(&record.hostname);
            }
        }
    }

    fn apply_challenge(&mut self, event: AcmeChallengeEvent) {
        match event {
            AcmeChallengeEvent::Added(record) | AcmeChallengeEvent::Updated(record) => {
                self.insert_challenge(record);
            }
            AcmeChallengeEvent::Removed(record) => {
                self.challenges.remove(&(record.hostname, record.token));
            }
        }
    }

    fn certificate_records(&self) -> Vec<CertificateRecord> {
        self.certificates.values().cloned().collect()
    }

    fn challenge_records(&self) -> Vec<AcmeChallengeRecord> {
        self.challenges.values().cloned().collect()
    }
}

// ---------------------------------------------------------------------------
// Sync logic
// ---------------------------------------------------------------------------

async fn load_initial_snapshot<S>(
    store: &S,
    cache: &ManagedTlsCache,
) -> Result<GatewaySnapshot, GatewayError>
where
    S: RoutingSnapshotReader + Send + Sync,
{
    let state = store.load_routing_state().await?;
    let mut snapshot = project(state).map_err(|err| GatewayError::Projection(err.to_string()))?;
    snapshot.certificates = project_certificates(&cache.certificate_records());
    snapshot.acme_challenges = project_acme_challenges(&cache.challenge_records());
    Ok(snapshot)
}

fn rebuild_snapshot(
    store_routing_state: ployz_types::model::RoutingState,
    cache: &ManagedTlsCache,
) -> Result<GatewaySnapshot, GatewayError> {
    let mut snapshot =
        project(store_routing_state).map_err(|err| GatewayError::Projection(err.to_string()))?;
    snapshot.certificates = project_certificates(&cache.certificate_records());
    snapshot.acme_challenges = project_acme_challenges(&cache.challenge_records());
    Ok(snapshot)
}

pub async fn load_projected_snapshot_from_store<S>(
    store: &S,
) -> Result<GatewaySnapshot, GatewayError>
where
    S: RoutingSnapshotReader + Send + Sync,
{
    // Boot path mirrors routing state: use bounded point-in-time queries for
    // the initial snapshot, then let the background sync loop attach live
    // subscriptions after listeners are able to start.
    info!("gateway loading initial certificate snapshot");
    let cert_records = store.list_certificates().await?;
    info!(
        certificates = cert_records.len(),
        "gateway loaded initial certificate snapshot"
    );
    info!("gateway loading initial ACME challenge snapshot");
    let challenge_records = store.list_acme_challenges().await?;
    info!(
        challenges = challenge_records.len(),
        "gateway loaded initial ACME challenge snapshot"
    );
    let mut cache = ManagedTlsCache::default();
    for record in cert_records {
        cache.insert_certificate(record);
    }
    for record in challenge_records {
        cache.insert_challenge(record);
    }
    load_initial_snapshot(store, &cache).await
}

pub async fn run_sync_loop<S>(store: S, snapshot: SharedSnapshot) -> Result<(), GatewayError>
where
    S: RoutingSnapshotReader + Send + Sync + 'static,
{
    // Build initial cache from subscription snapshots.
    let (cert_records, mut cert_rx) = store.subscribe_certificates().await?;
    let (challenge_records, mut chal_rx) = store.subscribe_acme_challenges().await?;
    let mut cache = ManagedTlsCache::default();
    for record in cert_records {
        cache.insert_certificate(record);
    }
    for record in challenge_records {
        cache.insert_challenge(record);
    }

    let mut refresh_rx = store.subscribe_routing_invalidations().await?;

    // Emit an initial snapshot using whatever we already have.
    refresh_and_replace(&store, &cache, &snapshot).await;

    loop {
        tokio::select! {
            Some(_) = refresh_rx.recv() => {
                tokio::time::sleep(REFRESH_DEBOUNCE).await;
                while refresh_rx.try_recv().is_ok() {}
                refresh_and_replace(&store, &cache, &snapshot).await;
            }
            Some(event) = cert_rx.recv() => {
                cache.apply_certificate(event);
                // drain any burst before rebuilding
                while let Ok(next) = cert_rx.try_recv() {
                    cache.apply_certificate(next);
                }
                refresh_and_replace(&store, &cache, &snapshot).await;
            }
            Some(event) = chal_rx.recv() => {
                cache.apply_challenge(event);
                while let Ok(next) = chal_rx.try_recv() {
                    cache.apply_challenge(next);
                }
                refresh_and_replace(&store, &cache, &snapshot).await;
            }
            else => break,
        }
    }

    Ok(())
}

async fn refresh_and_replace<S>(store: &S, cache: &ManagedTlsCache, snapshot: &SharedSnapshot)
where
    S: RoutingSnapshotReader + Send + Sync,
{
    let routing_state = match store.load_routing_state().await {
        Ok(state) => state,
        Err(err) => {
            warn!(
                ?err,
                "failed to load routing state for snapshot rebuild; keeping previous state"
            );
            return;
        }
    };
    match rebuild_snapshot(routing_state, cache) {
        Ok(next_snapshot) => {
            let http_routes = next_snapshot.http_routes.len();
            let tcp_routes = next_snapshot.tcp_routes.len();
            let certs = next_snapshot.certificates.len();
            let challenges = next_snapshot.acme_challenges.len();
            crate::metrics::update_route_counts(&next_snapshot);
            snapshot.replace(next_snapshot);
            info!(
                http_routes,
                tcp_routes, certs, challenges, "gateway snapshot refreshed"
            );
        }
        Err(err) => {
            warn!(
                ?err,
                "failed to rebuild gateway snapshot; keeping previous state"
            )
        }
    }
}

pub fn spawn_sync_thread_with_store<S>(
    store: S,
    snapshot: SharedSnapshot,
) -> Result<(), GatewayError>
where
    S: RoutingSnapshotReader + Send + Sync + 'static,
{
    std::thread::Builder::new()
        .name("ployz-gateway-sync".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    warn!(?err, "failed to create gateway sync runtime");
                    return;
                }
            };
            runtime.block_on(async move {
                if let Err(err) = run_sync_loop(store, snapshot).await {
                    warn!(?err, "gateway sync loop exited");
                }
            });
        })
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{CertificateRecord, CertificateState};

    fn record(hostname: &str) -> CertificateRecord {
        CertificateRecord {
            hostname: hostname.into(),
            issuer_url: "https://acme.test/directory".into(),
            account_id: "acct".into(),
            state: CertificateState::Active,
            active_version_id: None,
            versions: Vec::new(),
            order_url: None,
            last_error: None,
            requested_at: 0,
            updated_at: 0,
            next_renewal_at: None,
        }
    }

    #[test]
    fn certificate_cache_caps_at_max_cached_certificates() {
        let mut cache = ManagedTlsCache::default();
        for i in 0..=MAX_CACHED_CERTIFICATES {
            cache.insert_certificate(record(&format!("host-{i}.example.com")));
        }
        assert_eq!(
            cache.certificates.len(),
            MAX_CACHED_CERTIFICATES,
            "cache must not exceed the cap"
        );
    }

    #[test]
    fn certificate_cache_replaces_existing_hostname_at_cap() {
        // Updates to a hostname already in the cache must always succeed —
        // otherwise a renewal could be silently dropped once the cap is hit.
        let mut cache = ManagedTlsCache::default();
        for i in 0..MAX_CACHED_CERTIFICATES {
            cache.insert_certificate(record(&format!("host-{i}.example.com")));
        }
        assert_eq!(cache.certificates.len(), MAX_CACHED_CERTIFICATES);
        let mut updated = record("host-0.example.com");
        updated.account_id = "rotated".into();
        cache.insert_certificate(updated);
        assert_eq!(cache.certificates.len(), MAX_CACHED_CERTIFICATES);
        assert_eq!(
            cache.certificates["host-0.example.com"].account_id,
            "rotated"
        );
    }
}
