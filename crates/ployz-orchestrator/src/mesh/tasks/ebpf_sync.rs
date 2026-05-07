use crate::error::Result as PloyzResult;
use crate::mesh::MeshDataplane;
use crate::model::{MachineEvent, MachineId, MachineMembership};
use std::collections::HashMap;
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
    let mut machine_subnets = snapshot
        .iter()
        .filter_map(|machine| machine.subnet.map(|subnet| (machine.id.clone(), subnet)))
        .collect::<HashMap<_, _>>();

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
                    MachineEvent::Upsert(m) => {
                        if m.id == local_machine_id {
                            continue;
                        }
                        match m.subnet {
                            Some(subnet) => {
                                machine_subnets.insert(m.id.clone(), subnet);
                                if let Err(e) = dataplane.upsert_route(subnet, wg_ifindex).await {
                                    warn!(?e, %subnet, "ebpf_sync: upsert failed");
                                }
                            }
                            None => {
                                machine_subnets.remove(&m.id);
                            }
                        }
                    }
                    MachineEvent::Removed { id } => {
                        if id == &local_machine_id {
                            continue;
                        }
                        if let Some(subnet) = machine_subnets.remove(&id)
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

    #[tokio::test]
    async fn exits_when_machine_subscription_reports_failure() {
        let (event_tx, event_rx) = mpsc::channel(4);
        event_tx
            .send(Err(crate::error::Error::operation(
                "test_subscription",
                "closed",
            )))
            .await
            .expect("failure event should send");

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
        .expect("eBPF sync should exit when machine subscription reports failure");
    }
}
