use std::time::Duration;

use async_trait::async_trait;
use ipnet::Ipv4Net;

use ployz_nats::subnet_lock;
use ployz_nats::{Lease, LockAcquireError, NatsLocks};
use ployz_orchestrator::coordination::{
    ClaimError, ReservationId, SubnetClaim, SubnetClaimRelease, SubnetReservationCoordinator,
};
use ployz_types::model::MachineId;
use ployz_types::time::now_unix_secs;

/// JetStream KV-backed subnet reservation coordinator.
///
/// Acquisition is a single `kv.create` on `locks.subnet.<candidate>`. The
/// broker is the serialization point — concurrent acquirers see exactly one
/// `Ok` and the rest see [`ClaimError::AlreadyHeld`]. On daemon crash the
/// lease falls back to TTL expiry; explicit release is preferred for clean
/// shutdown so the next joiner does not have to wait for the TTL.
#[derive(Clone)]
pub struct NatsSubnetCoordinator {
    locks: NatsLocks,
}

impl NatsSubnetCoordinator {
    #[must_use]
    pub fn new(locks: NatsLocks) -> Self {
        Self { locks }
    }
}

#[async_trait]
impl SubnetReservationCoordinator for NatsSubnetCoordinator {
    async fn try_claim(
        &self,
        candidate: Ipv4Net,
        owner: &MachineId,
        ttl: Duration,
    ) -> Result<SubnetClaim, ClaimError> {
        let key = subnet_lock(candidate);
        let nonce = ReservationId::random().0;
        let expires_at = now_unix_secs().saturating_add(ttl.as_secs());
        match self
            .locks
            .acquire(&key, owner.0.clone(), nonce, ttl, expires_at)
            .await
        {
            Ok(lease) => Ok(SubnetClaim::new(Box::new(NatsSubnetClaimRelease {
                locks: self.locks.clone(),
                lease,
            }))),
            Err(error) => Err(claim_error_from_lock_acquire(error)),
        }
    }
}

fn claim_error_from_lock_acquire(error: LockAcquireError) -> ClaimError {
    match error {
        LockAcquireError::AlreadyHeld { .. } | LockAcquireError::Contention { .. } => {
            ClaimError::AlreadyHeld
        }
        error => ClaimError::Backend(error.to_string()),
    }
}

struct NatsSubnetClaimRelease {
    locks: NatsLocks,
    lease: Lease,
}

#[async_trait]
impl SubnetClaimRelease for NatsSubnetClaimRelease {
    async fn release(self: Box<Self>) -> Result<(), String> {
        let Self { locks, lease } = *self;
        locks
            .release(lease)
            .await
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_contention_is_retryable() {
        let result = claim_error_from_lock_acquire(LockAcquireError::Contention {
            key: "locks.subnet.x".into(),
            source: ployz_types::Error::operation("nats_lock_publish", "wrong sequence"),
        });

        assert!(matches!(result, ClaimError::AlreadyHeld));
    }
}
