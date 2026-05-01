use std::future::Future;
use std::time::Duration;

use crate::routes::{GatewayProjectionEvent, GatewayProjector, GatewaySnapshot, ProjectionDelta};
use ployz_store_api::{
    AcmeChallengeSubscriptionUpdate, CertificateSubscriptionUpdate, RoutingBatchSubscription,
    RoutingEventBatch, RoutingSubscription,
};
use ployz_types::model::{
    AcmeChallengeEvent, AcmeChallengeReadinessRecord, AcmeChallengeRecord, CertificateEvent,
    CertificateRecord, MachineId, RoutingState,
};
use ployz_types::time::now_unix_secs;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::GatewayError;
use crate::snapshot::SharedSnapshot;

// Safety belt against runaway upstream growth — not capacity targets.
const MAX_CACHED_CERTIFICATES: usize = 10_000;
const MAX_CACHED_CHALLENGES: usize = 10_000;

// ---------------------------------------------------------------------------
// RoutingSnapshotReader trait — consumer contract
// ---------------------------------------------------------------------------

pub trait RoutingSnapshotReader: Send + Sync {
    fn load_routing_state(
        &self,
    ) -> impl Future<Output = Result<RoutingState, GatewayError>> + Send + '_;

    fn subscribe_routing_batches<'a>(
        &'a self,
        subscription: RoutingSubscription,
    ) -> impl Future<Output = Result<RoutingBatchSubscription, GatewayError>> + Send + 'a;
    fn list_certificates(
        &self,
    ) -> impl Future<Output = Result<Vec<CertificateRecord>, GatewayError>> + Send + '_;
    fn subscribe_certificates(
        &self,
    ) -> impl Future<
        Output = Result<
            (
                Vec<CertificateRecord>,
                mpsc::Receiver<CertificateSubscriptionUpdate>,
            ),
            GatewayError,
        >,
    > + Send
    + '_;
    fn list_acme_challenges(
        &self,
    ) -> impl Future<Output = Result<Vec<AcmeChallengeRecord>, GatewayError>> + Send + '_;
    fn subscribe_acme_challenges(
        &self,
    ) -> impl Future<
        Output = Result<
            (
                Vec<AcmeChallengeRecord>,
                mpsc::Receiver<AcmeChallengeSubscriptionUpdate>,
            ),
            GatewayError,
        >,
    > + Send
    + '_;
    fn upsert_acme_challenge_readiness<'a>(
        &'a self,
        record: &'a AcmeChallengeReadinessRecord,
    ) -> impl Future<Output = Result<(), GatewayError>> + Send + 'a;
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
    let routing_state = store.load_routing_state().await?;
    let mut projector = GatewayProjector::new(routing_state)
        .map_err(|err| GatewayError::Projection(err.to_string()))?;
    let cert_records = store.list_certificates().await?;
    apply_initial_certificates(&mut projector, cert_records);
    let challenge_records = store.list_acme_challenges().await?;
    apply_initial_challenges(&mut projector, challenge_records);
    Ok(projector.snapshot_value())
}

