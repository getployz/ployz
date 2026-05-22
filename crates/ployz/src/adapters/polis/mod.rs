//! Polis adapter helpers for Ployz composition code.

mod acme;
mod domain;
mod serving;

use std::collections::BTreeMap;

#[allow(
    unused_imports,
    reason = "ACME attempt adapter is intentionally kept for external-state issuance before production composition wiring"
)]
pub(crate) use acme::AttemptingCertificateIssuer;
pub(crate) use domain::PolisDomainStatus;
pub(crate) use serving::PolisServingSnapshots;

use crate::error::{MachineFailure, PrimitiveFailure};
use crate::facts::{
    ProductCandidateRejection, ProductFact, ProductFactAppendOutcome, ProductFactConflict,
    ProductFactCursor, ProductFactKey, ProductFactKind, ProductFactPayload, ProductFactReceipt,
    ProductFactRejection, ProductFactResource, ProductFactTarget, ProductPayloadFailure,
    ProductProjectionCatchUp, ProductProjectionFreshness, ProductProjectionHealth,
    ProductProjectionSnapshot,
    machine::{
        MACHINE_JOINED_KIND, MACHINE_REMOVAL_STARTED_KIND, MACHINE_TOMBSTONED_KIND,
        MachineFactError, MachineMembershipFact, MachineMembershipReducer,
    },
};
use crate::machine::{MachineId, MachineMembership, MachineMembershipPort, MachineStatus};
use crate::operation::MutationContext;

#[must_use]
pub fn map_polis_error(error: polis::Error) -> PrimitiveFailure {
    match error {
        polis::Error::Unauthorized => PrimitiveFailure::Unauthorized,
        polis::Error::Conflict => PrimitiveFailure::Conflict,
        polis::Error::Timeout => PrimitiveFailure::Timeout,
        polis::Error::StaleFence => PrimitiveFailure::StaleFence,
        polis::Error::NoResponder => PrimitiveFailure::NoResponder,
        polis::Error::FreshnessUnknown => PrimitiveFailure::FreshnessUnknown,
        polis::Error::MalformedPayload => PrimitiveFailure::MalformedPayload,
        polis::Error::TerminalAlreadyWritten => PrimitiveFailure::OperationStateConflict,
    }
}

pub fn polis_fact_target(
    target: &ProductFactTarget,
) -> Result<polis::FactTarget, PrimitiveFailure> {
    let resource = polis_fact_resource(target.resource())?;
    let key = polis::FactKey::parse(target.key().as_str()).map_err(map_polis_error)?;
    let kind = polis::FactKind::parse(target.kind().as_str()).map_err(map_polis_error)?;
    Ok(polis::FactTarget::new(resource, key, kind))
}

pub fn polis_fact_payload(
    payload: &ProductFactPayload,
) -> Result<polis::FactPayload, PrimitiveFailure> {
    polis::FactPayload::new(payload.as_bytes().to_vec()).map_err(map_polis_error)
}

pub fn polis_fact_resource(
    resource: &ProductFactResource,
) -> Result<polis::ResourceId, PrimitiveFailure> {
    polis::ResourceId::parse(resource.as_str()).map_err(map_polis_error)
}

#[must_use]
pub fn polis_fact_cursor(cursor: ProductFactCursor) -> polis::FactCursor {
    polis::FactCursor::new(cursor.value())
}

#[must_use]
pub fn product_fact_cursor(cursor: polis::FactCursor) -> ProductFactCursor {
    ProductFactCursor::new(cursor.value())
}

pub fn product_fact_receipt(
    receipt: &polis::FactReceipt,
) -> Result<ProductFactReceipt, PrimitiveFailure> {
    Ok(ProductFactReceipt::new(
        ProductFactTarget::new(
            ProductFactResource::parse(receipt.resource().as_str())?,
            ProductFactKey::parse(receipt.key().as_str())?,
            ProductFactKind::parse(receipt.kind().as_str())?,
        ),
        product_fact_cursor(receipt.cursor()),
    ))
}

