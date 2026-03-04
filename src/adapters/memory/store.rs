use crate::error::{Error, Result};
use crate::store::{InviteStore, MachineStore, SyncProbe, SyncStatus};
use crate::store::model::{InviteRecord, MachineEvent, MachineId, MachineRecord};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use tokio::sync::mpsc;
use tracing::warn;

pub struct MemoryStore {
    inner: Mutex<StoreInner>,
}

struct StoreInner {
    machines: HashMap<MachineId, MachineRecord>,
    subscribers: Vec<mpsc::Sender<MachineEvent>>,
    invites: HashMap<String, InviteRecord>,
    sync_status: SyncStatus,
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryStore {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(StoreInner {
                machines: HashMap::new(),
                subscribers: Vec::new(),
                invites: HashMap::new(),
                sync_status: SyncStatus::Synced,
            }),
        }
    }

    pub fn set_sync_status(&self, status: SyncStatus) {
        self.lock_inner().sync_status = status;
    }

    fn lock_inner(&self) -> MutexGuard<'_, StoreInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn broadcast(inner: &mut StoreInner, event: MachineEvent) {
        inner
            .subscribers
            .retain(|tx| match tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("subscriber channel full, event dropped");
                    true
                }
            });
    }
}

impl SyncProbe for MemoryStore {
    async fn sync_status(&self) -> Result<SyncStatus> {
        Ok(self.lock_inner().sync_status)
    }
}

impl MachineStore for MemoryStore {
    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        let inner = self.lock_inner();
        Ok(inner.machines.values().cloned().collect())
    }

    async fn upsert_machine(&self, record: &MachineRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        let is_update = inner.machines.contains_key(&record.id);
        inner.machines.insert(record.id.clone(), record.clone());
        let event = if is_update {
            MachineEvent::Updated(record.clone())
        } else {
            MachineEvent::Added(record.clone())
        };
        Self::broadcast(&mut inner, event);
        Ok(())
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let mut inner = self.lock_inner();
        inner.machines.remove(id);
        Self::broadcast(&mut inner, MachineEvent::Removed { id: id.clone() });
        Ok(())
    }

    async fn subscribe_machines(
        &self,
    ) -> Result<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        let mut inner = self.lock_inner();
        let snapshot: Vec<MachineRecord> = inner.machines.values().cloned().collect();
        let (tx, rx) = mpsc::channel(64);
        inner.subscribers.push(tx);
        Ok((snapshot, rx))
    }
}

impl InviteStore for MemoryStore {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        let mut inner = self.lock_inner();
        if inner.invites.contains_key(&invite.id) {
            return Err(Error::operation(
                "invite_exists",
                format!("invite '{}' already exists", invite.id),
            ));
        }
        inner.invites.insert(invite.id.clone(), invite.clone());
        Ok(())
    }

    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()> {
        let mut inner = self.lock_inner();
        let invite = inner.invites.get(invite_id).ok_or_else(|| {
            Error::operation(
                "invite_not_found",
                format!("invite '{invite_id}' not found"),
            )
        })?;

        if now_unix_secs > invite.expires_at {
            return Err(Error::operation(
                "invite_expired",
                format!("invite '{invite_id}' is expired"),
            ));
        }

        inner.invites.remove(invite_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invite_is_single_use() {
        let store = MemoryStore::new();
        let invite = InviteRecord {
            id: "inv-1".into(),
            expires_at: 10_000,
        };

        store.create_invite(&invite).await.expect("create invite");
        store
            .consume_invite("inv-1", 100)
            .await
            .expect("consume invite once");

        let second = store.consume_invite("inv-1", 101).await;
        assert!(matches!(
            second,
            Err(Error::Operation {
                operation: "invite_not_found",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn invite_expiry_is_enforced() {
        let store = MemoryStore::new();
        let invite = InviteRecord {
            id: "inv-2".into(),
            expires_at: 50,
        };

        store.create_invite(&invite).await.expect("create invite");

        let expired = store.consume_invite("inv-2", 51).await;
        assert!(matches!(
            expired,
            Err(Error::Operation {
                operation: "invite_expired",
                ..
            })
        ));
    }
}
