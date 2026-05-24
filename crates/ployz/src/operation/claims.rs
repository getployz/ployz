use std::time::SystemTime;

use crate::error::PrimitiveFailure;
use crate::operation::identity::{PrincipalId, ResourceId, TypedResourceId, parse_non_empty};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FenceEpoch(u64);

impl FenceEpoch {
    pub fn new(value: u64) -> Result<Self, PrimitiveFailure> {
        if value == 0 || value == u64::MAX {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimHash(String);

impl ClaimHash {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmittedFenceToken {
    pub resource: ResourceId,
    pub holder: PrincipalId,
    pub epoch: FenceEpoch,
    pub claim_hash: ClaimHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimGuard<R> {
    resource: TypedResourceId<R>,
    submitted_fence: SubmittedFenceToken,
    expires_at: SystemTime,
}

impl<R> ClaimGuard<R> {
    pub fn new(
        resource: TypedResourceId<R>,
        submitted_fence: SubmittedFenceToken,
        expires_at: SystemTime,
    ) -> Result<Self, PrimitiveFailure> {
        if submitted_fence.resource.as_str() != resource.as_str() {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self {
            resource,
            submitted_fence,
            expires_at,
        })
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn test_new(
        resource: TypedResourceId<R>,
        holder: PrincipalId,
        epoch: FenceEpoch,
        claim_hash: ClaimHash,
        expires_at: SystemTime,
    ) -> Result<Self, PrimitiveFailure> {
        let submitted_fence = SubmittedFenceToken {
            resource: ResourceId::parse(resource.as_str())?,
            holder,
            epoch,
            claim_hash,
        };
        Self::new(resource, submitted_fence, expires_at)
    }

    #[must_use]
    pub fn resource(&self) -> &TypedResourceId<R> {
        &self.resource
    }

    #[must_use]
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    #[must_use]
    pub fn submitted_fence(&self) -> &SubmittedFenceToken {
        &self.submitted_fence
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_fence_epochs_are_malformed_payload() {
        assert_eq!(FenceEpoch::new(0), Err(PrimitiveFailure::MalformedPayload));
        assert_eq!(
            FenceEpoch::new(u64::MAX),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }

    #[test]
    fn claim_guard_is_ployz_owned_fence_dto() {
        enum TestResource {}

        let guard = ClaimGuard::test_new(
            TypedResourceId::<TestResource>::parse("domain:example.com").expect("resource"),
            PrincipalId::parse("node-a").expect("holder"),
            FenceEpoch::new(2).expect("epoch"),
            ClaimHash::parse("claim-hash").expect("hash"),
            SystemTime::UNIX_EPOCH,
        )
        .expect("guard");

        assert_eq!(guard.resource().as_str(), "domain:example.com");
        assert_eq!(guard.expires_at(), SystemTime::UNIX_EPOCH);

        let fence = guard.submitted_fence();
        assert_eq!(fence.resource.as_str(), "domain:example.com");
        assert_eq!(fence.holder.as_str(), "node-a");
        assert_eq!(fence.epoch.value(), 2);
        assert_eq!(fence.claim_hash.as_str(), "claim-hash");
    }

    #[test]
    fn mismatched_fence_resource_is_malformed_payload() {
        enum TestResource {}

        let guard = ClaimGuard::new(
            TypedResourceId::<TestResource>::parse("domain:example.com").expect("resource"),
            SubmittedFenceToken {
                resource: ResourceId::parse("domain:other.example.com").expect("resource"),
                holder: PrincipalId::parse("node-a").expect("holder"),
                epoch: FenceEpoch::new(2).expect("epoch"),
                claim_hash: ClaimHash::parse("claim-hash").expect("hash"),
            },
            SystemTime::UNIX_EPOCH,
        );

        assert!(matches!(guard, Err(PrimitiveFailure::MalformedPayload)));
    }
}
