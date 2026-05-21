//! Product projection ports.

use crate::error::ProjectionFailure;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductViewKey(String);

impl ProductViewKey {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
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
    pub payload: Vec<u8>,
    pub proof: ProductProofMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductProofMetadata {
    pub principal: String,
    pub scope: String,
    pub grant_epoch: u64,
    pub source_watermark: SourceWatermark,
    pub schema_version: u16,
}

pub trait ProductRecordDecoder<T> {
    fn decode(&self, record: ProductRecordEnvelope) -> Result<T, ProjectionFailure>;
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
        let record = ProductRecordEnvelope {
            payload: vec![1, 2, 3],
            proof: ProductProofMetadata {
                principal: "node-a".to_string(),
                scope: "cluster".to_string(),
                grant_epoch: 7,
                source_watermark: SourceWatermark::new(9),
                schema_version: 1,
            },
        };

        assert_eq!(record.proof.source_watermark, SourceWatermark::new(9));
    }
}
