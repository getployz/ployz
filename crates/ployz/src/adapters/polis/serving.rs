use std::time::SystemTime;

use crate::error::ServingFailure;
use crate::facts::{
    ProductFact, ProductFactAppendOutcome, ProductFactConflict, ProductFactRejection,
    ProductProjectionCatchUp,
    serving::{
        SERVING_COMMIT_KIND, SERVING_GENERATION_SLOT_KIND, ServingCommitFact, ServingCommitReducer,
        ServingFactError, ServingGenerationSlotFact,
    },
};
use crate::operation::{MutationContext, ScopeId};
use crate::serving::{
    RouteId, ServingCommitObservation, ServingCommitReceipt, ServingCommitRequest,
    ServingProjectionCatchUp, ServingProjectionCursor, ServingRouteState, ServingSnapshotPort,
};

#[derive(Debug)]
pub(crate) struct PolisServingSnapshots<S> {
    scope: ScopeId,
    projection: polis::MemoryProjectionSource<S>,
}

impl<S> PolisServingSnapshots<S> {
    #[must_use]
    pub(crate) fn new(scope: ScopeId, projection: polis::MemoryProjectionSource<S>) -> Self {
        Self { scope, projection }
    }
}

impl PolisServingSnapshots<polis::MemoryFactStore> {
    #[must_use]
    pub(crate) fn in_memory(scope: ScopeId) -> Self {
        Self::new(
            scope,
            polis::MemoryProjectionSource::new(polis::MemoryFactStore::new()),
        )
    }
}

impl<S> ServingSnapshotPort for PolisServingSnapshots<S>
where
    S: polis::FactStore,
{
    type Receipt = ServingCommitReceipt;

    fn commit_snapshot(
        &self,
        context: &MutationContext,
        request: &ServingCommitRequest,
    ) -> Result<ServingCommitReceipt, ServingFailure> {
        let slot = ServingGenerationSlotFact::from_request(request.clone())
            .map_err(map_serving_fact_error)?;
        self.append_serving_generation_slot(context, &slot)?;
        let fact =
            ServingCommitFact::from_request(request.clone()).map_err(map_serving_fact_error)?;
        let receipt = self.append_serving_commit(context, &fact)?;
        Ok(ServingCommitReceipt::new(
            fact.commit_id().clone(),
            request.commit_identity(),
            ServingProjectionCursor::from_product_cursor(receipt.cursor()),
        ))
    }

    fn commit_status(
        &self,
        request: &ServingCommitRequest,
    ) -> Result<ServingCommitObservation, ServingFailure> {
        let state = self.project_route_state(request.route())?;
        let Some(current) = state.current() else {
            return Ok(ServingCommitObservation::Missing);
        };
        if current.identity().generation() == request.generation() {
            return Ok(ServingCommitObservation::Current(current.clone()));
        }
        Ok(ServingCommitObservation::Missing)
    }

    fn catch_up_commits(
        &self,
        receipt: &ServingCommitReceipt,
        deadline: SystemTime,
    ) -> Result<ServingProjectionCatchUp, ServingFailure> {
        if SystemTime::now() >= deadline {
            return Ok(ServingProjectionCatchUp::TimedOut);
        }
        let route = receipt.identity().route();
        if self.project_route_state(route).is_err() {
            return Ok(ServingProjectionCatchUp::ProjectionFailed);
        }
        if SystemTime::now() >= deadline {
            return Ok(ServingProjectionCatchUp::TimedOut);
        }
        let request = polis::ProjectionCatchUpRequest::new(
            serving_projection_view(route)?,
            serving_fact_query(&self.scope, route)?,
            super::polis_fact_cursor(receipt.cursor().product_cursor()),
            deadline,
        );
        let catch_up = polis::ProjectionSource::catch_up(&self.projection, request)
            .map_err(map_serving_polis_error)?;
        Ok(map_serving_catch_up(super::product_projection_catch_up(
            catch_up,
        )))
    }
}

