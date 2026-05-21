use std::time::SystemTime;

use crate::error::PrimitiveFailure;
use crate::operation::identity::{PrincipalId, ResourceId, TypedResourceId, parse_non_empty};
use crate::operation::polis_boundary::map_polis_to_primitive;

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

impl SubmittedFenceToken {
    #[cfg(test)]
    pub(crate) fn fingerprint(&self) -> Result<polis::SubmittedFenceFingerprint, PrimitiveFailure> {
        polis::SubmittedFenceFingerprint::new(
            polis::FingerprintedResource::parse(self.resource.as_str())
                .map_err(map_polis_to_primitive)?,
            polis::PrincipalId::parse(self.holder.as_str()).map_err(map_polis_to_primitive)?,
            self.epoch.value(),
            self.claim_hash.as_str().as_bytes().to_vec(),
        )
        .map_err(map_polis_to_primitive)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimGuard<R> {
    resource: TypedResourceId<R>,
    submitted_fence: SubmittedFenceToken,
    proof: polis::ClaimGuard<R>,
}

impl<R> ClaimGuard<R> {
    pub fn from_acquired(proof: polis::ClaimGuard<R>) -> Result<Self, PrimitiveFailure> {
        let resource = TypedResourceId::parse(proof.resource().as_str())?;
        let fence = proof.fence();
        let submitted_fence = SubmittedFenceToken {
            resource: ResourceId::parse(resource.as_str())?,
            holder: PrincipalId::parse(fence.holder().as_str())?,
            epoch: FenceEpoch::new(fence.epoch().value())?,
            claim_hash: ClaimHash::parse(fence.claim_hash().as_str())?,
        };
        Ok(Self {
            resource,
            submitted_fence,
            proof,
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
        let lease = polis::ClaimLease::test_new(
            polis::ResourceId::parse(resource.as_str()).map_err(map_polis_to_primitive)?,
            polis::HolderId::parse(holder.as_str()).map_err(map_polis_to_primitive)?,
            polis::ClaimEpoch::new(epoch.value()).map_err(map_polis_to_primitive)?,
            polis::ClaimHash::parse(claim_hash.as_str()).map_err(map_polis_to_primitive)?,
            expires_at,
        );
        Self::from_acquired(lease.guard())
    }

    #[must_use]
    pub fn resource(&self) -> &TypedResourceId<R> {
        &self.resource
    }

    #[must_use]
    pub fn expires_at(&self) -> SystemTime {
        self.proof.expires_at()
    }

    #[must_use]
    pub fn submitted_fence(&self) -> &SubmittedFenceToken {
        &self.submitted_fence
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn proof(&self) -> &polis::ClaimGuard<R> {
        &self.proof
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
    fn claim_guard_wraps_polis_claim_proof() {
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
        assert_eq!(guard.proof().resource().as_str(), "domain:example.com");
        assert_eq!(guard.proof().fence().holder().as_str(), "node-a");
        assert_eq!(guard.proof().fence().epoch().value(), 2);
        assert_eq!(guard.proof().fence().claim_hash().as_str(), "claim-hash");
    }

    #[test]
    fn submitted_fence_fingerprint_includes_fence_identity() {
        let fence = SubmittedFenceToken {
            resource: ResourceId::parse("volume:data").expect("resource"),
            holder: PrincipalId::parse("node-a").expect("holder"),
            epoch: FenceEpoch::new(3).expect("epoch"),
            claim_hash: ClaimHash::parse("claim-hash-a").expect("hash"),
        };

        let fingerprint = fence.fingerprint().expect("fingerprint");

        assert_eq!(fingerprint.resource(), "volume:data");
        assert_eq!(fingerprint.holder(), "node-a");
        assert_eq!(fingerprint.epoch(), 3);
        assert_eq!(fingerprint.claim_hash(), b"claim-hash-a");
    }
}
