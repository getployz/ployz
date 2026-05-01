use crate::error::Result as PloyzResult;
use crate::mesh::MeshDataplane;
use crate::model::{MachineEvent, MachineId, MachineMembership};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

pub(crate) async fn run_ebpf_sync_task(
    snapshot: Vec<MachineMembership>,
    mut events: mpsc::Receiver<PloyzResult<MachineEvent>>,
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
            event = events.recv() => {
                let Some(event) = event else {
                    warn!("eBPF sync machine subscription closed");
                    break;
                };
                let event = match event {
                    Ok(event) => event,
                    Err(error) => {
                        warn!(%error, "eBPF sync machine subscription failed");
                        break;
                    }
                };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use async_trait::async_trait;
    use ipnet::Ipv4Net;

    struct NoopDataplane;

    #[async_trait]
    impl MeshDataplane for NoopDataplane {
        async fn set_observe(&self, _enabled: bool) -> Result<()> {
            Ok(())
        }

        async fn upsert_route(&self, _subnet: Ipv4Net, _ifindex: u32) -> Result<()> {
            Ok(())
        }

        async fn remove_route(&self, _subnet: Ipv4Net) -> Result<()> {
            Ok(())
        }

        async fn detach(&self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn exits_when_machine_subscription_closes() {
        let (event_tx, event_rx) = mpsc::channel(4);
        drop(event_tx);

        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            run_ebpf_sync_task(
                Vec::new(),
                event_rx,
                Arc::new(NoopDataplane),
                1,
                MachineId("self".into()),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("eBPF sync should exit when machine subscription closes");
    }
}
