use crate::dataplane::traits::{InviteStore, MachineStore, PortResult};
use crate::domain::model::{InviteRecord, MachineEvent, MachineId, MachineRecord, NetworkId};
use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use tokio::sync::mpsc;
use tracing::warn;

pub struct MemoryStore {
    inner: Mutex<StoreInner>,
}

struct StoreInner {
    machines: HashMap<NetworkId, HashMap<MachineId, MachineRecord>>,
    subscribers: HashMap<NetworkId, Vec<mpsc::Sender<MachineEvent>>>,
    invites: HashMap<NetworkId, HashMap<String, InviteRecord>>,
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
                subscribers: HashMap::new(),
                invites: HashMap::new(),
            }),
        }
    }

    fn lock_inner(&self) -> MutexGuard<'_, StoreInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn broadcast(inner: &mut StoreInner, network_id: &NetworkId, event: MachineEvent) {
        if let Some(subscribers) = inner.subscribers.get_mut(network_id) {
            subscribers.retain(|tx| match tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Closed(_)) => false,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    warn!("subscriber channel full, event dropped");
                    true
                }
            });
        }
    }
}

impl MachineStore for MemoryStore {
    async fn list_machines(&self, network_id: &NetworkId) -> PortResult<Vec<MachineRecord>> {
        let inner = self.lock_inner();
        Ok(inner
            .machines
            .get(network_id)
            .map(|machines| machines.values().cloned().collect())
            .unwrap_or_default())
    }

    async fn upsert_machine(
        &self,
        network_id: &NetworkId,
        record: &MachineRecord,
    ) -> PortResult<()> {
        let mut inner = self.lock_inner();
        let machines = inner.machines.entry(network_id.clone()).or_default();
        let is_update = machines.contains_key(&record.id);
        machines.insert(record.id.clone(), record.clone());
        let event = if is_update {
            MachineEvent::Updated(record.clone())
        } else {
            MachineEvent::Added(record.clone())
        };
        Self::broadcast(&mut inner, network_id, event);
        Ok(())
    }

    async fn delete_machine(&self, network_id: &NetworkId, id: &MachineId) -> PortResult<()> {
        let mut inner = self.lock_inner();
        if let Some(machines) = inner.machines.get_mut(network_id) {
            machines.remove(id);
        }
        Self::broadcast(
            &mut inner,
            network_id,
            MachineEvent::Removed { id: id.clone() },
        );
        Ok(())
    }

    async fn subscribe_machines(
        &self,
        network_id: &NetworkId,
    ) -> PortResult<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        let mut inner = self.lock_inner();
        let snapshot: Vec<MachineRecord> = inner
            .machines
            .get(network_id)
            .map(|machines| machines.values().cloned().collect())
            .unwrap_or_default();
        let (tx, rx) = mpsc::channel(64);
        inner
            .subscribers
            .entry(network_id.clone())
            .or_default()
            .push(tx);
        Ok((snapshot, rx))
    }
}

impl InviteStore for MemoryStore {
    async fn create_invite(&self, network_id: &NetworkId, invite: &InviteRecord) -> PortResult<()> {
        let mut inner = self.lock_inner();
        let invites = inner.invites.entry(network_id.clone()).or_default();
        if invites.contains_key(&invite.id) {
            return Err(crate::dataplane::traits::PortError::operation(
                "invite_exists",
                format!("invite '{}' already exists", invite.id),
            ));
        }
        invites.insert(invite.id.clone(), invite.clone());
        Ok(())
    }

    async fn consume_invite(
        &self,
        network_id: &NetworkId,
        invite_id: &str,
        now_unix_secs: u64,
    ) -> PortResult<InviteRecord> {
        let mut inner = self.lock_inner();
        let invites = inner.invites.get_mut(network_id).ok_or_else(|| {
            crate::dataplane::traits::PortError::operation(
                "invite_not_found",
                format!("invite '{invite_id}' not found"),
            )
        })?;

        let invite = invites.get_mut(invite_id).ok_or_else(|| {
            crate::dataplane::traits::PortError::operation(
                "invite_not_found",
                format!("invite '{invite_id}' not found"),
            )
        })?;

        if invite.revoked {
            return Err(crate::dataplane::traits::PortError::operation(
                "invite_revoked",
                format!("invite '{invite_id}' is revoked"),
            ));
        }

        if now_unix_secs > invite.expires_at {
            return Err(crate::dataplane::traits::PortError::operation(
                "invite_expired",
                format!("invite '{invite_id}' is expired"),
            ));
        }

        if invite.used >= invite.max_uses {
            return Err(crate::dataplane::traits::PortError::operation(
                "invite_consumed",
                format!("invite '{invite_id}' has no remaining uses"),
            ));
        }

        invite.used += 1;
        Ok(invite.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{MachineId, NetworkName};

    #[tokio::test]
    async fn invite_is_single_use() {
        let store = MemoryStore::new();
        let network_id = NetworkId("net-a".into());
        let invite = InviteRecord {
            id: "inv-1".into(),
            network_id: network_id.clone(),
            network_name: NetworkName("alpha".into()),
            issued_by: MachineId("m-founder".into()),
            expires_at: 10_000,
            nonce: "n".into(),
            max_uses: 1,
            used: 0,
            revoked: false,
        };

        store
            .create_invite(&network_id, &invite)
            .await
            .expect("create invite");

        let consumed = store
            .consume_invite(&network_id, "inv-1", 100)
            .await
            .expect("consume invite once");
        assert_eq!(consumed.used, 1);

        let second = store.consume_invite(&network_id, "inv-1", 101).await;
        assert!(matches!(
            second,
            Err(crate::dataplane::traits::PortError::Operation {
                operation: "invite_consumed",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn invite_expiry_is_enforced() {
        let store = MemoryStore::new();
        let network_id = NetworkId("net-a".into());
        let invite = InviteRecord {
            id: "inv-2".into(),
            network_id: network_id.clone(),
            network_name: NetworkName("alpha".into()),
            issued_by: MachineId("m-founder".into()),
            expires_at: 50,
            nonce: "n".into(),
            max_uses: 1,
            used: 0,
            revoked: false,
        };

        store
            .create_invite(&network_id, &invite)
            .await
            .expect("create invite");

        let expired = store.consume_invite(&network_id, "inv-2", 51).await;
        assert!(matches!(
            expired,
            Err(crate::dataplane::traits::PortError::Operation {
                operation: "invite_expired",
                ..
            })
        ));
    }
}