pub fn product_fact_append_outcome(
    outcome: polis::FactAppendOutcome,
) -> Result<ProductFactAppendOutcome, PrimitiveFailure> {
    match outcome {
        polis::FactAppendOutcome::Appended(receipt) => {
            product_fact_receipt(&receipt).map(ProductFactAppendOutcome::Appended)
        }
        polis::FactAppendOutcome::Replayed(receipt) => {
            product_fact_receipt(&receipt).map(ProductFactAppendOutcome::Replayed)
        }
        polis::FactAppendOutcome::Conflict(conflict) => {
            product_fact_conflict(&conflict).map(ProductFactAppendOutcome::Conflict)
        }
        polis::FactAppendOutcome::Rejected(polis::FactRejection::Unauthorized) => Ok(
            ProductFactAppendOutcome::Rejected(ProductFactRejection::Unauthorized),
        ),
    }
}

fn product_fact_conflict(
    conflict: &polis::FactConflict,
) -> Result<ProductFactConflict, PrimitiveFailure> {
    match conflict {
        polis::FactConflict::IdempotencyKeyReuse { existing } => product_fact_receipt(existing)
            .map(|existing| ProductFactConflict::IdempotencyKeyReuse { existing }),
        polis::FactConflict::KeyPayloadConflict {
            existing,
            new_candidate,
        } => Ok(ProductFactConflict::KeyPayloadConflict {
            existing: product_fact_receipt(existing)?,
            new_candidate: product_fact_receipt(new_candidate)?,
        }),
        polis::FactConflict::RejectedKeyPayloadConflict { existing } => {
            Ok(ProductFactConflict::RejectedKeyPayloadConflict {
                existing: product_fact_receipt(existing)?,
            })
        }
    }
}

#[must_use]
pub fn product_projection_freshness(
    freshness: polis::ProjectionFreshness,
) -> ProductProjectionFreshness {
    match freshness {
        polis::ProjectionFreshness::Fresh => ProductProjectionFreshness::Fresh,
        polis::ProjectionFreshness::Degraded => ProductProjectionFreshness::Degraded,
        polis::ProjectionFreshness::Unknown => ProductProjectionFreshness::Unknown,
    }
}

#[must_use]
pub fn product_projection_health(health: &polis::ProjectionHealth) -> ProductProjectionHealth {
    let mut candidate_rejections = BTreeMap::new();
    insert_candidate_count(
        &mut candidate_rejections,
        ProductCandidateRejection::Conflict,
        health.candidate_count(polis::CandidateStatus::Conflict),
    );
    insert_candidate_count(
        &mut candidate_rejections,
        ProductCandidateRejection::Unauthorized,
        health.candidate_count(polis::CandidateStatus::Unauthorized),
    );
    insert_candidate_count(
        &mut candidate_rejections,
        ProductCandidateRejection::Unverified,
        health.candidate_count(polis::CandidateStatus::Unverified),
    );
    insert_candidate_count(
        &mut candidate_rejections,
        ProductCandidateRejection::MissingPayload,
        health.candidate_count(polis::CandidateStatus::MissingPayload),
    );
    insert_candidate_count(
        &mut candidate_rejections,
        ProductCandidateRejection::SubstrateMalformed,
        health.candidate_count(polis::CandidateStatus::SubstrateMalformed),
    );
    insert_candidate_count(
        &mut candidate_rejections,
        ProductCandidateRejection::CrossScope,
        health.candidate_count(polis::CandidateStatus::CrossScope),
    );

    let mut payload_failures = BTreeMap::new();
    insert_payload_count(
        &mut payload_failures,
        ProductPayloadFailure::UnknownCandidate,
        health.payload_failure_count(polis::FactPayloadReadFailure::UnknownCandidate),
    );
    insert_payload_count(
        &mut payload_failures,
        ProductPayloadFailure::CandidateMismatch,
        health.payload_failure_count(polis::FactPayloadReadFailure::CandidateMismatch),
    );
    insert_payload_count(
        &mut payload_failures,
        ProductPayloadFailure::MissingPayload,
        health.payload_failure_count(polis::FactPayloadReadFailure::MissingPayload),
    );
    insert_payload_count(
        &mut payload_failures,
        ProductPayloadFailure::DigestMismatch,
        health.payload_failure_count(polis::FactPayloadReadFailure::DigestMismatch),
    );

    ProductProjectionHealth::new(candidate_rejections, payload_failures)
}

#[must_use]
pub fn product_projection_snapshot<V>(
    snapshot: polis::ProjectionSnapshot<V>,
) -> ProductProjectionSnapshot<V> {
    let source_cursor = snapshot.source_cursor().map(product_fact_cursor);
    let freshness = product_projection_freshness(snapshot.freshness());
    let health = product_projection_health(snapshot.health());
    ProductProjectionSnapshot::new(snapshot.into_view(), source_cursor, freshness, health)
}

