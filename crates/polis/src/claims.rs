//! Advisory claim and fencing primitives.

use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::time::SystemTime;

use crate::{Error, Result};

#[derive(Debug)]
pub struct ResourceId<R = ()> {
    value: String,
    _resource: PhantomData<fn() -> R>,
}

impl<R> Clone for ResourceId<R> {
    fn clone(&self) -> Self {
        Self {
            value: self.value.clone(),
            _resource: PhantomData,
        }
    }
}

impl<R> PartialEq for ResourceId<R> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<R> Eq for ResourceId<R> {}

impl<R> PartialOrd for ResourceId<R> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<R> Ord for ResourceId<R> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value.cmp(&other.value)
    }
}

impl<R> Hash for ResourceId<R> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.value.hash(state);
    }
}

impl<R> ResourceId<R> {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self {
            value,
            _resource: PhantomData,
        })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.value
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
    pub fn new(value: u64) -> Result<Self> {
        if value == 0 || value == u64::MAX {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimHash(String);

impl ClaimHash {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimLease<R = ()> {
    resource: ResourceId<R>,
    holder: HolderId,
    epoch: ClaimEpoch,
    claim_hash: ClaimHash,
    expires_at: SystemTime,
}

impl<R> ClaimLease<R> {
    #[cfg(test)]
    #[must_use]
    fn new(
        resource: ResourceId<R>,
        holder: HolderId,
        epoch: ClaimEpoch,
        claim_hash: ClaimHash,
        expires_at: SystemTime,
    ) -> Self {
        Self {
            resource,
            holder,
            epoch,
            claim_hash,
            expires_at,
        }
    }

    #[must_use]
    pub fn resource(&self) -> &ResourceId<R> {
        &self.resource
    }

    #[must_use]
    pub fn holder(&self) -> &HolderId {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> ClaimEpoch {
        self.epoch
    }

    #[must_use]
    pub fn claim_hash(&self) -> &ClaimHash {
        &self.claim_hash
    }

    #[must_use]
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    #[must_use]
    pub fn token(&self) -> FenceToken<R> {
        FenceToken {
            resource: self.resource.clone(),
            holder: self.holder.clone(),
            epoch: self.epoch,
            claim_hash: self.claim_hash.clone(),
        }
    }

    #[must_use]
    pub fn guard(&self) -> ClaimGuard<R> {
        ClaimGuard {
            resource: self.resource.clone(),
            fence: self.token(),
            expires_at: self.expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FenceToken<R = ()> {
    resource: ResourceId<R>,
    holder: HolderId,
    epoch: ClaimEpoch,
    claim_hash: ClaimHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimGuard<R = ()> {
    resource: ResourceId<R>,
    fence: FenceToken<R>,
    expires_at: SystemTime,
}

impl<R> ClaimGuard<R> {
    #[must_use]
    pub fn resource(&self) -> &ResourceId<R> {
        &self.resource
    }

    #[must_use]
    pub fn fence(&self) -> &FenceToken<R> {
        &self.fence
    }

    #[must_use]
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }
}

impl<R> FenceToken<R> {
    #[must_use]
    pub fn resource(&self) -> &ResourceId<R> {
        &self.resource
    }

    #[must_use]
    pub fn holder(&self) -> &HolderId {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> ClaimEpoch {
        self.epoch
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FenceCheck {
    Current,
    Expired,
    ResourceMismatch,
    HolderMismatch,
    StaleEpoch,
    ClaimHashMismatch,
}

pub struct FenceValidator;

impl FenceValidator {
    #[must_use]
    pub fn validate<R>(
        lease: &ClaimLease<R>,
        token: &FenceToken<R>,
        now: SystemTime,
    ) -> FenceCheck {
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
        if lease.claim_hash != token.claim_hash {
            return FenceCheck::ClaimHashMismatch;
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
            ClaimEpoch::new(3).expect("epoch"),
            ClaimHash::parse("hash-a").expect("hash"),
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
            epoch: ClaimEpoch::new(2).expect("epoch"),
            claim_hash: lease.claim_hash.clone(),
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
            claim_hash: lease.claim_hash.clone(),
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
            claim_hash: lease.claim_hash.clone(),
        };

        assert_eq!(
            FenceValidator::validate(&lease, &token, UNIX_EPOCH + Duration::from_secs(1)),
            FenceCheck::HolderMismatch
        );
    }

    #[test]
    fn same_epoch_with_different_claim_hash_is_rejected() {
        let lease = lease();
        let token = FenceToken {
            resource: lease.resource.clone(),
            holder: lease.holder.clone(),
            epoch: lease.epoch,
            claim_hash: ClaimHash::parse("hash-b").expect("hash"),
        };

        assert_eq!(
            FenceValidator::validate(&lease, &token, UNIX_EPOCH + Duration::from_secs(1)),
            FenceCheck::ClaimHashMismatch
        );
    }

    #[test]
    fn zero_and_reserved_epochs_are_rejected() {
        assert_eq!(ClaimEpoch::new(0), Err(Error::MalformedPayload));
        assert_eq!(ClaimEpoch::new(u64::MAX), Err(Error::MalformedPayload));
    }

    #[test]
    fn empty_claim_hash_is_rejected() {
        assert_eq!(ClaimHash::parse(""), Err(Error::MalformedPayload));
    }

    #[test]
    fn lease_produces_typed_claim_guard() {
        enum DomainResource {}

        let lease = ClaimLease::new(
            ResourceId::<DomainResource>::parse("domain:app.example.com").expect("resource"),
            HolderId::parse("node-a").expect("holder"),
            ClaimEpoch::new(3).expect("epoch"),
            ClaimHash::parse("hash-a").expect("hash"),
            UNIX_EPOCH + Duration::from_secs(10),
        );

        let guard = lease.guard();

        assert_eq!(guard.resource.as_str(), "domain:app.example.com");
    }
}