impl<S> PolisServingSnapshots<S>
where
    S: polis::FactStore,
{
    fn append_serving_commit(
        &self,
        context: &MutationContext,
        fact: &ServingCommitFact,
    ) -> Result<crate::facts::ProductFactReceipt, ServingFailure> {
        let envelope = fact.encode().map_err(map_serving_fact_error)?;
        match self.append_serving_fact(
            context,
            &envelope,
            polis::FactConflictPolicy::RecordCandidate,
        )? {
            ProductFactAppendOutcome::Appended(receipt)
            | ProductFactAppendOutcome::Replayed(receipt) => Ok(receipt),
            ProductFactAppendOutcome::Conflict(conflict) => Err(serving_conflict_failure(conflict)),
            ProductFactAppendOutcome::Rejected(ProductFactRejection::Unauthorized) => {
                Err(ServingFailure::SnapshotRejected)
            }
        }
    }

    fn append_serving_generation_slot(
        &self,
        context: &MutationContext,
        slot: &ServingGenerationSlotFact,
    ) -> Result<(), ServingFailure> {
        let envelope = slot.encode().map_err(map_serving_fact_error)?;
        match self.append_serving_fact(
            context,
            &envelope,
            polis::FactConflictPolicy::RejectCandidate,
        )? {
            ProductFactAppendOutcome::Appended(_) | ProductFactAppendOutcome::Replayed(_) => Ok(()),
            ProductFactAppendOutcome::Conflict(_) => Err(ServingFailure::ProjectionStale),
            ProductFactAppendOutcome::Rejected(ProductFactRejection::Unauthorized) => {
                Err(ServingFailure::SnapshotRejected)
            }
        }
    }

    fn append_serving_fact(
        &self,
        context: &MutationContext,
        envelope: &crate::facts::ProductFactEnvelope,
        conflict_policy: polis::FactConflictPolicy,
    ) -> Result<ProductFactAppendOutcome, ServingFailure> {
        let target = super::polis_fact_target(envelope.target()).map_err(map_serving_primitive)?;
        let payload =
            super::polis_fact_payload(envelope.payload()).map_err(map_serving_primitive)?;
        let authority =
            super::context_fact_append_authority(context).map_err(map_serving_primitive)?;
        let grant = polis::FactGrantService::new(ServingFactGrantAuthority)
            .issue_append(&authority, target.clone())
            .map_err(map_serving_polis_error)?;
        let request = polis::FactAppendRequest::new(
            polis::OperationId::parse(format!(
                "{}:{}",
                context.operation().as_str(),
                envelope.target().key().as_str()
            ))
            .map_err(map_serving_polis_error)?,
            polis::IdempotencyKey::parse(format!(
                "{}:{}",
                context.idempotency().as_str(),
                envelope.target().key().as_str()
            ))
            .map_err(map_serving_polis_error)?,
            authority,
            grant,
            target,
            payload,
            None,
        )
        .with_conflict_policy(conflict_policy);

        polis::FactStore::append(self.projection.facts(), request)
            .map_err(map_serving_polis_error)
            .and_then(|outcome| {
                super::product_fact_append_outcome(outcome).map_err(map_serving_primitive)
            })
    }

    fn project_route_state(&self, route: &RouteId) -> Result<ServingRouteState, ServingFailure> {
        let request = polis::ProjectionRequest::new(
            serving_projection_view(route)?,
            serving_fact_query(&self.scope, route)?,
            PolisServingReducer {
                reducer: ServingCommitReducer::new(route.clone()),
            },
        );
        polis::ProjectionSource::project(&self.projection, request)
            .map(|snapshot| snapshot.into_view())
            .map_err(map_serving_projection_error)
    }
}

struct PolisServingReducer {
    reducer: ServingCommitReducer,
}

impl polis::FactReducer for PolisServingReducer {
    type View = ServingRouteState;
    type Error = ServingFailure;

    fn reduce(
        &self,
        facts: Vec<polis::VerifiedFact>,
    ) -> std::result::Result<Self::View, Self::Error> {
        let mut commits = Vec::with_capacity(facts.len());
        for fact in facts {
            let decoded = ServingCommitFact::decode_payload(fact.payload().as_bytes())
                .map_err(map_serving_projection_fact_error)?;
            let expected_target = decoded
                .encode()
                .map_err(map_serving_projection_fact_error)?
                .target()
                .clone();
            let actual_target = super::product_fact_receipt(fact.receipt())
                .map_err(map_serving_primitive)?
                .target()
                .clone();
            if actual_target != expected_target {
                return Err(ServingFailure::ProjectionStale);
            }
            commits.push(decoded);
        }
        self.reducer
            .reduce(commits)
            .map_err(map_serving_projection_fact_error)
    }
}

struct ServingFactGrantAuthority;

impl polis::FactGrantAuthority for ServingFactGrantAuthority {
    fn decide(
        &self,
        _authority: &polis::AuthorityContext,
        target: &polis::FactTarget,
        purpose: polis::FactGrantPurpose,
    ) -> polis::FactGrantDecision {
        match purpose {
            polis::FactGrantPurpose::Append if serving_fact_target_allowed(target) => {
                polis::FactGrantDecision::allowed()
            }
            polis::FactGrantPurpose::Append | polis::FactGrantPurpose::ReplicaImport => {
                polis::FactGrantDecision::denied()
            }
        }
    }
}

