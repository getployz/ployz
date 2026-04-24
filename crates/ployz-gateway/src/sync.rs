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

// ---------------------------------------------------------------------------
// RoutingStore trait — consumer contract
// ---------------------------------------------------------------------------

pub trait RoutingStore: Send + Sync {
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
    fn apply_certificate(&mut self, event: CertificateEvent) {
        match event {
            CertificateEvent::Added(record) | CertificateEvent::Updated(record) => {
                self.certificates.insert(record.hostname.clone(), record);
            }
            CertificateEvent::Removed(record) => {
                self.certificates.remove(&record.hostname);
            }
        }
    }

    fn apply_challenge(&mut self, event: AcmeChallengeEvent) {
        match event {
            AcmeChallengeEvent::Added(record) | AcmeChallengeEvent::Updated(record) => {
                self.challenges
                    .insert((record.hostname.clone(), record.token.clone()), record);
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
    S: RoutingStore + Send + Sync,
{
    let state = store.load_routing_state().await?;
    let mut snapshot = project(state).map_err(|err| GatewayError::Projection(err.to_string()))?;
    snapshot.certificates = project_certificates(&cache.certificate_records());
    snapshot.acme_challenges = project_acme_challenges(&cache.challenge_records());
    Ok(snapshot)
}

fn rebuild_snapshot<S>(
    store_routing_state: ployz_types::model::RoutingState,
    cache: &ManagedTlsCache,
) -> Result<GatewaySnapshot, GatewayError>
where
    S: RoutingStore,
{
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
    S: RoutingStore + Send + Sync,
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
        cache.certificates.insert(record.hostname.clone(), record);
    }
    for record in challenge_records {
        cache
            .challenges
            .insert((record.hostname.clone(), record.token.clone()), record);
    }
    load_initial_snapshot(store, &cache).await
}

pub async fn run_sync_loop<S>(store: S, snapshot: SharedSnapshot) -> Result<(), GatewayError>
where
    S: RoutingStore + Send + Sync + 'static,
{
    // Build initial cache from subscription snapshots.
    let (cert_records, mut cert_rx) = store.subscribe_certificates().await?;
    let (challenge_records, mut chal_rx) = store.subscribe_acme_challenges().await?;
    let mut cache = ManagedTlsCache::default();
    for record in cert_records {
        cache.certificates.insert(record.hostname.clone(), record);
    }
    for record in challenge_records {
        cache
            .challenges
            .insert((record.hostname.clone(), record.token.clone()), record);
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
    S: RoutingStore + Send + Sync,
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
    match rebuild_snapshot::<S>(routing_state, cache) {
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
    S: RoutingStore + Send + Sync + 'static,
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
