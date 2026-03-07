use crate::drivers::StoreDriver;
use crate::model::MachineId;
use crate::network::endpoints::detect_endpoints;
use crate::store::MachineStore;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub(crate) async fn run_endpoint_refresh_task(
    machine_id: MachineId,
    listen_port: u16,
    store: StoreDriver,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(30 * 60));
    // Skip the first immediate tick — endpoints were just set during start_mesh
    interval.tick().await;

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("endpoint refresh task cancelled");
                break;
            }
            _ = interval.tick() => {
                let new_endpoints = detect_endpoints(listen_port).await;
                if new_endpoints.is_empty() {
                    warn!("endpoint detection returned no results, skipping update");
                    continue;
                }

                let current = match store.list_machines().await {
                    Ok(machines) => {
                        match machines.into_iter().find(|m| m.id == machine_id) {
                            Some(record) => record,
                            None => {
                                warn!("self record not found in store, skipping endpoint refresh");
                                continue;
                            }
                        }
                    }
                    Err(e) => {
                        warn!(?e, "failed to read machines for endpoint refresh");
                        continue;
                    }
                };

                if current.endpoints != new_endpoints {
                    info!(
                        old = ?current.endpoints,
                        new = ?new_endpoints,
                        "endpoints changed, updating"
                    );
                    let mut updated = current;
                    updated.endpoints = new_endpoints;
                    if let Err(e) = store.upsert_machine(&updated).await {
                        warn!(?e, "failed to update endpoints in store");
                    }
                }
            }
        }
    }
}
