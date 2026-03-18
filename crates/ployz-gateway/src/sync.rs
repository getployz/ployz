use std::future::Future;
use std::time::Duration;

use crate::routes::{GatewaySnapshot, project};
use ployz_store_api::RoutingInvalidationSubscription;
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
    ) -> impl Future<Output = Result<RoutingInvalidationSubscription, GatewayError>> + Send + '_;
}

// ---------------------------------------------------------------------------
// Sync logic
// ---------------------------------------------------------------------------

pub async fn load_projected_snapshot_from_store<S>(
    store: &S,
) -> Result<GatewaySnapshot, GatewayError>
where
    S: RoutingStore + Send + Sync,
{
    let state = store.load_routing_state().await?;
    project(state).map_err(|err| GatewayError::Projection(err.to_string()))
}

pub async fn run_sync_loop<S>(store: S, snapshot: SharedSnapshot) -> Result<(), GatewayError>
where
    S: RoutingStore + Send + Sync + 'static,
{
    let mut refresh_rx = store.subscribe_routing_invalidations().await?;

    while refresh_rx.recv().await.is_some() {
        tokio::time::sleep(REFRESH_DEBOUNCE).await;
        while refresh_rx.try_recv().is_ok() {}
        match load_projected_snapshot_from_store(&store).await {
            Ok(next_snapshot) => {
                let http_routes = next_snapshot.http_routes.len();
                let tcp_routes = next_snapshot.tcp_routes.len();
                snapshot.replace(next_snapshot);
                info!(http_routes, tcp_routes, "gateway snapshot refreshed");
            }
            Err(err) => {
                warn!(
                    ?err,
                    "failed to refresh gateway snapshot; keeping previous state"
                )
            }
        }
    }

    Ok(())
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
