use super::fixtures::*;
use ployz_core::ops::{
    DeployEvidence, DeployOperationState, DeployRunningStage, DeployTransition, OperationStatus,
    StatusProjectionError,
};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, OperationEventAppend,
    RecordDeployEvidenceError,
};

#[tokio::test]
async fn operation_repository_records_container_started_without_state_change() {
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
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        )
        .await
        .expect("wireguard ebpf transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("running transition records");
    let stored = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("container started event records");
    let duplicate = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("duplicate container started event is idempotent");

    assert_eq!(duplicate.sequence, stored.sequence);
    assert!(duplicate.duplicate);
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Running {
                stage: DeployRunningStage::StartingContainers,
            },
            last_event_sequence: event_sequence(5),
        })
    );
}

#[tokio::test]
async fn operation_repository_records_health_check_started_without_state_change() {
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
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        )
        .await
        .expect("wireguard ebpf transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("running transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            },
        )
        .await
        .expect("waiting transition records");

    let stored = repository
        .record_deploy_evidence(&operation_id("op_123"), DeployEvidence::HealthCheckStarted)
        .await
        .expect("health-check event records");
    let duplicate = repository
        .record_deploy_evidence(&operation_id("op_123"), DeployEvidence::HealthCheckStarted)
        .await
        .expect("duplicate health-check event is idempotent");

    assert_eq!(duplicate.sequence, stored.sequence);
    assert!(duplicate.duplicate);
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Running {
                stage: DeployRunningStage::WaitingForHealth,
            },
            last_event_sequence: event_sequence(6),
        })
    );
}

#[tokio::test]
async fn operation_repository_records_plan_created_without_state_change() {
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

    let stored = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::PlanCreated {
                plan: deploy_plan(),
            },
        )
        .await
        .expect("plan created event records");
    let duplicate = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::PlanCreated {
                plan: deploy_plan(),
            },
        )
        .await
        .expect("duplicate plan created event is idempotent");

    assert_eq!(duplicate.sequence, stored.sequence);
    assert!(duplicate.duplicate);
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Planning,
            last_event_sequence: event_sequence(3),
        })
    );
}

#[tokio::test]
async fn operation_repository_rejects_plan_retry_with_different_steps() {
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
    repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::PlanCreated {
                plan: deploy_plan_on("node_a"),
            },
        )
        .await
        .expect("first plan records");

    let mismatch = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::PlanCreated {
                plan: deploy_plan_on("node_b"),
            },
        )
        .await
        .expect_err("changed plan is rejected");

    assert!(matches!(
        mismatch,
        RecordDeployEvidenceError::PlanMismatch { .. }
    ));
}

#[tokio::test]
async fn operation_repository_retries_container_started_after_stage_advances() {
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
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        )
        .await
        .expect("wireguard ebpf transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("starting transition records");

    let stored = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("container started event records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            },
        )
        .await
        .expect("waiting transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: active_service_running(),
            },
        )
        .await
        .expect("committing transition records");
    let duplicate = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("duplicate remains idempotent after stage advances");

    assert_eq!(duplicate.sequence, stored.sequence);
    assert!(duplicate.duplicate);
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Running {
                stage: active_service_running(),
            },
            last_event_sequence: event_sequence(7),
        })
    );
}

#[tokio::test]
async fn operation_repository_keeps_status_cursor_when_retrying_durable_evidence() {
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
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("planning transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        )
        .await
        .expect("wireguard ebpf transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("starting transition records");
    let stored = event_log
        .append(OperationEventAppend::deploy_container_started(
            &operation_id("op_123"),
            &node_id("node_a"),
            &container_id("ctr_1"),
        ))
        .await
        .expect("container started event appends before status projection");

    let duplicate = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("duplicate retry accepts durable evidence");

    assert_eq!(duplicate.sequence, stored.sequence);
    assert!(duplicate.duplicate);
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Running {
                stage: DeployRunningStage::StartingContainers,
            },
            last_event_sequence: event_sequence(5),
        })
    );
}

#[tokio::test]
async fn operation_repository_accepts_durable_container_evidence_after_stage_advances() {
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
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("planning transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        )
        .await
        .expect("wireguard ebpf transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("starting transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            },
        )
        .await
        .expect("waiting transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: active_service_running(),
            },
        )
        .await
        .expect("committing transition records");
    let stored = event_log
        .append(OperationEventAppend::deploy_container_started(
            &operation_id("op_123"),
            &node_id("node_a"),
            &container_id("ctr_1"),
        ))
        .await
        .expect("container evidence is durable before retry");

    let duplicate = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("durable evidence after stage advance remains idempotent");

    assert_eq!(duplicate.sequence, stored.sequence);
    assert!(duplicate.duplicate);
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Running {
                stage: active_service_running(),
            },
            last_event_sequence: event_sequence(7),
        })
    );
}

