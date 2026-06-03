use std::time::{Duration, UNIX_EPOCH};

use super::{
    IdempotencyKey, OperationEvent, OperationEventKind, OperationFailureCode, OperationId,
    OperationKind, OperationLease, OperationLeaseEpoch, OperationLiveness, OperationOwner,
    OperationPayloadFingerprint, OperationRecord, OperationStage, OperationSubmission,
    OperationTerminal, PrincipalId, ScopeId,
};
use crate::PrimitiveFailure;

#[test]
fn running_operation_preserves_identity_and_driver_state() {
    let created_at = timestamp(10);
    let lease = lease("coordinator-a", 1, 10, 70);
    let record = OperationRecord::running(
        submission(
            "op-1",
            "deploy.apply",
            "scope-a",
            "idem-a",
            "principal-a",
            "payload-a",
        ),
        lease.clone(),
        OperationStage::parse("submitted").expect("stage"),
        created_at,
    );

    assert_eq!(record.operation().as_str(), "op-1");
    assert_eq!(record.kind().as_str(), "deploy.apply");
    assert_eq!(record.scope().as_str(), "scope-a");
    assert_eq!(record.idempotency().as_str(), "idem-a");
    assert_eq!(record.principal().as_str(), "principal-a");
    assert_eq!(
        record.current_stage().map(OperationStage::as_str),
        Some("submitted")
    );
    assert_eq!(record.lease(), Some(&lease));
    assert_eq!(
        record.liveness_at(timestamp(20)),
        OperationLiveness::Held {
            owner: owner("coordinator-a"),
            epoch: OperationLeaseEpoch::new(1).expect("epoch"),
            expires_at: timestamp(70),
        }
    );
}

#[test]
fn ordered_events_advance_cursor_for_one_operation() {
    let first = OperationEvent::new(
        operation("op-1"),
        OperationEventKind::stage(stage("submitted")),
        timestamp(10),
    );
    let second = first
        .next(OperationEventKind::stage(stage("running")), timestamp(11))
        .expect("next event");

    assert_eq!(first.sequence().value(), 1);
    assert_eq!(second.sequence().value(), 2);
    assert_eq!(second.cursor().operation().as_str(), "op-1");
    assert_eq!(second.cursor().sequence().value(), 2);
}

#[test]
fn lease_expiry_changes_liveness_without_terminal_status() {
    let mut record = OperationRecord::running(
        submission(
            "op-1",
            "deploy.apply",
            "scope-a",
            "idem-a",
            "principal-a",
            "payload-a",
        ),
        lease("coordinator-a", 1, 10, 70),
        stage("running"),
        timestamp(10),
    );

    assert_eq!(
        record.liveness_at(timestamp(71)),
        OperationLiveness::Expired {
            owner: owner("coordinator-a"),
            epoch: OperationLeaseEpoch::new(1).expect("epoch"),
            expired_at: timestamp(70),
        }
    );
    assert!(record.terminal().is_none());

    record
        .write_terminal(
            OperationTerminal::failed(failure("lease-lost")),
            &owner("coordinator-a"),
            OperationLeaseEpoch::new(1).expect("epoch"),
            timestamp(60),
        )
        .expect("terminal before expiry");
    assert!(matches!(
        record.terminal(),
        Some(OperationTerminal::Failed { .. })
    ));
}

#[test]
fn stale_or_wrong_lease_holder_cannot_checkpoint() {
    let record = OperationRecord::running(
        submission(
            "op-1",
            "deploy.apply",
            "scope-a",
            "idem-a",
            "principal-a",
            "payload-a",
        ),
        lease("coordinator-a", 2, 10, 70),
        stage("running"),
        timestamp(10),
    );

    assert_eq!(
        record.ensure_checkpoint_allowed(
            &owner("coordinator-b"),
            OperationLeaseEpoch::new(2).expect("epoch"),
            timestamp(20),
        ),
        Err(PrimitiveFailure::OperationInterrupted)
    );
    assert_eq!(
        record.ensure_checkpoint_allowed(
            &owner("coordinator-a"),
            OperationLeaseEpoch::new(1).expect("epoch"),
            timestamp(20),
        ),
        Err(PrimitiveFailure::OperationInterrupted)
    );
    assert_eq!(
        record.ensure_checkpoint_allowed(
            &owner("coordinator-a"),
            OperationLeaseEpoch::new(2).expect("epoch"),
            timestamp(71),
        ),
        Err(PrimitiveFailure::OperationInterrupted)
    );
}

#[test]
fn conflicting_idempotency_payload_is_structured_conflict() {
    let existing = submission(
        "op-1",
        "deploy.apply",
        "scope-a",
        "idem-a",
        "principal-a",
        "payload-a",
    );
    let conflict = submission(
        "op-2",
        "deploy.apply",
        "scope-a",
        "idem-a",
        "principal-a",
        "payload-b",
    );

    assert_eq!(
        existing.ensure_replay_matches(&conflict),
        Err(PrimitiveFailure::OperationStateConflict)
    );
}

#[test]
fn terminal_write_is_idempotent_but_conflicts_with_different_terminal() {
    let mut record = OperationRecord::running(
        submission(
            "op-1",
            "deploy.apply",
            "scope-a",
            "idem-a",
            "principal-a",
            "payload-a",
        ),
        lease("coordinator-a", 1, 10, 70),
        stage("running"),
        timestamp(10),
    );
    let terminal = OperationTerminal::succeeded();

    record
        .write_terminal(
            terminal.clone(),
            &owner("coordinator-a"),
            OperationLeaseEpoch::new(1).expect("epoch"),
            timestamp(20),
        )
        .expect("terminal");
    record
        .write_terminal(
            terminal,
            &owner("coordinator-a"),
            OperationLeaseEpoch::new(1).expect("epoch"),
            timestamp(21),
        )
        .expect("same terminal is replay");

    assert_eq!(
        record.write_terminal(
            OperationTerminal::failed(failure("different")),
            &owner("coordinator-a"),
            OperationLeaseEpoch::new(1).expect("epoch"),
            timestamp(22),
        ),
        Err(PrimitiveFailure::OperationStateConflict)
    );
}

fn submission(
    operation: &str,
    kind: &str,
    scope: &str,
    idempotency: &str,
    principal: &str,
    fingerprint: &str,
) -> OperationSubmission {
    OperationSubmission::new(
        OperationId::parse(operation).expect("operation"),
        OperationKind::parse(kind).expect("kind"),
        ScopeId::parse(scope).expect("scope"),
        IdempotencyKey::parse(idempotency).expect("idempotency"),
        PrincipalId::parse(principal).expect("principal"),
        OperationPayloadFingerprint::parse(fingerprint).expect("fingerprint"),
    )
}

fn operation(value: &str) -> OperationId {
    OperationId::parse(value).expect("operation")
}

fn stage(value: &str) -> OperationStage {
    OperationStage::parse(value).expect("stage")
}

fn owner(value: &str) -> OperationOwner {
    OperationOwner::parse(value).expect("owner")
}

fn failure(value: &str) -> OperationFailureCode {
    OperationFailureCode::parse(value).expect("failure")
}

fn lease(owner: &str, epoch: u64, renewed_at: u64, expires_at: u64) -> OperationLease {
    OperationLease::new(
        OperationOwner::parse(owner).expect("owner"),
        OperationLeaseEpoch::new(epoch).expect("epoch"),
        timestamp(renewed_at),
        timestamp(expires_at),
    )
    .expect("lease")
}

fn timestamp(seconds: u64) -> std::time::SystemTime {
    UNIX_EPOCH + Duration::from_secs(seconds)
}