pub async fn run_sync_loop<S>(
    store: S,
    snapshot: SharedSnapshot,
    machine_id: MachineId,
) -> Result<(), GatewayError>
where
    S: RoutingSnapshotReader + Send + Sync + 'static,
{
    loop {
        let consumer_id = format!("gateway.{}", machine_id.0);
        let (routing_state, mut routing_rx) = match store
            .subscribe_routing_batches(RoutingSubscription::durable(consumer_id))
            .await
        {
            Ok(subscription) => subscription,
            Err(error) => {
                crate::metrics::set_store_sync_healthy("routing", false);
                warn!(
                    ?error,
                    "gateway routing subscription setup failed; retrying"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let (cert_records, mut cert_rx) = match store.subscribe_certificates().await {
            Ok(subscription) => subscription,
            Err(error) => {
                crate::metrics::set_store_sync_healthy("certificates", false);
                warn!(
                    ?error,
                    "gateway certificate subscription setup failed; retrying"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let (challenge_records, mut chal_rx) = match store.subscribe_acme_challenges().await {
            Ok(subscription) => subscription,
            Err(error) => {
                crate::metrics::set_store_sync_healthy("acme_challenges", false);
                warn!(
                    ?error,
                    "gateway ACME challenge subscription setup failed; retrying"
                );
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let mut projector = match GatewayProjector::new(routing_state) {
            Ok(projector) => projector,
            Err(error) => {
                crate::metrics::set_store_sync_healthy("routing", false);
                warn!(?error, "gateway routing projection setup failed; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        apply_initial_certificates(&mut projector, cert_records);
        let ready_challenges = apply_initial_challenges(&mut projector, challenge_records);
        publish_full_snapshot(&snapshot, projector.snapshot_value());
        publish_challenge_readiness(&store, &machine_id, &ready_challenges).await;
        crate::metrics::set_store_sync_healthy("routing", true);
        crate::metrics::set_store_sync_healthy("certificates", true);
        crate::metrics::set_store_sync_healthy("acme_challenges", true);

        loop {
            tokio::select! {
                batch = routing_rx.recv() => {
                    let Some(batch) = batch else {
                        crate::metrics::set_store_sync_healthy("routing", false);
                        warn!("gateway routing event stream closed; resubscribing");
                        break;
                    };
                    let batch = match batch {
                        Ok(batch) => batch,
                        Err(error) => {
                            crate::metrics::set_store_sync_healthy("routing", false);
                            warn!(%error, "gateway routing event stream failed; resubscribing");
                            break;
                        }
                    };
                    apply_routing_batch(&mut projector, batch, &snapshot).await;
                }
                event = cert_rx.recv() => {
                    let Some(event) = event else {
                        crate::metrics::set_store_sync_healthy("certificates", false);
                        warn!("gateway certificate event stream closed; resubscribing");
                        break;
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            crate::metrics::set_store_sync_healthy("certificates", false);
                            warn!(%error, "gateway certificate event stream failed; resubscribing");
                            break;
                        }
                    };
                    if apply_certificate_batch(&mut projector, event, &mut cert_rx, &snapshot) {
                        crate::metrics::set_store_sync_healthy("certificates", false);
                        break;
                    }
                }
                event = chal_rx.recv() => {
                    let Some(event) = event else {
                        crate::metrics::set_store_sync_healthy("acme_challenges", false);
                        warn!("gateway ACME challenge event stream closed; resubscribing");
                        break;
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            crate::metrics::set_store_sync_healthy("acme_challenges", false);
                            warn!(%error, "gateway ACME challenge event stream failed; resubscribing");
                            break;
                        }
                    };
                    let (ready_challenges, challenge_stream_failed) = apply_challenge_batch(&mut projector, event, &mut chal_rx, &snapshot);
                    publish_challenge_readiness(&store, &machine_id, &ready_challenges).await;
                    if challenge_stream_failed {
                        crate::metrics::set_store_sync_healthy("acme_challenges", false);
                        break;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
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
        apply_certificate_event(projector, CertificateEvent::Added(record));
    }
}

fn apply_initial_challenges(
    projector: &mut GatewayProjector,
    records: Vec<AcmeChallengeRecord>,
) -> Vec<AcmeChallengeRecord> {
    if records.len() > MAX_CACHED_CHALLENGES {
        warn!(
            challenges = records.len(),
            cap = MAX_CACHED_CHALLENGES,
            "initial ACME challenge snapshot exceeds gateway cap"
        );
    }
    let mut applied = Vec::new();
    for record in records.into_iter().take(MAX_CACHED_CHALLENGES) {
        if apply_challenge_event(projector, AcmeChallengeEvent::Added(record.clone())).is_some() {
            applied.push(record);
        }
    }
    applied
}

async fn apply_routing_batch(
    projector: &mut GatewayProjector,
    batch: RoutingEventBatch,
    snapshot: &SharedSnapshot,
) {
    let batch_id = batch.batch_id.clone();
    let mut deltas = Vec::new();
    let mut candidate = projector.clone();
    for event in &batch.events {
        match candidate.apply(GatewayProjectionEvent::Routing(event.clone())) {
            Ok(ProjectionDelta::Empty) => {}
            Ok(delta) => deltas.push(delta),
            Err(err) => {
                warn!(
                    ?err,
                    batch_id = %batch_id,
                    "failed to apply gateway routing batch; keeping previous snapshot"
                );
                return;
            }
        }
    }
    *projector = candidate;
    if !deltas.is_empty() {
        publish_deltas(snapshot, &deltas);
    }
    if let Err(error) = batch.ack().await {
        warn!(?error, batch_id = %batch_id, "gateway routing batch ack failed");
    }
}

fn apply_certificate_batch(
    projector: &mut GatewayProjector,
    event: CertificateEvent,
    cert_rx: &mut mpsc::Receiver<CertificateSubscriptionUpdate>,
    snapshot: &SharedSnapshot,
) -> bool {
    let mut deltas = Vec::new();
    if let Some(delta) = apply_certificate_event(projector, event) {
        deltas.push(delta);
    }
    let mut stream_failed = false;
    while let Ok(event) = cert_rx.try_recv() {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                warn!(%error, "gateway certificate event stream failed while draining; resubscribing");
                stream_failed = true;
                break;
            }
        };
        if let Some(delta) = apply_certificate_event(projector, event) {
            deltas.push(delta);
        }
    }
    if !deltas.is_empty() {
        publish_deltas(snapshot, &deltas);
    }
    stream_failed
}

fn apply_challenge_batch(
    projector: &mut GatewayProjector,
    event: AcmeChallengeEvent,
    chal_rx: &mut mpsc::Receiver<AcmeChallengeSubscriptionUpdate>,
    snapshot: &SharedSnapshot,
) -> (Vec<AcmeChallengeRecord>, bool) {
    let mut deltas = Vec::new();
    let mut ready_challenges = Vec::new();
    let mut stream_failed = false;
    if let Some(delta) = apply_challenge_event(projector, event.clone()) {
        deltas.push(delta);
        if let Some(record) = challenge_record_for_readiness(&event) {
            ready_challenges.push(record);
        }
    }
    while let Ok(event) = chal_rx.try_recv() {
        let event = match event {
            Ok(event) => event,
            Err(error) => {
                warn!(%error, "gateway ACME challenge event stream failed while draining; resubscribing");
                stream_failed = true;
                break;
            }
        };
        if let Some(delta) = apply_challenge_event(projector, event.clone()) {
            deltas.push(delta);
            if let Some(record) = challenge_record_for_readiness(&event) {
                ready_challenges.push(record);
            }
        }
    }
    if !deltas.is_empty() {
        publish_deltas(snapshot, &deltas);
    }
    (ready_challenges, stream_failed)
}

fn challenge_record_for_readiness(event: &AcmeChallengeEvent) -> Option<AcmeChallengeRecord> {
    match event {
        AcmeChallengeEvent::Added(record) | AcmeChallengeEvent::Updated(record) => {
            Some(record.clone())
        }
        AcmeChallengeEvent::Removed(_) => None,
    }
}

async fn publish_challenge_readiness<S>(
    store: &S,
    machine_id: &MachineId,
    records: &[AcmeChallengeRecord],
) where
    S: RoutingSnapshotReader + Send + Sync,
{
    for record in records {
        let readiness = AcmeChallengeReadinessRecord {
            hostname: record.hostname.clone(),
            token: record.token.clone(),
            machine_id: machine_id.clone(),
            observed_at: now_unix_secs(),
        };
        if let Err(error) = store.upsert_acme_challenge_readiness(&readiness).await {
            warn!(
                hostname = %readiness.hostname,
                token = %readiness.token,
                machine_id = %readiness.machine_id,
                ?error,
                "failed to publish ACME challenge readiness observation"
            );
        }
    }
}

fn apply_one(
    projector: &mut GatewayProjector,
    event: GatewayProjectionEvent,
) -> Option<ProjectionDelta> {
    match projector.apply(event) {
        Ok(ProjectionDelta::Empty) => None,
        Ok(delta) => Some(delta),
        Err(err) => {
            warn!(
                ?err,
                "failed to apply gateway projection event; keeping previous state"
            );
            None
        }
    }
}

fn apply_certificate_event(
    projector: &mut GatewayProjector,
    event: CertificateEvent,
) -> Option<ProjectionDelta> {
    match &event {
        CertificateEvent::Added(record) | CertificateEvent::Updated(record)
            if !projector.has_certificate(&record.hostname)
                && projector.certificate_count() >= MAX_CACHED_CERTIFICATES =>
        {
            warn!(
                hostname = %record.hostname,
                cached = projector.certificate_count(),
                cap = MAX_CACHED_CERTIFICATES,
                "managed-TLS cache is at the certificate cap; dropping new record"
            );
            None
        }
        CertificateEvent::Added(_)
        | CertificateEvent::Updated(_)
        | CertificateEvent::Removed(_) => {
            apply_one(projector, GatewayProjectionEvent::Certificate(event))
        }
    }
}

fn apply_challenge_event(
    projector: &mut GatewayProjector,
    event: AcmeChallengeEvent,
) -> Option<ProjectionDelta> {
    match &event {
        AcmeChallengeEvent::Added(record) | AcmeChallengeEvent::Updated(record)
            if !projector.has_acme_challenge(&record.hostname, &record.token)
                && projector.acme_challenge_count() >= MAX_CACHED_CHALLENGES =>
        {
            warn!(
                hostname = %record.hostname,
                token = %record.token,
                cached = projector.acme_challenge_count(),
                cap = MAX_CACHED_CHALLENGES,
                "managed-TLS cache is at the challenge cap; dropping new record"
            );
            None
        }
        AcmeChallengeEvent::Added(_)
        | AcmeChallengeEvent::Updated(_)
        | AcmeChallengeEvent::Removed(_) => {
            apply_one(projector, GatewayProjectionEvent::AcmeChallenge(event))
        }
    }
}

fn publish_full_snapshot(snapshot: &SharedSnapshot, next_snapshot: GatewaySnapshot) {
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

fn publish_deltas(snapshot: &SharedSnapshot, deltas: &[ProjectionDelta]) {
    snapshot.apply_deltas(deltas);
    let state = snapshot.load();
    let (http_routes, tcp_routes) = state.route_counts();
    crate::metrics::update_route_count_values(http_routes, tcp_routes);
    info!(http_routes, tcp_routes, "gateway snapshot refreshed");
}

pub fn spawn_sync_thread_with_store<S>(
    store: S,
    snapshot: SharedSnapshot,
    machine_id: MachineId,
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
                if let Err(err) = run_sync_loop(store, snapshot, machine_id).await {
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

    #[test]
    fn initial_challenge_snapshot_marks_applied_records_ready() {
        let mut projector = GatewayProjector::new(RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: Vec::new(),
        })
        .expect("empty routing state projects");
        let record = AcmeChallengeRecord {
            hostname: "example.com".into(),
            token: "token-a".into(),
            key_authorization: "token-a.auth".into(),
            expires_at: 100,
            created_at: 1,
        };

        let ready = apply_initial_challenges(&mut projector, vec![record.clone()]);

        assert_eq!(ready, vec![record]);
    }

    #[test]
    fn removed_challenge_event_does_not_publish_readiness() {
        let event = AcmeChallengeEvent::Removed(AcmeChallengeRecord {
            hostname: "example.com".into(),
            token: "token-a".into(),
            key_authorization: "token-a.auth".into(),
            expires_at: 100,
            created_at: 1,
        });

        assert!(challenge_record_for_readiness(&event).is_none());
    }

    #[test]
    fn challenge_update_event_marks_record_ready() {
        let _metrics_guard = crate::metrics::ROUTE_METRICS_TEST_LOCK
            .lock()
            .expect("route metrics test lock should not be poisoned");
        let mut projector = GatewayProjector::new(RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: Vec::new(),
        })
        .expect("empty routing state projects");
        let snapshot = SharedSnapshot::new(projector.snapshot_value());
        let record = AcmeChallengeRecord {
            hostname: "example.com".into(),
            token: "token-a".into(),
            key_authorization: "token-a.auth".into(),
            expires_at: 100,
            created_at: 1,
        };
        let mut rx = mpsc::channel(1).1;

        let (ready, stream_failed) = apply_challenge_batch(
            &mut projector,
            AcmeChallengeEvent::Updated(record.clone()),
            &mut rx,
            &snapshot,
        );

        assert_eq!(ready, vec![record]);
        assert!(!stream_failed);
    }
}
