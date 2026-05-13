use crate::model::{MachineMembership, OverlayIp};
use ployz_store_api::MachineMembershipStore;
use ployz_store_api::StoreDriver;
use tokio::sync::{RwLock, mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use std::sync::Arc;

#[derive(Debug, Clone)]
pub(crate) enum SelfRecordMutation {
    PublishUp { bridge_ip: Option<OverlayIp> },
    Replace(MachineMembership),
}

#[derive(Debug)]
pub(crate) struct SelfRecordCommand {
    mutation: SelfRecordMutation,
    done: oneshot::Sender<Option<MachineMembership>>,
}

pub(crate) async fn apply_self_record_mutation(
    commands: &mpsc::Sender<SelfRecordCommand>,
    mutation: SelfRecordMutation,
) -> Option<MachineMembership> {
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
    authoritative_self: Arc<RwLock<MachineMembership>>,
    store: StoreDriver,
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
                let mut next = current.clone();
                apply_mutation(&mut next, mutation.clone());
                match store.upsert_self_machine(&next).await {
                    Ok(()) => {
                        current = next.clone();
                        *authoritative_self.write().await = next.clone();
                        let _ = done.send(Some(next));
                    }
                    Err(error) => {
                        warn!(?error, ?mutation, "self record update failed");
                        let _ = done.send(None);
                    }
                }
            }
        }
    }
}

fn apply_mutation(record: &mut MachineMembership, mutation: SelfRecordMutation) {
    match mutation {
        SelfRecordMutation::PublishUp { bridge_ip } => {
            let now = crate::time::now_unix_secs();
            if record.created_at == 0 {
                record.created_at = now;
            }
            record.updated_at = now;
            if let Some(bridge_ip) = bridge_ip {
                record.bridge_ip = Some(bridge_ip);
            }
        }
        SelfRecordMutation::Replace(next) => {
            *record = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MachineId, MachineLifecycle, MachineTopology, PublicKey};
    use ployz_store_memory::{MemoryService, MemoryStore, StoreDriverMemoryExt as _};
    use std::collections::BTreeMap;
    use std::net::Ipv6Addr;

    fn test_record() -> MachineMembership {
        MachineMembership {
            id: MachineId::new("self"),
            public_key: PublicKey([1; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            topology: MachineTopology::local(),
            region_role: crate::model::RegionRole::HomeData,
            subnet: None,
            bridge_ip: None,
            endpoints: vec!["127.0.0.1:51820".into()],
            lifecycle: MachineLifecycle::Standby,
            storage_role: crate::model::StorageParticipation::default_authority().into(),
            created_at: 0,
            updated_at: 0,
            labels: BTreeMap::new(),
        }
    }

    #[tokio::test]
    async fn writer_preserves_endpoints_when_publish_updates() {
        let authoritative_self = Arc::new(RwLock::new(test_record()));
        let store = Arc::new(MemoryStore::new());
        let service = Arc::new(MemoryService::new());
        let store_driver = StoreDriver::memory_with(store.clone(), service);
        let (tx, rx) = mpsc::channel(8);
        let cancel = CancellationToken::new();
        let task_cancel = cancel.clone();
        let writer_authoritative_self = authoritative_self.clone();
        let handle = tokio::spawn(async move {
            run_self_record_writer_task(writer_authoritative_self, store_driver, rx, task_cancel)
                .await;
        });

        let _ = apply_self_record_mutation(&tx, SelfRecordMutation::PublishUp { bridge_ip: None })
            .await;

        cancel.cancel();
        handle.await.expect("writer exits");

        let record = authoritative_self.read().await.clone();
        assert_eq!(record.endpoints, vec!["127.0.0.1:51820".to_string()]);
        assert_eq!(record.lifecycle, MachineLifecycle::Standby);
        assert!(record.updated_at > 0);
    }
}
