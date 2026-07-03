use super::fixtures::*;
use ployz_core::deploy::DeployCleanupContainer;
use ployz_core::ops::{
    DeployCleanupFailure, DeployCompletionOutcome, DeployEvidence, DeployOperationState,
    DeployRunningStage, DeployTransition, OperationStatus, StatusProjectionError,
};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, OperationEventAppend,
    OperationStatusWrite, RecordDeployTransitionError,
};
use ployz_test_support::containers;

#[tokio::test]
async fn operation_repository_records_transition_status_against_real_nats() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    repository
        .submit_deploy(deploy_submission("op_123", "svc_api"))
        .await
        .expect("submit accepted");

    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("planning transition records");
    let duplicate = repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("duplicate planning is satisfied");

    assert!(matches!(
        duplicate,
        OperationStatusWrite::AlreadySatisfied {
            current_sequence
        } if current_sequence == event_sequence(2)
    ));
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Planning,
            last_event_sequence: event_sequence(2),
        })
    );
}

#[tokio::test]
async fn operation_repository_records_deploy_completion_warning_outcome_against_real_nats() {
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
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingDataplane,
            },
        )
        .await
        .expect("dataplane prep records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("starting records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            },
        )
        .await
        .expect("health records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::ServingTargetCommit,
            },
        )
        .await
        .expect("active commit records");
    let cleanup_target = DeployCleanupContainer {
        machine_id: machine_id("machine_a"),
        container_id: container_id("ctr_old"),
        identity: containers::identity("svc_api")
            .entry("rev_old")
            .operation("op_old")
            .step("step_old")
            .build(),
    };
    repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::CleanupFinished {
                removed: Vec::new(),
                failed: vec![DeployCleanupFailure {
                    target: cleanup_target,
                    message: failure_message("container remove failed: busy"),
                }],
            },
        )
        .await
        .expect("cleanup warning evidence records");

    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Completed {
                outcome: DeployCompletionOutcome::CompletedWithWarnings,
            },
        )
        .await
        .expect("warning completion records");

    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Completed {
                outcome: DeployCompletionOutcome::CompletedWithWarnings,
            },
            last_event_sequence: event_sequence(8),
        })
    );
}

#[tokio::test]
async fn operation_repository_rejects_terminal_rewrite_against_real_nats() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    repository
        .submit_deploy(deploy_submission("op_terminal", "svc_api"))
        .await
        .expect("submit accepted");
    record_deploy_running(&repository, "op_terminal").await;

    repository
        .record_deploy_transition(
            &operation_id("op_terminal"),
            DeployTransition::Completed {
                outcome: DeployCompletionOutcome::Completed,
            },
        )
        .await
        .expect("completion records");
    let rewrite = repository
        .record_deploy_transition(
            &operation_id("op_terminal"),
            DeployTransition::Completed {
                outcome: DeployCompletionOutcome::CompletedWithWarnings,
            },
        )
        .await
        .expect_err("different terminal result is rejected");

    assert!(matches!(
        rewrite,
        RecordDeployTransitionError::ProjectStatus(StatusProjectionError::TerminalState { .. })
    ));
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_terminal"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_terminal"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Completed {
                outcome: DeployCompletionOutcome::Completed,
            },
            last_event_sequence: event_sequence(7),
        })
    );
}

#[tokio::test]
async fn operation_repository_rejects_duplicate_failed_transition_payload_mismatch() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    repository
        .submit_deploy(deploy_submission("op_123", "svc_api"))
        .await
        .expect("submit accepted");
    event_log
        .append(OperationEventAppend::deploy_transition(
            &operation_id("op_123"),
            &DeployTransition::Failed {
                failure: planning_failure("first failure"),
            },
        ))
        .await
        .expect("failed event stores without status projection");

    let mismatch = repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Failed {
                failure: planning_failure("second failure"),
            },
        )
        .await
        .expect_err("different failed payload is rejected");

    assert!(matches!(
        mismatch,
        RecordDeployTransitionError::StoredEventMismatch { .. }
    ));
}

async fn record_deploy_running(
    repository: &AsyncNatsOperationRepository,
    operation_id_value: &str,
) {
    repository
        .record_deploy_transition(
            &operation_id(operation_id_value),
            DeployTransition::Planning,
        )
        .await
        .expect("planning records");
    for stage in [
        DeployRunningStage::PreparingDataplane,
        DeployRunningStage::StartingContainers,
        DeployRunningStage::WaitingForHealth,
        DeployRunningStage::ServingTargetCommit,
    ] {
        repository
            .record_deploy_transition(
                &operation_id(operation_id_value),
                DeployTransition::Running { stage },
            )
            .await
            .expect("running stage records");
    }
}

#[tokio::test]
async fn operation_repository_rejects_duplicate_cancelled_transition_payload_mismatch() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    repository
        .submit_deploy(deploy_submission("op_123", "svc_api"))
        .await
        .expect("submit accepted");
    event_log
        .append(OperationEventAppend::deploy_transition(
            &operation_id("op_123"),
            &DeployTransition::Cancelled {
                reason: cancellation_reason("first cancel"),
            },
        ))
        .await
        .expect("cancelled event stores without status projection");

    let mismatch = repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Cancelled {
                reason: cancellation_reason("second cancel"),
            },
        )
        .await
        .expect_err("different cancelled payload is rejected");

    assert!(matches!(
        mismatch,
        RecordDeployTransitionError::StoredEventMismatch { .. }
    ));
}
