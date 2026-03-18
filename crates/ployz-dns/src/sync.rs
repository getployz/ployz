use std::collections::HashMap;
use std::future::Future;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::config::DnsError;
use crate::snapshot::{SharedDnsSnapshot, project_dns};

const REFRESH_DEBOUNCE: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------------------
// DnsStore trait — consumer contract
// ---------------------------------------------------------------------------

pub trait DnsStore: Send + Sync {
    fn load_routing_state(
        &self,
    ) -> impl Future<Output = Result<ployz_types::model::RoutingState, DnsError>> + Send + '_;
    fn subscribe_routing_invalidations(
        &self,
    ) -> impl Future<Output = Result<mpsc::Receiver<()>, DnsError>> + Send + '_;
}

impl DnsStore for ployz_store_corrosion::CorrosionRoutingStore {
    async fn load_routing_state(&self) -> Result<ployz_types::model::RoutingState, DnsError> {
        ployz_store_corrosion::CorrosionRoutingStore::load_routing_state(self)
            .await
            .map_err(|err| DnsError::Store(err.to_string()))
    }

    async fn subscribe_routing_invalidations(&self) -> Result<mpsc::Receiver<()>, DnsError> {
        ployz_store_corrosion::CorrosionRoutingStore::subscribe_routing_invalidations(self)
            .await
            .map_err(|err| DnsError::Store(err.to_string()))
    }
}

// ---------------------------------------------------------------------------
// Sync logic
// ---------------------------------------------------------------------------

pub async fn run_sync_loop<S>(store: S, snapshot: SharedDnsSnapshot) -> Result<(), DnsError>
where
    S: DnsStore + Send + Sync + 'static,
{
    let mut refresh_rx = store.subscribe_routing_invalidations().await?;

    while refresh_rx.recv().await.is_some() {
        tokio::time::sleep(REFRESH_DEBOUNCE).await;
        while refresh_rx.try_recv().is_ok() {}
        match store.load_routing_state().await {
            Ok(state) => {
                let next = project_dns(&state);
                let service_count: usize =
                    next.services.values().map(HashMap::len).sum();
                snapshot.replace(next);
                info!(service_count, "dns snapshot refreshed");
            }
            Err(err) => {
                warn!(
                    ?err,
                    "failed to refresh dns snapshot; keeping previous state"
                );
            }
        }
    }

    Ok(())
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