fn serving_fact_target_allowed(target: &polis::FactTarget) -> bool {
    match target.kind().as_str() {
        SERVING_COMMIT_KIND => target.resource().as_str().starts_with("serving-route:"),
        SERVING_GENERATION_SLOT_KIND => target
            .resource()
            .as_str()
            .starts_with("serving-route-generation:"),
        _ => false,
    }
}

fn serving_projection_view(route: &RouteId) -> Result<polis::ProjectionView, ServingFailure> {
    polis::ProjectionKey::parse(format!("ployz.serving.route:{}", route.as_str()))
        .map(polis::ProjectionView::new)
        .map_err(map_serving_polis_error)
}

fn serving_fact_query(
    scope: &ScopeId,
    route: &RouteId,
) -> Result<polis::FactQuery, ServingFailure> {
    let scope = polis::ScopeId::parse(scope.as_str()).map_err(map_serving_polis_error)?;
    let resource = polis::ResourceId::parse(format!("serving-route:{}", route.as_str()))
        .map_err(map_serving_polis_error)?;
    Ok(polis::FactQuery::new(scope).resource(resource))
}

fn serving_conflict_failure(_conflict: ProductFactConflict) -> ServingFailure {
    ServingFailure::SnapshotRejected
}

fn map_serving_catch_up(catch_up: ProductProjectionCatchUp) -> ServingProjectionCatchUp {
    match catch_up {
        ProductProjectionCatchUp::CaughtUp { .. } => ServingProjectionCatchUp::CaughtUp,
        ProductProjectionCatchUp::TimedOut { .. } => ServingProjectionCatchUp::TimedOut,
        ProductProjectionCatchUp::FreshnessUnknown { .. } => {
            ServingProjectionCatchUp::FreshnessUnknown
        }
        ProductProjectionCatchUp::ProjectionFailed { .. } => {
            ServingProjectionCatchUp::ProjectionFailed
        }
    }
}

fn map_serving_projection_error(error: polis::ProjectionError<ServingFailure>) -> ServingFailure {
    match error {
        polis::ProjectionError::Source(source) => map_serving_polis_error(source),
        polis::ProjectionError::SubstrateFailed { .. } => ServingFailure::ProjectionStale,
        polis::ProjectionError::Reducer(failure) => failure,
    }
}

fn map_serving_primitive(failure: crate::error::PrimitiveFailure) -> ServingFailure {
    match failure {
        crate::error::PrimitiveFailure::Unauthorized
        | crate::error::PrimitiveFailure::Conflict
        | crate::error::PrimitiveFailure::StaleFence
        | crate::error::PrimitiveFailure::MalformedPayload => ServingFailure::SnapshotRejected,
        crate::error::PrimitiveFailure::Timeout
        | crate::error::PrimitiveFailure::NoResponder
        | crate::error::PrimitiveFailure::FreshnessUnknown => {
            ServingFailure::LiveObservationUnknown
        }
        crate::error::PrimitiveFailure::OperationStateConflict
        | crate::error::PrimitiveFailure::OperationAlreadySucceeded
        | crate::error::PrimitiveFailure::OperationInProgress
        | crate::error::PrimitiveFailure::OperationAlreadyFailed
        | crate::error::PrimitiveFailure::OperationInterrupted => ServingFailure::ProjectionStale,
    }
}

fn map_serving_polis_error(error: polis::Error) -> ServingFailure {
    map_serving_primitive(super::map_polis_error(error))
}

fn map_serving_fact_error(_error: ServingFactError) -> ServingFailure {
    ServingFailure::SnapshotRejected
}

