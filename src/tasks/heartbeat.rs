use crate::drivers::{StoreDriver, WireguardDriver};
use crate::mesh::MeshNetwork;
use crate::model::{MachineId, MachineStatus};
use crate::store::MachineStore;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub(crate) async fn run_heartbeat_task(
    machine_id: MachineId,
    store: StoreDriver,
    network: WireguardDriver,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(5));
    // First tick fires immediately — report "up" as soon as mesh is running.

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("heartbeat task cancelled");
                break;
            }
            _ = interval.tick() => {
                heartbeat_once(&machine_id, &store, &network).await;
            }
        }
    }
}

async fn heartbeat_once(
    machine_id: &MachineId,
    store: &StoreDriver,
    network: &WireguardDriver,
) {
    let mut record = match store.list_machines().await {
        Ok(machines) => match machines.into_iter().find(|m| m.id == *machine_id) {
            Some(r) => r,
            None => {
                warn!("self record not found in store, skipping heartbeat");
                return;
            }
        },
        Err(e) => {
            warn!(?e, "failed to read machines for heartbeat");
            return;
        }
    };

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    record.status = MachineStatus::Up;
    record.last_heartbeat = now;
    record.updated_at = now;

    if let Some(bridge_ip) = network.bridge_ip().await {
        record.bridge_ip = Some(bridge_ip);
    }

    if let Err(e) = store.upsert_machine(&record).await {
        warn!(?e, "heartbeat upsert failed");
    }
}