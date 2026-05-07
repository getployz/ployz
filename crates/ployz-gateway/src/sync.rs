use std::future::Future;
use std::time::Duration;

use crate::routes::{GatewayProjectionEvent, GatewayProjector, GatewaySnapshot, ProjectionDelta};
use ployz_store_api::{
    AcmeChallengeSubscriptionUpdate, CertificateSubscriptionUpdate, RoutingEventEnvelope,
    RoutingEventSubscription,
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
const STREAM_ROUTING: &str = "routing";
const STREAM_CERTIFICATES: &str = "certificates";
const STREAM_ACME_CHALLENGES: &str = "acme_challenges";
const STORE_SYNC_STREAMS: [&str; 3] = [STREAM_ROUTING, STREAM_CERTIFICATES, STREAM_ACME_CHALLENGES];

// ---------------------------------------------------------------------------
// RoutingSnapshotReader trait — consumer contract
// ---------------------------------------------------------------------------

pub trait RoutingSnapshotReader: Send + Sync {
    fn load_routing_state(
        &self,
    ) -> impl Future<Output = Result<RoutingState, GatewayError>> + Send + '_;

    fn subscribe_routing_events(
        &self,
    ) -> impl Future<Output = Result<RoutingEventSubscription, GatewayError>> + Send + '_;
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
        let (routing_state, mut routing_rx) = match store.subscribe_routing_events().await {
            Ok(subscription) => subscription,
            Err(error) => {
                set_store_sync_generation_healthy(false);
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
                set_store_sync_generation_healthy(false);
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
                set_store_sync_generation_healthy(false);
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
                set_store_sync_generation_healthy(false);
                warn!(?error, "gateway routing projection setup failed; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        apply_initial_certificates(&mut projector, cert_records);
        let ready_challenges = apply_initial_challenges(&mut projector, challenge_records);
        publish_full_snapshot(&snapshot, projector.snapshot_value());
        publish_challenge_readiness(&store, &machine_id, &ready_challenges).await;
        set_store_sync_generation_healthy(true);

        loop {
            tokio::select! {
                envelope = routing_rx.recv() => {
                    let Some(envelope) = envelope else {
                        set_store_sync_generation_healthy(false);
                        warn!("gateway routing event stream closed; resubscribing");
                        break;
                    };
                    let envelope = match envelope {
                        Ok(envelope) => envelope,
                        Err(error) => {
                            set_store_sync_generation_healthy(false);
                            warn!(%error, "gateway routing event stream failed; resubscribing");
                            break;
                        }
                    };
                    apply_routing_envelope(&mut projector, envelope, &snapshot).await;
                }
                event = cert_rx.recv() => {
                    let Some(event) = event else {
                        set_store_sync_generation_healthy(false);
                        warn!("gateway certificate event stream closed; resubscribing");
                        break;
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            set_store_sync_generation_healthy(false);
                            warn!(%error, "gateway certificate event stream failed; resubscribing");
                            break;
                        }
                    };
                    if drain_certificate_events(&mut projector, event, &mut cert_rx, &snapshot) {
                        set_store_sync_generation_healthy(false);
                        break;
                    }
                }
                event = chal_rx.recv() => {
                    let Some(event) = event else {
                        set_store_sync_generation_healthy(false);
                        warn!("gateway ACME challenge event stream closed; resubscribing");
                        break;
                    };
                    let event = match event {
                        Ok(event) => event,
                        Err(error) => {
                            set_store_sync_generation_healthy(false);
                            warn!(%error, "gateway ACME challenge event stream failed; resubscribing");
                            break;
                        }
                    };
                    let (ready_challenges, challenge_stream_failed) = drain_challenge_events(&mut projector, event, &mut chal_rx, &snapshot);
                    publish_challenge_readiness(&store, &machine_id, &ready_challenges).await;
                    if challenge_stream_failed {
                        set_store_sync_generation_healthy(false);
                        break;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn set_store_sync_generation_healthy(healthy: bool) {
    for stream in STORE_SYNC_STREAMS {
        crate::metrics::set_store_sync_healthy(stream, healthy);
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

async fn apply_routing_envelope(
    projector: &mut GatewayProjector,
    envelope: RoutingEventEnvelope,
    snapshot: &SharedSnapshot,
) {
    let event_id = envelope.event_id.clone();
    let mut candidate = projector.clone();
    let delta = match candidate.apply(GatewayProjectionEvent::Routing(envelope.event.clone())) {
        Ok(ProjectionDelta::Empty) => None,
        Ok(delta) => Some(delta),
        Err(err) => {
            warn!(
                ?err,
                event_id = %event_id,
                "failed to apply gateway routing event; keeping previous snapshot"
            );
            return;
        }
    };
    *projector = candidate;
    if let Some(delta) = delta {
        publish_deltas(snapshot, &[delta]);
    }
    if let Err(error) = envelope.ack().await {
        set_store_sync_generation_healthy(false);
        warn!(?error, event_id = %event_id, "gateway routing event ack failed");
    }
}

fn drain_certificate_events(
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

fn drain_challenge_events(
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
    use prometheus::Encoder;
    use tokio::sync::oneshot;

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

        let (ready, stream_failed) = drain_challenge_events(
            &mut projector,
            AcmeChallengeEvent::Updated(record.clone()),
            &mut rx,
            &snapshot,
        );

        assert_eq!(ready, vec![record]);
        assert!(!stream_failed);
    }

    #[test]
    fn challenge_drain_publishes_applied_events_before_reporting_stream_failure() {
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
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(Err(ployz_types::error::Error::operation(
            "test_stream_failure",
            "challenge stream failed",
        )))
        .expect("queue stream error");

        let (ready, stream_failed) = drain_challenge_events(
            &mut projector,
            AcmeChallengeEvent::Added(record.clone()),
            &mut rx,
            &snapshot,
        );

        assert_eq!(ready, vec![record]);
        assert!(stream_failed);
        let view = snapshot.load().to_view_snapshot();
        assert!(
            view.acme_challenges
                .contains_key(&("example.com".into(), "token-a".into())),
            "applied challenge should remain visible before resubscribe"
        );
    }

    #[test]
    fn gateway_sync_generation_health_covers_all_streams() {
        let _metrics_guard = crate::metrics::ROUTE_METRICS_TEST_LOCK
            .lock()
            .expect("route metrics test lock should not be poisoned");

        set_store_sync_generation_healthy(true);
        set_store_sync_generation_healthy(false);

        let metrics = prometheus::gather();
        let mut buffer = Vec::new();
        prometheus::TextEncoder::new()
            .encode(&metrics, &mut buffer)
            .expect("encode metrics");
        let text = String::from_utf8(buffer).expect("metrics should be utf8");

        for stream in STORE_SYNC_STREAMS {
            assert!(
                text.contains(&format!(
                    "ployz_gateway_store_sync_healthy{{stream=\"{stream}\"}} 0"
                )),
                "missing unhealthy metric for {stream}:\n{text}"
            );
        }
    }

    #[tokio::test]
    async fn routing_ack_failure_marks_gateway_sync_unhealthy_after_snapshot_swap() {
        let _metrics_guard = crate::metrics::ROUTE_METRICS_TEST_LOCK
            .lock()
            .expect("route metrics test lock should not be poisoned");
        set_store_sync_generation_healthy(true);
        let mut projector = GatewayProjector::new(RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: Vec::new(),
        })
        .expect("empty routing state projects");
        let snapshot = SharedSnapshot::new(projector.snapshot_value());
        let (ack_tx, ack_rx) = oneshot::channel();
        drop(ack_rx);

        apply_routing_envelope(
            &mut projector,
            RoutingEventEnvelope::with_ack(
                "event-1",
                Some("test".into()),
                ployz_types::model::RoutingEvent::RevisionAdded(
                    ployz_types::model::ServiceRevisionRecord {
                        namespace: ployz_types::spec::Namespace("prod".into()),
                        service: "api".into(),
                        revision_hash: "rev-1".into(),
                        spec_json: "{}".into(),
                        created_by: MachineId("machine-1".into()),
                        created_at: 1,
                    },
                ),
                ack_tx,
            ),
            &snapshot,
        )
        .await;

        let metrics = prometheus::gather();
        let mut buffer = Vec::new();
        prometheus::TextEncoder::new()
            .encode(&metrics, &mut buffer)
            .expect("encode metrics");
        let text = String::from_utf8(buffer).expect("metrics should be utf8");
        for stream in STORE_SYNC_STREAMS {
            assert!(
                text.contains(&format!(
                    "ployz_gateway_store_sync_healthy{{stream=\"{stream}\"}} 0"
                )),
                "missing unhealthy metric for {stream}:\n{text}"
            );
            assert!(
                text.contains(&format!(
                    "ployz_gateway_store_sync_failures_total{{stream=\"{stream}\"}}"
                )),
                "missing failure counter for {stream}:\n{text}"
            );
        }
    }
}
