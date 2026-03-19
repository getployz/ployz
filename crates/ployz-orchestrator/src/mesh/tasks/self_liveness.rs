use crate::mesh::tasks::{SelfRecordMutation, apply_self_record_mutation};
use ployz_runtime_api::{MeshNetwork, WireguardDriver};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

const SELF_LIVENESS_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug)]
pub enum SelfLivenessCommand {
    TickNow { done: oneshot::Sender<()> },
}

pub(crate) async fn run_self_liveness_task(
    network: WireguardDriver,
    started: Arc<AtomicBool>,
    self_record_tx: mpsc::Sender<crate::mesh::tasks::self_record::SelfRecordCommand>,
    mut commands: mpsc::Receiver<SelfLivenessCommand>,
    cancel: CancellationToken,
) {
    started.store(true, Ordering::SeqCst);
    let mut interval = tokio::time::interval(SELF_LIVENESS_INTERVAL);

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("self liveness task cancelled");
                break;
            }
            _ = interval.tick() => {
                publish_liveness(&network, &self_record_tx).await;
            }
            Some(command) = commands.recv() => {
                match command {
                    SelfLivenessCommand::TickNow { done } => {
                        publish_liveness(&network, &self_record_tx).await;
                        let _ = done.send(());
                    }
                }
            }
        }
    }
}

async fn publish_liveness(
    network: &WireguardDriver,
    self_record_tx: &mpsc::Sender<crate::mesh::tasks::self_record::SelfRecordCommand>,
) {
    let now = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_secs(),
        Err(error) => {
            warn!(
                ?error,
                "system clock before unix epoch, skipping liveness publish"
            );
            return;
        }
    };

    let bridge_ip = network.bridge_ip().await;
    let _ = apply_self_record_mutation(
        self_record_tx,
        SelfRecordMutation::RefreshLiveness { now, bridge_ip },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::tasks::run_self_record_writer_task;
    use crate::model::{
        MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
    };
    use ployz_runtime_api::MemoryWireGuard;
    use ployz_store_api::MachineStore;
    use ployz_store_api::memory::MemoryStore;
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;
    use tokio::sync::RwLock;

    fn test_record() -> MachineRecord {
        MachineRecord {
            id: MachineId("self".into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            status: MachineStatus::Unknown,
            participation: Participation::Disabled,
            last_heartbeat: 0,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn self_liveness_tick_now_runs_one_sample_and_acknowledges() {
        let authoritative_self = Arc::new(RwLock::new(test_record()));
        let store = Arc::new(MemoryStore::new());
        let (self_record_tx, self_record_rx) = mpsc::channel(8);
        let writer_cancel = CancellationToken::new();
        let writer_task_cancel = writer_cancel.clone();
        let writer_authoritative_self = authoritative_self.clone();
        let writer_store = store.clone();
        let writer_handle = tokio::spawn(async move {
            run_self_record_writer_task(
                writer_authoritative_self,
                writer_store,
                self_record_rx,
                writer_task_cancel,
            )
            .await;
        });

        let network = Arc::new(MemoryWireGuard::new());
        let network_driver = WireguardDriver::memory_with(network);
        let started = Arc::new(AtomicBool::new(false));
        let started_flag = started.clone();
        let (command_tx, command_rx) = mpsc::channel(4);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let handle = tokio::spawn(async move {
            run_self_liveness_task(
                network_driver,
                started_flag,
                self_record_tx,
                command_rx,
                task_cancel,
            )
            .await;
        });

        let (done_tx, done_rx) = oneshot::channel();
        command_tx
            .send(SelfLivenessCommand::TickNow { done: done_tx })
            .await
            .expect("send tick");
        done_rx.await.expect("tick ack");

        cancel.cancel();
        writer_cancel.cancel();
        handle.await.expect("liveness task exits");
        writer_handle.await.expect("writer exits");

        let machines = store.list_machines().await.expect("list machines");
        let [record] = machines.as_slice() else {
            panic!("expected self record");
        };
        assert!(started.load(Ordering::SeqCst));
        assert_eq!(record.status, MachineStatus::Up);
        assert!(record.last_heartbeat > 0);
        assert_eq!(record.created_at, record.updated_at);
        assert_eq!(record.updated_at, record.last_heartbeat);
    }

    #[tokio::test]
    async fn self_liveness_preserves_existing_created_at() {
        let authoritative_self = Arc::new(RwLock::new(MachineRecord {
            created_at: 42,
            ..test_record()
        }));
        let store = Arc::new(MemoryStore::new());
        let (self_record_tx, self_record_rx) = mpsc::channel(8);
        let writer_cancel = CancellationToken::new();
        let writer_task_cancel = writer_cancel.clone();
        let writer_authoritative_self = authoritative_self.clone();
        let writer_store = store.clone();
        let writer_handle = tokio::spawn(async move {
            run_self_record_writer_task(
                writer_authoritative_self,
                writer_store,
                self_record_rx,
                writer_task_cancel,
            )
            .await;
        });

        publish_liveness(
            &WireguardDriver::memory_with(Arc::new(MemoryWireGuard::new())),
            &self_record_tx,
        )
        .await;

        writer_cancel.cancel();
        writer_handle.await.expect("writer exits");

        let machines = store.list_machines().await.expect("list machines");
        let [record] = machines.as_slice() else {
            panic!("expected self record");
        };
        assert_eq!(record.created_at, 42);
        assert_eq!(record.status, MachineStatus::Up);
        assert!(record.updated_at > 0);
        assert!(record.last_heartbeat > 0);
    }
}
