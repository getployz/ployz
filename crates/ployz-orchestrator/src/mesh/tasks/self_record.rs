use crate::model::MachineRecord;
use ployz_store_api::MachineStore;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) enum SelfRecordMutation {
    SetEndpoints { endpoints: Vec<String> },
    Replace(MachineRecord),
}

#[derive(Debug)]
pub(crate) struct SelfRecordCommand {
    mutation: SelfRecordMutation,
    done: oneshot::Sender<Option<MachineRecord>>,
}

pub(crate) async fn apply_self_record_mutation(
    commands: &mpsc::Sender<SelfRecordCommand>,
    mutation: SelfRecordMutation,
) -> Option<MachineRecord> {
    let (done_tx, done_rx) = oneshot::channel();
    commands
        .send(SelfRecordCommand {
            mutation,
            done: done_tx,
        })
        .await
        .ok()?;
    done_rx.await.ok()?
}

pub(crate) async fn run_self_record_writer_task(
    authoritative_self: Arc<RwLock<MachineRecord>>,
    store: Arc<dyn MachineStore>,
    mut commands: mpsc::Receiver<SelfRecordCommand>,
    cancel: CancellationToken,
) {
    let mut current = authoritative_self.read().await.clone();

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                info!("self record writer task cancelled");
                break;
            }
            Some(command) = commands.recv() => {
                let SelfRecordCommand { mutation, done } = command;
                let previous = current.clone();
                apply_mutation(&mut current, mutation);
                if current == previous {
                    let _ = done.send(Some(current.clone()));
                    continue;
                }
                preserve_founder_owned_fields(&mut current, store.as_ref()).await;
                match store.upsert_self_machine(&current).await {
                    Ok(()) => {
                        *authoritative_self.write().await = current.clone();
                        let _ = done.send(Some(current.clone()));
                    }
                    Err(error) => {
                        current = previous;
                        warn!(?error, "self record update failed");
                        let _ = done.send(None);
                    }
                }
            }
        }
    }
}

fn apply_mutation(record: &mut MachineRecord, mutation: SelfRecordMutation) {
    match mutation {
        SelfRecordMutation::SetEndpoints { endpoints } => {
            record.endpoints = endpoints;
        }
        SelfRecordMutation::Replace(next) => {
            *record = next;
        }
    }
}

async fn preserve_founder_owned_fields(record: &mut MachineRecord, store: &dyn MachineStore) {
    let durable = match store.list_machines().await {
        Ok(machines) => machines.into_iter().find(|machine| machine.id == record.id),
        Err(error) => {
            warn!(?error, machine_id = %record.id, "self record writer: failed to read durable machine snapshot");
            None
        }
    };
    let Some(durable) = durable else {
        return;
    };
    record.admitted = durable.admitted;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DrainState, MachineId, MachineStatus, OverlayIp, PublicKey};
    use ployz_test_support::MemoryStore;
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

    fn test_record() -> MachineRecord {
        MachineRecord {
            id: MachineId("self".into()),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            status: MachineStatus::Unknown,
            admitted: true,
            drain_state: DrainState::Active,
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn writer_updates_endpoints() {
        let authoritative_self = Arc::new(RwLock::new(test_record()));
        let store = Arc::new(MemoryStore::new());
        let (tx, rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let writer_authoritative_self = authoritative_self.clone();
        let handle = tokio::spawn(async move {
            run_self_record_writer_task(writer_authoritative_self, store, rx, task_cancel).await;
        });

        let endpoints = vec!["10.0.0.1:51820".into(), "10.0.0.2:51820".into()];
        let _ = apply_self_record_mutation(
            &tx,
            SelfRecordMutation::SetEndpoints {
                endpoints: endpoints.clone(),
            },
        )
        .await;
        cancel.cancel();
        handle.await.expect("writer exits");

        let record = authoritative_self.read().await.clone();
        assert_eq!(record.endpoints, endpoints);
        assert_eq!(record.status, MachineStatus::Unknown);
    }

    #[tokio::test]
    async fn writer_replaces_record_atomically() {
        let authoritative_self = Arc::new(RwLock::new(test_record()));
        let store = Arc::new(MemoryStore::new());
        let (tx, rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let writer_authoritative_self = authoritative_self.clone();
        let handle = tokio::spawn(async move {
            run_self_record_writer_task(writer_authoritative_self, store, rx, task_cancel).await;
        });

        let mut replacement = test_record();
        replacement.status = MachineStatus::Up;
        replacement.drain_state = DrainState::Drained;
        replacement.updated_at = 123;
        let _ = apply_self_record_mutation(&tx, SelfRecordMutation::Replace(replacement)).await;

        cancel.cancel();
        handle.await.expect("writer exits");

        let record = authoritative_self.read().await.clone();
        assert_eq!(record.drain_state, DrainState::Drained);
        assert_eq!(record.updated_at, 123);
        assert_eq!(record.status, MachineStatus::Up);
    }

    #[tokio::test]
    async fn writer_preserves_durable_admitted_flag() {
        let authoritative_self = Arc::new(RwLock::new(test_record()));
        let store = Arc::new(MemoryStore::new());
        let mut durable = test_record();
        durable.admitted = true;
        store
            .upsert_self_machine(&durable)
            .await
            .expect("seed durable self record");
        let (tx, rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let writer_authoritative_self = authoritative_self.clone();
        let writer_store = store.clone();
        let handle = tokio::spawn(async move {
            run_self_record_writer_task(writer_authoritative_self, writer_store, rx, task_cancel)
                .await;
        });

        let _ = apply_self_record_mutation(
            &tx,
            SelfRecordMutation::SetEndpoints {
                endpoints: vec!["10.0.0.1:51820".into()],
            },
        )
        .await;

        cancel.cancel();
        handle.await.expect("writer exits");

        let record = authoritative_self.read().await.clone();
        assert!(record.admitted);
        let stored = store
            .list_machines()
            .await
            .expect("list machines")
            .into_iter()
            .find(|machine| machine.id == record.id)
            .expect("stored self record");
        assert!(stored.admitted);
    }
}
