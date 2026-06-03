use std::time::SystemTime;

use crate::error::PrimitiveFailure;

use super::identity::OperationOwner;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationLeaseEpoch(u64);

impl OperationLeaseEpoch {
    pub fn new(value: u64) -> Result<Self, PrimitiveFailure> {
        if value == 0 {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationLease {
    owner: OperationOwner,
    epoch: OperationLeaseEpoch,
    renewed_at: SystemTime,
    expires_at: SystemTime,
}

impl OperationLease {
    pub fn new(
        owner: OperationOwner,
        epoch: OperationLeaseEpoch,
        renewed_at: SystemTime,
        expires_at: SystemTime,
    ) -> Result<Self, PrimitiveFailure> {
        if expires_at <= renewed_at {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self {
            owner,
            epoch,
            renewed_at,
            expires_at,
        })
    }

    #[must_use]
    pub fn owner(&self) -> &OperationOwner {
        &self.owner
    }

    #[must_use]
    pub fn epoch(&self) -> OperationLeaseEpoch {
        self.epoch
    }

    #[must_use]
    pub fn renewed_at(&self) -> SystemTime {
        self.renewed_at
    }

    #[must_use]
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    #[must_use]
    pub fn liveness_at(&self, now: SystemTime) -> OperationLiveness {
        if now > self.expires_at {
            OperationLiveness::Expired {
                owner: self.owner.clone(),
                epoch: self.epoch,
                expired_at: self.expires_at,
            }
        } else {
            OperationLiveness::Held {
                owner: self.owner.clone(),
                epoch: self.epoch,
                expires_at: self.expires_at,
            }
        }
    }

    pub fn ensure_checkpoint_allowed(
        &self,
        owner: &OperationOwner,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> Result<(), PrimitiveFailure> {
        if self.owner != *owner || self.epoch != epoch || now > self.expires_at {
            return Err(PrimitiveFailure::OperationInterrupted);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationLiveness {
    Held {
        owner: OperationOwner,
        epoch: OperationLeaseEpoch,
        expires_at: SystemTime,
    },
    Expired {
        owner: OperationOwner,
        epoch: OperationLeaseEpoch,
        expired_at: SystemTime,
    },
    NoActiveDriver,
}
