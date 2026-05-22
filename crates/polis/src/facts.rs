//! Product-neutral durable fact append and candidate-read primitives.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use crate::authority::{AuthorityContext, Authorized, GrantEpoch};
use crate::claims::ResourceId;
use crate::identity::{PrincipalId, ScopeId};
use crate::operations::{IdempotencyKey, OperationId, SubmittedFenceFingerprint};
use crate::{Error, Result};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactAppendScope {}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactKey(String);

impl FactKey {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactKind(String);

impl FactKind {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        parse_non_empty(value, Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactTarget {
    resource: ResourceId,
    key: FactKey,
    kind: FactKind,
}

impl FactTarget {
    #[must_use]
    pub fn new(resource: ResourceId, key: FactKey, kind: FactKind) -> Self {
        Self {
            resource,
            key,
            kind,
        }
    }

    #[must_use]
    pub fn resource(&self) -> &ResourceId {
        &self.resource
    }

    #[must_use]
    pub fn key(&self) -> &FactKey {
        &self.key
    }

    #[must_use]
    pub fn kind(&self) -> &FactKind {
        &self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactId(Vec<u8>);

impl FactId {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactCursor(u64);

impl FactCursor {
    #[must_use]
    pub fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactPayloadDigest(Vec<u8>);

impl FactPayloadDigest {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactPayload(Vec<u8>);

impl FactPayload {
    pub fn new(value: impl Into<Vec<u8>>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    #[must_use]
    pub fn digest(&self) -> FactPayloadDigest {
        let mut hasher = Sha256::new();
        write_component(&mut hasher, "polis.fact.payload.v1");
        write_bytes(&mut hasher, &self.0);
        FactPayloadDigest(hasher.finalize().to_vec())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactGrantPurpose {
    Append,
    ReplicaImport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FactGrantOutcome {
    Allowed,
    Denied,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactGrantDecision {
    outcome: FactGrantOutcome,
}

impl FactGrantDecision {
    #[must_use]
    pub const fn allowed() -> Self {
        Self {
            outcome: FactGrantOutcome::Allowed,
        }
    }

    #[must_use]
    pub const fn denied() -> Self {
        Self {
            outcome: FactGrantOutcome::Denied,
        }
    }

    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            outcome: FactGrantOutcome::Unknown,
        }
    }

    #[must_use]
    pub fn is_allowed(&self) -> bool {
        self.outcome == FactGrantOutcome::Allowed
    }
}

pub trait FactGrantAuthority {
    fn decide(
        &self,
        authority: &AuthorityContext,
        target: &FactTarget,
        purpose: FactGrantPurpose,
    ) -> FactGrantDecision;
}

pub struct FactGrantService<G> {
    grants: G,
}

impl<G> FactGrantService<G> {
    #[must_use]
    pub fn new(grants: G) -> Self {
        Self { grants }
    }
}

impl<G> FactGrantService<G>
where
    G: FactGrantAuthority,
{
    pub fn issue_append(
        &self,
        authority: &Authorized<FactAppendScope>,
        target: FactTarget,
    ) -> Result<FactWriteGrant> {
        self.issue(authority, target, FactGrantPurpose::Append)
    }

    pub fn issue_replica_import(
        &self,
        authority: &Authorized<FactAppendScope>,
        target: FactTarget,
    ) -> Result<FactWriteGrant> {
        self.issue(authority, target, FactGrantPurpose::ReplicaImport)
    }

    fn issue(
        &self,
        authority: &Authorized<FactAppendScope>,
        target: FactTarget,
        purpose: FactGrantPurpose,
    ) -> Result<FactWriteGrant> {
        if self
            .grants
            .decide(authority.context(), &target, purpose)
            .is_allowed()
        {
            return Ok(FactWriteGrant::new(authority, target, purpose));
        }
        Err(Error::Unauthorized)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactWriteGrant {
    principal: PrincipalId,
    scope: ScopeId,
    target: FactTarget,
    authority_epoch: GrantEpoch,
    purpose: FactGrantPurpose,
}

impl FactWriteGrant {
    #[must_use]
    fn new(
        authority: &Authorized<FactAppendScope>,
        target: FactTarget,
        purpose: FactGrantPurpose,
    ) -> Self {
        Self {
            principal: authority.context().principal().clone(),
            scope: authority.context().scope().clone(),
            target,
            authority_epoch: authority.context().epoch(),
            purpose,
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
    pub fn target(&self) -> &FactTarget {
        &self.target
    }

    #[must_use]
    pub fn resource(&self) -> &ResourceId {
        self.target.resource()
    }

    #[must_use]
    pub fn key(&self) -> &FactKey {
        self.target.key()
    }

    #[must_use]
    pub fn kind(&self) -> &FactKind {
        self.target.kind()
    }

    #[must_use]
    pub fn authority_epoch(&self) -> GrantEpoch {
        self.authority_epoch
    }

    #[must_use]
    pub fn purpose(&self) -> FactGrantPurpose {
        self.purpose
    }

    #[must_use]
    pub fn permits(&self, request: &FactAppendRequest) -> bool {
        self.purpose == FactGrantPurpose::Append
            && self.principal == *request.authority.context().principal()
            && self.scope == *request.authority.context().scope()
            && self.authority_epoch == request.authority.context().epoch()
            && self.target == request.target
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactAppendRequest {
    operation: OperationId,
    idempotency: IdempotencyKey,
    authority: Authorized<FactAppendScope>,
    grant: FactWriteGrant,
    target: FactTarget,
    payload: FactPayload,
    submitted_fence: Option<Box<SubmittedFenceFingerprint>>,
    conflict_policy: FactConflictPolicy,
}

impl FactAppendRequest {
    #[must_use]
    pub fn new(
        operation: OperationId,
        idempotency: IdempotencyKey,
        authority: Authorized<FactAppendScope>,
        grant: FactWriteGrant,
        target: FactTarget,
        payload: FactPayload,
        submitted_fence: Option<SubmittedFenceFingerprint>,
    ) -> Self {
        Self {
            operation,
            idempotency,
            authority,
            grant,
            target,
            payload,
            submitted_fence: submitted_fence.map(Box::new),
            conflict_policy: FactConflictPolicy::RecordCandidate,
        }
    }

    #[must_use]
    pub fn with_conflict_policy(mut self, conflict_policy: FactConflictPolicy) -> Self {
        self.conflict_policy = conflict_policy;
        self
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn idempotency(&self) -> &IdempotencyKey {
        &self.idempotency
    }

    #[must_use]
    pub fn authority(&self) -> &Authorized<FactAppendScope> {
        &self.authority
    }

    #[must_use]
    pub fn grant(&self) -> &FactWriteGrant {
        &self.grant
    }

    #[must_use]
    pub fn target(&self) -> &FactTarget {
        &self.target
    }

    #[must_use]
    pub fn resource(&self) -> &ResourceId {
        self.target.resource()
    }

    #[must_use]
    pub fn key(&self) -> &FactKey {
        self.target.key()
    }

    #[must_use]
    pub fn kind(&self) -> &FactKind {
        self.target.kind()
    }

    #[must_use]
    pub fn payload(&self) -> &FactPayload {
        &self.payload
    }

    #[must_use]
    pub fn submitted_fence(&self) -> Option<&SubmittedFenceFingerprint> {
        self.submitted_fence.as_deref()
    }

    #[must_use]
    pub fn conflict_policy(&self) -> FactConflictPolicy {
        self.conflict_policy
    }

    pub fn validate(self) -> FactAppendValidation {
        if !self.grant.permits(&self) {
            return Err(FactRejection::Unauthorized);
        }
        let fingerprint = FactAppendFingerprint::for_request(&self);
        let replay_key = FactReplayKey::for_request(&self);
        Ok(ValidatedFactAppend {
            request: self,
            fingerprint,
            replay_key,
        })
    }
}

pub type FactAppendValidation = std::result::Result<ValidatedFactAppend, FactRejection>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedFactAppend {
    request: FactAppendRequest,
    fingerprint: FactAppendFingerprint,
    replay_key: FactReplayKey,
}

impl ValidatedFactAppend {
    #[must_use]
    pub fn request(&self) -> &FactAppendRequest {
        &self.request
    }

    #[must_use]
    pub fn fingerprint(&self) -> &FactAppendFingerprint {
        &self.fingerprint
    }

    #[must_use]
    pub fn replay_key(&self) -> &FactReplayKey {
        &self.replay_key
    }

    #[must_use]
    pub fn into_request(self) -> FactAppendRequest {
        self.request
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactAppendOutcome {
    Appended(Box<FactReceipt>),
    Replayed(Box<FactReceipt>),
    Conflict(Box<FactConflict>),
    Rejected(FactRejection),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactConflict {
    IdempotencyKeyReuse {
        existing: Box<FactReceipt>,
    },
    KeyPayloadConflict {
        existing: Box<FactReceipt>,
        new_candidate: Box<FactReceipt>,
    },
    RejectedKeyPayloadConflict {
        existing: Box<FactReceipt>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FactRejection {
    Unauthorized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FactConflictPolicy {
    RecordCandidate,
    RejectCandidate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactReceipt {
    id: FactId,
    cursor: FactCursor,
    operation: OperationId,
    idempotency: IdempotencyKey,
    author: PrincipalId,
    scope: ScopeId,
    authority_epoch: GrantEpoch,
    target: FactTarget,
    payload_digest: FactPayloadDigest,
    submitted_fence: Option<Box<SubmittedFenceFingerprint>>,
}

impl FactReceipt {
    #[must_use]
    pub fn from_validated_append(append: &ValidatedFactAppend, cursor: FactCursor) -> Self {
        let request = append.request();
        Self {
            id: FactId(append.fingerprint().0.clone()),
            cursor,
            operation: request.operation.clone(),
            idempotency: request.idempotency.clone(),
            author: request.authority.context().principal().clone(),
            scope: request.authority.context().scope().clone(),
            authority_epoch: request.authority.context().epoch(),
            target: request.target.clone(),
            payload_digest: request.payload.digest(),
            submitted_fence: request.submitted_fence.clone(),
        }
    }

    #[must_use]
    pub fn id(&self) -> &FactId {
        &self.id
    }

    #[must_use]
    pub fn cursor(&self) -> FactCursor {
        self.cursor
    }

    #[must_use]
    pub fn operation(&self) -> &OperationId {
        &self.operation
    }

    #[must_use]
    pub fn idempotency(&self) -> &IdempotencyKey {
        &self.idempotency
    }

    #[must_use]
    pub fn author(&self) -> &PrincipalId {
        &self.author
    }

    #[must_use]
    pub fn scope(&self) -> &ScopeId {
        &self.scope
    }

    #[must_use]
    pub fn authority_epoch(&self) -> GrantEpoch {
        self.authority_epoch
    }

    #[must_use]
    pub fn target(&self) -> &FactTarget {
        &self.target
    }

    #[must_use]
    pub fn resource(&self) -> &ResourceId {
        self.target.resource()
    }

    #[must_use]
    pub fn key(&self) -> &FactKey {
        self.target.key()
    }

    #[must_use]
    pub fn kind(&self) -> &FactKind {
        self.target.kind()
    }

    #[must_use]
    pub fn payload_digest(&self) -> &FactPayloadDigest {
        &self.payload_digest
    }

    #[must_use]
    pub fn submitted_fence(&self) -> Option<&SubmittedFenceFingerprint> {
        self.submitted_fence.as_deref()
    }

    #[must_use]
    fn address(&self) -> FactAddress {
        FactAddress {
            scope: self.scope.clone(),
            target: self.target.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCandidate {
    receipt: FactReceipt,
    status: CandidateStatus,
}

impl FactCandidate {
    #[must_use]
    pub fn new(receipt: FactReceipt, status: CandidateStatus) -> Self {
        Self { receipt, status }
    }

    #[must_use]
    pub fn receipt(&self) -> &FactReceipt {
        &self.receipt
    }

    #[must_use]
    pub fn status(&self) -> CandidateStatus {
        self.status
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CandidateStatus {
    Verified,
    Conflict,
    Unauthorized,
    Unverified,
    MissingPayload,
    SubstrateMalformed,
    CrossScope,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FactQuery {
    scope: ScopeId,
    resource: Option<ResourceId>,
    key: Option<FactKey>,
    kind: Option<FactKind>,
    after: Option<FactCursor>,
}

impl FactQuery {
    #[must_use]
    pub fn new(scope: ScopeId) -> Self {
        Self {
            scope,
            resource: None,
            key: None,
            kind: None,
            after: None,
        }
    }

    #[must_use]
    pub fn resource(mut self, resource: ResourceId) -> Self {
        self.resource = Some(resource);
        self
    }

    #[must_use]
    pub fn key(mut self, key: FactKey) -> Self {
        self.key = Some(key);
        self
    }

    #[must_use]
    pub fn kind(mut self, kind: FactKind) -> Self {
        self.kind = Some(kind);
        self
    }

    #[must_use]
    pub fn after(mut self, cursor: FactCursor) -> Self {
        self.after = Some(cursor);
        self
    }

    #[must_use]
    pub fn matches(&self, receipt: &FactReceipt) -> bool {
        if self.scope != *receipt.scope() {
            return false;
        }
        if let Some(resource) = &self.resource
            && resource != receipt.resource()
        {
            return false;
        }
        if let Some(key) = &self.key
            && key != receipt.key()
        {
            return false;
        }
        if let Some(kind) = &self.kind
            && kind != receipt.kind()
        {
            return false;
        }
        if let Some(after) = self.after
            && receipt.cursor() <= after
        {
            return false;
        }
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FactPayloadBatch {
    payloads: BTreeMap<FactId, FactPayload>,
    failures: BTreeMap<FactId, FactPayloadReadFailure>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FactPayloadReadFailure {
    UnknownCandidate,
    CandidateMismatch,
    MissingPayload,
    DigestMismatch,
}

impl FactPayloadBatch {
    #[must_use]
    pub fn from_parts(
        payloads: BTreeMap<FactId, FactPayload>,
        failures: BTreeMap<FactId, FactPayloadReadFailure>,
    ) -> Self {
        Self { payloads, failures }
    }

    #[must_use]
    pub fn get(&self, candidate: &FactCandidate) -> Option<&FactPayload> {
        self.payloads.get(candidate.receipt().id())
    }

    #[must_use]
    pub fn failure(&self, candidate: &FactCandidate) -> Option<FactPayloadReadFailure> {
        self.failures.get(candidate.receipt().id()).copied()
    }

    #[must_use]
    pub fn failure_counts(&self) -> BTreeMap<FactPayloadReadFailure, usize> {
        let mut counts = BTreeMap::new();
        for failure in self.failures.values() {
            *counts.entry(*failure).or_default() += 1;
        }
        counts
    }

    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.payloads.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.payloads.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactCandidateSet {
    candidates: Vec<FactCandidate>,
    complete_through: FactCursor,
}

impl FactCandidateSet {
    /// Builds a complete prefix for a query.
    ///
    /// Implementors must include every matching candidate with a cursor at or
    /// before `complete_through`, in ascending cursor order. Projection
    /// catch-up relies on this watermark as a completeness proof, not as a
    /// paging cursor.
    #[must_use]
    pub fn complete_prefix(candidates: Vec<FactCandidate>, complete_through: FactCursor) -> Self {
        Self {
            candidates,
            complete_through,
        }
    }

    #[must_use]
    pub fn source_cursor(&self) -> FactCursor {
        self.complete_through
    }

    #[must_use]
    pub fn complete_through(&self) -> FactCursor {
        self.complete_through
    }

    #[must_use]
    pub fn as_slice(&self) -> &[FactCandidate] {
        &self.candidates
    }

    pub fn iter(&self) -> std::slice::Iter<'_, FactCandidate> {
        self.candidates.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.candidates.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.candidates.is_empty()
    }

    #[must_use]
    pub fn into_candidates(self) -> Vec<FactCandidate> {
        self.candidates
    }
}

pub trait FactStore {
    fn append(&self, request: FactAppendRequest) -> Result<FactAppendOutcome>;

    /// Returns a complete candidate prefix for `query`.
    ///
    /// The returned set must contain every matching candidate through
    /// `FactCandidateSet::complete_through()` and candidates must be ordered by
    /// ascending cursor. Partial pages must not advertise a cursor beyond their
    /// complete prefix.
    fn list_candidates(&self, query: FactQuery) -> Result<FactCandidateSet>;

    fn read_payloads(&self, candidates: &[FactCandidate]) -> Result<FactPayloadBatch>;
}

#[derive(Debug, Default)]
pub struct MemoryFactStore {
    state: RefCell<MemoryFactState>,
}

impl MemoryFactStore {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl FactStore for MemoryFactStore {
    fn append(&self, request: FactAppendRequest) -> Result<FactAppendOutcome> {
        let append = match request.validate() {
            Ok(append) => append,
            Err(rejection) => return Ok(FactAppendOutcome::Rejected(rejection)),
        };
        let mut state = self.state.borrow_mut();

        let fingerprint = append.fingerprint().clone();
        let replay_key = append.replay_key().clone();
        if let Some(existing) = state.by_idempotency.get(&replay_key) {
            if existing.fingerprint == fingerprint {
                return Ok(existing.replay());
            }
            return Ok(FactAppendOutcome::Conflict(Box::new(
                FactConflict::IdempotencyKeyReuse {
                    existing: Box::new(existing.receipt.clone()),
                },
            )));
        }

        let existing = state.earliest_conflicting_receipt(append.request());
        if let Some(existing) = &existing
            && append.request().conflict_policy() == FactConflictPolicy::RejectCandidate
        {
            return Ok(FactAppendOutcome::Conflict(Box::new(
                FactConflict::RejectedKeyPayloadConflict {
                    existing: Box::new(existing.clone()),
                },
            )));
        }

        let stored = state.insert_authorized(append);
        let original_outcome = match &existing {
            Some(existing) => StoredAppendOutcome::KeyPayloadConflict {
                existing: Box::new(existing.clone()),
            },
            None => StoredAppendOutcome::Appended,
        };

        state.by_idempotency.insert(
            replay_key,
            StoredAppend {
                fingerprint: stored.fingerprint.clone(),
                receipt: stored.receipt.clone(),
                original_outcome,
            },
        );
        state.order.push(stored.receipt.id().clone());
        state
            .records
            .insert(stored.receipt.id().clone(), stored.record);

        match existing {
            Some(existing) => Ok(FactAppendOutcome::Conflict(Box::new(
                FactConflict::KeyPayloadConflict {
                    existing: Box::new(existing),
                    new_candidate: Box::new(stored.receipt),
                },
            ))),
            None => Ok(FactAppendOutcome::Appended(Box::new(stored.receipt))),
        }
    }

    fn list_candidates(&self, query: FactQuery) -> Result<FactCandidateSet> {
        let state = self.state.borrow();
        let conflicts = state.conflicting_addresses();
        let candidates = state
            .order
            .iter()
            .filter_map(|id| state.records.get(id))
            .filter(|record| query.matches(&record.receipt))
            .map(|record| record.candidate(&conflicts))
            .collect();
        Ok(FactCandidateSet::complete_prefix(
            candidates,
            FactCursor::new(state.next_cursor),
        ))
    }

    fn read_payloads(&self, candidates: &[FactCandidate]) -> Result<FactPayloadBatch> {
        let state = self.state.borrow();
        let mut payloads = BTreeMap::new();
        let mut failures = BTreeMap::new();

        for candidate in candidates {
            let Some(record) = state.records.get(candidate.receipt().id()) else {
                failures.insert(
                    candidate.receipt().id().clone(),
                    FactPayloadReadFailure::UnknownCandidate,
                );
                continue;
            };
            if record.receipt != *candidate.receipt() {
                failures.insert(
                    candidate.receipt().id().clone(),
                    FactPayloadReadFailure::CandidateMismatch,
                );
                continue;
            }
            let Some(payload) = record.payload.as_ref() else {
                failures.insert(
                    candidate.receipt().id().clone(),
                    FactPayloadReadFailure::MissingPayload,
                );
                continue;
            };
            if payload.digest() != *candidate.receipt().payload_digest() {
                failures.insert(
                    candidate.receipt().id().clone(),
                    FactPayloadReadFailure::DigestMismatch,
                );
                continue;
            }
            payloads.insert(candidate.receipt().id().clone(), payload.clone());
        }

        Ok(FactPayloadBatch { payloads, failures })
    }
}

#[derive(Debug, Default)]
struct MemoryFactState {
    next_cursor: u64,
    by_idempotency: BTreeMap<FactReplayKey, StoredAppend>,
    records: BTreeMap<FactId, StoredFact>,
    order: Vec<FactId>,
}

impl MemoryFactState {
    fn insert_authorized(&mut self, append: ValidatedFactAppend) -> StoredInsert {
        self.next_cursor += 1;
        let fingerprint = append.fingerprint().clone();
        let receipt =
            FactReceipt::from_validated_append(&append, FactCursor::new(self.next_cursor));
        let request = append.into_request();
        let record = StoredFact {
            receipt: receipt.clone(),
            payload: Some(request.payload),
        };
        StoredInsert {
            fingerprint,
            receipt,
            record,
        }
    }

    fn conflicting_addresses(&self) -> BTreeSet<FactAddress> {
        let mut digests_by_address: BTreeMap<FactAddress, BTreeSet<FactPayloadDigest>> =
            BTreeMap::new();
        for record in self.records.values() {
            digests_by_address
                .entry(record.receipt.address())
                .or_default()
                .insert(record.receipt.payload_digest().clone());
        }
        digests_by_address
            .into_iter()
            .filter_map(|(address, digests)| {
                if digests.len() > 1 {
                    return Some(address);
                }
                None
            })
            .collect()
    }

    fn earliest_conflicting_receipt(&self, request: &FactAppendRequest) -> Option<FactReceipt> {
        let payload_digest = request.payload().digest();
        self.order
            .iter()
            .filter_map(|id| self.records.get(id))
            .find(|record| {
                record.receipt.scope() == request.authority().context().scope()
                    && record.receipt.target() == request.target()
                    && record.receipt.payload_digest() != &payload_digest
            })
            .map(|record| record.receipt.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredAppend {
    fingerprint: FactAppendFingerprint,
    receipt: FactReceipt,
    original_outcome: StoredAppendOutcome,
}

impl StoredAppend {
    fn replay(&self) -> FactAppendOutcome {
        match &self.original_outcome {
            StoredAppendOutcome::Appended => {
                FactAppendOutcome::Replayed(Box::new(self.receipt.clone()))
            }
            StoredAppendOutcome::KeyPayloadConflict { existing } => {
                FactAppendOutcome::Conflict(Box::new(FactConflict::KeyPayloadConflict {
                    existing: existing.clone(),
                    new_candidate: Box::new(self.receipt.clone()),
                }))
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StoredAppendOutcome {
    Appended,
    KeyPayloadConflict { existing: Box<FactReceipt> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredFact {
    receipt: FactReceipt,
    payload: Option<FactPayload>,
}

impl StoredFact {
    fn candidate(&self, conflicts: &BTreeSet<FactAddress>) -> FactCandidate {
        let status = if self.payload.is_none() {
            CandidateStatus::MissingPayload
        } else if conflicts.contains(&self.receipt.address()) {
            CandidateStatus::Conflict
        } else {
            CandidateStatus::Verified
        };
        FactCandidate {
            receipt: self.receipt.clone(),
            status,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredInsert {
    fingerprint: FactAppendFingerprint,
    receipt: FactReceipt,
    record: StoredFact,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactAppendFingerprint(Vec<u8>);

impl FactAppendFingerprint {
    fn for_request(request: &FactAppendRequest) -> Self {
        let mut hasher = Sha256::new();
        write_component(&mut hasher, "polis.fact.append.v1");
        write_component(&mut hasher, request.operation().as_str());
        write_component(&mut hasher, request.idempotency().as_str());
        write_component(
            &mut hasher,
            request.authority().context().principal().as_str(),
        );
        write_component(&mut hasher, request.authority().context().scope().as_str());
        hasher.update(request.authority().context().epoch().value().to_be_bytes());
        write_component(&mut hasher, request.resource().as_str());
        write_component(&mut hasher, request.key().as_str());
        write_component(&mut hasher, request.kind().as_str());
        write_bytes(&mut hasher, request.payload().digest().as_bytes());
        match request.submitted_fence() {
            Some(fence) => {
                write_component(&mut hasher, "submitted-fence");
                write_component(&mut hasher, fence.resource());
                write_component(&mut hasher, fence.holder());
                hasher.update(fence.epoch().to_be_bytes());
                write_bytes(&mut hasher, fence.claim_hash());
            }
            None => write_component(&mut hasher, "no-submitted-fence"),
        }
        match request.conflict_policy() {
            FactConflictPolicy::RecordCandidate => {
                write_component(&mut hasher, "record-conflict-candidate");
            }
            FactConflictPolicy::RejectCandidate => {
                write_component(&mut hasher, "reject-conflict-candidate");
            }
        }
        Self(hasher.finalize().to_vec())
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct FactReplayKey {
    principal: PrincipalId,
    scope: ScopeId,
    idempotency: IdempotencyKey,
}

impl FactReplayKey {
    fn for_request(request: &FactAppendRequest) -> Self {
        Self {
            principal: request.authority().context().principal().clone(),
            scope: request.authority().context().scope().clone(),
            idempotency: request.idempotency().clone(),
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
    pub fn idempotency(&self) -> &IdempotencyKey {
        &self.idempotency
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct FactAddress {
    scope: ScopeId,
    target: FactTarget,
}

fn parse_non_empty<T>(value: impl Into<String>, build: impl FnOnce(String) -> T) -> Result<T> {
    let value = value.into();
    if value.trim().is_empty() {
        return Err(Error::MalformedPayload);
    }
    Ok(build(value))
}

fn write_component(hasher: &mut Sha256, value: &str) {
    write_bytes(hasher, value.as_bytes());
}

fn write_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(value.len().to_be_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::authority::{Authority, AuthorityDecision, AuthorityService};

    struct AllowAuthority;

    impl Authority for AllowAuthority {
        fn decide(&self, _principal: &PrincipalId, _scope: &ScopeId) -> AuthorityDecision {
            AuthorityDecision::allowed(GrantEpoch::new(7))
        }
    }

    struct AllowGrantAuthority;

    impl FactGrantAuthority for AllowGrantAuthority {
        fn decide(
            &self,
            _authority: &AuthorityContext,
            _target: &FactTarget,
            _purpose: FactGrantPurpose,
        ) -> FactGrantDecision {
            FactGrantDecision::allowed()
        }
    }

    struct DenyGrantAuthority;

    impl FactGrantAuthority for DenyGrantAuthority {
        fn decide(
            &self,
            _authority: &AuthorityContext,
            _target: &FactTarget,
            _purpose: FactGrantPurpose,
        ) -> FactGrantDecision {
            FactGrantDecision::denied()
        }
    }

    fn authorized() -> Authorized<FactAppendScope> {
        authorized_as("node-a", "cluster")
    }

    fn authorized_as(principal: &str, scope: &str) -> Authorized<FactAppendScope> {
        AuthorityService::new(AllowAuthority)
            .authorize(
                PrincipalId::parse(principal).expect("principal"),
                ScopeId::parse(scope).expect("scope"),
            )
            .expect("authorized")
    }

    fn resource(value: &str) -> ResourceId {
        ResourceId::parse(value).expect("resource")
    }

    fn key(value: &str) -> FactKey {
        FactKey::parse(value).expect("key")
    }

    fn kind(value: &str) -> FactKind {
        FactKind::parse(value).expect("kind")
    }

    fn target(resource_value: &str, key_value: &str, kind_value: &str) -> FactTarget {
        FactTarget::new(resource(resource_value), key(key_value), kind(kind_value))
    }

    fn payload(value: &[u8]) -> FactPayload {
        FactPayload::new(value.to_vec()).expect("payload")
    }

    fn append_grant(authority: &Authorized<FactAppendScope>, target: FactTarget) -> FactWriteGrant {
        FactGrantService::new(AllowGrantAuthority)
            .issue_append(authority, target)
            .expect("fact write grant")
    }

    fn replica_import_grant(
        authority: &Authorized<FactAppendScope>,
        target: FactTarget,
    ) -> FactWriteGrant {
        FactGrantService::new(AllowGrantAuthority)
            .issue_replica_import(authority, target)
            .expect("replica import grant")
    }

    fn request(
        operation: &str,
        idempotency: &str,
        resource: ResourceId,
        key: FactKey,
        kind: FactKind,
        payload: FactPayload,
    ) -> FactAppendRequest {
        let authority = authorized();
        let target = FactTarget::new(resource, key, kind);
        let grant = append_grant(&authority, target.clone());
        FactAppendRequest::new(
            OperationId::parse(operation).expect("operation"),
            IdempotencyKey::parse(idempotency).expect("idempotency"),
            authority,
            grant,
            target,
            payload,
            None,
        )
    }

    fn request_with_authority(
        operation: &str,
        idempotency: &str,
        authority: Authorized<FactAppendScope>,
        target: FactTarget,
        payload: FactPayload,
    ) -> FactAppendRequest {
        let grant = append_grant(&authority, target.clone());
        FactAppendRequest::new(
            OperationId::parse(operation).expect("operation"),
            IdempotencyKey::parse(idempotency).expect("idempotency"),
            authority,
            grant,
            target,
            payload,
            None,
        )
    }

    fn request_with_grant(
        operation: &str,
        idempotency: &str,
        target: FactTarget,
        payload: FactPayload,
        grant: FactWriteGrant,
    ) -> FactAppendRequest {
        FactAppendRequest::new(
            OperationId::parse(operation).expect("operation"),
            IdempotencyKey::parse(idempotency).expect("idempotency"),
            authorized(),
            grant,
            target,
            payload,
            None,
        )
    }

    fn receipt(outcome: FactAppendOutcome) -> FactReceipt {
        match outcome {
            FactAppendOutcome::Appended(receipt) | FactAppendOutcome::Replayed(receipt) => *receipt,
            FactAppendOutcome::Conflict(_) | FactAppendOutcome::Rejected(_) => {
                panic!("expected receipt outcome")
            }
        }
    }

    #[test]
    fn denied_target_grant_authority_cannot_issue_write_grant() {
        let authority = authorized();
        let result = FactGrantService::new(DenyGrantAuthority).issue_append(
            &authority,
            target(
                "machine:node-a",
                "membership/node-a",
                "ployz.machine.joined.v1",
            ),
        );

        assert_eq!(result, Err(Error::Unauthorized));
    }

    #[test]
    fn appending_the_same_request_replays_the_same_receipt() {
        let store = MemoryFactStore::new();
        let request = request(
            "op-1",
            "idem-1",
            resource("machine:node-a"),
            key("membership/node-a"),
            kind("ployz.machine.joined.v1"),
            payload(b"joined"),
        );

        let first = receipt(store.append(request.clone()).expect("first append"));
        let second = receipt(store.append(request).expect("replay"));

        assert_eq!(second, first);
    }

    #[test]
    fn replay_requires_a_permitting_fact_write_grant() {
        let store = MemoryFactStore::new();
        let authority = authorized();
        let target = target(
            "machine:node-a",
            "membership/node-a",
            "ployz.machine.joined.v1",
        );
        let first = FactAppendRequest::new(
            OperationId::parse("op-1").expect("operation"),
            IdempotencyKey::parse("idem-1").expect("idempotency"),
            authority.clone(),
            append_grant(&authority, target.clone()),
            target.clone(),
            payload(b"joined"),
            None,
        );
        let replay_with_replica_grant = FactAppendRequest::new(
            OperationId::parse("op-1").expect("operation"),
            IdempotencyKey::parse("idem-1").expect("idempotency"),
            authority.clone(),
            replica_import_grant(&authority, target.clone()),
            target,
            payload(b"joined"),
            None,
        );

        store.append(first).expect("first append");
        let outcome = store
            .append(replay_with_replica_grant)
            .expect("replay attempt");

        assert_eq!(
            outcome,
            FactAppendOutcome::Rejected(FactRejection::Unauthorized)
        );
    }

    #[test]
    fn same_key_and_different_payload_returns_conflict_and_leaves_both_candidates_visible() {
        let store = MemoryFactStore::new();
        let scope = authorized().context().scope().clone();
        let resource = resource("machine:node-a");
        let key = key("membership/node-a");
        let kind = kind("ployz.machine.joined.v1");

        let first = store
            .append(request(
                "op-1",
                "idem-1",
                resource.clone(),
                key.clone(),
                kind.clone(),
                payload(b"joined-a"),
            ))
            .expect("first append");
        let second = store
            .append(request(
                "op-2",
                "idem-2",
                resource.clone(),
                key.clone(),
                kind.clone(),
                payload(b"joined-b"),
            ))
            .expect("conflicting append");

        assert!(matches!(first, FactAppendOutcome::Appended(_)));
        assert!(matches!(
            second,
            FactAppendOutcome::Conflict(conflict)
                if matches!(conflict.as_ref(), FactConflict::KeyPayloadConflict { .. })
        ));
        let candidates = store
            .list_candidates(FactQuery::new(scope).resource(resource).key(key).kind(kind))
            .expect("candidates");

        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .all(|candidate| candidate.status() == CandidateStatus::Conflict)
        );
    }

    #[test]
    fn rejecting_key_payload_conflict_does_not_store_candidate() {
        let store = MemoryFactStore::new();
        let scope = authorized().context().scope().clone();
        let resource = resource("serving-route-generation:route-a:7");
        let key = key("serving/route-a/generation/7");
        let kind = kind("ployz.serving.generation-slot.v1");

        let first = store
            .append(request(
                "op-1",
                "idem-1",
                resource.clone(),
                key.clone(),
                kind.clone(),
                payload(b"target=gateway-a"),
            ))
            .expect("first append");
        let second = store
            .append(
                request(
                    "op-2",
                    "idem-2",
                    resource.clone(),
                    key.clone(),
                    kind.clone(),
                    payload(b"target=gateway-b"),
                )
                .with_conflict_policy(FactConflictPolicy::RejectCandidate),
            )
            .expect("conflicting append");

        assert!(matches!(first, FactAppendOutcome::Appended(_)));
        assert!(matches!(
            second,
            FactAppendOutcome::Conflict(conflict)
                if matches!(
                    conflict.as_ref(),
                    FactConflict::RejectedKeyPayloadConflict { .. }
                )
        ));
        let candidates = store
            .list_candidates(FactQuery::new(scope).resource(resource).key(key).kind(kind))
            .expect("candidates");

        let [candidate] = candidates.as_slice() else {
            panic!("expected one candidate");
        };
        assert_eq!(candidate.status(), CandidateStatus::Verified);
    }

    #[test]
    fn listing_candidates_returns_cursor_order_and_source_cursor() {
        let store = MemoryFactStore::new();
        let scope = authorized().context().scope().clone();
        let resource = resource("machine:node-a");
        let key = key("membership/node-a");
        let kind = kind("ployz.machine.joined.v1");
        store
            .append(request(
                "op-1",
                "idem-1",
                resource.clone(),
                key.clone(),
                kind.clone(),
                payload(b"joined-a"),
            ))
            .expect("first append");
        store
            .append(request(
                "op-2",
                "idem-2",
                resource.clone(),
                key.clone(),
                kind.clone(),
                payload(b"joined-b"),
            ))
            .expect("second append");

        let candidates = store
            .list_candidates(
                FactQuery::new(scope.clone())
                    .resource(resource)
                    .key(key)
                    .kind(kind),
            )
            .expect("candidates");
        let after_first = store
            .list_candidates(FactQuery::new(scope).after(FactCursor::new(1)))
            .expect("after first");

        assert_eq!(candidates.source_cursor(), FactCursor::new(2));
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.receipt().cursor())
                .collect::<Vec<_>>(),
            vec![FactCursor::new(1), FactCursor::new(2)]
        );
        assert_eq!(after_first.len(), 1);
        assert_eq!(
            after_first
                .iter()
                .map(|candidate| candidate.receipt().cursor())
                .collect::<Vec<_>>(),
            vec![FactCursor::new(2)]
        );
    }

    #[test]
    fn same_idempotency_key_with_different_fingerprint_returns_conflict() {
        let store = MemoryFactStore::new();
        store
            .append(request(
                "op-1",
                "idem-1",
                resource("machine:node-a"),
                key("membership/node-a"),
                kind("ployz.machine.joined.v1"),
                payload(b"joined"),
            ))
            .expect("first append");

        let outcome = store
            .append(request(
                "op-1",
                "idem-1",
                resource("machine:node-a"),
                key("membership/node-a"),
                kind("ployz.machine.joined.v1"),
                payload(b"different"),
            ))
            .expect("idempotency conflict");

        assert!(matches!(
            outcome,
            FactAppendOutcome::Conflict(conflict)
                if matches!(conflict.as_ref(), FactConflict::IdempotencyKeyReuse { .. })
        ));
    }

    #[test]
    fn idempotency_keys_are_namespaced_by_scope_and_principal() {
        let store = MemoryFactStore::new();
        let first = request_with_authority(
            "op-1",
            "idem-1",
            authorized_as("node-a", "cluster-a"),
            target(
                "machine:node-a",
                "membership/node-a",
                "ployz.machine.joined.v1",
            ),
            payload(b"joined"),
        );
        let second = request_with_authority(
            "op-2",
            "idem-1",
            authorized_as("node-a", "cluster-b"),
            target(
                "machine:node-a",
                "membership/node-a",
                "ployz.machine.joined.v1",
            ),
            payload(b"joined"),
        );
        let third = request_with_authority(
            "op-3",
            "idem-1",
            authorized_as("node-b", "cluster-a"),
            target(
                "machine:node-b",
                "membership/node-b",
                "ployz.machine.joined.v1",
            ),
            payload(b"joined"),
        );

        assert!(matches!(
            store.append(first).expect("first append"),
            FactAppendOutcome::Appended(_)
        ));
        assert!(matches!(
            store.append(second).expect("second append"),
            FactAppendOutcome::Appended(_)
        ));
        assert!(matches!(
            store.append(third).expect("third append"),
            FactAppendOutcome::Appended(_)
        ));
    }

    #[test]
    fn retrying_conflicting_append_preserves_conflict_outcome() {
        let store = MemoryFactStore::new();
        let resource = resource("machine:node-a");
        let key = key("membership/node-a");
        let kind = kind("ployz.machine.joined.v1");
        store
            .append(request(
                "op-1",
                "idem-1",
                resource.clone(),
                key.clone(),
                kind.clone(),
                payload(b"joined-a"),
            ))
            .expect("first append");
        let conflicting = request("op-2", "idem-2", resource, key, kind, payload(b"joined-b"));

        let first_result = store
            .append(conflicting.clone())
            .expect("conflicting append");
        let retry_result = store.append(conflicting).expect("conflict replay");

        assert!(matches!(
            first_result,
            FactAppendOutcome::Conflict(conflict)
                if matches!(conflict.as_ref(), FactConflict::KeyPayloadConflict { .. })
        ));
        assert!(matches!(
            retry_result,
            FactAppendOutcome::Conflict(conflict)
                if matches!(conflict.as_ref(), FactConflict::KeyPayloadConflict { .. })
        ));
    }

    #[test]
    fn key_payload_conflict_reports_the_earliest_existing_receipt() {
        let store = MemoryFactStore::new();
        let resource = resource("machine:node-a");
        let key = key("membership/node-a");
        let kind = kind("ployz.machine.joined.v1");
        let first = receipt(
            store
                .append(request(
                    "op-1",
                    "idem-1",
                    resource.clone(),
                    key.clone(),
                    kind.clone(),
                    payload(b"joined-a"),
                ))
                .expect("first append"),
        );
        store
            .append(request(
                "op-2",
                "idem-2",
                resource.clone(),
                key.clone(),
                kind.clone(),
                payload(b"joined-b"),
            ))
            .expect("second append");

        let third = store
            .append(request(
                "op-3",
                "idem-3",
                resource,
                key,
                kind,
                payload(b"joined-c"),
            ))
            .expect("third append");

        let FactAppendOutcome::Conflict(conflict) = third else {
            panic!("expected conflict");
        };
        let FactConflict::KeyPayloadConflict { existing, .. } = conflict.as_ref() else {
            panic!("expected key payload conflict");
        };
        assert_eq!(existing.cursor(), first.cursor());
    }

    #[test]
    fn payload_reads_are_bound_to_exact_candidate_identity() {
        let store = MemoryFactStore::new();
        let scope = authorized().context().scope().clone();
        let first = receipt(
            store
                .append(request(
                    "op-1",
                    "idem-1",
                    resource("machine:node-a"),
                    key("membership/node-a"),
                    kind("ployz.machine.joined.v1"),
                    payload(b"same-payload"),
                ))
                .expect("first append"),
        );
        let second = receipt(
            store
                .append(request(
                    "op-2",
                    "idem-2",
                    resource("machine:node-b"),
                    key("membership/node-b"),
                    kind("ployz.machine.joined.v1"),
                    payload(b"same-payload"),
                ))
                .expect("second append"),
        );
        assert_eq!(first.payload_digest(), second.payload_digest());
        let candidates = store
            .list_candidates(FactQuery::new(scope))
            .expect("candidates");
        let first_candidate = candidates
            .iter()
            .find(|candidate| candidate.receipt().id() == first.id())
            .cloned()
            .expect("first candidate");
        let second_candidate = candidates
            .iter()
            .find(|candidate| candidate.receipt().id() == second.id())
            .cloned()
            .expect("second candidate");

        let batch = store
            .read_payloads(std::slice::from_ref(&first_candidate))
            .expect("payloads");

        assert_eq!(
            batch.get(&first_candidate).map(FactPayload::as_bytes),
            Some(b"same-payload".as_slice())
        );
        assert_eq!(batch.get(&second_candidate), None);
        assert_eq!(batch.failure(&second_candidate), None);
        assert!(batch.is_complete());
    }

    #[test]
    fn payload_reads_report_unknown_candidate_instead_of_silent_omission() {
        let source = MemoryFactStore::new();
        let empty_store = MemoryFactStore::new();
        let scope = authorized().context().scope().clone();
        source
            .append(request(
                "op-1",
                "idem-1",
                resource("machine:node-a"),
                key("membership/node-a"),
                kind("ployz.machine.joined.v1"),
                payload(b"joined"),
            ))
            .expect("append");
        let candidates = source
            .list_candidates(FactQuery::new(scope))
            .expect("candidates");
        let candidate = candidates.iter().next().cloned().expect("candidate");

        let batch = empty_store
            .read_payloads(std::slice::from_ref(&candidate))
            .expect("payloads");

        assert_eq!(
            batch.failure(&candidate),
            Some(FactPayloadReadFailure::UnknownCandidate)
        );
        assert!(!batch.is_complete());
    }

    #[test]
    fn submitted_fence_fingerprint_participates_in_append_fingerprinting() {
        let store = MemoryFactStore::new();
        let authority = authorized();
        let target = target("volume:data", "volume/data/owner", "ployz.volume.owner.v1");
        let grant = append_grant(&authority, target.clone());
        let first_fence =
            SubmittedFenceFingerprint::parse("volume:data", "node-a", 3, b"claim-hash-a".to_vec())
                .expect("submitted fence");
        let second_fence =
            SubmittedFenceFingerprint::parse("volume:data", "node-a", 4, b"claim-hash-b".to_vec())
                .expect("submitted fence");
        let first = FactAppendRequest::new(
            OperationId::parse("op-1").expect("operation"),
            IdempotencyKey::parse("idem-1").expect("idempotency"),
            authority.clone(),
            grant.clone(),
            target.clone(),
            payload(b"owner=node-a"),
            Some(first_fence),
        );
        let second = FactAppendRequest::new(
            OperationId::parse("op-1").expect("operation"),
            IdempotencyKey::parse("idem-1").expect("idempotency"),
            authority,
            grant,
            target,
            payload(b"owner=node-a"),
            Some(second_fence),
        );

        store.append(first).expect("first append");
        let outcome = store.append(second).expect("conflict");

        assert!(matches!(
            outcome,
            FactAppendOutcome::Conflict(conflict)
                if matches!(conflict.as_ref(), FactConflict::IdempotencyKeyReuse { .. })
        ));
    }

    #[test]
    fn broad_scope_authorization_without_a_matching_fact_write_grant_cannot_append() {
        let authority = authorized();
        let grant = append_grant(
            &authority,
            target(
                "machine:node-a",
                "membership/other",
                "ployz.machine.joined.v1",
            ),
        );
        let store = MemoryFactStore::new();

        let outcome = store
            .append(request_with_grant(
                "op-1",
                "idem-1",
                target(
                    "machine:node-a",
                    "membership/node-a",
                    "ployz.machine.joined.v1",
                ),
                payload(b"joined"),
                grant,
            ))
            .expect("append");

        assert_eq!(
            outcome,
            FactAppendOutcome::Rejected(FactRejection::Unauthorized)
        );
    }

    #[test]
    fn wrong_resource_key_kind_principal_or_authority_epoch_in_grant_rejects_append() {
        let store = MemoryFactStore::new();
        let authority = authorized();
        let wrong_principal_authority = AuthorityService::new(AllowAuthority)
            .authorize(
                PrincipalId::parse("node-b").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
            )
            .expect("authorized");
        let wrong_epoch_authority = AuthorityService::new(EpochAuthority(8))
            .authorize(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
            )
            .expect("authorized");
        let grants = [
            append_grant(
                &authority,
                target(
                    "machine:node-b",
                    "membership/node-a",
                    "ployz.machine.joined.v1",
                ),
            ),
            append_grant(
                &authority,
                target(
                    "machine:node-a",
                    "membership/node-b",
                    "ployz.machine.joined.v1",
                ),
            ),
            append_grant(
                &authority,
                target(
                    "machine:node-a",
                    "membership/node-a",
                    "ployz.machine.tombstoned.v1",
                ),
            ),
            append_grant(
                &wrong_principal_authority,
                target(
                    "machine:node-a",
                    "membership/node-a",
                    "ployz.machine.joined.v1",
                ),
            ),
            append_grant(
                &wrong_epoch_authority,
                target(
                    "machine:node-a",
                    "membership/node-a",
                    "ployz.machine.joined.v1",
                ),
            ),
        ];

        for (index, grant) in grants.into_iter().enumerate() {
            let outcome = store
                .append(request_with_grant(
                    &format!("op-{index}"),
                    &format!("idem-{index}"),
                    target(
                        "machine:node-a",
                        "membership/node-a",
                        "ployz.machine.joined.v1",
                    ),
                    payload(b"joined"),
                    grant,
                ))
                .expect("append");
            assert_eq!(
                outcome,
                FactAppendOutcome::Rejected(FactRejection::Unauthorized)
            );
        }
    }

    #[test]
    fn replica_import_authority_does_not_authorize_local_fact_writes() {
        let authority = authorized();
        let target = target(
            "machine:node-a",
            "membership/node-a",
            "ployz.machine.joined.v1",
        );
        let grant = replica_import_grant(&authority, target.clone());
        let store = MemoryFactStore::new();

        let outcome = store
            .append(request_with_grant(
                "op-1",
                "idem-1",
                target,
                payload(b"joined"),
                grant,
            ))
            .expect("append");

        assert_eq!(
            outcome,
            FactAppendOutcome::Rejected(FactRejection::Unauthorized)
        );
    }

    struct EpochAuthority(u64);

    impl Authority for EpochAuthority {
        fn decide(&self, _principal: &PrincipalId, _scope: &ScopeId) -> AuthorityDecision {
            AuthorityDecision::allowed(GrantEpoch::new(self.0))
        }
    }
}