pub fn require_fresh_projection<V, E>(
    snapshot: polis::ProjectionSnapshot<V>,
    degraded: impl FnOnce(ProductProjectionHealth) -> E,
) -> Result<V, E> {
    let snapshot = product_projection_snapshot(snapshot);
    if snapshot.freshness() == ProductProjectionFreshness::Fresh
        && !snapshot.health().has_failures()
    {
        return Ok(snapshot.into_view());
    }
    Err(degraded(snapshot.health().clone()))
}

fn insert_candidate_count(
    counts: &mut BTreeMap<ProductCandidateRejection, usize>,
    rejection: ProductCandidateRejection,
    count: usize,
) {
    if count > 0 {
        counts.insert(rejection, count);
    }
}

fn insert_payload_count(
    counts: &mut BTreeMap<ProductPayloadFailure, usize>,
    failure: ProductPayloadFailure,
    count: usize,
) {
    if count > 0 {
        counts.insert(failure, count);
    }
}

#[must_use]
#[cfg(test)]
pub fn polis_projection_catch_up_request(
    view: polis::ProjectionView,
    query: polis::FactQuery,
    request: crate::facts::ProductProjectionCatchUpRequest,
) -> polis::ProjectionCatchUpRequest {
    polis::ProjectionCatchUpRequest::new(
        view,
        query,
        polis_fact_cursor(request.cursor()),
        request.deadline(),
    )
}

#[must_use]
pub fn product_projection_catch_up(catch_up: polis::ProjectionCatchUp) -> ProductProjectionCatchUp {
    match catch_up {
        polis::ProjectionCatchUp::CaughtUp { source_cursor, .. } => {
            ProductProjectionCatchUp::CaughtUp {
                source_cursor: product_fact_cursor(source_cursor),
            }
        }
        polis::ProjectionCatchUp::TimedOut {
            requested, current, ..
        } => ProductProjectionCatchUp::TimedOut {
            requested: product_fact_cursor(requested),
            current: current.map(product_fact_cursor),
        },
        polis::ProjectionCatchUp::FreshnessUnknown { requested, .. } => {
            ProductProjectionCatchUp::FreshnessUnknown {
                requested: product_fact_cursor(requested),
            }
        }
        polis::ProjectionCatchUp::ProjectionFailed {
            requested,
            source_cursor,
            health,
            ..
        } => ProductProjectionCatchUp::ProjectionFailed {
            requested: product_fact_cursor(requested),
            source_cursor: product_fact_cursor(source_cursor),
            health: product_projection_health(&health),
        },
    }
}

#[derive(Debug)]
pub struct PolisMachineMembership<S> {
    projection: polis::MemoryProjectionSource<S>,
}

impl<S> PolisMachineMembership<S> {
    #[must_use]
    pub fn new(projection: polis::MemoryProjectionSource<S>) -> Self {
        Self { projection }
    }
}

impl PolisMachineMembership<polis::MemoryFactStore> {
    #[must_use]
    pub fn in_memory() -> Self {
        Self::new(polis::MemoryProjectionSource::new(
            polis::MemoryFactStore::new(),
        ))
    }
}

impl<S> MachineMembershipPort for PolisMachineMembership<S>
where
    S: polis::FactStore,
{
    fn observe(
        &self,
        context: &MutationContext,
        machine: &MachineId,
    ) -> Result<MachineStatus, MachineFailure> {
        self.project_machine_status(context, machine)
    }

    fn join(
        &self,
        context: &MutationContext,
        membership: &MachineMembership,
    ) -> Result<MachineMembership, MachineFailure> {
        let fact = MachineMembershipFact::joined(membership.clone());
        match self.append_machine_fact(context, &fact)? {
            ProductFactAppendOutcome::Appended(_) | ProductFactAppendOutcome::Replayed(_) => {}
            ProductFactAppendOutcome::Conflict(conflict) => {
                return Err(machine_conflict_failure(conflict, membership));
            }
            ProductFactAppendOutcome::Rejected(ProductFactRejection::Unauthorized) => {
                return Err(MachineFailure::MutationRejected);
            }
        }

        match self.project_machine_status(context, &membership.machine)? {
            MachineStatus::Joined(joined) if joined == *membership => Ok(joined),
            MachineStatus::Joined(joined) => Err(MachineFailure::MembershipConflict {
                machine: joined.machine,
                epoch: Some(joined.epoch),
            }),
            MachineStatus::Conflicted { machine, epoch } => {
                Err(MachineFailure::MembershipConflict {
                    machine,
                    epoch: Some(epoch),
                })
            }
            MachineStatus::Removing(removal) => Err(MachineFailure::MachineRemoving {
                machine: removal.machine,
                epoch: removal.epoch,
            }),
            MachineStatus::Tombstoned(tombstone) => Err(MachineFailure::MachineTombstoned {
                machine: tombstone.machine,
                epoch: tombstone.epoch,
            }),
            MachineStatus::Absent => Err(MachineFailure::MutationMismatch {
                machine: membership.machine.clone(),
            }),
        }
    }
}

