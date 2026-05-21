use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::{self, Display, Formatter};
use std::num::NonZeroU64;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

use mvp_identity::{NodeId, VisibleNodes};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeaseResource(String);

impl LeaseResource {
    #[must_use]
    pub fn from_segments<const N: usize>(segments: [&str; N]) -> Self {
        let mut encoded = String::new();
        for segment in segments {
            if !encoded.is_empty() {
                encoded.push('.');
            }
            encode_resource_segment_into(segment, &mut encoded);
        }
        Self(encoded)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for LeaseResource {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeaseHolder(String);

impl LeaseHolder {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for LeaseHolder {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseCommandContext {
    visible_nodes: VisibleNodes,
}

impl LeaseCommandContext {
    #[must_use]
    pub fn new(visible_nodes: VisibleNodes) -> Self {
        Self { visible_nodes }
    }

    #[must_use]
    pub fn visible_nodes(&self) -> &VisibleNodes {
        &self.visible_nodes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub struct LeaseEpoch(NonZeroU64);

impl LeaseEpoch {
    #[must_use]
    pub fn first() -> Self {
        Self(NonZeroU64::MIN)
    }

    pub fn from_u64(value: u64) -> Result<Self, LeaseEpochError> {
        if value == u64::MAX {
            return Err(LeaseEpochError::MaxValue);
        }
        let Some(value) = NonZeroU64::new(value) else {
            return Err(LeaseEpochError::Zero);
        };
        Ok(Self(value))
    }

    pub fn next(self) -> Result<Self, LeaseEpochError> {
        let next = self
            .0
            .get()
            .checked_add(1)
            .ok_or(LeaseEpochError::MaxValue)?;
        Self::from_u64(next)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0.get()
    }
}

impl Display for LeaseEpoch {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.get())
    }
}

impl<'de> Deserialize<'de> for LeaseEpoch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::from_u64(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LeaseEpochError {
    #[error("lease epoch must be greater than zero")]
    Zero,
    #[error("lease epoch u64::MAX is reserved to prevent fencing counter overflow")]
    MaxValue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeaseTimestamp(u64);

impl LeaseTimestamp {
    #[must_use]
    pub fn from_secs(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn checked_add(self, duration: LeaseDuration) -> Self {
        Self(self.0.saturating_add(duration.as_secs()))
    }
}

impl Display for LeaseTimestamp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeaseDuration(NonZeroU64);

impl LeaseDuration {
    pub fn from_secs(seconds: u64) -> Result<Self, LeasePolicyError> {
        let Some(seconds) = NonZeroU64::new(seconds) else {
            return Err(LeasePolicyError::ZeroTtl);
        };
        Ok(Self(seconds))
    }

    #[must_use]
    pub fn as_secs(self) -> u64 {
        self.0.get()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseAcquirePolicy {
    ttl: LeaseDuration,
}

impl LeaseAcquirePolicy {
    #[must_use]
    pub fn new(ttl: LeaseDuration) -> Self {
        Self { ttl }
    }

    #[must_use]
    pub fn ttl(&self) -> LeaseDuration {
        self.ttl
    }
}

#[derive(Debug, Error)]
pub enum LeasePolicyError {
    #[error("lease TTL must be greater than zero")]
    ZeroTtl,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeaseContentHash([u8; 32]);

impl Display for LeaseContentHash {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl LeaseContentHash {
    #[must_use]
    pub fn as_hex(self) -> String {
        self.to_string()
    }

    pub fn from_hex(value: &str) -> Result<Self, LeaseContentHashParseError> {
        if value.len() != 64 {
            return Err(LeaseContentHashParseError::InvalidLength {
                expected: 64,
                actual: value.len(),
            });
        }
        let mut bytes = [0_u8; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = decode_hex_nibble(chunk[0])
                .ok_or(LeaseContentHashParseError::InvalidHex { index: index * 2 })?;
            let low =
                decode_hex_nibble(chunk[1]).ok_or(LeaseContentHashParseError::InvalidHex {
                    index: index * 2 + 1,
                })?;
            bytes[index] = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LeaseContentHashParseError {
    #[error("lease content hash must be {expected} hex characters, got {actual}")]
    InvalidLength { expected: usize, actual: usize },
    #[error("lease content hash contains invalid hex at byte index {index}")]
    InvalidHex { index: usize },
}

fn decode_hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct LeaseBookId(u64);

static NEXT_LEASE_BOOK_ID: AtomicU64 = AtomicU64::new(1);

fn next_lease_book_id() -> LeaseBookId {
    LeaseBookId(NEXT_LEASE_BOOK_ID.fetch_add(1, Ordering::Relaxed))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseFact {
    Claimed(LeaseClaimed),
    Renewed(LeaseRenewed),
    Released(LeaseReleased),
}

impl LeaseFact {
    #[must_use]
    pub fn resource(&self) -> &LeaseResource {
        match self {
            Self::Claimed(fact) => &fact.resource,
            Self::Renewed(fact) => &fact.resource,
            Self::Released(fact) => &fact.resource,
        }
    }

    #[must_use]
    pub fn epoch(&self) -> LeaseEpoch {
        match self {
            Self::Claimed(fact) => fact.epoch,
            Self::Renewed(fact) => fact.epoch,
            Self::Released(fact) => fact.epoch,
        }
    }

    #[must_use]
    pub fn holder(&self) -> &LeaseHolder {
        match self {
            Self::Claimed(fact) => &fact.holder,
            Self::Renewed(fact) => &fact.holder,
            Self::Released(fact) => &fact.holder,
        }
    }

    #[must_use]
    pub fn content_hash(&self) -> LeaseContentHash {
        match self {
            Self::Claimed(fact) => claimed_content_hash(fact),
            Self::Renewed(fact) => renewed_content_hash(fact),
            Self::Released(fact) => released_content_hash(fact),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseClaimed {
    pub resource: LeaseResource,
    pub holder: LeaseHolder,
    pub epoch: LeaseEpoch,
    pub acquired_at: LeaseTimestamp,
    pub expires_at: LeaseTimestamp,
}

impl LeaseClaimed {
    #[must_use]
    pub fn new(
        resource: LeaseResource,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        acquired_at: LeaseTimestamp,
        expires_at: LeaseTimestamp,
    ) -> Self {
        Self {
            resource,
            holder,
            epoch,
            acquired_at,
            expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRenewed {
    pub resource: LeaseResource,
    pub holder: LeaseHolder,
    pub epoch: LeaseEpoch,
    pub claim_hash: LeaseContentHash,
    pub renewed_at: LeaseTimestamp,
    pub expires_at: LeaseTimestamp,
}

impl LeaseRenewed {
    #[must_use]
    pub fn new(
        resource: LeaseResource,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        renewed_at: LeaseTimestamp,
        expires_at: LeaseTimestamp,
    ) -> Self {
        Self {
            resource,
            holder,
            epoch,
            claim_hash,
            renewed_at,
            expires_at,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseReleased {
    pub resource: LeaseResource,
    pub holder: LeaseHolder,
    pub epoch: LeaseEpoch,
    pub claim_hash: LeaseContentHash,
    pub release: LeaseRelease,
}

impl LeaseReleased {
    #[must_use]
    pub fn new_at(
        resource: LeaseResource,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        released_at: LeaseTimestamp,
    ) -> Self {
        Self {
            resource,
            holder,
            epoch,
            claim_hash,
            release: LeaseRelease::At(released_at),
        }
    }

    #[must_use]
    pub fn new_dropped(
        resource: LeaseResource,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
    ) -> Self {
        Self {
            resource,
            holder,
            epoch,
            claim_hash,
            release: LeaseRelease::DroppedWithoutTimestamp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LeaseRelease {
    At(LeaseTimestamp),
    DroppedWithoutTimestamp,
}

#[derive(Debug)]
pub struct LeaseGuard {
    book_id: LeaseBookId,
    resource: LeaseResource,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
    release_on_drop: Option<DropRelease>,
}

impl LeaseGuard {
    fn new(
        book_id: LeaseBookId,
        resource: LeaseResource,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        claim_hash: LeaseContentHash,
        release_on_drop: DropRelease,
    ) -> Self {
        Self {
            book_id,
            resource,
            holder,
            epoch,
            claim_hash,
            release_on_drop: Some(release_on_drop),
        }
    }

    #[must_use]
    pub fn resource(&self) -> &LeaseResource {
        &self.resource
    }

    #[must_use]
    pub fn holder(&self) -> &LeaseHolder {
        &self.holder
    }

    #[must_use]
    pub fn epoch(&self) -> LeaseEpoch {
        self.epoch
    }

    #[must_use]
    pub fn claim_hash(&self) -> LeaseContentHash {
        self.claim_hash
    }

    fn disarm_drop_release(&mut self) {
        self.release_on_drop = None;
    }
}

impl Drop for LeaseGuard {
    fn drop(&mut self) {
        let Some(release) = self.release_on_drop.take() else {
            return;
        };
        let Some(inner) = release.inner.upgrade() else {
            return;
        };
        inner
            .borrow_mut()
            .push_fact(LeaseFact::Released(LeaseReleased::new_dropped(
                self.resource.clone(),
                self.holder.clone(),
                self.epoch,
                self.claim_hash,
            )));
    }
}

#[derive(Debug)]
struct DropRelease {
    inner: Weak<RefCell<LeaseInner>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeaseState {
    Vacant {
        resource: LeaseResource,
        next_epoch: LeaseEpoch,
    },
    Active {
        current: LeaseCurrent,
        superseded: Vec<LeaseSuperseded>,
    },
    Expired {
        previous: LeaseCurrent,
        expired_at: LeaseTimestamp,
        next_epoch: Option<LeaseEpoch>,
        superseded: Vec<LeaseSuperseded>,
    },
    Released {
        previous: LeaseCurrent,
        release: LeaseRelease,
        next_epoch: Option<LeaseEpoch>,
        superseded: Vec<LeaseSuperseded>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseCurrent {
    pub resource: LeaseResource,
    pub holder: LeaseHolder,
    pub epoch: LeaseEpoch,
    pub acquired_at: LeaseTimestamp,
    pub expires_at: LeaseTimestamp,
    pub content_hash: LeaseContentHash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseSuperseded {
    pub resource: LeaseResource,
    pub holder: LeaseHolder,
    pub epoch: LeaseEpoch,
    pub content_hash: LeaseContentHash,
    pub by_epoch: LeaseEpoch,
    pub by_holder: LeaseHolder,
    pub by_content_hash: LeaseContentHash,
    pub at: LeaseTimestamp,
}

#[derive(Debug)]
pub struct LeaseAcquired {
    guard: LeaseGuard,
    visible_nodes: VisibleNodes,
}

impl LeaseAcquired {
    #[must_use]
    pub fn into_parts(self) -> (LeaseGuard, VisibleNodes) {
        (self.guard, self.visible_nodes)
    }

    #[must_use]
    pub fn into_guard(self) -> LeaseGuard {
        self.guard
    }

    #[must_use]
    pub fn guard(&self) -> &LeaseGuard {
        &self.guard
    }

    #[must_use]
    pub fn visible_nodes(&self) -> &VisibleNodes {
        &self.visible_nodes
    }
}

#[derive(Debug)]
pub enum LeaseDecision {
    Acquired(LeaseAcquired),
    Conflict(LeaseConflict),
}

impl LeaseDecision {
    pub fn into_acquired(self) -> Result<LeaseAcquired, LeaseError> {
        match self {
            Self::Acquired(acquired) => Ok(acquired),
            Self::Conflict(conflict) => Err(LeaseError::Conflict(conflict)),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[error(
    "lease {resource} conflicts with fact {conflicting_fact} held by {conflicting_holder} at epoch {conflicting_epoch} observed at {observed_at}"
)]
pub struct LeaseConflict {
    pub resource: LeaseResource,
    pub conflicting_holder: LeaseHolder,
    pub conflicting_epoch: LeaseEpoch,
    pub conflicting_fact: LeaseContentHash,
    pub observed_at: LeaseTimestamp,
    pub visible_nodes: VisibleNodes,
}

#[derive(Debug, Error)]
pub enum LeaseError {
    #[error(transparent)]
    Conflict(#[from] LeaseConflict),
    #[error("lease guard for {resource} epoch {epoch} belongs to another lease book")]
    ForeignGuard {
        resource: LeaseResource,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
    },
    #[error("lease guard for {resource} epoch {epoch} is stale")]
    StaleGuard {
        resource: LeaseResource,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
    },
    #[error(
        "lease guard for {resource} epoch {epoch} was superseded by {by_holder} at epoch {by_epoch}"
    )]
    Superseded {
        resource: LeaseResource,
        holder: LeaseHolder,
        epoch: LeaseEpoch,
        by_holder: LeaseHolder,
        by_epoch: LeaseEpoch,
    },
    #[error(
        "lease {resource} exhausted monotonic epochs at {last_epoch} observed at {observed_at}"
    )]
    EpochOverflow {
        resource: LeaseResource,
        last_epoch: LeaseEpoch,
        observed_at: LeaseTimestamp,
    },
}

#[derive(Debug, Clone)]
pub struct LeaseBook {
    inner: Rc<RefCell<LeaseInner>>,
}

#[derive(Debug)]
struct LeaseInner {
    id: LeaseBookId,
    facts_by_resource: BTreeMap<LeaseResource, Vec<LeaseFact>>,
    fact_count: usize,
}

impl LeaseInner {
    fn new() -> Self {
        Self {
            id: next_lease_book_id(),
            facts_by_resource: BTreeMap::new(),
            fact_count: 0,
        }
    }

    fn push_fact(&mut self, fact: LeaseFact) {
        self.facts_by_resource
            .entry(fact.resource().clone())
            .or_default()
            .push(fact);
        self.fact_count += 1;
    }
}

impl LeaseBook {
    #[expect(
        clippy::new_without_default,
        reason = "LeaseBook owns non-trivial lease state; use LeaseBook::new() explicitly"
    )]
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Rc::new(RefCell::new(LeaseInner::new())),
        }
    }

    #[cfg(any(test, feature = "harness"))]
    #[must_use]
    pub fn fact_count(&self) -> usize {
        self.inner.borrow().fact_count
    }

    #[cfg(any(test, feature = "harness"))]
    #[must_use]
    pub fn importer(&self) -> LeaseFactImporter<'_> {
        LeaseFactImporter { book: self }
    }

    pub fn record_observed_fact(&self, fact: LeaseFact) {
        self.push_fact(fact);
    }

    fn id(&self) -> LeaseBookId {
        self.inner.borrow().id
    }

    fn push_fact(&self, fact: LeaseFact) {
        self.inner.borrow_mut().push_fact(fact);
    }

    #[must_use]
    pub fn state(&self, resource: &LeaseResource, now: LeaseTimestamp) -> LeaseState {
        let inner = self.inner.borrow();
        let facts = inner
            .facts_by_resource
            .get(resource)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        reduce_lease_state(facts, resource, now)
    }

    pub fn try_acquire(
        &self,
        resource: LeaseResource,
        holder: LeaseHolder,
        now: LeaseTimestamp,
        policy: &LeaseAcquirePolicy,
        context: LeaseCommandContext,
    ) -> Result<LeaseDecision, LeaseError> {
        let state = self.state(&resource, now);
        let next_epoch = match state {
            LeaseState::Vacant { next_epoch, .. } => next_epoch,
            LeaseState::Expired {
                next_epoch: Some(next_epoch),
                ..
            }
            | LeaseState::Released {
                next_epoch: Some(next_epoch),
                ..
            } => next_epoch,
            LeaseState::Expired {
                previous,
                next_epoch: None,
                ..
            }
            | LeaseState::Released {
                previous,
                next_epoch: None,
                ..
            } => {
                return Err(LeaseError::EpochOverflow {
                    resource,
                    last_epoch: previous.epoch,
                    observed_at: now,
                });
            }
            LeaseState::Active { current, .. } => {
                return Ok(LeaseDecision::Conflict(LeaseConflict {
                    resource,
                    conflicting_holder: current.holder.clone(),
                    conflicting_epoch: current.epoch,
                    conflicting_fact: current.content_hash,
                    observed_at: now,
                    visible_nodes: context.visible_nodes,
                }));
            }
        };
        let expires_at = now.checked_add(policy.ttl());
        let claim = LeaseClaimed::new(
            resource.clone(),
            holder.clone(),
            next_epoch,
            now,
            expires_at,
        );
        let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
        self.push_fact(LeaseFact::Claimed(claim));
        Ok(LeaseDecision::Acquired(LeaseAcquired {
            guard: LeaseGuard::new(
                self.id(),
                resource,
                holder,
                next_epoch,
                claim_hash,
                DropRelease {
                    inner: Rc::downgrade(&self.inner),
                },
            ),
            visible_nodes: context.visible_nodes,
        }))
    }

    pub fn renew(
        &self,
        guard: &LeaseGuard,
        now: LeaseTimestamp,
        policy: &LeaseAcquirePolicy,
    ) -> Result<(), LeaseError> {
        self.assert_current(guard, now)?;
        self.push_fact(LeaseFact::Renewed(LeaseRenewed::new(
            guard.resource.clone(),
            guard.holder.clone(),
            guard.epoch,
            guard.claim_hash,
            now,
            now.checked_add(policy.ttl()),
        )));
        Ok(())
    }

    pub fn release(&self, guard: &mut LeaseGuard, now: LeaseTimestamp) -> Result<(), LeaseError> {
        self.assert_current(guard, now)?;
        self.push_fact(LeaseFact::Released(LeaseReleased::new_at(
            guard.resource.clone(),
            guard.holder.clone(),
            guard.epoch,
            guard.claim_hash,
            now,
        )));
        guard.disarm_drop_release();
        Ok(())
    }

    pub fn assert_current(
        &self,
        guard: &LeaseGuard,
        now: LeaseTimestamp,
    ) -> Result<(), LeaseError> {
        if guard.book_id != self.id() {
            return Err(LeaseError::ForeignGuard {
                resource: guard.resource.clone(),
                holder: guard.holder.clone(),
                epoch: guard.epoch,
            });
        }
        match self.state(&guard.resource, now) {
            LeaseState::Active { current, .. }
                if current.holder == guard.holder
                    && current.epoch == guard.epoch
                    && current.content_hash == guard.claim_hash =>
            {
                Ok(())
            }
            LeaseState::Active { current, .. } => Err(LeaseError::Superseded {
                resource: guard.resource.clone(),
                holder: guard.holder.clone(),
                epoch: guard.epoch,
                by_holder: current.holder.clone(),
                by_epoch: current.epoch,
            }),
            LeaseState::Vacant { .. }
            | LeaseState::Expired { .. }
            | LeaseState::Released { .. } => Err(LeaseError::StaleGuard {
                resource: guard.resource.clone(),
                holder: guard.holder.clone(),
                epoch: guard.epoch,
            }),
        }
    }
}

#[cfg(any(test, feature = "harness"))]
pub struct LeaseFactImporter<'a> {
    book: &'a LeaseBook,
}

#[cfg(any(test, feature = "harness"))]
impl LeaseFactImporter<'_> {
    pub fn record(&self, fact: LeaseFact) {
        self.book.push_fact(fact);
    }
}

mod ledger;

use ledger::{
    claimed_content_hash, reduce_lease_state, released_content_hash, renewed_content_hash,
};

fn encode_resource_segment_into(segment: &str, encoded: &mut String) {
    for byte in segment.bytes() {
        match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('~');
                encoded.push(hex_digit(byte >> 4));
                encoded.push(hex_digit(byte & 0x0f));
            }
        }
    }
}

fn hex_digit(value: u8) -> char {
    match value {
        0..=9 => (b'0' + value) as char,
        10..=15 => (b'A' + value - 10) as char,
        _ => unreachable!("hex digit caller masks to four bits"),
    }
}

pub mod harness {
    use super::{NodeId, VisibleNodes};

    #[must_use]
    pub fn visible_nodes<const N: usize>(nodes: [&str; N]) -> VisibleNodes {
        VisibleNodes::new(nodes.into_iter().map(NodeId::new))
    }
}

#[cfg(test)]
mod tests;
