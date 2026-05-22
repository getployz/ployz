//! Polis adapter helpers for Ployz composition code.

use std::time::SystemTime;

use crate::acme::{
    CertificateAuthorityPort, CertificateIssueOutcome, CertificateIssueRequest,
    CertificateIssuerPort,
};
use crate::error::{CertificateFailure, PrimitiveFailure};
use crate::facts::{
    ProductCandidateRejection, ProductFactAppendOutcome, ProductFactConflict, ProductFactCursor,
    ProductFactKey, ProductFactKind, ProductFactPayload, ProductFactReceipt, ProductFactRejection,
    ProductFactResource, ProductFactTarget, ProductPayloadFailure, ProductProjectionCatchUp,
    ProductProjectionCatchUpRequest, ProductProjectionFreshness, ProductProjectionHealth,
};
use crate::operation::MutationContext;
use std::collections::BTreeMap;

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
pub fn polis_projection_catch_up_request(
    view: polis::ProjectionView,
    query: polis::FactQuery,
    request: ProductProjectionCatchUpRequest,
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

#[derive(Debug, Clone)]
pub struct AttemptingCertificateIssuer<A, I> {
    attempts: A,
    issuer: I,
}

impl<A, I> AttemptingCertificateIssuer<A, I> {
    #[must_use]
    pub fn new(attempts: A, issuer: I) -> Self {
        Self { attempts, issuer }
    }
}

impl<A, I> CertificateIssuerPort for AttemptingCertificateIssuer<A, I>
where
    A: polis::OperationBackend,
    I: CertificateAuthorityPort,
{
    fn issue_certificate(
        &self,
        context: &MutationContext,
        request: &CertificateIssueRequest,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        let attempt_request = certificate_attempt_request(context, request)?;
        match polis::begin_attempt(&self.attempts, attempt_request.clone())
            .map_err(map_polis_certificate_error)?
        {
            polis::AttemptStart::Started(attempt) => {
                if let Err(failure) = self.issuer.issue_certificate(context, request) {
                    return match attempt.failed(certificate_failure_payload(&failure)) {
                        Ok(()) => Err(failure),
                        Err(error) => self.replay_or_map_terminal_error(&attempt_request, error),
                    };
                }
                if let Err(error) = attempt.record(polis::OperationEvidence {
                    recorded_at: SystemTime::now(),
                    kind: polis::EvidenceKind::Checkpoint(b"certificate-issued".to_vec()),
                }) {
                    return self.replay_or_map_terminal_error(&attempt_request, error);
                }
                match attempt.succeeded() {
                    Ok(()) => Ok(CertificateIssueOutcome::Issued),
                    Err(error) => self.replay_or_map_terminal_error(&attempt_request, error),
                }
            }
            polis::AttemptStart::Replayed(polis::AttemptReplay::Succeeded { .. }) => {
                Ok(CertificateIssueOutcome::Replayed)
            }
            polis::AttemptStart::Replayed(polis::AttemptReplay::Open {
                operation,
                owner_deadline,
            }) if owner_deadline <= SystemTime::now() => {
                self.interrupt_expired_attempt(&attempt_request, &operation)
            }
            polis::AttemptStart::Replayed(polis::AttemptReplay::Open { .. }) => {
                Ok(CertificateIssueOutcome::InProgress)
            }
            polis::AttemptStart::Replayed(polis::AttemptReplay::Interrupted { .. }) => {
                Ok(CertificateIssueOutcome::Interrupted)
            }
            polis::AttemptStart::Replayed(polis::AttemptReplay::Failed { payload, .. }) => {
                Err(certificate_failure_from_payload(&payload))
            }
        }
    }
}

impl<A, I> AttemptingCertificateIssuer<A, I>
where
    A: polis::OperationBackend,
{
    fn interrupt_expired_attempt(
        &self,
        request: &polis::AttemptRequest,
        operation: &polis::OperationId,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match self
            .attempts
            .close(operation, polis::TerminalMarker::Interrupted)
        {
            Ok(()) => Ok(CertificateIssueOutcome::Interrupted),
            Err(polis::Error::TerminalAlreadyWritten) => {
                self.replay_after_terminal_conflict(request)
            }
            Err(error) => Err(map_polis_certificate_error(error)),
        }
    }

    fn replay_after_terminal_conflict(
        &self,
        request: &polis::AttemptRequest,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        let operation_request = polis::OperationRequest::new(
            request.operation().clone(),
            request.idempotency().clone(),
            request.fingerprint().clone(),
            request.owner_deadline(),
        );
        match self
            .attempts
            .start_or_replay(&operation_request)
            .map_err(map_polis_certificate_error)?
        {
            polis::BackendOperationStart::Started => Err(CertificateFailure::IssuanceFailed),
            polis::BackendOperationStart::Replayed {
                terminal: Some(polis::TerminalMarker::Succeeded),
                ..
            } => Ok(CertificateIssueOutcome::Replayed),
            polis::BackendOperationStart::Replayed {
                terminal: Some(polis::TerminalMarker::Failed(payload)),
                ..
            } => Err(certificate_failure_from_payload(&payload)),
            polis::BackendOperationStart::Replayed {
                terminal: Some(polis::TerminalMarker::Interrupted),
                ..
            } => Ok(CertificateIssueOutcome::Interrupted),
            polis::BackendOperationStart::Replayed { terminal: None, .. } => {
                Err(CertificateFailure::FreshnessUnknown)
            }
        }
    }

    fn replay_or_map_terminal_error(
        &self,
        request: &polis::AttemptRequest,
        error: polis::Error,
    ) -> Result<CertificateIssueOutcome, CertificateFailure> {
        match error {
            polis::Error::TerminalAlreadyWritten => self.replay_after_terminal_conflict(request),
            other @ (polis::Error::Unauthorized
            | polis::Error::Conflict
            | polis::Error::Timeout
            | polis::Error::StaleFence
            | polis::Error::NoResponder
            | polis::Error::FreshnessUnknown
            | polis::Error::MalformedPayload) => Err(map_polis_certificate_error(other)),
        }
    }
}

fn certificate_attempt_request(
    context: &MutationContext,
    request: &CertificateIssueRequest,
) -> Result<polis::AttemptRequest, CertificateFailure> {
    let hostname = request.binding.hostname.as_str();
    let actor = polis::PrincipalId::parse(context.authority().principal().as_str())
        .map_err(map_polis_certificate_error)?;
    let scope = polis::ScopeId::parse(context.authority().scope().as_str())
        .map_err(map_polis_certificate_error)?;
    let command =
        polis::CommandKind::parse("acme.issue_certificate").map_err(map_polis_certificate_error)?;
    let resource = polis::FingerprintedResource::parse(format!("cert:{hostname}"))
        .map_err(map_polis_certificate_error)?;
    let fingerprint = polis::RequestFingerprint::builder(
        actor,
        scope,
        command,
        "ployz.acme.certificate_issue.v1",
        polis::GrantEpoch::new(context.authority().epoch().value()),
    )
    .field("hostname", hostname)
    .field_time("minimum_valid_until", request.deadline.expires_at)
    .resource(resource)
    .finish()
    .map_err(map_polis_certificate_error)?;
    Ok(polis::AttemptRequest::new(
        polis::OperationId::parse(format!(
            "{}:acme-issue:{hostname}",
            context.operation().as_str()
        ))
        .map_err(map_polis_certificate_error)?,
        polis::IdempotencyKey::parse(format!(
            "{}:acme-issue:{hostname}",
            context.idempotency().as_str()
        ))
        .map_err(map_polis_certificate_error)?,
        fingerprint,
        context.deadline(),
    ))
}

fn map_polis_certificate_error(error: polis::Error) -> CertificateFailure {
    match error {
        polis::Error::Unauthorized | polis::Error::StaleFence => {
            CertificateFailure::UnauthorizedBinding
        }
        polis::Error::Timeout | polis::Error::FreshnessUnknown => {
            CertificateFailure::FreshnessUnknown
        }
        polis::Error::Conflict
        | polis::Error::NoResponder
        | polis::Error::MalformedPayload
        | polis::Error::TerminalAlreadyWritten => CertificateFailure::IssuanceFailed,
    }
}

fn certificate_failure_payload(failure: &CertificateFailure) -> Vec<u8> {
    match failure {
        CertificateFailure::UnauthorizedBinding => b"unauthorized_binding".to_vec(),
        CertificateFailure::ChallengeFailed => b"challenge_failed".to_vec(),
        CertificateFailure::IssuanceFailed => b"issuance_failed".to_vec(),
        CertificateFailure::MaterialUnsafe => b"material_unsafe".to_vec(),
        CertificateFailure::SafetyWindowFailed => b"safety_window_failed".to_vec(),
        CertificateFailure::KnownRevoked => b"known_revoked".to_vec(),
        CertificateFailure::FreshnessUnknown => b"freshness_unknown".to_vec(),
        CertificateFailure::ActivationRejected => b"activation_rejected".to_vec(),
    }
}

fn certificate_failure_from_payload(payload: &[u8]) -> CertificateFailure {
    match payload {
        b"unauthorized_binding" => CertificateFailure::UnauthorizedBinding,
        b"challenge_failed" => CertificateFailure::ChallengeFailed,
        b"issuance_failed" => CertificateFailure::IssuanceFailed,
        b"material_unsafe" => CertificateFailure::MaterialUnsafe,
        b"safety_window_failed" => CertificateFailure::SafetyWindowFailed,
        b"known_revoked" => CertificateFailure::KnownRevoked,
        b"freshness_unknown" => CertificateFailure::FreshnessUnknown,
        b"activation_rejected" => CertificateFailure::ActivationRejected,
        _ => CertificateFailure::IssuanceFailed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acme::{CertificateDeadline, Hostname, HttpsBinding};
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };
    use std::cell::RefCell;
    use std::collections::{BTreeMap, VecDeque};
    use std::rc::Rc;
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
        let request = ProductProjectionCatchUpRequest::new(ProductFactCursor::new(5), UNIX_EPOCH);
        let mapped = polis_projection_catch_up_request(projection_view(), fact_query(), request);

        assert_eq!(mapped.cursor(), polis::FactCursor::new(5));
        assert_eq!(mapped.deadline(), UNIX_EPOCH);
    }

    #[test]
    fn attempt_issuer_records_only_non_secret_checkpoint_and_succeeds() {
        let backend = FakeOperationBackend::default();
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend.clone(), authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("issue");

        assert_eq!(outcome, CertificateIssueOutcome::Issued);
        assert_eq!(*authority.calls.borrow(), 1);
        assert_eq!(
            backend
                .terminal
                .borrow()
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vec![polis::TerminalMarker::Succeeded]
        );
        let records = backend.records.borrow();
        assert_eq!(records.len(), 1);
        let Some(record) = records.first() else {
            panic!("expected recorded evidence");
        };
        let polis::EvidenceKind::Checkpoint(payload) = &record.kind else {
            panic!("expected checkpoint");
        };
        assert_eq!(payload, b"certificate-issued");
        assert!(
            !payload
                .windows(b"PRIVATE KEY".len())
                .any(|window| window == b"PRIVATE KEY")
        );
    }

    #[test]
    fn succeeded_replay_skips_external_issuer() {
        let backend = FakeOperationBackend::with_replay(Some(polis::TerminalMarker::Succeeded));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Replayed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn open_replay_reports_in_progress_without_external_issuer() {
        let backend =
            FakeOperationBackend::with_open_replay(SystemTime::now() + Duration::from_secs(60));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::InProgress);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn expired_open_replay_terminalizes_interrupted_without_external_issuer() {
        let backend = FakeOperationBackend::with_open_replay(UNIX_EPOCH);
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend.clone(), authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Interrupted);
        assert_eq!(*authority.calls.borrow(), 0);
        assert_eq!(
            backend
                .terminal
                .borrow()
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vec![polis::TerminalMarker::Interrupted]
        );
    }

    #[test]
    fn expired_open_replay_close_conflict_replays_success_terminal() {
        let backend = FakeOperationBackend::with_open_replay_then_close_conflict(
            UNIX_EPOCH,
            Some(polis::TerminalMarker::Succeeded),
        );
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Replayed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn expired_open_replay_close_conflict_replays_failed_terminal() {
        let backend = FakeOperationBackend::with_open_replay_then_close_conflict(
            UNIX_EPOCH,
            Some(polis::TerminalMarker::Failed(b"challenge_failed".to_vec())),
        );
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .expect_err("failed replay");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn external_issuer_failure_terminalizes_failed_attempt() {
        let backend = FakeOperationBackend::default();
        let authority = FakeCertificateAuthority::failing(CertificateFailure::ChallengeFailed);
        let issuer = AttemptingCertificateIssuer::new(backend.clone(), authority);

        let error = issuer
            .issue_certificate(&context(), &request())
            .expect_err("failure");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(
            backend
                .terminal
                .borrow()
                .values()
                .cloned()
                .collect::<Vec<_>>(),
            vec![polis::TerminalMarker::Failed(b"challenge_failed".to_vec())]
        );
    }

    #[test]
    fn failed_replay_preserves_certificate_failure_class() {
        let backend = FakeOperationBackend::with_replay(Some(polis::TerminalMarker::Failed(
            b"challenge_failed".to_vec(),
        )));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .expect_err("failed replay");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(*authority.calls.borrow(), 0);
    }

    #[test]
    fn started_success_close_conflict_replays_actual_success_terminal() {
        let backend = FakeOperationBackend::with_started_then_close_conflict(Some(
            polis::TerminalMarker::Succeeded,
        ));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Replayed);
        assert_eq!(*authority.calls.borrow(), 1);
    }

    #[test]
    fn started_failure_close_conflict_replays_actual_success_terminal() {
        let backend = FakeOperationBackend::with_started_then_close_conflict(Some(
            polis::TerminalMarker::Succeeded,
        ));
        let authority = FakeCertificateAuthority::failing(CertificateFailure::ChallengeFailed);
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let outcome = issuer
            .issue_certificate(&context(), &request())
            .expect("replay");

        assert_eq!(outcome, CertificateIssueOutcome::Replayed);
        assert_eq!(*authority.calls.borrow(), 1);
    }

    #[test]
    fn started_success_close_conflict_replays_actual_failed_terminal() {
        let backend = FakeOperationBackend::with_started_then_close_conflict(Some(
            polis::TerminalMarker::Failed(b"challenge_failed".to_vec()),
        ));
        let authority = FakeCertificateAuthority::default();
        let issuer = AttemptingCertificateIssuer::new(backend, authority.clone());

        let error = issuer
            .issue_certificate(&context(), &request())
            .expect_err("failed replay");

        assert_eq!(error, CertificateFailure::ChallengeFailed);
        assert_eq!(*authority.calls.borrow(), 1);
    }

    #[derive(Clone, Default)]
    struct FakeOperationBackend {
        replays: Rc<RefCell<VecDeque<FakeOperationReplay>>>,
        records: Rc<RefCell<Vec<polis::OperationEvidence>>>,
        terminal: Rc<RefCell<BTreeMap<polis::OperationId, polis::TerminalMarker>>>,
        close_conflicts: Rc<RefCell<usize>>,
        starts_before_replay: Rc<RefCell<usize>>,
    }

    impl FakeOperationBackend {
        fn with_replay(terminal: Option<polis::TerminalMarker>) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([FakeOperationReplay {
                    owner_deadline: UNIX_EPOCH + Duration::from_secs(60),
                    terminal,
                }]))),
                records: Rc::new(RefCell::new(Vec::new())),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(0)),
                starts_before_replay: Rc::new(RefCell::new(0)),
            }
        }

        fn with_open_replay(owner_deadline: SystemTime) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([FakeOperationReplay {
                    owner_deadline,
                    terminal: None,
                }]))),
                records: Rc::new(RefCell::new(Vec::new())),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(0)),
                starts_before_replay: Rc::new(RefCell::new(0)),
            }
        }

        fn with_open_replay_then_close_conflict(
            owner_deadline: SystemTime,
            terminal: Option<polis::TerminalMarker>,
        ) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([
                    FakeOperationReplay {
                        owner_deadline,
                        terminal: None,
                    },
                    FakeOperationReplay {
                        owner_deadline,
                        terminal,
                    },
                ]))),
                records: Rc::new(RefCell::new(Vec::new())),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(1)),
                starts_before_replay: Rc::new(RefCell::new(0)),
            }
        }

        fn with_started_then_close_conflict(terminal: Option<polis::TerminalMarker>) -> Self {
            Self {
                replays: Rc::new(RefCell::new(VecDeque::from([FakeOperationReplay {
                    owner_deadline: UNIX_EPOCH + Duration::from_secs(60),
                    terminal,
                }]))),
                records: Rc::new(RefCell::new(Vec::new())),
                terminal: Rc::new(RefCell::new(BTreeMap::new())),
                close_conflicts: Rc::new(RefCell::new(1)),
                starts_before_replay: Rc::new(RefCell::new(1)),
            }
        }
    }

    #[derive(Clone)]
    struct FakeOperationReplay {
        owner_deadline: SystemTime,
        terminal: Option<polis::TerminalMarker>,
    }

    impl polis::OperationBackend for FakeOperationBackend {
        fn start_or_replay(
            &self,
            request: &polis::OperationRequest,
        ) -> polis::Result<polis::BackendOperationStart> {
            let mut starts_before_replay = self.starts_before_replay.borrow_mut();
            if *starts_before_replay > 0 {
                *starts_before_replay -= 1;
                return Ok(polis::BackendOperationStart::Started);
            }
            drop(starts_before_replay);
            if let Some(replay) = self.replays.borrow_mut().pop_front() {
                return Ok(polis::BackendOperationStart::Replayed {
                    operation: request.operation().clone(),
                    owner_deadline: replay.owner_deadline,
                    terminal: replay.terminal,
                });
            }
            Ok(polis::BackendOperationStart::Started)
        }

        fn record(
            &self,
            _operation: &polis::OperationId,
            evidence: polis::OperationEvidence,
        ) -> polis::Result<()> {
            self.records.borrow_mut().push(evidence);
            Ok(())
        }

        fn close(
            &self,
            operation: &polis::OperationId,
            marker: polis::TerminalMarker,
        ) -> polis::Result<()> {
            let mut close_conflicts = self.close_conflicts.borrow_mut();
            if *close_conflicts > 0 {
                *close_conflicts -= 1;
                return Err(polis::Error::TerminalAlreadyWritten);
            }
            self.terminal.borrow_mut().insert(operation.clone(), marker);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeCertificateAuthority {
        result: Result<(), CertificateFailure>,
        calls: Rc<RefCell<usize>>,
    }

    impl Default for FakeCertificateAuthority {
        fn default() -> Self {
            Self {
                result: Ok(()),
                calls: Rc::new(RefCell::new(0)),
            }
        }
    }

    impl FakeCertificateAuthority {
        fn failing(failure: CertificateFailure) -> Self {
            Self {
                result: Err(failure),
                calls: Rc::new(RefCell::new(0)),
            }
        }
    }

    impl CertificateAuthorityPort for FakeCertificateAuthority {
        fn issue_certificate(
            &self,
            _context: &MutationContext,
            _request: &CertificateIssueRequest,
        ) -> Result<(), CertificateFailure> {
            *self.calls.borrow_mut() += 1;
            self.result.clone()
        }
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

    fn request() -> CertificateIssueRequest {
        CertificateIssueRequest::new(
            HttpsBinding::new(Hostname::parse("app.example.com").expect("hostname")),
            CertificateDeadline {
                expires_at: UNIX_EPOCH + Duration::from_secs(3_600),
            },
        )
    }
}
