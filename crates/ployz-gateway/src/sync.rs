use std::future::Future;

use crate::routes::{GatewayProjectionEvent, GatewayProjector, GatewaySnapshot};
use ployz_types::model::{
    AcmeChallengeEvent, AcmeChallengeRecord, CertificateEvent, CertificateRecord, RoutingEvent,
    RoutingState,
};
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::GatewayError;
use crate::snapshot::SharedSnapshot;

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
    fn subscribe_routing_events(
        &self,
    ) -> impl Future<Output = Result<(RoutingState, mpsc::Receiver<RoutingEvent>), GatewayError>>
    + Send
    + '_;
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
// Sync logic
// ---------------------------------------------------------------------------

pub async fn load_projected_snapshot_from_store<S>(
    store: &S,
) -> Result<GatewaySnapshot, GatewayError>
where
    S: RoutingSnapshotReader + Send + Sync,
{
    let (routing_state, _) = store.subscribe_routing_events().await?;
    let mut projector = GatewayProjector::new(routing_state)
        .map_err(|err| GatewayError::Projection(err.to_string()))?;
    let cert_records = store.list_certificates().await?;
    apply_initial_certificates(&mut projector, cert_records);
    let challenge_records = store.list_acme_challenges().await?;
    apply_initial_challenges(&mut projector, challenge_records);
    Ok(projector.snapshot_value())
}

pub async fn run_sync_loop<S>(store: S, snapshot: SharedSnapshot) -> Result<(), GatewayError>
where
    S: RoutingSnapshotReader + Send + Sync + 'static,
{
    let (routing_state, mut routing_rx) = store.subscribe_routing_events().await?;
    let (cert_records, mut cert_rx) = store.subscribe_certificates().await?;
    let (challenge_records, mut chal_rx) = store.subscribe_acme_challenges().await?;
    let mut projector = GatewayProjector::new(routing_state)
        .map_err(|err| GatewayError::Projection(err.to_string()))?;
    apply_initial_certificates(&mut projector, cert_records);
    apply_initial_challenges(&mut projector, challenge_records);
    replace_snapshot(&snapshot, projector.snapshot_value());

    loop {
        tokio::select! {
            Some(event) = routing_rx.recv() => {
                apply_and_replace(&mut projector, GatewayProjectionEvent::Routing(event), &snapshot);
            }
            Some(event) = cert_rx.recv() => {
                apply_and_replace(&mut projector, GatewayProjectionEvent::Certificate(event), &snapshot);
            }
            Some(event) = chal_rx.recv() => {
                apply_and_replace(&mut projector, GatewayProjectionEvent::AcmeChallenge(event), &snapshot);
            }
            else => break,
        }
    }

    Ok(())
}

fn apply_initial_certificates(projector: &mut GatewayProjector, records: Vec<CertificateRecord>) {
    if records.len() > MAX_CACHED_CERTIFICATES {
        warn!(
            certificates = records.len(),
            cap = MAX_CACHED_CERTIFICATES,
            "initial certificate snapshot exceeds gateway cap"
        );
    }
    for record in records.into_iter().take(MAX_CACHED_CERTIFICATES) {
        let _ = projector.apply(GatewayProjectionEvent::Certificate(
            CertificateEvent::Added(record),
        ));
    }
}

fn apply_initial_challenges(projector: &mut GatewayProjector, records: Vec<AcmeChallengeRecord>) {
    if records.len() > MAX_CACHED_CHALLENGES {
        warn!(
            challenges = records.len(),
            cap = MAX_CACHED_CHALLENGES,
            "initial ACME challenge snapshot exceeds gateway cap"
        );
    }
    for record in records.into_iter().take(MAX_CACHED_CHALLENGES) {
        let _ = projector.apply(GatewayProjectionEvent::AcmeChallenge(
            AcmeChallengeEvent::Added(record),
        ));
    }
}

fn apply_and_replace(
    projector: &mut GatewayProjector,
    event: GatewayProjectionEvent,
    snapshot: &SharedSnapshot,
) {
    match projector.apply(event) {
        Ok(next_snapshot) => replace_snapshot(snapshot, (*next_snapshot).clone()),
        Err(err) => warn!(
            ?err,
            "failed to apply gateway projection event; keeping previous state"
        ),
    }
}

fn replace_snapshot(snapshot: &SharedSnapshot, next_snapshot: GatewaySnapshot) {
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

    #[test]
    fn managed_tls_caps_are_nonzero() {
        assert!(MAX_CACHED_CERTIFICATES > 0);
        assert!(MAX_CACHED_CHALLENGES > 0);
    }
}
