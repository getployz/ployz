//! Product projection ports.

use crate::error::ProjectionFailure;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductViewKey(String);

impl ProductViewKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, ProjectionFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceWatermark(u64);

impl SourceWatermark {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionFreshness {
    Fresh(SourceWatermark),
    Stale {
        last_seen: SourceWatermark,
        required: SourceWatermark,
    },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductView<T> {
    pub value: T,
    pub freshness: ProjectionFreshness,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionRead<T> {
    Found(ProductView<T>),
    Missing,
    Stale(ProjectionFreshness),
}

pub trait ProjectionPort<T> {
    fn read_view(&self, key: &ProductViewKey) -> Result<ProjectionRead<T>, ProjectionFailure>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductRecordEnvelope {
    payload: Vec<u8>,
    metadata: ProductRecordMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductRecordMetadata {
    principal: ProductPrincipalId,
    scope: ProductScopeId,
    grant_epoch: ProductGrantEpoch,
    source_watermark: SourceWatermark,
    schema_version: u16,
}

impl ProductRecordEnvelope {
    #[must_use]
    pub(crate) fn new(payload: Vec<u8>, metadata: ProductRecordMetadata) -> Self {
        Self { payload, metadata }
    }

    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    #[must_use]
    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }

    #[must_use]
    pub fn metadata(&self) -> &ProductRecordMetadata {
        &self.metadata
    }
}

impl ProductRecordMetadata {
    #[must_use]
    pub(crate) fn new(
        principal: ProductPrincipalId,
        scope: ProductScopeId,
        grant_epoch: ProductGrantEpoch,
        source_watermark: SourceWatermark,
        schema_version: u16,
    ) -> Self {
        Self {
            principal,
            scope,
            grant_epoch,
            source_watermark,
            schema_version,
        }
    }

    #[must_use]
    pub fn principal(&self) -> &ProductPrincipalId {
        &self.principal
    }

    #[must_use]
    pub fn scope(&self) -> &ProductScopeId {
        &self.scope
    }

    #[must_use]
    pub fn grant_epoch(&self) -> ProductGrantEpoch {
        self.grant_epoch
    }

    #[must_use]
    pub fn source_watermark(&self) -> SourceWatermark {
        self.source_watermark
    }

    #[must_use]
    pub fn schema_version(&self) -> u16 {
        self.schema_version
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductPrincipalId(String);

impl ProductPrincipalId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ProjectionFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductScopeId(String);

impl ProductScopeId {
    pub fn parse(value: impl Into<String>) -> Result<Self, ProjectionFailure> {
        parse_non_empty(value, Self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductGrantEpoch(u64);

impl ProductGrantEpoch {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

pub trait ProductRecordDecoder<T> {
    fn decode(&self, record: ProductRecordEnvelope) -> Result<T, ProjectionFailure>;
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, ProjectionFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(ProjectionFailure::InvalidPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_freshness_is_not_fresh() {
        let freshness = ProjectionFreshness::Unknown;

        assert!(!matches!(freshness, ProjectionFreshness::Fresh(_)));
    }

    #[test]
    fn product_record_envelope_keeps_proof_metadata() {
        let record = ProductRecordEnvelope::new(
            vec![1, 2, 3],
            ProductRecordMetadata::new(
                ProductPrincipalId::parse("node-a").expect("principal"),
                ProductScopeId::parse("cluster").expect("scope"),
                ProductGrantEpoch::new(7),
                SourceWatermark::new(9),
                1,
            ),
        );

        assert_eq!(
            record.metadata().source_watermark(),
            SourceWatermark::new(9)
        );
        assert_eq!(record.metadata().grant_epoch().value(), 7);
    }

    #[test]
    fn empty_product_view_key_is_rejected() {
        assert_eq!(
            ProductViewKey::parse(""),
            Err(ProjectionFailure::InvalidPayload)
        );
    }
}