#[tokio::test]
async fn operation_repository_rejects_container_started_for_non_running_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let missing = repository
        .record_deploy_evidence(
            &operation_id("op_missing"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect_err("missing operation is rejected");
    assert!(matches!(
        missing,
        RecordDeployEvidenceError::MissingOperation { .. }
    ));

    repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            default_lease_claim(),
        )
        .await
        .expect("submit accepted");
    let accepted = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect_err("accepted operation is rejected");
    assert!(matches!(
        accepted,
        RecordDeployEvidenceError::ProjectStatus(StatusProjectionError::InvalidTransition { .. })
    ));

    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("planning transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        )
        .await
        .expect("wireguard ebpf transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("running transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            },
        )
        .await
        .expect("waiting transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: active_service_running(),
            },
        )
        .await
        .expect("committing transition records");
    let late = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("late container evidence records while operation is still running");
    assert_eq!(late.sequence, event_sequence(7));
    assert!(!late.duplicate);
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Running {
                stage: active_service_running(),
            },
            last_event_sequence: event_sequence(7),
        })
    );

    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Completed)
        .await
        .expect("completed transition records");
    let terminal_duplicate = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("already-durable evidence remains idempotent after terminal state");
    assert_eq!(terminal_duplicate.sequence, event_sequence(7));
    assert!(terminal_duplicate.duplicate);
}

#[tokio::test]
async fn operation_repository_rejects_health_check_started_for_non_running_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let missing = repository
        .record_deploy_evidence(
            &operation_id("op_missing"),
            DeployEvidence::HealthCheckStarted,
        )
        .await
        .expect_err("missing operation is rejected");
    assert!(matches!(
        missing,
        RecordDeployEvidenceError::MissingOperation { .. }
    ));

    repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            default_lease_claim(),
        )
        .await
        .expect("submit accepted");
    let accepted = repository
        .record_deploy_evidence(&operation_id("op_123"), DeployEvidence::HealthCheckStarted)
        .await
        .expect_err("accepted operation is rejected");
    assert!(matches!(
        accepted,
        RecordDeployEvidenceError::ProjectStatus(StatusProjectionError::InvalidTransition { .. })
    ));

    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("planning transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        )
        .await
        .expect("wireguard ebpf transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("running transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            },
        )
        .await
        .expect("waiting transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: active_service_running(),
            },
        )
        .await
        .expect("committing transition records");
    let late = repository
        .record_deploy_evidence(&operation_id("op_123"), DeployEvidence::HealthCheckStarted)
        .await
        .expect("late health evidence records while operation is still running");
    assert_eq!(late.sequence, event_sequence(7));
    assert!(!late.duplicate);
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Running {
                stage: active_service_running(),
            },
            last_event_sequence: event_sequence(7),
        })
    );
}

#[tokio::test]
async fn operation_repository_rejects_plan_created_after_planning() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let missing = repository
        .record_deploy_evidence(
            &operation_id("op_missing"),
            DeployEvidence::PlanCreated {
                plan: deploy_plan(),
            },
        )
        .await
        .expect_err("missing operation is rejected");
    assert!(matches!(
        missing,
        RecordDeployEvidenceError::MissingOperation { .. }
    ));

    repository
        .submit_deploy(
            deploy_submission("op_123", "idem_1", "svc_api"),
            default_lease_claim(),
        )
        .await
        .expect("submit accepted");
    let accepted = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::PlanCreated {
                plan: deploy_plan(),
            },
        )
        .await
        .expect_err("accepted operation is rejected");
    assert!(matches!(
        accepted,
        RecordDeployEvidenceError::ProjectStatus(StatusProjectionError::InvalidTransition { .. })
    ));

    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("planning transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        )
        .await
        .expect("wireguard ebpf transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("running transition records");
    let late = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::PlanCreated {
                plan: deploy_plan(),
            },
        )
        .await
        .expect("late plan evidence records after execution starts");
    assert_eq!(late.sequence, event_sequence(5));
    assert!(!late.duplicate);
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Running {
                stage: DeployRunningStage::StartingContainers,
            },
            last_event_sequence: event_sequence(5),
        })
    );
}

#[tokio::test]
async fn operation_repository_rejects_fresh_evidence_after_completed_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    submit_deploy(&repository, "op_123").await;
    record_active_commit_stage(&repository, "op_123").await;
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Completed)
        .await
        .expect("completed transition records");

    assert_fresh_terminal_evidence_rejected(&repository, "op_123").await;
}

#[tokio::test]
async fn operation_repository_rejects_fresh_evidence_after_failed_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    submit_deploy(&repository, "op_123").await;
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Failed {
                failure: planning_failure("deploy failed"),
            },
        )
        .await
        .expect("failed transition records");

    assert_fresh_terminal_evidence_rejected(&repository, "op_123").await;
}