impl<S> PolisMachineMembership<S>
where
    S: polis::FactStore,
{
    fn append_machine_fact(
        &self,
        context: &MutationContext,
        fact: &MachineMembershipFact,
    ) -> Result<ProductFactAppendOutcome, MachineFailure> {
        let envelope = fact.encode().map_err(map_machine_fact_error)?;
        let target = polis_fact_target(envelope.target()).map_err(map_machine_primitive)?;
        let payload = polis_fact_payload(envelope.payload()).map_err(map_machine_primitive)?;
        let authority = context_fact_append_authority(context).map_err(map_machine_primitive)?;
        let grant = polis::FactGrantService::new(MachineFactGrantAuthority)
            .issue_append(&authority, target.clone())
            .map_err(map_machine_polis_error)?;
        let request = polis::FactAppendRequest::new(
            polis::OperationId::parse(format!(
                "{}:{}",
                context.operation().as_str(),
                envelope.target().key().as_str()
            ))
            .map_err(map_machine_polis_error)?,
            polis::IdempotencyKey::parse(format!(
                "{}:{}",
                context.idempotency().as_str(),
                envelope.target().key().as_str()
            ))
            .map_err(map_machine_polis_error)?,
            authority,
            grant,
            target,
            payload,
            None,
        );
        let outcome = polis::FactStore::append(self.projection.facts(), request)
            .map_err(map_machine_polis_error)?;
        product_fact_append_outcome(outcome).map_err(map_machine_primitive)
    }

    fn project_machine_status(
        &self,
        context: &MutationContext,
        machine: &MachineId,
    ) -> Result<MachineStatus, MachineFailure> {
        let request = polis::ProjectionRequest::new(
            machine_projection_view(machine)?,
            machine_fact_query(context, machine)?,
            PolisMachineReducer {
                reducer: MachineMembershipReducer::new(machine.clone()),
            },
        );
        polis::ProjectionSource::project(&self.projection, request)
            .map_err(map_machine_projection_error)
            .and_then(|snapshot| {
                require_fresh_projection(snapshot, |_| MachineFailure::ProjectionUnavailable)
            })
    }
}

fn machine_conflict_failure(
    conflict: ProductFactConflict,
    desired: &MachineMembership,
) -> MachineFailure {
    match conflict {
        ProductFactConflict::KeyPayloadConflict { .. }
        | ProductFactConflict::RejectedKeyPayloadConflict { .. } => {
            MachineFailure::MembershipConflict {
                machine: desired.machine.clone(),
                epoch: Some(desired.epoch),
            }
        }
        ProductFactConflict::IdempotencyKeyReuse { .. } => MachineFailure::MutationRejected,
    }
}

struct PolisMachineReducer {
    reducer: MachineMembershipReducer,
}

impl polis::FactReducer for PolisMachineReducer {
    type View = MachineStatus;
    type Error = MachineFailure;

    fn reduce(
        &self,
        facts: Vec<polis::VerifiedFact>,
    ) -> std::result::Result<Self::View, Self::Error> {
        let mut machine_facts = Vec::with_capacity(facts.len());
        for fact in facts {
            let decoded = MachineMembershipFact::decode_payload(fact.payload().as_bytes())
                .map_err(map_machine_fact_error)?;
            let expected_target = decoded
                .encode()
                .map_err(map_machine_fact_error)?
                .target()
                .clone();
            let actual_target = product_fact_receipt(fact.receipt())
                .map_err(map_machine_primitive)?
                .target()
                .clone();
            if actual_target != expected_target {
                return Err(MachineFailure::InvalidPayload);
            }
            machine_facts.push(decoded);
        }
        self.reducer
            .reduce(machine_facts)
            .map_err(map_machine_fact_error)
    }
}

