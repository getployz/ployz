use super::fixtures::*;
use ployz_core::ops::{
    DeployRunningStage, DeployTransition, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayRequest, OperationStatus,
};
use ployz_nats::operations::{ReplayOperationEventsError, SubmitOperationError};

#[tokio::test]
async fn operation_repository_duplicate_operation_id_recovers_submit() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_deploy(deploy_submission("op_123", "svc_api"))
        .await
        .expect("first submit accepted");
    let duplicate = repository
        .submit_deploy(deploy_submission("op_123", "svc_api"))
        .await
        .expect("duplicate operation id recovers");

    assert!(first.should_start_execution);
    assert!(duplicate.should_start_execution);
    assert_eq!(first.operation_id, operation_id("op_123"));
    assert_eq!(duplicate.operation_id, operation_id("op_123"));
    assert_eq!(duplicate.start_sequence, first.start_sequence);
    assert_eq!(duplicate.target, first.target);
}

#[tokio::test]
async fn operation_repository_accepts_empty_deploy_target() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let accepted = repository
        .submit_deploy(empty_deploy_submission("op_empty"))
        .await
        .expect("empty deploy submit accepted");

    assert!(accepted.target.services.is_empty());
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_empty"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::deploy_accepted(
            operation_id("op_empty"),
            service_id("production"),
            accepted.start_sequence,
        ))
    );
}

#[tokio::test]
async fn operation_repository_duplicate_operation_id_after_progress_does_not_restart() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    repository
        .submit_deploy(deploy_submission("op_123", "svc_api"))
        .await
        .expect("submit accepted");
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("planning records");
    let duplicate = repository
        .submit_deploy(deploy_submission("op_123", "svc_api"))
        .await
        .expect("duplicate operation id recovers");

    assert!(!duplicate.should_start_execution);
}

#[tokio::test]
async fn operation_repository_rejects_operation_id_reuse_across_kinds() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let first = repository
        .submit_deploy(deploy_submission("op_123", "svc_api"))
        .await
        .expect("deploy submit accepted");

    let rejected = repository
        .submit_backup(backup_submission("op_123"))
        .await
        .expect_err("operation id reuse across kinds is rejected");

    assert!(matches!(
        rejected,
        SubmitOperationError::DuplicateSequenceMismatch { sequence }
            if sequence == first.start_sequence
    ));
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::deploy_accepted(
            operation_id("op_123"),
            service_id("svc_api"),
            first.start_sequence,
        ))
    );
    let page = repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_123"),
            start_sequence: first.start_sequence,
            limit: event_replay_limit(10),
        })
        .await
        .expect("deploy replay succeeds");
    assert_eq!(page.events.len(), 1);
}

#[tokio::test]
async fn operation_repository_backup_submit_is_durable() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_backup(backup_submission("op_backup"))
        .await
        .expect("first backup accepted");

    assert!(first.should_start_execution);
    assert_eq!(first.operation_id, operation_id("op_backup"));
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_backup"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::backup_accepted(
            operation_id("op_backup"),
            first.start_sequence,
        ))
    );

    let page = repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_backup"),
            start_sequence: first.start_sequence,
            limit: event_replay_limit(10),
        })
        .await
        .expect("backup replay succeeds");

    let [event] = page.events.as_slice() else {
        panic!("expected one backup event");
    };
    assert_eq!(
        event.event,
        OperationEvent::BackupCreateSubmitted {
            operation_id: operation_id("op_backup"),
            target: first.target,
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
        .submit_deploy(deploy_submission("op_123", "svc_api"))
        .await
        .expect("submit accepted");
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("planning recorded");
    for stage in [
        DeployRunningStage::PreparingDataplane,
        DeployRunningStage::StartingContainers,
        DeployRunningStage::WaitingForHealth,
        DeployRunningStage::ActiveServiceCommit,
    ] {
        repository
            .record_deploy_transition(&operation_id("op_123"), DeployTransition::Running { stage })
            .await
            .expect("running stage recorded");
    }
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::completed())
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
