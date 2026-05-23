//! Product-neutral projection source primitives.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use crate::facts::{
    CandidateStatus, FactCandidate, FactCandidateSet, FactCursor, FactPayload,
    FactPayloadReadFailure, FactQuery, FactReceipt, FactStore,
};
use crate::{Error, Result};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectionKey(String);

impl ProjectionKey {
    pub fn parse(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(Error::MalformedPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProjectionView {
    key: ProjectionKey,
}

impl ProjectionView {
    #[must_use]
    pub fn new(key: ProjectionKey) -> Self {
        Self { key }
    }

    #[must_use]
    pub fn key(&self) -> &ProjectionKey {
        &self.key
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRequest<R> {
    view: ProjectionView,
    query: FactQuery,
    reducer: R,
}

impl<R> ProjectionRequest<R> {
    #[must_use]
    pub fn new(view: ProjectionView, query: FactQuery, reducer: R) -> Self {
        Self {
            view,
            query,
            reducer,
        }
    }

    #[must_use]
    pub fn view(&self) -> &ProjectionView {
        &self.view
    }

    #[must_use]
    pub fn query(&self) -> &FactQuery {
        &self.query
    }

    #[must_use]
    pub fn reducer(&self) -> &R {
        &self.reducer
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionCatchUpRequest {
    view: ProjectionView,
    query: FactQuery,
    cursor: FactCursor,
    deadline: SystemTime,
}

impl ProjectionCatchUpRequest {
    #[must_use]
    pub fn new(
        view: ProjectionView,
        query: FactQuery,
        cursor: FactCursor,
        deadline: SystemTime,
    ) -> Self {
        Self {
            view,
            query,
            cursor,
            deadline,
        }
    }

    #[must_use]
    pub fn view(&self) -> &ProjectionView {
        &self.view
    }

    #[must_use]
    pub fn query(&self) -> &FactQuery {
        &self.query
    }

    #[must_use]
    pub fn cursor(&self) -> FactCursor {
        self.cursor
    }

    #[must_use]
    pub fn deadline(&self) -> SystemTime {
        self.deadline
    }
}

impl From<ProjectionRequestIdentity> for ProjectionCatchUpRequest {
    fn from(identity: ProjectionRequestIdentity) -> Self {
        Self::new(
            identity.view,
            identity.query,
            identity.cursor,
            SystemTime::now() + Duration::from_secs(1),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionRequestIdentity {
    view: ProjectionView,
    query: FactQuery,
    cursor: FactCursor,
}

impl ProjectionRequestIdentity {
    #[must_use]
    pub fn new(view: ProjectionView, query: FactQuery, cursor: FactCursor) -> Self {
        Self {
            view,
            query,
            cursor,
        }
    }

    #[must_use]
    pub fn view(&self) -> &ProjectionView {
        &self.view
    }

    #[must_use]
    pub fn query(&self) -> &FactQuery {
        &self.query
    }

    #[must_use]
    pub fn cursor(&self) -> FactCursor {
        self.cursor
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionSnapshot<T> {
    view: T,
    source_cursor: Option<FactCursor>,
    freshness: ProjectionFreshness,
    health: ProjectionHealth,
}

impl<T> ProjectionSnapshot<T> {
    #[must_use]
    pub fn new(
        view: T,
        source_cursor: Option<FactCursor>,
        freshness: ProjectionFreshness,
        health: ProjectionHealth,
    ) -> Self {
        Self {
            view,
            source_cursor,
            freshness,
            health,
        }
    }

    #[must_use]
    pub fn view(&self) -> &T {
        &self.view
    }

    #[must_use]
    pub fn into_view(self) -> T {
        self.view
    }

    #[must_use]
    pub fn source_cursor(&self) -> Option<FactCursor> {
        self.source_cursor
    }

    #[must_use]
    pub fn freshness(&self) -> ProjectionFreshness {
        self.freshness
    }

    #[must_use]
    pub fn health(&self) -> &ProjectionHealth {
        &self.health
    }

    #[must_use]
    pub fn catch_up_identity(
        &self,
        view: ProjectionView,
        query: FactQuery,
    ) -> Option<ProjectionRequestIdentity> {
        self.source_cursor
            .map(|cursor| ProjectionRequestIdentity::new(view, query, cursor))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionFreshness {
    Fresh,
    Degraded,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectionHealth {
    candidate_status_counts: BTreeMap<CandidateStatus, usize>,
    payload_failure_counts: BTreeMap<FactPayloadReadFailure, usize>,
}

impl ProjectionHealth {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn candidate_count(&self, status: CandidateStatus) -> usize {
        self.candidate_status_counts
            .get(&status)
            .copied()
            .unwrap_or(0)
    }

    #[must_use]
    pub fn payload_failure_count(&self, failure: FactPayloadReadFailure) -> usize {
        self.payload_failure_counts
            .get(&failure)
            .copied()
            .unwrap_or(0)
    }

    #[must_use]
    pub fn has_failures(&self) -> bool {
        self.candidate_status_counts
            .iter()
            .any(|(status, count)| is_fatal_status(*status) && *count > 0)
            || self.payload_failure_counts.values().any(|count| *count > 0)
    }

    #[must_use]
    pub fn has_rejections(&self) -> bool {
        self.candidate_status_counts
            .iter()
            .any(|(status, count)| !is_projectable_status(*status) && *count > 0)
            || self.payload_failure_counts.values().any(|count| *count > 0)
    }

    fn record_candidate(&mut self, status: CandidateStatus) {
        *self.candidate_status_counts.entry(status).or_default() += 1;
    }

    fn record_payload_failure(&mut self, failure: FactPayloadReadFailure) {
        *self.payload_failure_counts.entry(failure).or_default() += 1;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionCatchUp {
    CaughtUp {
        view: ProjectionView,
        source_cursor: FactCursor,
    },
    TimedOut {
        view: ProjectionView,
        requested: FactCursor,
        current: Option<FactCursor>,
    },
    FreshnessUnknown {
        view: ProjectionView,
        requested: FactCursor,
    },
    ProjectionFailed {
        view: ProjectionView,
        requested: FactCursor,
        source_cursor: FactCursor,
        health: ProjectionHealth,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedFact {
    receipt: FactReceipt,
    payload: FactPayload,
}

impl VerifiedFact {
    #[must_use]
    pub fn new(receipt: FactReceipt, payload: FactPayload) -> Self {
        Self { receipt, payload }
    }

    #[must_use]
    pub fn receipt(&self) -> &FactReceipt {
        &self.receipt
    }

    #[must_use]
    pub fn payload(&self) -> &FactPayload {
        &self.payload
    }
}

pub trait FactReducer {
    type View;
    type Error;

    fn reduce(&self, facts: Vec<VerifiedFact>) -> std::result::Result<Self::View, Self::Error>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionError<E> {
    Source(Error),
    SubstrateFailed { health: ProjectionHealth },
    Reducer(E),
}

pub trait ProjectionSource {
    fn project<R>(
        &self,
        request: ProjectionRequest<R>,
    ) -> std::result::Result<ProjectionSnapshot<R::View>, ProjectionError<R::Error>>
    where
        R: FactReducer;

    fn catch_up(&self, request: ProjectionCatchUpRequest) -> Result<ProjectionCatchUp>;
}

#[derive(Debug)]
pub struct MemoryProjectionSource<S> {
    facts: S,
    state: RefCell<BTreeMap<ProjectionStateKey, ProjectionState>>,
}

impl<S> MemoryProjectionSource<S> {
    #[must_use]
    pub fn new(facts: S) -> Self {
        Self {
            facts,
            state: RefCell::new(BTreeMap::new()),
        }
    }

    #[must_use]
    pub fn facts(&self) -> &S {
        &self.facts
    }

    pub fn clear_projection_cache(&self) {
        self.state.borrow_mut().clear();
    }
}

impl<S> ProjectionSource for MemoryProjectionSource<S>
where
    S: FactStore,
{
    fn project<R>(
        &self,
        request: ProjectionRequest<R>,
    ) -> std::result::Result<ProjectionSnapshot<R::View>, ProjectionError<R::Error>>
    where
        R: FactReducer,
    {
        let candidate_set = self
            .facts
            .list_candidates(request.query.clone())
            .map_err(ProjectionError::Source)?;
        let mut health = health_for_candidates(&candidate_set);
        if health.has_failures() {
            self.record_failed(&request, candidate_set.source_cursor(), health.clone());
            return Err(ProjectionError::SubstrateFailed { health });
        }

        let verified_candidates = verified_candidates(candidate_set.as_slice());
        let payloads = self
            .facts
            .read_payloads(&verified_candidates)
            .map_err(ProjectionError::Source)?;
        for (failure, count) in payloads.failure_counts() {
            for _ in 0..count {
                health.record_payload_failure(failure);
            }
        }
        if health.has_failures() || !payloads.is_complete() {
            self.record_failed(&request, candidate_set.source_cursor(), health.clone());
            return Err(ProjectionError::SubstrateFailed { health });
        }

        let mut facts = Vec::with_capacity(verified_candidates.len());
        for candidate in verified_candidates {
            let Some(payload) = payloads.get(&candidate) else {
                health.record_payload_failure(FactPayloadReadFailure::MissingPayload);
                self.record_failed(&request, candidate_set.source_cursor(), health.clone());
                return Err(ProjectionError::SubstrateFailed { health });
            };
            facts.push(VerifiedFact::new(
                candidate.receipt().clone(),
                payload.clone(),
            ));
        }

        let view = request
            .reducer
            .reduce(facts)
            .map_err(ProjectionError::Reducer)?;
        let freshness = if health.has_rejections() {
            ProjectionFreshness::Degraded
        } else {
            ProjectionFreshness::Fresh
        };
        self.record_ready(&request, candidate_set.source_cursor());

        Ok(ProjectionSnapshot::new(
            view,
            Some(candidate_set.source_cursor()),
            freshness,
            health,
        ))
    }

    fn catch_up(&self, request: ProjectionCatchUpRequest) -> Result<ProjectionCatchUp> {
        let key = ProjectionStateKey::new(request.view.clone(), request.query.clone());
        let expired = SystemTime::now() >= request.deadline;
        match self.state.borrow().get(&key) {
            Some(ProjectionState::Ready { source_cursor }) if *source_cursor >= request.cursor => {
                Ok(ProjectionCatchUp::CaughtUp {
                    view: request.view,
                    source_cursor: *source_cursor,
                })
            }
            Some(ProjectionState::Failed {
                source_cursor,
                health,
            }) => Ok(ProjectionCatchUp::ProjectionFailed {
                view: request.view,
                requested: request.cursor,
                source_cursor: *source_cursor,
                health: health.clone(),
            }),
            Some(ProjectionState::Ready { source_cursor }) if expired => {
                Ok(ProjectionCatchUp::TimedOut {
                    view: request.view,
                    requested: request.cursor,
                    current: Some(*source_cursor),
                })
            }
            Some(ProjectionState::Ready { .. }) => Ok(ProjectionCatchUp::FreshnessUnknown {
                view: request.view,
                requested: request.cursor,
            }),
            None if expired => Ok(ProjectionCatchUp::TimedOut {
                view: request.view,
                requested: request.cursor,
                current: None,
            }),
            None => Ok(ProjectionCatchUp::FreshnessUnknown {
                view: request.view,
                requested: request.cursor,
            }),
        }
    }
}

impl<S> MemoryProjectionSource<S> {
    fn record_ready<R>(&self, request: &ProjectionRequest<R>, source_cursor: FactCursor) {
        self.state.borrow_mut().insert(
            ProjectionStateKey::for_request(request),
            ProjectionState::Ready { source_cursor },
        );
    }

    fn record_failed<R>(
        &self,
        request: &ProjectionRequest<R>,
        source_cursor: FactCursor,
        health: ProjectionHealth,
    ) {
        self.state.borrow_mut().insert(
            ProjectionStateKey::for_request(request),
            ProjectionState::Failed {
                source_cursor,
                health,
            },
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct ProjectionStateKey {
    view: ProjectionView,
    query: FactQuery,
}

impl ProjectionStateKey {
    fn new(view: ProjectionView, query: FactQuery) -> Self {
        Self { view, query }
    }

    fn for_request<R>(request: &ProjectionRequest<R>) -> Self {
        Self::new(request.view.clone(), request.query.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ProjectionState {
    Ready {
        source_cursor: FactCursor,
    },
    Failed {
        source_cursor: FactCursor,
        health: ProjectionHealth,
    },
}

fn health_for_candidates(candidate_set: &FactCandidateSet) -> ProjectionHealth {
    let mut health = ProjectionHealth::new();
    for candidate in candidate_set.iter() {
        health.record_candidate(candidate.status());
    }
    health
}

fn verified_candidates(candidates: &[FactCandidate]) -> Vec<FactCandidate> {
    candidates
        .iter()
        .filter(|candidate| candidate.status() == CandidateStatus::Verified)
        .cloned()
        .collect()
}

fn is_projectable_status(status: CandidateStatus) -> bool {
    match status {
        CandidateStatus::Verified => true,
        CandidateStatus::Conflict
        | CandidateStatus::Unauthorized
        | CandidateStatus::Unverified
        | CandidateStatus::MissingPayload
        | CandidateStatus::SubstrateMalformed
        | CandidateStatus::CrossScope => false,
    }
}

fn is_fatal_status(status: CandidateStatus) -> bool {
    match status {
        CandidateStatus::Unverified
        | CandidateStatus::MissingPayload
        | CandidateStatus::SubstrateMalformed => true,
        CandidateStatus::Verified
        | CandidateStatus::Conflict
        | CandidateStatus::Unauthorized
        | CandidateStatus::CrossScope => false,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::authority::{Authority, AuthorityContext, AuthorityDecision, AuthorityService};
    use crate::facts::{
        FactAppendOutcome, FactAppendRequest, FactAppendScope, FactCandidate, FactCandidateSet,
        FactGrantAuthority, FactGrantDecision, FactGrantPurpose, FactGrantService, FactKey,
        FactKind, FactPayload, FactPayloadBatch, FactPayloadReadFailure, FactReceipt,
        MemoryFactStore,
    };
    use crate::identity::{PrincipalId, ScopeId};
    use crate::operations::{IdempotencyKey, OperationId};
    use crate::{FactTarget, GrantEpoch, ResourceId};

    use super::*;

    #[derive(Clone)]
    struct CountReducer;

    impl FactReducer for CountReducer {
        type View = usize;
        type Error = DecodeError;

        fn reduce(&self, facts: Vec<VerifiedFact>) -> std::result::Result<Self::View, Self::Error> {
            Ok(facts.len())
        }
    }

    #[derive(Clone)]
    struct DecodeReducer;

    impl FactReducer for DecodeReducer {
        type View = Vec<String>;
        type Error = DecodeError;

        fn reduce(&self, facts: Vec<VerifiedFact>) -> std::result::Result<Self::View, Self::Error> {
            facts
                .into_iter()
                .map(|fact| {
                    String::from_utf8(fact.payload().as_bytes().to_vec())
                        .map_err(|_error| DecodeError::InvalidUtf8)
                })
                .collect()
        }
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    enum DecodeError {
        InvalidUtf8,
    }

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

    struct StaticFactStore {
        candidates: Vec<FactCandidate>,
        payloads: BTreeMap<crate::FactId, FactPayload>,
        source_cursor: FactCursor,
    }

    impl FactStore for StaticFactStore {
        fn append(&self, _request: FactAppendRequest) -> Result<FactAppendOutcome> {
            Err(Error::MalformedPayload)
        }

        fn list_candidates(&self, query: FactQuery) -> Result<FactCandidateSet> {
            Ok(FactCandidateSet::complete_prefix(
                self.candidates
                    .iter()
                    .filter(|candidate| query.matches(candidate.receipt()))
                    .cloned()
                    .collect(),
                self.source_cursor,
            ))
        }

        fn read_payloads(&self, candidates: &[FactCandidate]) -> Result<FactPayloadBatch> {
            let mut payloads = BTreeMap::new();
            let mut failures = BTreeMap::new();
            for candidate in candidates {
                match self.payloads.get(candidate.receipt().id()) {
                    Some(payload) => {
                        payloads.insert(candidate.receipt().id().clone(), payload.clone());
                    }
                    None => {
                        failures.insert(
                            candidate.receipt().id().clone(),
                            FactPayloadReadFailure::MissingPayload,
                        );
                    }
                }
            }
            Ok(FactPayloadBatch::from_parts(payloads, failures))
        }
    }

    fn scope() -> ScopeId {
        ScopeId::parse("cluster").expect("scope")
    }

    fn projection_view() -> ProjectionView {
        ProjectionView::new(ProjectionKey::parse("machine-membership").expect("projection key"))
    }

    fn query() -> FactQuery {
        FactQuery::new(scope())
    }

    fn catch_up_request(
        view: ProjectionView,
        query: FactQuery,
        cursor: FactCursor,
        deadline: SystemTime,
    ) -> ProjectionCatchUpRequest {
        ProjectionCatchUpRequest::new(view, query, cursor, deadline)
    }

    fn future_deadline() -> SystemTime {
        SystemTime::now() + Duration::from_secs(60)
    }

    fn store_with_fact(payload: &[u8]) -> MemoryFactStore {
        let store = MemoryFactStore::new();
        store
            .append(request("op-1", "idem-1", payload))
            .expect("append");
        store
    }

    fn request(operation: &str, idempotency: &str, payload: &[u8]) -> FactAppendRequest {
        let authority = AuthorityService::new(AllowAuthority)
            .authorize::<FactAppendScope>(PrincipalId::parse("node-a").expect("principal"), scope())
            .expect("authorized");
        let target = FactTarget::new(
            ResourceId::parse("machine:node-a").expect("resource"),
            FactKey::parse("membership/node-a").expect("key"),
            FactKind::parse("ployz.machine.joined.v1").expect("kind"),
        );
        let grant = FactGrantService::new(AllowGrantAuthority)
            .issue_append(&authority, target.clone())
            .expect("grant");
        FactAppendRequest::new(
            OperationId::parse(operation).expect("operation"),
            IdempotencyKey::parse(idempotency).expect("idempotency"),
            authority,
            grant,
            target,
            FactPayload::new(payload.to_vec()).expect("payload"),
            None,
        )
    }

    fn receipt_for(payload: &[u8], cursor: FactCursor) -> (FactReceipt, FactPayload) {
        let append = request("op-static", "idem-static", payload)
            .validate()
            .expect("validated append");
        let receipt = FactReceipt::from_validated_append(&append, cursor);
        (
            receipt,
            FactPayload::new(payload.to_vec()).expect("payload"),
        )
    }

    #[test]
    fn full_rebuild_after_clearing_projection_cache_yields_same_view() {
        let source = MemoryProjectionSource::new(store_with_fact(b"joined"));
        let request = ProjectionRequest::new(projection_view(), query(), CountReducer);

        let first = source.project(request.clone()).expect("first projection");
        source.clear_projection_cache();
        let second = source.project(request).expect("rebuilt projection");

        assert_eq!(first.view(), second.view());
        assert_eq!(first.source_cursor(), second.source_cursor());
    }

    #[test]
    fn missing_payload_prevents_verified_projection_and_reports_failure() {
        let (receipt, _payload) = receipt_for(b"joined", FactCursor::new(1));
        let store = StaticFactStore {
            candidates: vec![FactCandidate::new(receipt, CandidateStatus::MissingPayload)],
            payloads: BTreeMap::new(),
            source_cursor: FactCursor::new(1),
        };
        let source = MemoryProjectionSource::new(store);

        let result = source.project(ProjectionRequest::new(
            projection_view(),
            query(),
            CountReducer,
        ));

        assert!(matches!(
            result,
            Err(ProjectionError::SubstrateFailed { health })
                if health.candidate_count(CandidateStatus::MissingPayload) == 1
        ));
    }

    #[test]
    fn verified_candidate_missing_payload_records_payload_failure() {
        let (receipt, _payload) = receipt_for(b"joined", FactCursor::new(1));
        let store = StaticFactStore {
            candidates: vec![FactCandidate::new(receipt, CandidateStatus::Verified)],
            payloads: BTreeMap::new(),
            source_cursor: FactCursor::new(1),
        };
        let source = MemoryProjectionSource::new(store);

        let result = source.project(ProjectionRequest::new(
            projection_view(),
            query(),
            CountReducer,
        ));

        assert!(matches!(
            result,
            Err(ProjectionError::SubstrateFailed { health })
                if health.payload_failure_count(FactPayloadReadFailure::MissingPayload) == 1
        ));
    }

    #[test]
    fn unverified_candidate_blocks_projection_catch_up() {
        let (receipt, payload) = receipt_for(b"joined", FactCursor::new(1));
        let store = StaticFactStore {
            candidates: vec![FactCandidate::new(
                receipt.clone(),
                CandidateStatus::Unverified,
            )],
            payloads: BTreeMap::from([(receipt.id().clone(), payload)]),
            source_cursor: FactCursor::new(1),
        };
        let source = MemoryProjectionSource::new(store);
        let view = projection_view();
        let query = query();

        let projection = source.project(ProjectionRequest::new(
            view.clone(),
            query.clone(),
            CountReducer,
        ));
        let catch_up = source
            .catch_up(catch_up_request(
                view,
                query,
                FactCursor::new(1),
                future_deadline(),
            ))
            .expect("catch up");

        assert!(matches!(
            projection,
            Err(ProjectionError::SubstrateFailed { health })
                if health.candidate_count(CandidateStatus::Unverified) == 1
        ));
        assert!(matches!(
            catch_up,
            ProjectionCatchUp::ProjectionFailed { source_cursor, .. }
                if source_cursor == FactCursor::new(1)
        ));
    }

    #[test]
    fn catch_up_succeeds_only_when_projection_has_consumed_requested_cursor() {
        let source = MemoryProjectionSource::new(store_with_fact(b"joined"));
        let view = projection_view();
        let query = query();
        source
            .project(ProjectionRequest::new(
                view.clone(),
                query.clone(),
                CountReducer,
            ))
            .expect("projection");

        let caught_up = source
            .catch_up(catch_up_request(
                view.clone(),
                query.clone(),
                FactCursor::new(1),
                future_deadline(),
            ))
            .expect("catch up");
        let timed_out = source
            .catch_up(catch_up_request(
                view,
                query,
                FactCursor::new(2),
                UNIX_EPOCH,
            ))
            .expect("catch up");

        assert!(matches!(
            caught_up,
            ProjectionCatchUp::CaughtUp { source_cursor, .. }
                if source_cursor == FactCursor::new(1)
        ));
        assert!(matches!(
            timed_out,
            ProjectionCatchUp::TimedOut {
                current: Some(current),
                ..
            } if current == FactCursor::new(1)
        ));
    }

    #[test]
    fn catch_up_is_scoped_to_projection_query_identity() {
        let source = MemoryProjectionSource::new(store_with_fact(b"joined"));
        let view = projection_view();
        let narrow_query = query().key(FactKey::parse("membership/other").expect("key"));
        let broad_query = query();
        source
            .project(ProjectionRequest::new(
                view.clone(),
                narrow_query,
                CountReducer,
            ))
            .expect("projection");

        let catch_up = source
            .catch_up(catch_up_request(
                view,
                broad_query,
                FactCursor::new(1),
                future_deadline(),
            ))
            .expect("catch up");

        assert!(matches!(
            catch_up,
            ProjectionCatchUp::FreshnessUnknown { .. }
        ));
    }

    #[test]
    fn unknown_freshness_is_explicit_before_projection_exists() {
        let source = MemoryProjectionSource::new(store_with_fact(b"joined"));

        let catch_up = source
            .catch_up(catch_up_request(
                projection_view(),
                query(),
                FactCursor::new(1),
                future_deadline(),
            ))
            .expect("catch up");

        assert!(matches!(
            catch_up,
            ProjectionCatchUp::FreshnessUnknown { .. }
        ));
    }

    #[test]
    fn no_projection_state_times_out_when_deadline_is_expired() {
        let source = MemoryProjectionSource::new(store_with_fact(b"joined"));

        let catch_up = source
            .catch_up(catch_up_request(
                projection_view(),
                query(),
                FactCursor::new(1),
                UNIX_EPOCH,
            ))
            .expect("catch up");

        assert!(matches!(
            catch_up,
            ProjectionCatchUp::TimedOut { current: None, .. }
        ));
    }

    #[test]
    fn projection_health_reports_redacted_rejected_candidate_counts() {
        let (receipt, _payload) = receipt_for(b"joined", FactCursor::new(1));
        let store = StaticFactStore {
            candidates: vec![FactCandidate::new(receipt, CandidateStatus::Unauthorized)],
            payloads: BTreeMap::new(),
            source_cursor: FactCursor::new(1),
        };
        let source = MemoryProjectionSource::new(store);

        let snapshot = source
            .project(ProjectionRequest::new(
                projection_view(),
                query(),
                CountReducer,
            ))
            .expect("projection");

        assert_eq!(snapshot.view(), &0);
        assert_eq!(
            snapshot
                .health()
                .candidate_count(CandidateStatus::Unauthorized),
            1
        );
        assert_eq!(snapshot.freshness(), ProjectionFreshness::Degraded);
    }

    #[test]
    fn product_decode_failure_is_reducer_error_not_candidate_status() {
        let source = MemoryProjectionSource::new(store_with_fact(&[0xff]));

        let result = source.project(ProjectionRequest::new(
            projection_view(),
            query(),
            DecodeReducer,
        ));

        assert_eq!(
            result,
            Err(ProjectionError::Reducer(DecodeError::InvalidUtf8))
        );
    }
}
