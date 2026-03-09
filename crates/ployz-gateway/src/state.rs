use std::path::PathBuf;
use std::time::Duration;

use ployz::{CorrosionStore, RoutingStore};
use ployz_routing::{GatewaySnapshot, project};
use thiserror::Error;
use tokio::runtime::Builder;
use tracing::{info, warn};

use crate::SharedSnapshot;

const REFRESH_DEBOUNCE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub data_dir: PathBuf,
    pub network: String,
}

#[derive(Debug, Error)]
pub enum GatewayError {
    #[error("failed to reach routing store: {0}")]
    Store(String),
    #[error("projection failed: {0}")]
    Projection(String),
    #[error("gateway sync runtime failed: {0}")]
    Runtime(String),
}

pub fn load_initial_snapshot(config: &GatewayConfig) -> Result<GatewaySnapshot, GatewayError> {
    let runtime = Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;
    runtime.block_on(async {
        let store = connect_store(config).await?;
        load_projected_snapshot_from_store(&store).await
    })
}

pub fn spawn_sync_thread(
    config: GatewayConfig,
    snapshot: SharedSnapshot,
) -> Result<(), GatewayError> {
    std::thread::Builder::new()
        .name("ployz-gateway-sync".into())
        .spawn(move || {
            let runtime = match Builder::new_multi_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(err) => {
                    warn!(?err, "failed to create gateway sync runtime");
                    return;
                }
            };
            runtime.block_on(async move {
                let store = match connect_store(&config).await {
                    Ok(store) => store,
                    Err(err) => {
                        warn!(?err, "failed to connect gateway routing store");
                        return;
                    }
                };
                if let Err(err) = run_sync_loop(store, snapshot).await {
                    warn!(?err, "gateway sync loop exited");
                }
            });
        })
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;
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
            let runtime = match Builder::new_multi_thread().enable_all().build() {
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

pub async fn connect_store(config: &GatewayConfig) -> Result<CorrosionStore, GatewayError> {
    CorrosionStore::connect_for_network(&config.data_dir, &config.network)
        .await
        .map_err(|err| GatewayError::Store(err.to_string()))
}

pub async fn load_projected_snapshot_from_store<S>(store: &S) -> Result<GatewaySnapshot, GatewayError>
where
    S: RoutingStore + Send + Sync,
{
    let state = store
        .load_routing_state()
        .await
        .map_err(|err| GatewayError::Store(err.to_string()))?;
    project(state).map_err(|err| GatewayError::Projection(err.to_string()))
}

async fn run_sync_loop<S>(store: S, snapshot: SharedSnapshot) -> Result<(), GatewayError>
where
    S: RoutingStore + Send + Sync + 'static,
{
    let mut refresh_rx = store
        .subscribe_routing_invalidations()
        .await
        .map_err(|err| GatewayError::Store(err.to_string()))?;

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
            Err(err) => warn!(?err, "failed to refresh gateway snapshot; keeping previous state"),
        }
    }

    Ok(())
}
