use std::time::Duration;

use crate::routes::{GatewaySnapshot, project};
use ployz_store_api::{RoutingStore, SubscriptionPoll};
use tracing::info;

use crate::config::GatewayError;
use crate::snapshot::SharedSnapshot;

const REFRESH_DEBOUNCE: Duration = Duration::from_millis(100);

// ---------------------------------------------------------------------------
// Sync logic
// ---------------------------------------------------------------------------

pub async fn load_projected_snapshot_from_store<S>(
    store: &S,
) -> Result<GatewaySnapshot, GatewayError>
where
    S: RoutingStore + Send + Sync,
{
    let state = store
        .load_routing_state()
        .await
        .map_err(|err| GatewayError::Store(err.to_string()))?;
    project(state).map_err(|err| GatewayError::Projection(err.to_string()))
}

pub async fn run_sync_loop<S>(store: S, snapshot: SharedSnapshot) -> Result<(), GatewayError>
where
    S: RoutingStore + Send + Sync + 'static,
{
    let mut refresh_rx = store
        .subscribe_routing_invalidations()
        .await
        .map_err(|err| GatewayError::Store(err.to_string()))?;

    while refresh_rx.recv().await.is_some() {
        tokio::time::sleep(REFRESH_DEBOUNCE).await;
        while matches!(refresh_rx.try_recv(), SubscriptionPoll::Item(())) {}
        match load_projected_snapshot_from_store(&store).await {
            Ok(next_snapshot) => {
                let http_routes = next_snapshot.http_routes.len();
                let tcp_routes = next_snapshot.tcp_routes.len();
                snapshot.replace(next_snapshot);
                info!(http_routes, tcp_routes, "gateway snapshot refreshed");
            }
            Err(err) => {
                tracing::warn!(
                    ?err,
                    "failed to refresh gateway snapshot; keeping previous state"
                )
            }
        }
    }

    Ok(())
}
