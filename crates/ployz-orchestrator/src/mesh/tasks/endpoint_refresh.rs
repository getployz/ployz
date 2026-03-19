use crate::mesh::tasks::{SelfRecordMutation, apply_self_record_mutation};
use crate::model::MachineId;
use ployz_runtime_api::EndpointDiscovery;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub(crate) async fn run_endpoint_refresh_task(
    machine_id: MachineId,
    listen_port: u16,
    endpoint_discovery: Arc<dyn EndpointDiscovery>,
    authoritative_self: Arc<RwLock<crate::model::MachineRecord>>,
    self_record_tx: tokio::sync::mpsc::Sender<crate::mesh::tasks::self_record::SelfRecordCommand>,
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
                let new_endpoints = match endpoint_discovery.detect_endpoints(listen_port).await {
                    Ok(endpoints) => endpoints,
                    Err(error) => {
                        warn!(?error, "endpoint detection failed, skipping update");
                        continue;
                    }
                };
                if new_endpoints.is_empty() {
                    warn!("endpoint detection returned no results, skipping update");
                    continue;
                }

                let current = authoritative_self.read().await.clone();
                if current.id != machine_id {
                    warn!("authoritative self record mismatch, skipping endpoint refresh");
                    continue;
                }

                if current.endpoints != new_endpoints {
                    info!(
                        old = ?current.endpoints,
                        new = ?new_endpoints,
                        "endpoints changed, updating"
                    );
                    let _ = apply_self_record_mutation(
                        &self_record_tx,
                        SelfRecordMutation::SetEndpoints {
                            endpoints: new_endpoints,
                        },
                    ).await;
                }
            }
        }
    }
}
