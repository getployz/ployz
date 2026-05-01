use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use tracing::{info, warn};

use crate::config::DnsError;
use crate::snapshot::{SharedDnsSnapshot, project_dns};
use ployz_store_api::RoutingSubscription;
use ployz_types::model::MachineId;

// ---------------------------------------------------------------------------
// DnsStore trait — consumer contract
// ---------------------------------------------------------------------------

pub trait DnsStore: Send + Sync {
    fn subscribe_routing_batches<'a>(
        &'a self,
        subscription: RoutingSubscription,
    ) -> impl Future<Output = Result<ployz_store_api::RoutingBatchSubscription, DnsError>> + Send + 'a;
}

// ---------------------------------------------------------------------------
// Sync logic
// ---------------------------------------------------------------------------

pub async fn run_sync_loop<S>(
    store: S,
    snapshot: SharedDnsSnapshot,
    machine_id: MachineId,
) -> Result<(), DnsError>
where
    S: DnsStore + Send + Sync + 'static,
{
    loop {
        let consumer_id = format!("dns.{}", machine_id.0);
        let (mut state, mut routing_rx) = match store
            .subscribe_routing_batches(RoutingSubscription::durable(consumer_id))
            .await
        {
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

        while let Some(batch) = routing_rx.recv().await {
            let batch = match batch {
                Ok(batch) => batch,
                Err(error) => {
                    crate::metrics::set_store_sync_healthy("routing", false);
                    warn!(%error, "dns routing event stream failed; resubscribing");
                    break;
                }
            };
            ployz_store_api::apply_routing_events(&mut state, batch.events.clone());
            replace_dns_snapshot(&state, &snapshot);
            if let Err(error) = batch.ack().await {
                warn!(?error, "dns routing batch ack failed after snapshot swap");
                crate::metrics::set_store_sync_healthy("routing", false);
                break;
            }
        }
        crate::metrics::set_store_sync_healthy("routing", false);
        warn!("dns routing event stream closed; resubscribing");
        tokio::time::sleep(Duration::from_secs(1)).await;
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
    machine_id: MachineId,
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
                if let Err(err) = run_sync_loop(store, snapshot, machine_id).await {
                    warn!(?err, "dns sync loop exited");
                }
            });
        })
        .map_err(|err| DnsError::Runtime(err.to_string()))?;
    Ok(())
}
