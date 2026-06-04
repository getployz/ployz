use super::fixtures::*;
use ployz_core::ops::{
    DeployRunningStage, DeployTransition, OperationEventReplayCursor, OperationEventReplayRequest,
};
use ployz_nats::operations::ReplayOperationEventsError;

#[tokio::test]
async fn operation_repository_duplicate_submit_returns_original_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
        .await
        .expect("first submit accepted");
    let second = repository
        .submit_deploy(deploy_submission("op_456", "idem_1", "svc_other"))
        .await
        .expect("duplicate submit accepted");

    assert_eq!(first, second);
    assert_eq!(first.operation_id, operation_id("op_123"));
    assert!(
        repository
            .operation_status(&operation_id("op_456"))
            .await
            .expect("status lookup succeeds")
            .is_none()
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
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
