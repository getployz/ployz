//! Product-owned fact boundary types.
//!
//! Feature modules should describe product facts with these values and traits.
//! Raw substrate records and projection internals stay behind
//! `crates/ployz/src/adapters/**`.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::error::PrimitiveFailure;
use crate::operation::MutationContext;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductFactResource(String);

impl ProductFactResource {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductFactKey(String);

impl ProductFactKey {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductFactKind(String);

impl ProductFactKind {
    pub fn parse(value: impl Into<String>) -> Result<Self, PrimitiveFailure> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductFactPayload(Vec<u8>);

impl ProductFactPayload {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self, PrimitiveFailure> {
        let value = value.into();
        if value.is_empty() {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductFactCursor(u64);

impl ProductFactCursor {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductFactEnvelope {
    target: ProductFactTarget,
    payload: ProductFactPayload,
}

impl ProductFactEnvelope {
    #[must_use]
    pub fn new(target: ProductFactTarget, payload: ProductFactPayload) -> Self {
        Self { target, payload }
    }

    #[must_use]
    pub fn target(&self) -> &ProductFactTarget {
        &self.target
    }

    #[must_use]
    pub fn payload(&self) -> &ProductFactPayload {
        &self.payload
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProductFactTarget {
    resource: ProductFactResource,
    key: ProductFactKey,
    kind: ProductFactKind,
}

impl ProductFactTarget {
    #[must_use]
    pub fn new(resource: ProductFactResource, key: ProductFactKey, kind: ProductFactKind) -> Self {
        Self {
            resource,
            key,
            kind,
        }
    }

    #[must_use]
    pub fn resource(&self) -> &ProductFactResource {
        &self.resource
    }

    #[must_use]
    pub fn key(&self) -> &ProductFactKey {
        &self.key
    }

    #[must_use]
    pub fn kind(&self) -> &ProductFactKind {
        &self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductFactReceipt {
    target: ProductFactTarget,
    cursor: ProductFactCursor,
}

impl ProductFactReceipt {
    #[must_use]
    pub fn new(target: ProductFactTarget, cursor: ProductFactCursor) -> Self {
        Self { target, cursor }
    }

    #[must_use]
    pub fn target(&self) -> &ProductFactTarget {
        &self.target
    }

    #[must_use]
    pub fn cursor(&self) -> ProductFactCursor {
        self.cursor
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductFactAppendOutcome {
    Appended(ProductFactReceipt),
    Replayed(ProductFactReceipt),
    Conflict(ProductFactConflict),
    Rejected(ProductFactRejection),
}

impl ProductFactAppendOutcome {
    #[must_use]
    pub fn accepted_receipt(&self) -> Option<&ProductFactReceipt> {
        match self {
            Self::Appended(receipt) | Self::Replayed(receipt) => Some(receipt),
            Self::Conflict(_) | Self::Rejected(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductFactConflict {
    IdempotencyKeyReuse {
        existing: ProductFactReceipt,
    },
    KeyPayloadConflict {
        existing: ProductFactReceipt,
        new_candidate: ProductFactReceipt,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductFactRejection {
    Unauthorized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductFactAppendError<EncodeError, WriteError> {
    Encode(EncodeError),
    Write(WriteError),
}

impl<EncodeError, WriteError> ProductFactAppendError<EncodeError, WriteError> {
    #[must_use]
    pub fn from_encode(error: EncodeError) -> Self {
        Self::Encode(error)
    }

    #[must_use]
    pub fn from_write(error: WriteError) -> Self {
        Self::Write(error)
    }
}

pub trait ProductFact {
    type Error;

    fn encode(&self) -> Result<ProductFactEnvelope, Self::Error>;
}

pub trait ProductFactWriter<F>
where
    F: ProductFact,
{
    type WriteError;

    fn append_fact(
        &self,
        context: &MutationContext,
        fact: &F,
    ) -> Result<ProductFactAppendOutcome, ProductFactAppendError<F::Error, Self::WriteError>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProductProjectionFreshness {
    Fresh,
    Degraded,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductProjectionHealth {
    candidate_rejections: BTreeMap<ProductCandidateRejection, usize>,
    payload_failures: BTreeMap<ProductPayloadFailure, usize>,
}

impl ProductProjectionHealth {
    #[must_use]
    pub fn new(
        candidate_rejections: BTreeMap<ProductCandidateRejection, usize>,
        payload_failures: BTreeMap<ProductPayloadFailure, usize>,
    ) -> Self {
        Self {
            candidate_rejections,
            payload_failures,
        }
    }

    #[must_use]
    pub fn healthy() -> Self {
        Self::new(BTreeMap::new(), BTreeMap::new())
    }

    #[must_use]
    pub fn rejected_candidates(&self) -> usize {
        self.candidate_rejections.values().sum()
    }

    #[must_use]
    pub fn candidate_count(&self, rejection: ProductCandidateRejection) -> usize {
        self.candidate_rejections
            .get(&rejection)
            .copied()
            .unwrap_or(0)
    }

    #[must_use]
    pub fn payload_failures(&self) -> usize {
        self.payload_failures.values().sum()
    }

    #[must_use]
    pub fn payload_failure_count(&self, failure: ProductPayloadFailure) -> usize {
        self.payload_failures.get(&failure).copied().unwrap_or(0)
    }

    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.rejected_candidates() > 0 || self.payload_failures() > 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProductCandidateRejection {
    Conflict,
    Unauthorized,
    Unverified,
    MissingPayload,
    SubstrateMalformed,
    CrossScope,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProductPayloadFailure {
    UnknownCandidate,
    CandidateMismatch,
    MissingPayload,
    DigestMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProductProjectionSnapshot<V> {
    view: V,
    source_cursor: Option<ProductFactCursor>,
    freshness: ProductProjectionFreshness,
    health: ProductProjectionHealth,
}

impl<V> ProductProjectionSnapshot<V> {
    #[must_use]
    pub fn new(
        view: V,
        source_cursor: Option<ProductFactCursor>,
        freshness: ProductProjectionFreshness,
        health: ProductProjectionHealth,
    ) -> Self {
        Self {
            view,
            source_cursor,
            freshness,
            health,
        }
    }

    #[must_use]
    pub fn view(&self) -> &V {
        &self.view
    }

    #[must_use]
    pub fn into_view(self) -> V {
        self.view
    }

    #[must_use]
    pub fn source_cursor(&self) -> Option<ProductFactCursor> {
        self.source_cursor
    }

    #[must_use]
    pub fn freshness(&self) -> ProductProjectionFreshness {
        self.freshness
    }

    #[must_use]
    pub fn health(&self) -> &ProductProjectionHealth {
        &self.health
    }

    #[must_use]
    pub fn catch_up_request(
        &self,
        deadline: SystemTime,
    ) -> Option<ProductProjectionCatchUpRequest> {
        self.source_cursor
            .map(|cursor| ProductProjectionCatchUpRequest::new(cursor, deadline))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProductProjectionCatchUpRequest {
    cursor: ProductFactCursor,
    deadline: SystemTime,
}

impl ProductProjectionCatchUpRequest {
    #[must_use]
    pub fn new(cursor: ProductFactCursor, deadline: SystemTime) -> Self {
        Self { cursor, deadline }
    }

    #[must_use]
    pub fn cursor(self) -> ProductFactCursor {
        self.cursor
    }

    #[must_use]
    pub fn deadline(self) -> SystemTime {
        self.deadline
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductProjectionCatchUp {
    CaughtUp {
        source_cursor: ProductFactCursor,
    },
    TimedOut {
        requested: ProductFactCursor,
        current: Option<ProductFactCursor>,
    },
    FreshnessUnknown {
        requested: ProductFactCursor,
    },
    ProjectionFailed {
        requested: ProductFactCursor,
        source_cursor: ProductFactCursor,
        health: ProductProjectionHealth,
    },
}

pub trait ProductViewReader<V> {
    type Error;

    fn read_view(
        &self,
        context: &MutationContext,
    ) -> Result<ProductProjectionSnapshot<V>, Self::Error>;

    fn catch_up(
        &self,
        context: &MutationContext,
        request: ProductProjectionCatchUpRequest,
    ) -> Result<ProductProjectionCatchUp, Self::Error>;
}

fn parse_non_empty<T>(
    value: impl Into<String>,
    build: impl FnOnce(String) -> T,
) -> Result<T, PrimitiveFailure> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(PrimitiveFailure::MalformedPayload);
    }
    Ok(build(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn product_fact_payload_rejects_empty_bytes() {
        assert_eq!(
            ProductFactPayload::new(Vec::<u8>::new()),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }

    #[test]
    fn product_fact_identity_rejects_empty_strings() {
        assert_eq!(
            ProductFactResource::parse(""),
            Err(PrimitiveFailure::MalformedPayload)
        );
        assert_eq!(
            ProductFactKey::parse(""),
            Err(PrimitiveFailure::MalformedPayload)
        );
        assert_eq!(
            ProductFactKind::parse(""),
            Err(PrimitiveFailure::MalformedPayload)
        );
    }

    #[test]
    fn product_fact_append_error_separates_encode_and_write_failures() {
        let encode: ProductFactAppendError<&str, PrimitiveFailure> =
            ProductFactAppendError::from_encode("bad schema");
        let write: ProductFactAppendError<&str, PrimitiveFailure> =
            ProductFactAppendError::from_write(PrimitiveFailure::Conflict);

        assert_eq!(encode, ProductFactAppendError::Encode("bad schema"));
        assert_eq!(
            write,
            ProductFactAppendError::Write(PrimitiveFailure::Conflict)
        );
    }

    #[test]
    fn product_projection_snapshot_preserves_visibility_status() {
        let health = ProductProjectionHealth::new(
            BTreeMap::from([(ProductCandidateRejection::Conflict, 2)]),
            BTreeMap::from([(ProductPayloadFailure::MissingPayload, 1)]),
        );
        let snapshot = ProductProjectionSnapshot::new(
            "machine-view",
            Some(ProductFactCursor::new(42)),
            ProductProjectionFreshness::Degraded,
            health.clone(),
        );

        assert_eq!(snapshot.view(), &"machine-view");
        assert_eq!(snapshot.source_cursor(), Some(ProductFactCursor::new(42)));
        assert_eq!(snapshot.freshness(), ProductProjectionFreshness::Degraded);
        assert_eq!(snapshot.health(), &health);
        assert_eq!(
            snapshot
                .health()
                .candidate_count(ProductCandidateRejection::Conflict),
            2
        );
        assert_eq!(
            snapshot
                .health()
                .payload_failure_count(ProductPayloadFailure::MissingPayload),
            1
        );
        assert!(snapshot.health().has_failures());
    }

    #[test]
    fn product_projection_snapshot_builds_catch_up_request() {
        let deadline = SystemTime::UNIX_EPOCH;
        let snapshot = ProductProjectionSnapshot::new(
            (),
            Some(ProductFactCursor::new(7)),
            ProductProjectionFreshness::Fresh,
            ProductProjectionHealth::healthy(),
        );

        let request = snapshot
            .catch_up_request(deadline)
            .expect("cursor-backed snapshot should produce request");

        assert_eq!(request.cursor(), ProductFactCursor::new(7));
        assert_eq!(request.deadline(), deadline);
    }
}
