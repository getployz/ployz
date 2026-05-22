//! Polis adapter helpers for Ployz composition code.

mod acme;
mod domain;
mod machine;
mod serving;

use std::collections::BTreeMap;

pub(crate) use acme::AttemptingCertificateIssuer;
pub(crate) use domain::PolisDomainStatus;
pub(crate) use machine::PolisMachineMembership;
pub(crate) use serving::PolisServingSnapshots;

use crate::error::PrimitiveFailure;
use crate::facts::{
    ProductCandidateRejection, ProductFactAppendOutcome, ProductFactConflict, ProductFactCursor,
    ProductFactEnvelope, ProductFactKey, ProductFactKind, ProductFactPayload, ProductFactReceipt,
    ProductFactRejection, ProductFactResource, ProductFactTarget, ProductPayloadFailure,
    ProductProjectionCatchUp, ProductProjectionFreshness, ProductProjectionHealth,
    ProductProjectionSnapshot,
};
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

pub fn append_product_fact<S, G>(
    facts: &S,
    context: &MutationContext,
    envelope: &ProductFactEnvelope,
    grant_authority: G,
    conflict_policy: polis::FactConflictPolicy,
) -> Result<ProductFactAppendOutcome, PrimitiveFailure>
where
    S: polis::FactStore,
    G: polis::FactGrantAuthority,
{
    let target = polis_fact_target(envelope.target())?;
    let payload = polis_fact_payload(envelope.payload())?;
    let authority = context_fact_append_authority(context)?;
    let grant = polis::FactGrantService::new(grant_authority)
        .issue_append(&authority, target.clone())
        .map_err(map_polis_error)?;
    let request = polis::FactAppendRequest::new(
        polis::OperationId::parse(format!(
            "{}:{}",
            context.operation().as_str(),
            envelope.target().key().as_str()
        ))
        .map_err(map_polis_error)?,
        polis::IdempotencyKey::parse(format!(
            "{}:{}",
            context.idempotency().as_str(),
            envelope.target().key().as_str()
        ))
        .map_err(map_polis_error)?,
        authority,
        grant,
        target,
        payload,
        None,
    )
    .with_conflict_policy(conflict_policy);

    polis::FactStore::append(facts, request)
        .map_err(map_polis_error)
        .and_then(product_fact_append_outcome)
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::UNIX_EPOCH;

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
