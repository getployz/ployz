use crate::drivers::StoreDriver;
use crate::model::{MachineId, MachineStatus};
use crate::store::MachineStore;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub(crate) async fn run_heartbeat_task(
    machine_id: MachineId,
    store: StoreDriver,
    cancel: CancellationToken,
) {
    let mut interval = tokio::time::interval(Duration::from_secs(60));
    // First tick fires immediately — report "up" as soon as mesh is running.

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("heartbeat task cancelled");
                break;
            }
            _ = interval.tick() => {
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if let Err(e) = store.update_heartbeat(&machine_id, MachineStatus::Up, now).await {
                    warn!(?e, "heartbeat update failed");
                }
            }
        }
    }
}