fn map_serving_projection_fact_error(_error: ServingFactError) -> ServingFailure {
    ServingFailure::ProjectionStale
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::acme::Hostname;
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, MutationContext, OperationId,
        PrincipalId, ScopeId,
    };
    use crate::serving::{
        ServingCommitRequest, ServingCommittedSnapshot, ServingGeneration,
        ServingProjectionCatchUp, ServingSnapshotPort, ServingTarget,
    };

    use super::*;

    #[test]
    fn projects_committed_serving_snapshot_for_exact_route_identity() {
        let snapshots = serving_snapshots();
        let request = commit_request(7);
        snapshots
            .commit_snapshot(&context(), &request)
            .expect("commit");

        assert_eq!(
            snapshots.commit_status(&request).expect("status"),
            ServingCommitObservation::Current(ServingCommittedSnapshot::new(
                ServingCommitFact::from_request(request.clone())
                    .expect("fact")
                    .commit_id()
                    .clone(),
                request.commit_identity(),
            ))
        );
    }

    #[test]
    fn newer_generation_supersedes_older_serving_commit() {
        let snapshots = serving_snapshots();
        let old = commit_request(7);
        let new = commit_request(9);
        snapshots
            .commit_snapshot(&context(), &old)
            .expect("old commit");
        snapshots
            .commit_snapshot(&context(), &new)
            .expect("new commit");

        assert_eq!(
            snapshots.commit_status(&new).expect("new status"),
            ServingCommitObservation::Current(ServingCommittedSnapshot::new(
                ServingCommitFact::from_request(new.clone())
                    .expect("fact")
                    .commit_id()
                    .clone(),
                new.commit_identity(),
            ))
        );
        assert_eq!(
            snapshots.commit_status(&old).expect("old status"),
            ServingCommitObservation::Missing
        );
    }

    #[test]
    fn catch_up_reports_committed_snapshot_visibility() {
        let snapshots = serving_snapshots();
        let request = commit_request(7);
        let receipt = snapshots
            .commit_snapshot(&context(), &request)
            .expect("commit");

        assert_eq!(
            snapshots
                .catch_up_commits(&receipt, SystemTime::now() + Duration::from_secs(60))
                .expect("catch up"),
            ServingProjectionCatchUp::CaughtUp
        );
    }

    #[test]
    fn catch_up_respects_expired_deadline() {
        let snapshots = serving_snapshots();
        let request = commit_request(7);
        let receipt = snapshots
            .commit_snapshot(&context(), &request)
            .expect("commit");

        assert_eq!(
            snapshots
                .catch_up_commits(&receipt, UNIX_EPOCH)
                .expect("catch up"),
            ServingProjectionCatchUp::TimedOut
        );
    }

    #[test]
    fn same_generation_route_conflict_is_rejected_before_route_projection_is_poisoned() {
        let snapshots = serving_snapshots();
        let request = commit_request(7);
        let conflicting = ServingCommitRequest::new(
            request.route().clone(),
            Hostname::parse("admin.example.com").expect("hostname"),
            ServingTarget::parse("gateway-b").expect("target"),
            request.generation(),
        );
        snapshots
            .commit_snapshot(&context(), &request)
            .expect("first commit");

        assert_eq!(
            snapshots.commit_snapshot(&context_for("deploy-2", "deploy-2"), &conflicting),
            Err(ServingFailure::ProjectionStale)
        );
        assert_eq!(
            snapshots
                .catch_up_commits(
                    &snapshots
                        .commit_snapshot(&context_for("deploy-3", "deploy-3"), &request)
                        .expect("idempotent matching commit"),
                    SystemTime::now() + Duration::from_secs(60),
                )
                .expect("catch up"),
            ServingProjectionCatchUp::CaughtUp
        );
    }

    #[test]
    fn same_generation_route_conflict_is_observed_before_append() {
        let snapshots = serving_snapshots();
        let request = commit_request(7);
        let conflicting = ServingCommitRequest::new(
            request.route().clone(),
            Hostname::parse("admin.example.com").expect("hostname"),
            ServingTarget::parse("gateway-b").expect("target"),
            request.generation(),
        );
        snapshots
            .commit_snapshot(&context(), &request)
            .expect("first commit");

        assert_eq!(
            snapshots
                .commit_status(&conflicting)
                .expect("conflict status")
                .try_confirm_commit(&conflicting),
            Err(ServingFailure::ProjectionStale)
        );
    }

    #[test]
    fn same_generation_route_conflict_is_rejected_without_poisoning_route_projection() {
        let snapshots = serving_snapshots();
        let request = commit_request(7);
        let conflicting = ServingCommitRequest::new(
            request.route().clone(),
            Hostname::parse("admin.example.com").expect("hostname"),
            ServingTarget::parse("gateway-b").expect("target"),
            request.generation(),
        );
        snapshots
            .commit_snapshot(&context(), &request)
            .expect("first commit");

        assert_eq!(
            snapshots.commit_snapshot(&context_for("deploy-2", "deploy-2"), &conflicting),
            Err(ServingFailure::ProjectionStale)
        );
        assert!(matches!(
            snapshots.commit_status(&request).expect("status"),
            ServingCommitObservation::Current(current) if current.matches_request(&request)
        ));
    }

    fn serving_snapshots() -> PolisServingSnapshots<polis::MemoryFactStore> {
        PolisServingSnapshots::in_memory(ScopeId::parse("cluster").expect("scope"))
    }

    fn context() -> MutationContext {
        context_for("deploy-1", "deploy-1")
    }

    fn context_for(operation: &str, idempotency: &str) -> MutationContext {
        MutationContext::new(
            OperationId::parse(operation).expect("operation"),
            IdempotencyKey::parse(idempotency).expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn commit_request(generation: u64) -> ServingCommitRequest {
        ServingCommitRequest::new(
            RouteId::parse("route-app").expect("route"),
            Hostname::parse("app.example.com").expect("hostname"),
            ServingTarget::parse("gateway-a").expect("target"),
            ServingGeneration::new(generation),
        )
    }
}
