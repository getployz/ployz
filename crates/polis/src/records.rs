//! Authorized record substrate.

use crate::authority::{Authorized, GrantEpoch};
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
    principal: PrincipalId,
    scope: ScopeId,
    grant_epoch: GrantEpoch,
    source_watermark: SourceWatermark,
    schema_version: SchemaVersion,
}

impl ProofMetadata {
    #[must_use]
    pub fn new(
        authority: &Authorized,
        source_watermark: SourceWatermark,
        schema_version: SchemaVersion,
    ) -> Self {
        let context = authority.context();
        Self {
            principal: context.principal().clone(),
            scope: context.scope().clone(),
            grant_epoch: context.epoch(),
            source_watermark,
            schema_version,
        }
    }

    #[must_use]
    pub fn principal(&self) -> &PrincipalId {
        &self.principal
    }

    #[must_use]
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }

    #[must_use]
    pub fn grant_epoch(&self) -> GrantEpoch {
        self.grant_epoch
    }

    #[must_use]
    pub fn source_watermark(&self) -> SourceWatermark {
        self.source_watermark
    }

    #[must_use]
    pub fn schema_version(&self) -> SchemaVersion {
        self.schema_version
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedRecord {
    payload: Bytes,
    proof: ProofMetadata,
}

impl AuthorizedRecord {
    pub fn new(
        payload: Bytes,
        authority: &Authorized,
        source_watermark: SourceWatermark,
        schema_version: SchemaVersion,
    ) -> Result<Self> {
        if payload.is_empty() {
            return Err(Error::MalformedPayload);
        }
        let proof = ProofMetadata::new(authority, source_watermark, schema_version);
        Ok(Self { payload, proof })
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    #[must_use]
    pub fn into_payload(self) -> Bytes {
        self.payload
    }

    #[must_use]
    pub fn proof(&self) -> &ProofMetadata {
        &self.proof
    }
}

pub trait RecordAuthorizer {
    fn authorize(&self, raw: RawRecord) -> Result<AuthorizedRecord>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{AuthorityContext, Authorized, GrantEpoch};

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
        let authorized = Authorized::new(authority);
        let record = AuthorizedRecord::new(
            vec![1, 2, 3],
            &authorized,
            SourceWatermark::new(9),
            SchemaVersion::new(1),
        )
        .expect("record");

        assert_eq!(record.proof().grant_epoch(), GrantEpoch::new(3));
    }
}
