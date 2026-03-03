use crate::dataplane::traits::{MembershipStore, PortResult};
use crate::domain::model::{MachineEvent, MachineId, MachineRecord};
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
            }),
        }
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

impl MembershipStore for MemoryStore {
    async fn list_machines(&self) -> PortResult<Vec<MachineRecord>> {
        let inner = self.lock_inner();
        Ok(inner.machines.values().cloned().collect())
    }

    async fn upsert_machine(&self, record: &MachineRecord) -> PortResult<()> {
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

    async fn delete_machine(&self, id: &MachineId) -> PortResult<()> {
        let mut inner = self.lock_inner();
        inner.machines.remove(id);
        Self::broadcast(&mut inner, MachineEvent::Removed { id: id.clone() });
        Ok(())
    }

    async fn subscribe_machines(
        &self,
    ) -> PortResult<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        let mut inner = self.lock_inner();
        let snapshot: Vec<MachineRecord> = inner.machines.values().cloned().collect();
        let (tx, rx) = mpsc::channel(64);
        inner.subscribers.push(tx);
        Ok((snapshot, rx))
    }
}
