use std::collections::HashMap;
use std::time::Duration;

use ployz_store_api::{RoutingStore, SubscriptionPoll};
use tracing::info;

use crate::config::DnsError;
use crate::snapshot::{SharedDnsSnapshot, project_dns};

const REFRESH_DEBOUNCE: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------------------
// Sync logic
// ---------------------------------------------------------------------------

pub async fn run_sync_loop<S>(store: S, snapshot: SharedDnsSnapshot) -> Result<(), DnsError>
where
    S: RoutingStore + Send + Sync + 'static,
{
    let mut refresh_rx = store
        .subscribe_routing_invalidations()
        .await
        .map_err(|err| DnsError::Store(err.to_string()))?;

    while refresh_rx.recv().await.is_some() {
        tokio::time::sleep(REFRESH_DEBOUNCE).await;
        while matches!(refresh_rx.try_recv(), SubscriptionPoll::Item(())) {}
        match store.load_routing_state().await {
            Ok(state) => {
                let next = project_dns(&state);
                let service_count: usize = next.services.values().map(HashMap::len).sum();
                snapshot.replace(next);
                info!(service_count, "dns snapshot refreshed");
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "failed to refresh dns snapshot; keeping previous state"
                );
            }
        }
    }

    Ok(())
}
