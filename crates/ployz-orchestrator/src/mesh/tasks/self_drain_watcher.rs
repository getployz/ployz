use super::SelfRecordMutation;
use super::self_record::SelfRecordCommand;
use crate::model::{MachineEvent, MachineId, MachineRecord};
use ployz_store_api::MachineEventSubscription;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::info;

pub(crate) async fn run_self_drain_watcher_task(
    machine_id: MachineId,
    authoritative_self: Arc<RwLock<MachineRecord>>,
    self_record_tx: mpsc::Sender<SelfRecordCommand>,
    initial: Vec<MachineRecord>,
    mut events: MachineEventSubscription,
    cancel: CancellationToken,
) {
    if let Some(initial_drain) = initial
        .into_iter()
        .find(|record| record.id == machine_id)
        .map(|record| record.drain)
    {
        reconcile_if_needed(&authoritative_self, &self_record_tx, initial_drain).await;
    }

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("self drain watcher task cancelled");
                break;
            }
            event = events.recv() => {
                let Some(event) = event else { break };
                let (record, tombstone) = match event {
                    MachineEvent::Added(record) | MachineEvent::Updated(record) => (record, false),
                    MachineEvent::Removed(record) => (record, true),
                };
                if record.id != machine_id || tombstone {
                    continue;
                }
                reconcile_if_needed(&authoritative_self, &self_record_tx, record.drain).await;
            }
        }
    }
}

async fn reconcile_if_needed(
    authoritative_self: &Arc<RwLock<MachineRecord>>,
    self_record_tx: &mpsc::Sender<SelfRecordCommand>,
    store_drain: bool,
) {
    let current_drain = authoritative_self.read().await.drain;
    if current_drain == store_drain {
        return;
    }
    let _ = super::apply_self_record_mutation(
        self_record_tx,
        SelfRecordMutation::SetDrain { drain: store_drain },
    )
    .await;
}
