use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ipnet::Ipv4Net;
use tokio::sync::Mutex;

use ployz_types::model::MachineId;

/// Reasons a `try_claim` call may fail.
#[derive(Debug)]
pub enum ClaimError {
    /// The candidate subnet is currently held by another lease.
    AlreadyHeld,
    /// The coordinator backend itself failed (NATS unreachable, internal
    /// state corruption, etc). Surface upward — this is not a contention
    /// signal.
    Backend(String),
}

impl fmt::Display for ClaimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyHeld => f.write_str("subnet lock already held"),
            Self::Backend(message) => write!(f, "subnet lock backend: {message}"),
        }
    }
}

impl std::error::Error for ClaimError {}

/// Backend-owned release handle. Implementors release the underlying lock
/// when `release` is called; if the holder crashes without calling release,
/// the lock falls back to TTL expiry.
#[async_trait]
pub trait SubnetClaimRelease: Send + Sync {
    async fn release(self: Box<Self>) -> Result<(), String>;
}

/// An opaque holder for a successful subnet claim.
pub struct SubnetClaim {
    inner: Box<dyn SubnetClaimRelease>,
}

impl SubnetClaim {
    #[must_use]
    pub fn new(inner: Box<dyn SubnetClaimRelease>) -> Self {
        Self { inner }
    }

    /// Release the claim. On error the caller may log and continue — the
    /// lease TTL guarantees the lock is eventually freed.
    pub async fn release(self) -> Result<(), String> {
        let Self { inner } = self;
        inner.release().await
    }
}

impl fmt::Debug for SubnetClaim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SubnetClaim").finish_non_exhaustive()
    }
}

/// Atomically reserves subnets across the mesh. The implementation owns the
/// uniqueness invariant — concurrent calls for the same subnet must produce
/// at most one `Ok` result.
#[async_trait]
pub trait SubnetReservationCoordinator: Send + Sync {
    async fn try_claim(
        &self,
        candidate: Ipv4Net,
        owner: &MachineId,
        ttl: Duration,
    ) -> Result<SubnetClaim, ClaimError>;
}

/// In-memory coordinator suitable for tests and single-node memory runtimes.
/// Backed by a single `Mutex<HashSet<_>>`; `try_claim` returns `AlreadyHeld`
/// for any subnet already in the set.
#[derive(Default, Clone)]
pub struct MemorySubnetCoordinator {
    held: Arc<Mutex<HashSet<Ipv4Net>>>,
}

impl MemorySubnetCoordinator {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl SubnetReservationCoordinator for MemorySubnetCoordinator {
    async fn try_claim(
        &self,
        candidate: Ipv4Net,
        _owner: &MachineId,
        _ttl: Duration,
    ) -> Result<SubnetClaim, ClaimError> {
        let mut held = self.held.lock().await;
        if !held.insert(candidate) {
            return Err(ClaimError::AlreadyHeld);
        }
        Ok(SubnetClaim::new(Box::new(MemorySubnetClaimRelease {
            held: self.held.clone(),
            subnet: candidate,
        })))
    }
}

struct MemorySubnetClaimRelease {
    held: Arc<Mutex<HashSet<Ipv4Net>>>,
    subnet: Ipv4Net,
}

#[async_trait]
impl SubnetClaimRelease for MemorySubnetClaimRelease {
    async fn release(self: Box<Self>) -> Result<(), String> {
        let Self { held, subnet } = *self;
        held.lock().await.remove(&subnet);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subnet(value: &str) -> Ipv4Net {
        value.parse().expect("valid subnet")
    }

    #[tokio::test]
    async fn first_claim_wins_then_second_sees_already_held() {
        let coord = MemorySubnetCoordinator::new();
        let owner = MachineId::new("a");
        let candidate = subnet("10.210.1.0/24");
        let claim = coord
            .try_claim(candidate, &owner, Duration::from_secs(30))
            .await
            .expect("first claim succeeds");
        let conflict = coord
            .try_claim(candidate, &owner, Duration::from_secs(30))
            .await;
        assert!(matches!(conflict, Err(ClaimError::AlreadyHeld)));
        claim.release().await.expect("release succeeds");
        let _retake = coord
            .try_claim(candidate, &owner, Duration::from_secs(30))
            .await
            .expect("retake after release succeeds");
    }

    #[tokio::test]
    async fn distinct_subnets_do_not_conflict() {
        let coord = MemorySubnetCoordinator::new();
        let owner = MachineId::new("a");
        let _a = coord
            .try_claim(subnet("10.0.1.0/24"), &owner, Duration::from_secs(30))
            .await
            .expect("first claim");
        let _b = coord
            .try_claim(subnet("10.0.2.0/24"), &owner, Duration::from_secs(30))
            .await
            .expect("second distinct claim");
    }
}