#[tokio::test]
async fn operation_repository_rejects_fresh_evidence_after_cancelled_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    submit_deploy(&repository, "op_123").await;
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Cancelled {
                reason: cancellation_reason("cancelled by operator"),
            },
        )
        .await
        .expect("cancelled transition records");

    assert_fresh_terminal_evidence_rejected(&repository, "op_123").await;
}

#[tokio::test]
async fn operation_repository_rejects_durable_evidence_after_terminal_cursor() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());

    submit_deploy(&repository, "op_completed").await;
    record_active_commit_stage(&repository, "op_completed").await;
    repository
        .record_deploy_transition(&operation_id("op_completed"), DeployTransition::Completed)
        .await
        .expect("completed transition records");
    assert_durable_terminal_evidence_rejected(&repository, &event_log, "op_completed").await;

    submit_deploy(&repository, "op_failed").await;
    repository
        .record_deploy_transition(
            &operation_id("op_failed"),
            DeployTransition::Failed {
                failure: planning_failure("deploy failed"),
            },
        )
        .await
        .expect("failed transition records");
    assert_durable_terminal_evidence_rejected(&repository, &event_log, "op_failed").await;

    submit_deploy(&repository, "op_cancelled").await;
    repository
        .record_deploy_transition(
            &operation_id("op_cancelled"),
            DeployTransition::Cancelled {
                reason: cancellation_reason("cancelled by operator"),
            },
        )
        .await
        .expect("cancelled transition records");
    assert_durable_terminal_evidence_rejected(&repository, &event_log, "op_cancelled").await;
}

async fn submit_deploy(repository: &AsyncNatsOperationRepository, operation: &str) {
    repository
        .submit_deploy(
            deploy_submission(operation, &format!("idem_{operation}"), "svc_api"),
            default_lease_claim(),
        )
        .await
        .expect("submit accepted");
}

async fn record_active_commit_stage(repository: &AsyncNatsOperationRepository, operation: &str) {
    repository
        .record_deploy_transition(&operation_id(operation), DeployTransition::Planning)
        .await
        .expect("planning transition records");
    repository
        .record_deploy_transition(
            &operation_id(operation),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        )
        .await
        .expect("wireguard ebpf transition records");
    repository
        .record_deploy_transition(
            &operation_id(operation),
            DeployTransition::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("starting transition records");
    repository
        .record_deploy_transition(
            &operation_id(operation),
            DeployTransition::Running {
                stage: DeployRunningStage::WaitingForHealth,
            },
        )
        .await
        .expect("waiting transition records");
    repository
        .record_deploy_transition(
            &operation_id(operation),
            DeployTransition::Running {
                stage: active_service_running(),
            },
        )
        .await
        .expect("active commit transition records");
}

async fn assert_fresh_terminal_evidence_rejected(
    repository: &AsyncNatsOperationRepository,
    operation: &str,
) {
    for evidence in fresh_terminal_evidence() {
        let error = repository
            .record_deploy_evidence(&operation_id(operation), evidence)
            .await
            .expect_err("fresh terminal evidence is rejected");
        assert!(matches!(
            error,
            RecordDeployEvidenceError::ProjectStatus(
                StatusProjectionError::InvalidTransition { .. }
                    | StatusProjectionError::TerminalState { .. }
            )
        ));
    }
}

fn fresh_terminal_evidence() -> Vec<DeployEvidence> {
    vec![
        DeployEvidence::PlanCreated {
            plan: deploy_plan_on("node_terminal"),
        },
        DeployEvidence::ContainerStarted {
            node_id: node_id("node_terminal"),
            container_id: container_id("ctr_terminal"),
        },
        DeployEvidence::HealthCheckStarted,
    ]
}

async fn assert_durable_terminal_evidence_rejected(
    repository: &AsyncNatsOperationRepository,
    event_log: &AsyncNatsOperationEventLog,
    operation: &str,
) {
    let terminal_status = repository
        .operation_status(&operation_id(operation))
        .await
        .expect("status lookup succeeds")
        .expect("operation exists");
    assert!(terminal_status.is_terminal());

    event_log
        .append(OperationEventAppend::deploy_container_started(
            &operation_id(operation),
            &node_id("node_terminal"),
            &container_id("ctr_terminal"),
        ))
        .await
        .expect("late evidence is durable before retry");

    let error = repository
        .record_deploy_evidence(
            &operation_id(operation),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_terminal"),
                container_id: container_id("ctr_terminal"),
            },
        )
        .await
        .expect_err("durable evidence after terminal cursor is rejected");
    assert!(matches!(
        error,
        RecordDeployEvidenceError::ProjectStatus(
            StatusProjectionError::InvalidTransition { .. }
                | StatusProjectionError::TerminalState { .. }
        )
    ));
    assert_eq!(
        repository
            .operation_status(&operation_id(operation))
            .await
            .expect("status lookup succeeds"),
        Some(terminal_status)
    );
}
