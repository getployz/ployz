//! Authorized record substrate.

use crate::authority::{AuthorityContext, GrantEpoch};
use crate::identity::{PrincipalId, ScopeId, SourceWatermark};
use crate::{Error, Result};

pub type Bytes = Vec<u8>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRecord {
    pub source: RecordSource,
    pub payload: Bytes,
    pub watermark: SourceWatermark,
}

impl RawRecord {
    pub fn new(source: RecordSource, payload: Bytes, watermark: SourceWatermark) -> Result<Self> {
        if payload.is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self {
            source,
            payload,
            watermark,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordSource {
    pub principal: PrincipalId,
    pub scope: ScopeId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SchemaVersion(u16);

impl SchemaVersion {
    #[must_use]
    pub fn new(value: u16) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofMetadata {
    pub principal: PrincipalId,
    pub scope: ScopeId,
    pub grant_epoch: GrantEpoch,
    pub source_watermark: SourceWatermark,
    pub schema_version: SchemaVersion,
}

impl ProofMetadata {
    #[must_use]
    pub fn new(
        authority: AuthorityContext,
        source_watermark: SourceWatermark,
        schema_version: SchemaVersion,
    ) -> Self {
        Self {
            principal: authority.principal,
            scope: authority.scope,
            grant_epoch: authority.epoch,
            source_watermark,
            schema_version,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedRecord {
    pub payload: Bytes,
    pub proof: ProofMetadata,
}

impl AuthorizedRecord {
    #[must_use]
    pub fn new(payload: Bytes, proof: ProofMetadata) -> Self {
        Self { payload, proof }
    }
}

pub trait RecordAuthorizer {
    fn authorize(&self, raw: RawRecord) -> Result<AuthorizedRecord>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{AuthorityContext, GrantEpoch};

    #[test]
    fn empty_payload_is_rejected_before_authorization() {
        let principal = PrincipalId::parse("node-a").expect("principal");
        let scope = ScopeId::parse("cluster").expect("scope");
        let source = RecordSource { principal, scope };

        assert_eq!(
            RawRecord::new(source, Vec::new(), SourceWatermark::new(1)),
            Err(Error::MalformedPayload)
        );
    }

    #[test]
    fn authorized_record_carries_proof_metadata() {
        let principal = PrincipalId::parse("node-a").expect("principal");
        let scope = ScopeId::parse("cluster").expect("scope");
        let authority = AuthorityContext::new(principal, scope, GrantEpoch::new(3));
        let proof = ProofMetadata::new(authority, SourceWatermark::new(9), SchemaVersion::new(1));
        let record = AuthorizedRecord::new(vec![1, 2, 3], proof);

        assert_eq!(record.proof.grant_epoch, GrantEpoch::new(3));
    }
}
