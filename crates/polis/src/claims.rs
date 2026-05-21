//! Advisory claim and fencing primitives.

use std::time::SystemTime;

use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ResourceId(String);

impl ResourceId {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HolderId(String);

impl HolderId {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimEpoch(u64);

impl ClaimEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimLease {
    pub resource: ResourceId,
    pub holder: HolderId,
    pub epoch: ClaimEpoch,
    pub expires_at: SystemTime,
}

impl ClaimLease {
    #[must_use]
    pub fn new(
        resource: ResourceId,
        holder: HolderId,
        epoch: ClaimEpoch,
        expires_at: SystemTime,
    ) -> Self {
        Self {
            resource,
            holder,
            epoch,
            expires_at,
        }
    }

    #[must_use]
    pub fn token(&self) -> FenceToken {
        FenceToken {
            resource: self.resource.clone(),
            holder: self.holder.clone(),
            epoch: self.epoch,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceToken {
    pub resource: ResourceId,
    pub holder: HolderId,
    pub epoch: ClaimEpoch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceCheck {
    Current,
    Expired,
    ResourceMismatch,
    HolderMismatch,
    StaleEpoch,
}

pub struct FenceValidator;

impl FenceValidator {
    #[must_use]
    pub fn validate(lease: &ClaimLease, token: &FenceToken, now: SystemTime) -> FenceCheck {
        if now >= lease.expires_at {
            return FenceCheck::Expired;
        }
        if lease.resource != token.resource {
            return FenceCheck::ResourceMismatch;
        }
        if lease.holder != token.holder {
            return FenceCheck::HolderMismatch;
        }
        if lease.epoch != token.epoch {
            return FenceCheck::StaleEpoch;
        }
        FenceCheck::Current
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn lease() -> ClaimLease {
        ClaimLease::new(
            ResourceId::parse("cert:example.com").expect("resource"),
            HolderId::parse("node-a").expect("holder"),
            ClaimEpoch::new(3),
            UNIX_EPOCH + Duration::from_secs(10),
        )
    }

    #[test]
    fn matching_unexpired_token_is_current() {
        let lease = lease();

        assert_eq!(
            FenceValidator::validate(&lease, &lease.token(), UNIX_EPOCH + Duration::from_secs(1)),
            FenceCheck::Current
        );
    }

    #[test]
    fn expired_lease_is_not_current() {
        let lease = lease();

        assert_eq!(
            FenceValidator::validate(&lease, &lease.token(), UNIX_EPOCH + Duration::from_secs(10)),
            FenceCheck::Expired
        );
    }

    #[test]
    fn stale_epoch_is_rejected() {
        let lease = lease();
        let token = FenceToken {
            resource: lease.resource.clone(),
            holder: lease.holder.clone(),
            epoch: ClaimEpoch::new(2),
        };

        assert_eq!(
            FenceValidator::validate(&lease, &token, UNIX_EPOCH + Duration::from_secs(1)),
            FenceCheck::StaleEpoch
        );
    }

    #[test]
    fn wrong_resource_is_rejected() {
        let lease = lease();
        let token = FenceToken {
            resource: ResourceId::parse("cert:other.example.com").expect("resource"),
            holder: lease.holder.clone(),
            epoch: lease.epoch,
        };

        assert_eq!(
            FenceValidator::validate(&lease, &token, UNIX_EPOCH + Duration::from_secs(1)),
            FenceCheck::ResourceMismatch
        );
    }

    #[test]
    fn wrong_holder_is_rejected() {
        let lease = lease();
        let token = FenceToken {
            resource: lease.resource.clone(),
            holder: HolderId::parse("node-b").expect("holder"),
            epoch: lease.epoch,
        };

        assert_eq!(
            FenceValidator::validate(&lease, &token, UNIX_EPOCH + Duration::from_secs(1)),
            FenceCheck::HolderMismatch
        );
    }
}