fn machine_projection_view(machine: &MachineId) -> Result<polis::ProjectionView, MachineFailure> {
    polis::ProjectionKey::parse(format!("ployz.machine.membership:{}", machine.as_str()))
        .map(polis::ProjectionView::new)
        .map_err(map_machine_polis_error)
}

fn machine_fact_query(
    context: &MutationContext,
    machine: &MachineId,
) -> Result<polis::FactQuery, MachineFailure> {
    let scope = polis::ScopeId::parse(context.authority().scope().as_str())
        .map_err(map_machine_polis_error)?;
    let resource = polis::ResourceId::parse(format!("machine:{}", machine.as_str()))
        .map_err(map_machine_polis_error)?;
    Ok(polis::FactQuery::new(scope).resource(resource))
}

fn context_fact_append_authority(
    context: &MutationContext,
) -> Result<polis::Authorized<polis::FactAppendScope>, PrimitiveFailure> {
    polis::AuthorityService::new(ContextFactAuthority {
        epoch: context.authority().epoch().value(),
    })
    .authorize(
        polis::PrincipalId::parse(context.authority().principal().as_str())
            .map_err(map_polis_error)?,
        polis::ScopeId::parse(context.authority().scope().as_str()).map_err(map_polis_error)?,
    )
    .map_err(map_polis_error)
}

struct ContextFactAuthority {
    epoch: u64,
}

impl polis::Authority for ContextFactAuthority {
    fn decide(
        &self,
        _principal: &polis::PrincipalId,
        _scope: &polis::ScopeId,
    ) -> polis::AuthorityDecision {
        polis::AuthorityDecision::allowed(polis::GrantEpoch::new(self.epoch))
    }
}

struct MachineFactGrantAuthority;

impl polis::FactGrantAuthority for MachineFactGrantAuthority {
    fn decide(
        &self,
        _authority: &polis::AuthorityContext,
        target: &polis::FactTarget,
        purpose: polis::FactGrantPurpose,
    ) -> polis::FactGrantDecision {
        match purpose {
            polis::FactGrantPurpose::Append if machine_fact_target_allowed(target) => {
                polis::FactGrantDecision::allowed()
            }
            polis::FactGrantPurpose::Append | polis::FactGrantPurpose::ReplicaImport => {
                polis::FactGrantDecision::denied()
            }
        }
    }
}

fn machine_fact_target_allowed(target: &polis::FactTarget) -> bool {
    target.resource().as_str().starts_with("machine:")
        && matches!(
            target.kind().as_str(),
            MACHINE_JOINED_KIND | MACHINE_REMOVAL_STARTED_KIND | MACHINE_TOMBSTONED_KIND
        )
}

fn map_machine_projection_error(error: polis::ProjectionError<MachineFailure>) -> MachineFailure {
    match error {
        polis::ProjectionError::Source(source) => map_machine_polis_error(source),
        polis::ProjectionError::SubstrateFailed { .. } => MachineFailure::ProjectionUnavailable,
        polis::ProjectionError::Reducer(failure) => failure,
    }
}

fn map_machine_primitive(failure: PrimitiveFailure) -> MachineFailure {
    match failure {
        PrimitiveFailure::MalformedPayload => MachineFailure::InvalidPayload,
        PrimitiveFailure::Unauthorized
        | PrimitiveFailure::Conflict
        | PrimitiveFailure::Timeout
        | PrimitiveFailure::StaleFence
        | PrimitiveFailure::NoResponder
        | PrimitiveFailure::FreshnessUnknown
        | PrimitiveFailure::OperationStateConflict
        | PrimitiveFailure::OperationAlreadySucceeded
        | PrimitiveFailure::OperationInProgress
        | PrimitiveFailure::OperationAlreadyFailed
        | PrimitiveFailure::OperationInterrupted => MachineFailure::ProjectionUnavailable,
    }
}

fn map_machine_polis_error(error: polis::Error) -> MachineFailure {
    map_machine_primitive(map_polis_error(error))
}

