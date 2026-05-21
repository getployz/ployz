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
    holder: PrincipalId,
    epoch: FenceEpoch,
    claim_hash: ClaimHash,
    expires_at: SystemTime,
}

impl<R> ClaimGuard<R> {
    #[must_use]
    pub(crate) fn new(
        resource: TypedResourceId<R>,
        holder: PrincipalId,
        epoch: FenceEpoch,
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
    pub fn resource(&self) -> &TypedResourceId<R> {
        &self.resource
    }

    #[must_use]
    pub fn holder(&self) -> &PrincipalId {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> FenceEpoch {
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
}
