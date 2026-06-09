use super::fixtures::*;
use ployz_core::deploy::DeployCleanupContainer;
use ployz_core::ids::StepId;
use ployz_core::node::ManagedContainerKind;
use ployz_core::ops::{
    DeployCleanupFailure, DeployCompletionOutcome, DeployEvidence, DeployOperationState,
    DeployRunningStage, DeployTransition, OperationStatus,
};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, OperationEventAppend, OperationStatusWrite,
    RecordDeployTransitionError,
};

#[tokio::test]
async fn operation_repository_records_transition_status_against_real_nats() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            default_lease_claim(),
        )
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
            .operation_status(&operation_id("op_123"))
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
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            default_lease_claim(),
        )
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
                stage: DeployRunningStage::PreparingWireGuardEbpf,
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
                stage: DeployRunningStage::ActiveServiceCommit,
            },
        )
        .await
        .expect("active commit records");
    let cleanup_target = DeployCleanupContainer {
        node_id: node_id("node_a"),
        container_id: container_id("ctr_old"),
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_old"),
        operation_id: operation_id("op_old"),
        step_id: StepId::try_new("step_old").expect("valid step id"),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
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
            .operation_status(&operation_id("op_123"))
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
async fn operation_repository_rejects_duplicate_failed_transition_payload_mismatch() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            default_lease_claim(),
        )
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
        RecordDeployTransitionError::StoredTransitionMismatch { .. }
    ));
}

#[tokio::test]
async fn operation_repository_rejects_duplicate_cancelled_transition_payload_mismatch() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            default_lease_claim(),
        )
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
        RecordDeployTransitionError::StoredTransitionMismatch { .. }
    ));
}
