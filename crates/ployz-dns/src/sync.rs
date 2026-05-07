use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use tracing::{info, warn};

use crate::config::DnsError;
use crate::snapshot::{SharedDnsSnapshot, project_dns};

// ---------------------------------------------------------------------------
// DnsStore trait — consumer contract
// ---------------------------------------------------------------------------

pub trait DnsStore: Send + Sync {
    fn subscribe_routing_events(
        &self,
    ) -> impl Future<Output = Result<ployz_store_api::RoutingEventSubscription, DnsError>> + Send + '_;
}

// ---------------------------------------------------------------------------
// Sync logic
// ---------------------------------------------------------------------------

pub async fn run_sync_loop<S>(store: S, snapshot: SharedDnsSnapshot) -> Result<(), DnsError>
where
    S: DnsStore + Send + Sync + 'static,
{
    loop {
        let (mut state, mut routing_rx) = match store.subscribe_routing_events().await {
            Ok(subscription) => subscription,
            Err(error) => {
                crate::metrics::set_store_sync_healthy("routing", false);
                warn!(?error, "dns routing subscription setup failed; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        replace_dns_snapshot(&state, &snapshot);
        crate::metrics::set_store_sync_healthy("routing", true);

        while let Some(envelope) = routing_rx.recv().await {
            let envelope = match envelope {
                Ok(envelope) => envelope,
                Err(error) => {
                    crate::metrics::set_store_sync_healthy("routing", false);
                    warn!(%error, "dns routing event stream failed; resubscribing");
                    break;
                }
            };
            apply_routing_envelope(&mut state, envelope, &snapshot).await;
        }
        crate::metrics::set_store_sync_healthy("routing", false);
        warn!("dns routing event stream closed; resubscribing");
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

async fn apply_routing_envelope(
    state: &mut ployz_types::model::RoutingState,
    envelope: ployz_store_api::RoutingEventEnvelope,
    snapshot: &SharedDnsSnapshot,
) {
    ployz_store_api::apply_routing_event(state, envelope.event.clone());
    replace_dns_snapshot(state, snapshot);
    if let Err(error) = envelope.ack().await {
        crate::metrics::set_store_sync_healthy("routing", false);
        warn!(?error, "dns routing event ack failed after snapshot swap");
    }
}

fn replace_dns_snapshot(state: &ployz_types::model::RoutingState, snapshot: &SharedDnsSnapshot) {
    let next = project_dns(state);
    let service_count: usize = next.services.values().map(HashMap::len).sum();
    snapshot.replace(next);
    info!(service_count, "dns snapshot refreshed");
}

pub fn spawn_sync_thread_with_store<S>(
    store: S,
    snapshot: SharedDnsSnapshot,
) -> Result<(), DnsError>
where
    S: DnsStore + Send + Sync + 'static,
{
    std::thread::Builder::new()
        .name("ployz-dns-sync".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    warn!(?err, "failed to create dns sync runtime");
                    return;
                }
            };
            runtime.block_on(async move {
                if let Err(err) = run_sync_loop(store, snapshot).await {
                    warn!(?err, "dns sync loop exited");
                }
            });
        })
        .map_err(|err| DnsError::Runtime(err.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::MachineId;
    use prometheus::Encoder;
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn routing_ack_failure_marks_dns_sync_unhealthy_after_snapshot_swap() {
        crate::metrics::set_store_sync_healthy("routing", true);
        let snapshot = SharedDnsSnapshot::new(crate::DnsSnapshot::empty());
        let mut state = ployz_types::model::RoutingState {
            machines: Vec::new(),
            revisions: Vec::new(),
            releases: Vec::new(),
            instances: Vec::new(),
        };
        let (ack_tx, ack_rx) = oneshot::channel();
        drop(ack_rx);

        apply_routing_envelope(
            &mut state,
            ployz_store_api::RoutingEventEnvelope::with_ack(
                "event-1",
                Some("test".into()),
                ployz_types::model::RoutingEvent::RevisionUpsert(
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
        assert!(text.contains("ployz_dns_store_sync_healthy{stream=\"routing\"} 0"));
        assert!(text.contains("ployz_dns_store_sync_failures_total{stream=\"routing\"}"));
    }
}
