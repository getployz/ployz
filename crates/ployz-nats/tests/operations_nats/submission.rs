use super::fixtures::*;
use ployz_core::ops::{
    DeployRunningStage, DeployTransition, OperationEventReplayCursor, OperationEventReplayRequest,
    OperationOwnershipStatus,
};
use ployz_nats::operations::{
    OperationLeaseClaim, OperationLeaseClaimError, ReplayOperationEventsError,
};

#[tokio::test]
async fn operation_repository_duplicate_submit_returns_original_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            default_lease_claim(),
        )
        .await
        .expect("first submit accepted");
    let second = repository
        .submit_deploy(
            deploy_submission("op_456", "idem_1", "svc_other"),
            default_lease_claim(),
        )
        .await
        .expect("duplicate submit accepted");

    assert_eq!(first, second);
    assert_eq!(first.operation_id, operation_id("op_123"));
    assert_eq!(first.lease.owner_id, owner_id("control_a"));
    assert!(
        repository
            .operation_status(&operation_id("op_456"))
            .await
            .expect("status lookup succeeds")
            .is_none()
    );
}

#[test]
fn operation_lease_claim_rejects_expired_window() {
    assert_eq!(
        OperationLeaseClaim::try_new(owner_id("control_a"), lease_time(100), lease_time(100)),
        Err(OperationLeaseClaimError::AlreadyExpired {
            now: lease_time(100),
            expires_at: lease_time(100),
        })
    );
}

#[tokio::test]
async fn operation_repository_duplicate_submit_returns_active_stored_owner_lease() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            lease_claim("control_a", 100, 160),
        )
        .await
        .expect("first submit accepted");
    let second = repository
        .submit_deploy(
            deploy_submission("op_456", "idem_1", "svc_other"),
            lease_claim("control_b", 120, 180),
        )
        .await
        .expect("duplicate submit accepted");

    assert_eq!(second.operation_id, first.operation_id);
    assert_eq!(second.start_sequence, first.start_sequence);
    assert_eq!(second.lease, first.lease);
}

#[tokio::test]
async fn operation_repository_status_snapshot_reports_expired_owner_lease() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            lease_claim("control_a", 100, 160),
        )
        .await
        .expect("submit accepted");

    let snapshot = repository
        .operation_status_snapshot(&operation_id("op_123"), lease_time(161))
        .await
        .expect("status snapshot succeeds")
        .expect("operation exists");

    assert_eq!(
        snapshot.ownership,
        OperationOwnershipStatus::Expired {
            lease: accepted.lease
        }
    );
}

#[tokio::test]
async fn operation_repository_replay_rejects_missing_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let error = repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_missing"),
            start_sequence: event_sequence(1),
            limit: event_replay_limit(10),
        })
        .await
        .expect_err("missing operation is rejected");

    assert!(matches!(
        error,
        ReplayOperationEventsError::MissingOperation { operation_id }
            if operation_id == self::operation_id("op_missing")
    ));
}

#[tokio::test]
async fn operation_repository_replay_marks_terminal_operation_caught_up_as_terminal() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            default_lease_claim(),
        )
        .await
        .expect("submit accepted");
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("planning recorded");
    for stage in [
        DeployRunningStage::StartingContainers,
        DeployRunningStage::WaitingForHealth,
        DeployRunningStage::RouteCutover,
        DeployRunningStage::ActiveServiceCommit,
    ] {
        repository
            .record_deploy_transition(&operation_id("op_123"), DeployTransition::Running { stage })
            .await
            .expect("running stage recorded");
    }
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Completed)
        .await
        .expect("completion recorded");

    let page = repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_123"),
            start_sequence: accepted.start_sequence,
            limit: event_replay_limit(10),
        })
        .await
        .expect("terminal operation replay succeeds");

    assert_eq!(page.cursor, OperationEventReplayCursor::Terminal);
}