fn map_machine_fact_error(error: MachineFactError) -> MachineFailure {
    match error {
        MachineFactError::InvalidPayload | MachineFactError::TargetMismatch => {
            MachineFailure::InvalidPayload
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine::{
        IrohEndpointId, MachineEpoch, MachineNetworkIdentity, MachineRemoval, MachineRemovalReason,
        MachineTombstone, OverlayIp, WireGuardPublicKey,
    };
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn maps_terminal_conflict_without_display_parsing() {
        assert_eq!(
            map_polis_error(polis::Error::TerminalAlreadyWritten),
            PrimitiveFailure::OperationStateConflict
        );
    }

    #[test]
    fn product_fact_target_maps_to_polis_target() {
        let target = ProductFactTarget::new(
            ProductFactResource::parse("machine:node-a").expect("resource"),
            crate::facts::ProductFactKey::parse("membership/node-a").expect("key"),
            crate::facts::ProductFactKind::parse("ployz.machine.joined.v1").expect("kind"),
        );

        let mapped = polis_fact_target(&target).expect("mapped target");

        assert_eq!(mapped.resource().as_str(), "machine:node-a");
        assert_eq!(mapped.key().as_str(), "membership/node-a");
        assert_eq!(mapped.kind().as_str(), "ployz.machine.joined.v1");
    }

    #[test]
    fn product_fact_payload_maps_to_polis_payload() {
        let payload = ProductFactPayload::new(b"joined".to_vec()).expect("payload");

        let mapped = polis_fact_payload(&payload).expect("mapped payload");

        assert_eq!(mapped.as_bytes(), b"joined");
    }

    #[test]
    fn product_fact_receipt_maps_from_polis_receipt() {
        let receipt = polis_fact_receipt();

        let mapped = product_fact_receipt(&receipt).expect("product receipt");

        assert_eq!(mapped.target().resource().as_str(), "machine:node-a");
        assert_eq!(mapped.target().key().as_str(), "membership/node-a");
        assert_eq!(mapped.target().kind().as_str(), "ployz.machine.joined.v1");
        assert_eq!(mapped.cursor(), ProductFactCursor::new(9));
    }

    #[test]
    fn product_fact_append_outcome_maps_replay_and_conflict() {
        let receipt = polis_fact_receipt();
        let replayed = product_fact_append_outcome(polis::FactAppendOutcome::Replayed(Box::new(
            receipt.clone(),
        )))
        .expect("replayed");

        assert_eq!(
            replayed.accepted_receipt().expect("accepted").cursor(),
            ProductFactCursor::new(9)
        );
        assert_eq!(
            product_fact_append_outcome(polis::FactAppendOutcome::Conflict(Box::new(
                polis::FactConflict::IdempotencyKeyReuse {
                    existing: Box::new(receipt.clone())
                }
            )))
            .expect("conflict"),
            ProductFactAppendOutcome::Conflict(ProductFactConflict::IdempotencyKeyReuse {
                existing: product_fact_receipt(&receipt).expect("receipt")
            })
        );
        assert_eq!(
            product_fact_append_outcome(polis::FactAppendOutcome::Rejected(
                polis::FactRejection::Unauthorized
            ))
            .expect("rejected"),
            ProductFactAppendOutcome::Rejected(ProductFactRejection::Unauthorized)
        );
    }

    #[test]
    fn product_projection_catch_up_maps_without_exposing_raw_shapes() {
        let caught_up = product_projection_catch_up(polis::ProjectionCatchUp::CaughtUp {
            view: projection_view(),
            source_cursor: polis::FactCursor::new(11),
        });
        let timed_out = product_projection_catch_up(polis::ProjectionCatchUp::TimedOut {
            view: projection_view(),
            requested: polis::FactCursor::new(12),
            current: Some(polis::FactCursor::new(10)),
        });
        let failed = product_projection_catch_up(polis::ProjectionCatchUp::ProjectionFailed {
            view: projection_view(),
            requested: polis::FactCursor::new(12),
            source_cursor: polis::FactCursor::new(10),
            health: polis::ProjectionHealth::new(),
        });

        assert_eq!(
            caught_up,
            ProductProjectionCatchUp::CaughtUp {
                source_cursor: ProductFactCursor::new(11)
            }
        );
        assert_eq!(
            timed_out,
            ProductProjectionCatchUp::TimedOut {
                requested: ProductFactCursor::new(12),
                current: Some(ProductFactCursor::new(10))
            }
        );
        assert_eq!(
            failed,
            ProductProjectionCatchUp::ProjectionFailed {
                requested: ProductFactCursor::new(12),
                source_cursor: ProductFactCursor::new(10),
                health: ProductProjectionHealth::healthy()
            }
        );
    }

    #[test]
    fn polis_projection_catch_up_request_keeps_deadline_and_cursor() {
        let request = crate::facts::ProductProjectionCatchUpRequest::new(
            ProductFactCursor::new(5),
            UNIX_EPOCH,
        );
        let mapped = polis_projection_catch_up_request(projection_view(), fact_query(), request);

        assert_eq!(mapped.cursor(), polis::FactCursor::new(5));
        assert_eq!(mapped.deadline(), UNIX_EPOCH);
    }

    #[test]
    fn fact_backed_machine_same_epoch_identity_conflict_projects_conflicted() {
        let membership = PolisMachineMembership::in_memory();
        membership
            .join(&context(), &machine_membership("node-a", 1, "fd00::1"))
            .expect("first join");
        let error = membership
            .join(&context(), &machine_membership("node-a", 1, "fd00::2"))
            .expect_err("conflict");

        assert_eq!(
            error,
            MachineFailure::MembershipConflict {
                machine: MachineId::parse("node-a").expect("machine"),
                epoch: Some(MachineEpoch::new(1).expect("epoch")),
            }
        );
        assert!(matches!(
            membership
                .observe(&context(), &MachineId::parse("node-a").expect("machine"))
                .expect("observe"),
            MachineStatus::Conflicted { machine, epoch }
                if machine.as_str() == "node-a" && epoch.value() == 1
        ));
    }

    #[test]
    fn fact_backed_machine_removal_reason_conflict_still_projects_removing() {
        let membership = PolisMachineMembership::in_memory();
        membership
            .append_machine_fact(
                &context(),
                &MachineMembershipFact::joined(machine_membership("node-a", 1, "fd00::1")),
            )
            .expect("joined");
        membership
            .append_machine_fact(
                &context(),
                &MachineMembershipFact::removal_started(MachineRemoval {
                    machine: MachineId::parse("node-a").expect("machine"),
                    epoch: MachineEpoch::new(2).expect("epoch"),
                    reason: MachineRemovalReason::parse("graceful").expect("reason"),
                }),
            )
            .expect("removing");
        membership
            .append_machine_fact(
                &context(),
                &MachineMembershipFact::removal_started(MachineRemoval {
                    machine: MachineId::parse("node-a").expect("machine"),
                    epoch: MachineEpoch::new(2).expect("epoch"),
                    reason: MachineRemovalReason::parse("operator").expect("reason"),
                }),
            )
            .expect("second removing");

        assert!(matches!(
            membership
                .observe(&context(), &MachineId::parse("node-a").expect("machine"))
                .expect("observe"),
            MachineStatus::Removing(removal)
                if removal.machine.as_str() == "node-a" && removal.epoch.value() == 2
        ));
    }

    #[test]
    fn fact_backed_machine_tombstone_reason_conflict_still_projects_tombstoned() {
        let membership = PolisMachineMembership::in_memory();
        membership
            .append_machine_fact(
                &context(),
                &MachineMembershipFact::joined(machine_membership("node-a", 1, "fd00::1")),
            )
            .expect("joined");
        for reason in ["force", "operator"] {
            membership
                .append_machine_fact(
                    &context(),
                    &MachineMembershipFact::tombstoned(
                        MachineTombstone {
                            machine: MachineId::parse("node-a").expect("machine"),
                            epoch: MachineEpoch::new(2).expect("epoch"),
                        },
                        MachineRemovalReason::parse(reason).expect("reason"),
                    ),
                )
                .expect("tombstone");
        }

        assert!(matches!(
            membership
                .observe(&context(), &MachineId::parse("node-a").expect("machine"))
                .expect("observe"),
            MachineStatus::Tombstoned(tombstone)
                if tombstone.machine.as_str() == "node-a" && tombstone.epoch.value() == 2
        ));
    }

    #[test]
    fn degraded_projection_is_not_exposed_as_machine_status() {
        let membership = PolisMachineMembership::in_memory();
        append_conflicting_raw_machine_fact(&membership, "payload-a");
        append_conflicting_raw_machine_fact(&membership, "payload-b");

        assert_eq!(
            membership.observe(&context(), &MachineId::parse("node-a").expect("machine")),
            Err(MachineFailure::ProjectionUnavailable)
        );
    }

    fn context() -> MutationContext {
        MutationContext::new(
            OperationId::parse("deploy-1").expect("operation"),
            IdempotencyKey::parse("deploy-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            UNIX_EPOCH + Duration::from_secs(60),
        )
    }

    fn append_conflicting_raw_machine_fact(
        membership: &PolisMachineMembership<polis::MemoryFactStore>,
        payload: &str,
    ) {
        let target = polis::FactTarget::new(
            polis::ResourceId::parse("machine:node-a").expect("resource"),
            polis::FactKey::parse("/facts/node/node-a/conflict").expect("key"),
            polis::FactKind::parse(MACHINE_JOINED_KIND).expect("kind"),
        );
        let authority = context_fact_append_authority(&context()).expect("authority");
        let grant = polis::FactGrantService::new(MachineFactGrantAuthority)
            .issue_append(&authority, target.clone())
            .expect("grant");
        let request = polis::FactAppendRequest::new(
            polis::OperationId::parse(format!("raw-machine-{payload}")).expect("operation"),
            polis::IdempotencyKey::parse(format!("raw-machine-{payload}")).expect("idempotency"),
            authority,
            grant,
            target,
            polis::FactPayload::new(payload.as_bytes().to_vec()).expect("payload"),
            None,
        );

        polis::FactStore::append(membership.projection.facts(), request).expect("append");
    }

    fn machine_membership(machine: &str, epoch: u64, overlay_ip: &str) -> MachineMembership {
        MachineMembership::new(
            MachineId::parse(machine).expect("machine"),
            MachineEpoch::new(epoch).expect("epoch"),
            MachineNetworkIdentity::new(
                OverlayIp::parse(overlay_ip).expect("overlay ip"),
                IrohEndpointId::parse(format!("iroh-{machine}")).expect("iroh endpoint"),
                WireGuardPublicKey::parse(format!("wg-{machine}")).expect("wireguard key"),
            ),
        )
    }

    struct AllowFactAuthority;

    impl polis::Authority for AllowFactAuthority {
        fn decide(
            &self,
            _principal: &polis::PrincipalId,
            _scope: &polis::ScopeId,
        ) -> polis::AuthorityDecision {
            polis::AuthorityDecision::allowed(polis::GrantEpoch::new(7))
        }
    }

    struct AllowFactGrantAuthority;

    impl polis::FactGrantAuthority for AllowFactGrantAuthority {
        fn decide(
            &self,
            _authority: &polis::AuthorityContext,
            _target: &polis::FactTarget,
            _purpose: polis::FactGrantPurpose,
        ) -> polis::FactGrantDecision {
            polis::FactGrantDecision::allowed()
        }
    }

    fn polis_fact_receipt() -> polis::FactReceipt {
        let authority = polis::AuthorityService::new(AllowFactAuthority)
            .authorize(
                polis::PrincipalId::parse("node-a").expect("principal"),
                polis::ScopeId::parse("cluster").expect("scope"),
            )
            .expect("authorized");
        let target = polis::FactTarget::new(
            polis::ResourceId::parse("machine:node-a").expect("resource"),
            polis::FactKey::parse("membership/node-a").expect("key"),
            polis::FactKind::parse("ployz.machine.joined.v1").expect("kind"),
        );
        let grant = polis::FactGrantService::new(AllowFactGrantAuthority)
            .issue_append(&authority, target.clone())
            .expect("grant");
        let request = polis::FactAppendRequest::new(
            polis::OperationId::parse("op-1").expect("operation"),
            polis::IdempotencyKey::parse("idem-1").expect("idempotency"),
            authority,
            grant,
            target,
            polis::FactPayload::new(b"joined".to_vec()).expect("payload"),
            None,
        );
        let validated = request.validate().expect("validated");

        polis::FactReceipt::from_validated_append(&validated, polis::FactCursor::new(9))
    }

    fn projection_view() -> polis::ProjectionView {
        polis::ProjectionView::new(
            polis::ProjectionKey::parse("machine-membership").expect("projection key"),
        )
    }

    fn fact_query() -> polis::FactQuery {
        polis::FactQuery::new(polis::ScopeId::parse("cluster").expect("scope"))
    }
}
