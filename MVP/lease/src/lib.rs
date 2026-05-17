use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Display, Formatter};
use std::num::NonZeroU64;
use std::rc::{Rc, Weak};
use std::sync::atomic::{AtomicU64, Ordering};

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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct VisibleNode(String);

impl VisibleNode {
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for VisibleNode {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseCommandContext {
    visible_nodes: BTreeSet<VisibleNode>,
}

impl LeaseCommandContext {
    #[must_use]
    pub fn new(visible_nodes: BTreeSet<VisibleNode>) -> Self {
        Self { visible_nodes }
    }

    #[must_use]
    pub fn visible_nodes(&self) -> &BTreeSet<VisibleNode> {
        &self.visible_nodes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeaseEpoch(NonZeroU64);

impl LeaseEpoch {
    #[must_use]
    pub fn first() -> Self {
        Self(NonZeroU64::MIN)
    }

    pub fn from_u64(value: u64) -> Result<Self, LeaseEpochError> {
        let Some(value) = NonZeroU64::new(value) else {
            return Err(LeaseEpochError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub fn next(self) -> Self {
        Self(NonZeroU64::new(self.0.get().saturating_add(1)).expect("nonzero saturated epoch"))
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

#[derive(Debug, Error, PartialEq, Eq)]
pub enum LeaseEpochError {
    #[error("lease epoch must be greater than zero")]
    Zero,
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
            Self::Claimed(fact) => fact.resource(),
            Self::Renewed(fact) => fact.resource(),
            Self::Released(fact) => fact.resource(),
        }
    }

    #[must_use]
    pub fn epoch(&self) -> LeaseEpoch {
        match self {
            Self::Claimed(fact) => fact.epoch(),
            Self::Renewed(fact) => fact.epoch(),
            Self::Released(fact) => fact.epoch(),
        }
    }

    #[must_use]
    pub fn holder(&self) -> &LeaseHolder {
        match self {
            Self::Claimed(fact) => fact.holder(),
            Self::Renewed(fact) => fact.holder(),
            Self::Released(fact) => fact.holder(),
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
    resource: LeaseResource,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    acquired_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
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
    pub fn acquired_at(&self) -> LeaseTimestamp {
        self.acquired_at
    }

    #[must_use]
    pub fn expires_at(&self) -> LeaseTimestamp {
        self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRenewed {
    resource: LeaseResource,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
    renewed_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
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

    #[must_use]
    pub fn renewed_at(&self) -> LeaseTimestamp {
        self.renewed_at
    }

    #[must_use]
    pub fn expires_at(&self) -> LeaseTimestamp {
        self.expires_at
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseReleased {
    resource: LeaseResource,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
    release: LeaseRelease,
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

    #[must_use]
    pub fn release(&self) -> LeaseRelease {
        self.release
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
        next_epoch: LeaseEpoch,
        superseded: Vec<LeaseSuperseded>,
    },
    Released {
        previous: LeaseCurrent,
        release: LeaseRelease,
        next_epoch: LeaseEpoch,
        superseded: Vec<LeaseSuperseded>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseCurrent {
    resource: LeaseResource,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    acquired_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
    content_hash: LeaseContentHash,
}

impl LeaseCurrent {
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
    pub fn acquired_at(&self) -> LeaseTimestamp {
        self.acquired_at
    }

    #[must_use]
    pub fn expires_at(&self) -> LeaseTimestamp {
        self.expires_at
    }

    #[must_use]
    pub fn content_hash(&self) -> LeaseContentHash {
        self.content_hash
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseSuperseded {
    resource: LeaseResource,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    content_hash: LeaseContentHash,
    by_epoch: LeaseEpoch,
    by_holder: LeaseHolder,
    by_content_hash: LeaseContentHash,
    at: LeaseTimestamp,
}

impl LeaseSuperseded {
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
    pub fn content_hash(&self) -> LeaseContentHash {
        self.content_hash
    }

    #[must_use]
    pub fn by_epoch(&self) -> LeaseEpoch {
        self.by_epoch
    }

    #[must_use]
    pub fn by_holder(&self) -> &LeaseHolder {
        &self.by_holder
    }

    #[must_use]
    pub fn by_content_hash(&self) -> LeaseContentHash {
        self.by_content_hash
    }

    #[must_use]
    pub fn at(&self) -> LeaseTimestamp {
        self.at
    }
}

#[derive(Debug)]
pub struct LeaseAcquired {
    guard: LeaseGuard,
    visible_nodes: BTreeSet<VisibleNode>,
}

impl LeaseAcquired {
    #[must_use]
    pub fn into_parts(self) -> (LeaseGuard, BTreeSet<VisibleNode>) {
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
    pub fn visible_nodes(&self) -> &BTreeSet<VisibleNode> {
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
    resource: LeaseResource,
    conflicting_holder: LeaseHolder,
    conflicting_epoch: LeaseEpoch,
    conflicting_fact: LeaseContentHash,
    observed_at: LeaseTimestamp,
    visible_nodes: BTreeSet<VisibleNode>,
}

impl LeaseConflict {
    #[must_use]
    pub fn resource(&self) -> &LeaseResource {
        &self.resource
    }

    #[must_use]
    pub fn conflicting_holder(&self) -> &LeaseHolder {
        &self.conflicting_holder
    }

    #[must_use]
    pub fn conflicting_epoch(&self) -> LeaseEpoch {
        self.conflicting_epoch
    }

    #[must_use]
    pub fn conflicting_fact(&self) -> LeaseContentHash {
        self.conflicting_fact
    }

    #[must_use]
    pub fn observed_at(&self) -> LeaseTimestamp {
        self.observed_at
    }

    #[must_use]
    pub fn visible_nodes(&self) -> &BTreeSet<VisibleNode> {
        &self.visible_nodes
    }
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
            LeaseState::Vacant { next_epoch, .. }
            | LeaseState::Expired { next_epoch, .. }
            | LeaseState::Released { next_epoch, .. } => next_epoch,
            LeaseState::Active { current, .. } => {
                return Ok(LeaseDecision::Conflict(LeaseConflict {
                    resource,
                    conflicting_holder: current.holder().clone(),
                    conflicting_epoch: current.epoch(),
                    conflicting_fact: current.content_hash(),
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
            guard.resource().clone(),
            guard.holder().clone(),
            guard.epoch(),
            guard.claim_hash(),
            now,
            now.checked_add(policy.ttl()),
        )));
        Ok(())
    }

    pub fn release(&self, guard: &mut LeaseGuard, now: LeaseTimestamp) -> Result<(), LeaseError> {
        self.assert_current(guard, now)?;
        self.push_fact(LeaseFact::Released(LeaseReleased::new_at(
            guard.resource().clone(),
            guard.holder().clone(),
            guard.epoch(),
            guard.claim_hash(),
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
                resource: guard.resource().clone(),
                holder: guard.holder().clone(),
                epoch: guard.epoch(),
            });
        }
        match self.state(guard.resource(), now) {
            LeaseState::Active { current, .. }
                if current.holder() == guard.holder()
                    && current.epoch() == guard.epoch()
                    && current.content_hash() == guard.claim_hash() =>
            {
                Ok(())
            }
            LeaseState::Active { current, .. } => Err(LeaseError::Superseded {
                resource: guard.resource().clone(),
                holder: guard.holder().clone(),
                epoch: guard.epoch(),
                by_holder: current.holder().clone(),
                by_epoch: current.epoch(),
            }),
            LeaseState::Vacant { .. }
            | LeaseState::Expired { .. }
            | LeaseState::Released { .. } => Err(LeaseError::StaleGuard {
                resource: guard.resource().clone(),
                holder: guard.holder().clone(),
                epoch: guard.epoch(),
            }),
        }
    }
}

impl Default for LeaseBook {
    fn default() -> Self {
        Self::new()
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

fn reduce_lease_state(
    facts: &[LeaseFact],
    resource: &LeaseResource,
    now: LeaseTimestamp,
) -> LeaseState {
    let mut highest_epoch = None;
    let mut candidates = Vec::new();
    for fact in facts {
        let LeaseFact::Claimed(claim) = fact else {
            continue;
        };
        if claim.resource() != resource {
            continue;
        }
        match highest_epoch {
            Some(epoch) if claim.epoch() < epoch => {}
            Some(epoch) if claim.epoch() == epoch => {
                candidates.push(LeaseCandidate::from_claim(claim));
            }
            Some(_) | None => {
                highest_epoch = Some(claim.epoch());
                candidates.clear();
                candidates.push(LeaseCandidate::from_claim(claim));
            }
        }
    }
    let Some(highest_epoch) = highest_epoch else {
        return LeaseState::Vacant {
            resource: resource.clone(),
            next_epoch: LeaseEpoch::first(),
        };
    };
    candidates.sort_by_key(|candidate| candidate.content_hash);

    let [winner, losers @ ..] = candidates.as_slice() else {
        return LeaseState::Vacant {
            resource: resource.clone(),
            next_epoch: LeaseEpoch::first(),
        };
    };

    let expires_at = latest_expiry(facts, winner);
    let current = LeaseCurrent {
        resource: winner.resource.clone(),
        holder: winner.holder.clone(),
        epoch: winner.epoch,
        acquired_at: winner.acquired_at,
        expires_at,
        content_hash: winner.content_hash,
    };
    let superseded = losers
        .iter()
        .map(|loser| LeaseSuperseded {
            resource: loser.resource.clone(),
            holder: loser.holder.clone(),
            epoch: loser.epoch,
            content_hash: loser.content_hash,
            by_epoch: winner.epoch,
            by_holder: winner.holder.clone(),
            by_content_hash: winner.content_hash,
            at: winner.acquired_at,
        })
        .collect::<Vec<_>>();

    if let Some(release) = latest_release(facts, winner, expires_at, now) {
        return LeaseState::Released {
            previous: current,
            release,
            next_epoch: highest_epoch.next(),
            superseded,
        };
    }

    if expires_at <= now {
        return LeaseState::Expired {
            previous: current,
            expired_at: expires_at,
            next_epoch: highest_epoch.next(),
            superseded,
        };
    }

    LeaseState::Active {
        current,
        superseded,
    }
}

#[derive(Debug, Clone)]
struct LeaseCandidate {
    resource: LeaseResource,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    acquired_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
    content_hash: LeaseContentHash,
}

impl LeaseCandidate {
    fn from_claim(claim: &LeaseClaimed) -> Self {
        Self {
            resource: claim.resource().clone(),
            holder: claim.holder().clone(),
            epoch: claim.epoch(),
            acquired_at: claim.acquired_at(),
            expires_at: claim.expires_at(),
            content_hash: claimed_content_hash(claim),
        }
    }
}

fn latest_release(
    facts: &[LeaseFact],
    candidate: &LeaseCandidate,
    expires_at: LeaseTimestamp,
    now: LeaseTimestamp,
) -> Option<LeaseRelease> {
    facts
        .iter()
        .filter_map(|fact| match fact {
            LeaseFact::Released(released)
                if released.resource() == &candidate.resource
                    && released.holder() == &candidate.holder
                    && released.epoch() == candidate.epoch
                    && released.claim_hash() == candidate.content_hash =>
            {
                release_if_observable(released.release(), candidate.acquired_at, expires_at, now)
            }
            LeaseFact::Claimed(_) | LeaseFact::Renewed(_) | LeaseFact::Released(_) => None,
        })
        .max_by_key(|release| release_order(*release))
}

fn latest_expiry(facts: &[LeaseFact], candidate: &LeaseCandidate) -> LeaseTimestamp {
    let mut renewals = facts
        .iter()
        .filter_map(|fact| match fact {
            LeaseFact::Renewed(renewed)
                if renewed.resource() == &candidate.resource
                    && renewed.holder() == &candidate.holder
                    && renewed.epoch() == candidate.epoch
                    && renewed.claim_hash() == candidate.content_hash =>
            {
                Some((
                    renewed.renewed_at(),
                    renewed_content_hash(renewed),
                    renewed.expires_at(),
                ))
            }
            LeaseFact::Claimed(_) | LeaseFact::Renewed(_) | LeaseFact::Released(_) => None,
        })
        .collect::<Vec<_>>();
    renewals.sort_by_key(|(renewed_at, content_hash, _expires_at)| (*renewed_at, *content_hash));

    let mut expires_at = candidate.expires_at;
    for (renewed_at, _content_hash, renewed_expires_at) in renewals {
        if renewed_at >= candidate.acquired_at
            && renewed_at < expires_at
            && renewed_expires_at > renewed_at
        {
            expires_at = renewed_expires_at;
        }
    }
    expires_at
}

fn release_if_observable(
    release: LeaseRelease,
    acquired_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
    now: LeaseTimestamp,
) -> Option<LeaseRelease> {
    match release {
        LeaseRelease::At(released_at)
            if released_at >= acquired_at && released_at < expires_at && released_at <= now =>
        {
            Some(release)
        }
        LeaseRelease::DroppedWithoutTimestamp => Some(release),
        LeaseRelease::At(_) => None,
    }
}

fn release_order(release: LeaseRelease) -> u64 {
    match release {
        LeaseRelease::At(timestamp) => timestamp.value(),
        LeaseRelease::DroppedWithoutTimestamp => 0,
    }
}

fn hash_str(hasher: &mut blake3::Hasher, value: &str) {
    hash_u64(hasher, value.len() as u64);
    hasher.update(value.as_bytes());
}

fn hash_u64(hasher: &mut blake3::Hasher, value: u64) {
    hasher.update(&value.to_be_bytes());
}

fn claimed_content_hash(fact: &LeaseClaimed) -> LeaseContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lease-claimed");
    hash_str(&mut hasher, fact.resource().as_str());
    hash_str(&mut hasher, fact.holder().as_str());
    hash_u64(&mut hasher, fact.epoch().value());
    hash_u64(&mut hasher, fact.acquired_at().value());
    hash_u64(&mut hasher, fact.expires_at().value());
    LeaseContentHash(*hasher.finalize().as_bytes())
}

fn renewed_content_hash(fact: &LeaseRenewed) -> LeaseContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lease-renewed");
    hash_str(&mut hasher, fact.resource().as_str());
    hash_str(&mut hasher, fact.holder().as_str());
    hash_u64(&mut hasher, fact.epoch().value());
    hash_content_hash(&mut hasher, fact.claim_hash());
    hash_u64(&mut hasher, fact.renewed_at().value());
    hash_u64(&mut hasher, fact.expires_at().value());
    LeaseContentHash(*hasher.finalize().as_bytes())
}

fn released_content_hash(fact: &LeaseReleased) -> LeaseContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"lease-released");
    hash_str(&mut hasher, fact.resource().as_str());
    hash_str(&mut hasher, fact.holder().as_str());
    hash_u64(&mut hasher, fact.epoch().value());
    hash_content_hash(&mut hasher, fact.claim_hash());
    hash_release(&mut hasher, fact.release());
    LeaseContentHash(*hasher.finalize().as_bytes())
}

fn hash_content_hash(hasher: &mut blake3::Hasher, value: LeaseContentHash) {
    hasher.update(&value.0);
}

fn hash_release(hasher: &mut blake3::Hasher, release: LeaseRelease) {
    match release {
        LeaseRelease::At(timestamp) => {
            hasher.update(b"at");
            hash_u64(hasher, timestamp.value());
        }
        LeaseRelease::DroppedWithoutTimestamp => {
            hasher.update(b"dropped-without-timestamp");
        }
    }
}

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
    use std::collections::BTreeSet;

    use super::VisibleNode;

    #[must_use]
    pub fn visible_nodes<const N: usize>(nodes: [&str; N]) -> BTreeSet<VisibleNode> {
        nodes.into_iter().map(VisibleNode::new).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness::visible_nodes;

    fn resource() -> LeaseResource {
        LeaseResource::from_segments(["acme", "http01", "example.com", "token-a"])
    }

    fn holder(value: &str) -> LeaseHolder {
        LeaseHolder::new(value)
    }

    fn at(value: u64) -> LeaseTimestamp {
        LeaseTimestamp::from_secs(value)
    }

    fn policy() -> LeaseAcquirePolicy {
        LeaseAcquirePolicy::new(LeaseDuration::from_secs(10).expect("ttl"))
    }

    fn context() -> LeaseCommandContext {
        LeaseCommandContext::new(visible_nodes(["node-a", "node-b"]))
    }

    fn local_context() -> LeaseCommandContext {
        LeaseCommandContext::new(BTreeSet::new())
    }

    fn second_epoch() -> LeaseEpoch {
        LeaseEpoch::from_u64(2).expect("second epoch")
    }

    fn claim(
        holder: LeaseHolder,
        acquired_at: LeaseTimestamp,
        expires_at: LeaseTimestamp,
    ) -> LeaseClaimed {
        LeaseClaimed::new(
            resource(),
            holder,
            LeaseEpoch::first(),
            acquired_at,
            expires_at,
        )
    }

    fn claim_hash(claim: &LeaseClaimed) -> LeaseContentHash {
        LeaseFact::Claimed(claim.clone()).content_hash()
    }

    #[test]
    fn first_local_claim_becomes_active_and_reports_visible_nodes() {
        let book = LeaseBook::new();
        let decision = book
            .try_acquire(
                resource(),
                holder("issuer-a"),
                at(100),
                &policy(),
                context(),
            )
            .expect("acquire succeeds");

        let LeaseDecision::Acquired(acquired) = decision else {
            panic!("expected acquired");
        };
        assert_eq!(acquired.guard().epoch(), LeaseEpoch::first());
        assert_eq!(acquired.visible_nodes().len(), 2);
        assert!(matches!(
            book.state(&resource(), at(101)),
            LeaseState::Active { .. }
        ));
    }

    #[test]
    fn local_claim_does_not_require_visible_peer_witnesses() {
        let book = LeaseBook::new();
        let decision = book
            .try_acquire(
                resource(),
                holder("issuer-a"),
                at(100),
                &policy(),
                local_context(),
            )
            .expect("local acquire succeeds");

        let LeaseDecision::Acquired(acquired) = decision else {
            panic!("expected acquired");
        };
        assert!(acquired.visible_nodes().is_empty());
        assert_eq!(book.fact_count(), 1);
    }

    #[test]
    fn active_claim_returns_conflict_before_mutation() {
        let book = LeaseBook::new();
        let first = book
            .try_acquire(
                resource(),
                holder("issuer-a"),
                at(100),
                &policy(),
                context(),
            )
            .expect("first acquire");
        let before_conflict = book.fact_count();
        let second = book
            .try_acquire(
                resource(),
                holder("issuer-b"),
                at(101),
                &policy(),
                context(),
            )
            .expect("contention is a decision");

        assert!(matches!(first, LeaseDecision::Acquired(_)));
        assert!(matches!(
            second,
            LeaseDecision::Conflict(conflict)
                if conflict.conflicting_holder() == &holder("issuer-a")
                    && conflict.visible_nodes().len() == 2
        ));
        assert_eq!(book.fact_count(), before_conflict);
        assert!(matches!(
            book.state(&resource(), at(101)),
            LeaseState::Active { current, superseded }
                if current.holder() == &holder("issuer-a") && superseded.is_empty()
        ));
    }

    #[test]
    fn renewal_by_current_holder_extends_expiry() {
        let book = LeaseBook::new();
        let acquired = book
            .try_acquire(
                resource(),
                holder("issuer-a"),
                at(100),
                &policy(),
                context(),
            )
            .expect("acquire")
            .into_acquired()
            .expect("acquired");
        let guard = acquired.guard();

        book.renew(guard, at(105), &policy()).expect("renew");

        assert!(matches!(
            book.state(&resource(), at(114)),
            LeaseState::Active {
                current,
                ..
            } if current.expires_at() == at(115)
        ));
    }

    #[test]
    fn stale_holder_cannot_renew() {
        let book = LeaseBook::new();
        let stale = book
            .try_acquire(
                resource(),
                holder("issuer-a"),
                at(100),
                &policy(),
                context(),
            )
            .expect("first acquire")
            .into_acquired()
            .expect("acquired")
            .into_guard();
        drop(stale);
        let newer = book
            .try_acquire(
                resource(),
                holder("issuer-b"),
                at(101),
                &policy(),
                context(),
            )
            .expect("second acquire")
            .into_acquired()
            .expect("acquired");

        let error = book
            .renew(newer.guard(), at(112), &policy())
            .expect_err("expired guard cannot renew");

        assert!(matches!(error, LeaseError::StaleGuard { .. }));
    }

    #[test]
    fn guard_from_another_book_cannot_mutate_matching_state() {
        let original_book = LeaseBook::new();
        let foreign_guard = original_book
            .try_acquire(
                resource(),
                holder("issuer-a"),
                at(100),
                &policy(),
                context(),
            )
            .expect("acquire")
            .into_acquired()
            .expect("acquired")
            .into_guard();
        let other_book = LeaseBook::new();
        other_book.importer().record(LeaseFact::Claimed(claim(
            holder("issuer-a"),
            at(100),
            at(110),
        )));

        let error = other_book
            .renew(&foreign_guard, at(101), &policy())
            .expect_err("foreign guard cannot renew");

        assert!(matches!(error, LeaseError::ForeignGuard { .. }));
    }

    #[test]
    fn explicit_release_ends_ownership() {
        let book = LeaseBook::new();
        let mut guard = book
            .try_acquire(
                resource(),
                holder("issuer-a"),
                at(100),
                &policy(),
                context(),
            )
            .expect("acquire")
            .into_acquired()
            .expect("acquired")
            .into_guard();

        book.release(&mut guard, at(101)).expect("release");

        assert!(matches!(
            book.state(&resource(), at(102)),
            LeaseState::Released {
                next_epoch,
                ..
            } if next_epoch == second_epoch()
        ));
    }

    #[test]
    fn dropping_local_guard_records_best_effort_release() {
        let book = LeaseBook::new();
        {
            let _guard = book
                .try_acquire(
                    resource(),
                    holder("issuer-a"),
                    at(100),
                    &policy(),
                    context(),
                )
                .expect("acquire")
                .into_acquired()
                .expect("acquired")
                .into_guard();
        }

        assert!(matches!(
            book.state(&resource(), at(101)),
            LeaseState::Released { .. }
        ));
        assert!(book.fact_count() >= 2);
    }

    #[test]
    fn expired_lease_allows_next_holder_with_incremented_epoch() {
        let book = LeaseBook::new();
        let guard = book
            .try_acquire(
                resource(),
                holder("issuer-a"),
                at(100),
                &policy(),
                context(),
            )
            .expect("first acquire")
            .into_acquired()
            .expect("acquired")
            .into_guard();
        std::mem::forget(guard);

        let second = book
            .try_acquire(
                resource(),
                holder("issuer-b"),
                at(111),
                &policy(),
                context(),
            )
            .expect("second acquire");

        assert!(matches!(
            second,
            LeaseDecision::Acquired(acquired)
                if acquired.guard().holder() == &holder("issuer-b")
                    && acquired.guard().epoch() == second_epoch()
        ));
    }

    #[test]
    fn conflicting_same_epoch_claims_reduce_deterministically_with_superseded_loser() {
        let first_claim = claim(holder("issuer-a"), at(100), at(110));
        let second_claim = claim(holder("issuer-b"), at(100), at(110));
        let first_hash = claim_hash(&first_claim);
        let second_hash = claim_hash(&second_claim);
        let winner_hash = first_hash.min(second_hash);
        let loser_hash = first_hash.max(second_hash);

        let forward = active_state_for_claim_order([first_claim.clone(), second_claim.clone()]);
        let reversed = active_state_for_claim_order([second_claim, first_claim]);

        for (current, superseded) in [forward, reversed] {
            assert_eq!(current.epoch(), LeaseEpoch::first());
            assert_eq!(current.content_hash(), winner_hash);
            assert_eq!(superseded.len(), 1);
            assert_eq!(superseded[0].content_hash(), loser_hash);
            assert_eq!(superseded[0].by_content_hash(), winner_hash);
            assert_eq!(superseded[0].by_epoch(), LeaseEpoch::first());
        }
    }

    fn active_state_for_claim_order(
        claims: [LeaseClaimed; 2],
    ) -> (LeaseCurrent, Vec<LeaseSuperseded>) {
        let book = LeaseBook::new();
        for claim in claims {
            book.importer().record(LeaseFact::Claimed(claim));
        }

        let LeaseState::Active {
            current,
            superseded,
        } = book.state(&resource(), at(101))
        else {
            panic!("expected deterministic active state");
        };
        (current, superseded)
    }

    #[test]
    fn renew_for_superseded_same_holder_claim_does_not_extend_winner() {
        let book = LeaseBook::new();
        let first_claim = claim(holder("issuer-a"), at(100), at(110));
        let second_claim = claim(holder("issuer-a"), at(101), at(111));
        let first_hash = LeaseFact::Claimed(first_claim.clone()).content_hash();
        let second_hash = LeaseFact::Claimed(second_claim.clone()).content_hash();
        let winner_hash = first_hash.min(second_hash);
        let loser_hash = first_hash.max(second_hash);
        book.importer().record(LeaseFact::Claimed(first_claim));
        book.importer().record(LeaseFact::Claimed(second_claim));
        book.importer().record(LeaseFact::Renewed(LeaseRenewed::new(
            resource(),
            holder("issuer-a"),
            LeaseEpoch::first(),
            loser_hash,
            at(105),
            at(200),
        )));

        assert!(matches!(
            book.state(&resource(), at(150)),
            LeaseState::Expired { previous, .. } if previous.content_hash() == winner_hash
        ));
    }

    #[test]
    fn imported_stale_renewal_after_expiry_does_not_resurrect_claim() {
        let book = LeaseBook::new();
        let expired_claim = claim(holder("issuer-a"), at(100), at(110));
        let expired_hash = claim_hash(&expired_claim);
        book.importer()
            .record(LeaseFact::Claimed(expired_claim.clone()));
        book.importer().record(LeaseFact::Renewed(LeaseRenewed::new(
            resource(),
            holder("issuer-a"),
            LeaseEpoch::first(),
            expired_hash,
            at(150),
            at(160),
        )));

        assert!(matches!(
            book.state(&resource(), at(120)),
            LeaseState::Expired { previous, .. } if previous.content_hash() == expired_hash
        ));

        let second = book
            .try_acquire(
                resource(),
                holder("issuer-b"),
                at(120),
                &policy(),
                context(),
            )
            .expect("takeover after real expiry");

        assert!(matches!(
            second,
            LeaseDecision::Acquired(acquired)
                if acquired.guard().holder() == &holder("issuer-b")
                    && acquired.guard().epoch() == second_epoch()
        ));
    }

    #[test]
    fn release_for_superseded_same_holder_claim_does_not_release_winner() {
        let book = LeaseBook::new();
        let first_claim = claim(holder("issuer-a"), at(100), at(110));
        let second_claim = claim(holder("issuer-a"), at(101), at(111));
        let first_hash = LeaseFact::Claimed(first_claim.clone()).content_hash();
        let second_hash = LeaseFact::Claimed(second_claim.clone()).content_hash();
        let winner_hash = first_hash.min(second_hash);
        let loser_hash = first_hash.max(second_hash);
        book.importer().record(LeaseFact::Claimed(first_claim));
        book.importer().record(LeaseFact::Claimed(second_claim));
        book.importer()
            .record(LeaseFact::Released(LeaseReleased::new_at(
                resource(),
                holder("issuer-a"),
                LeaseEpoch::first(),
                loser_hash,
                at(102),
            )));

        assert!(matches!(
            book.state(&resource(), at(103)),
            LeaseState::Active { current, .. } if current.content_hash() == winner_hash
        ));
    }

    #[test]
    fn zero_epoch_is_unrepresentable() {
        let error = LeaseEpoch::from_u64(0).expect_err("zero epoch fails");

        assert_eq!(error, LeaseEpochError::Zero);
    }

    #[test]
    fn segmented_resource_encoding_escapes_delimiters() {
        let resource = LeaseResource::from_segments(["acme", "http01", "Example.COM", "a.b~c"]);

        assert_eq!(resource.as_str(), "acme.http01.Example~2ECOM.a~2Eb~7Ec");
    }
}
