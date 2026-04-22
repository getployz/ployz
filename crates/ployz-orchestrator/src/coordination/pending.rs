use super::types::{Reservation, ResourceKey, Vote, VoteConflict};
use ipnet::Ipv4Net;
use std::collections::{HashMap, HashSet};
use tokio::sync::RwLock;

#[derive(Default)]
pub struct PendingReservations {
    entries: RwLock<HashMap<ResourceKey, Reservation>>,
}

impl PendingReservations {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn prepare(&self, candidate: Reservation, committed_taken: bool, now: u64) -> Vote {
        let mut entries = self.entries.write().await;
        retain_live(&mut entries, now);
        if let Some(existing) = entries.get(&candidate.key) {
            if existing.nonce == candidate.nonce {
                return Vote::Allow;
            }
            return Vote::Deny(VoteConflict::HeldBy {
                holder: existing.owner.clone(),
                reservation_id: existing.id.clone(),
            });
        }
        if committed_taken {
            return Vote::Deny(VoteConflict::AlreadyCommitted);
        }
        entries.insert(candidate.key.clone(), candidate);
        Vote::Allow
    }

    pub async fn renew(&self, key: &ResourceKey, nonce: &str, expires_at: u64, now: u64) -> Vote {
        let mut entries = self.entries.write().await;
        retain_live(&mut entries, now);
        let Some(existing) = entries.get_mut(key) else {
            return Vote::Deny(VoteConflict::Expired);
        };
        if existing.nonce != nonce {
            return Vote::Deny(VoteConflict::OwnerMismatch);
        }
        existing.expires_at = expires_at;
        Vote::Allow
    }

    pub async fn release(&self, key: &ResourceKey, nonce: &str, now: u64) -> Vote {
        let mut entries = self.entries.write().await;
        retain_live(&mut entries, now);
        let Some(existing) = entries.get(key) else {
            return Vote::Allow;
        };
        if existing.nonce != nonce {
            return Vote::Deny(VoteConflict::OwnerMismatch);
        }
        entries.remove(key);
        Vote::Allow
    }

    pub async fn active_subnets(&self, now: u64) -> HashSet<Ipv4Net> {
        let mut entries = self.entries.write().await;
        retain_live(&mut entries, now);
        entries
            .keys()
            .filter_map(|key| match key {
                ResourceKey::Subnet(subnet) => Some(*subnet),
                _ => None,
            })
            .collect()
    }
}

fn retain_live(entries: &mut HashMap<ResourceKey, Reservation>, now: u64) {
    entries.retain(|_, reservation| reservation.expires_at > now);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coordination::{ReservationId, ResourceKey};
    use ployz_types::model::MachineId;

    fn reservation(key: ResourceKey, owner: &str, nonce: &str, expires_at: u64) -> Reservation {
        Reservation {
            id: ReservationId::random(),
            key,
            owner: MachineId(owner.into()),
            nonce: nonce.into(),
            expires_at,
        }
    }

    #[tokio::test]
    async fn first_prepare_wins_and_same_nonce_replays() {
        let pending = PendingReservations::new();
        let key = ResourceKey::DeployNamespace("ns-1".into());
        let candidate = reservation(key.clone(), "founder", "nonce-1", 10);
        assert_eq!(
            pending.prepare(candidate.clone(), false, 0).await,
            Vote::Allow
        );
        assert_eq!(pending.prepare(candidate, false, 0).await, Vote::Allow);
        assert!(matches!(
            pending
                .prepare(reservation(key, "other", "nonce-2", 10), false, 0)
                .await,
            Vote::Deny(VoteConflict::HeldBy { .. })
        ));
    }

    #[tokio::test]
    async fn ttl_expiry_frees_pending_entry() {
        let pending = PendingReservations::new();
        let key = ResourceKey::Subnet("10.210.5.0/24".parse().expect("valid subnet"));
        assert_eq!(
            pending
                .prepare(reservation(key.clone(), "founder", "nonce", 5), false, 0)
                .await,
            Vote::Allow
        );
        assert_eq!(
            pending
                .prepare(reservation(key, "other", "nonce-2", 10), false, 6)
                .await,
            Vote::Allow
        );
    }

    #[tokio::test]
    async fn release_and_renew_are_nonce_gated() {
        let pending = PendingReservations::new();
        let key = ResourceKey::DeployNamespace("ns-1".into());
        assert_eq!(
            pending
                .prepare(reservation(key.clone(), "founder", "nonce", 10), false, 0)
                .await,
            Vote::Allow
        );
        assert!(matches!(
            pending.renew(&key, "wrong", 20, 0).await,
            Vote::Deny(VoteConflict::OwnerMismatch)
        ));
        assert!(matches!(
            pending.release(&key, "wrong", 0).await,
            Vote::Deny(VoteConflict::OwnerMismatch)
        ));
        assert_eq!(pending.renew(&key, "nonce", 20, 0).await, Vote::Allow);
        assert_eq!(pending.release(&key, "nonce", 0).await, Vote::Allow);
    }
}
