use crate::mesh::MeshDataplane;
use crate::model::{MachineEvent, MachineId, MachineRecord};
use ployz_store_api::MachineEventSubscription;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub(crate) async fn run_ebpf_sync_task(
    snapshot: Vec<MachineRecord>,
    mut events: MachineEventSubscription,
    dataplane: Arc<dyn MeshDataplane>,
    wg_ifindex: u32,
    local_machine_id: MachineId,
    cancel: CancellationToken,
) {
    // Seed from snapshot
    for machine in &snapshot {
        if machine.id == local_machine_id {
            continue;
        }
        if let Some(subnet) = machine.subnet
            && let Err(e) = dataplane.upsert_route(subnet, wg_ifindex).await
        {
            warn!(?e, %subnet, "ebpf_sync: failed to seed route");
        }
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("ebpf sync task cancelled");
                break;
            }
            Some(event) = events.recv() => {
                match &event {
                    MachineEvent::Added(m) | MachineEvent::Updated(m) => {
                        if m.id == local_machine_id {
                            continue;
                        }
                        if let Some(subnet) = m.subnet
                            && let Err(e) = dataplane.upsert_route(subnet, wg_ifindex).await {
                                warn!(?e, %subnet, "ebpf_sync: upsert failed");
                            }
                    }
                    MachineEvent::Removed(m) => {
                        if m.id == local_machine_id {
                            continue;
                        }
                        if let Some(subnet) = m.subnet
                            && let Err(e) = dataplane.remove_route(subnet).await {
                                warn!(?e, %subnet, "ebpf_sync: remove failed");
                            }
                    }
                }
            }
        }
    }
}
