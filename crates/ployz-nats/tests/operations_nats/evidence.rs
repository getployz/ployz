use super::fixtures::*;
use ployz_core::ops::{
    DeployEvidence, DeployOperationState, DeployRunningStage, DeployTransition, OperationStatus,
    StatusProjectionError,
};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, OperationEventAppend, RecordDeployEvidenceError,
};

#[tokio::test]
async fn operation_repository_records_container_started_without_state_change() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    repository
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
            last_event_sequence: event_sequence(4),
        })
    );
}

#[tokio::test]
async fn operation_repository_records_health_check_started_without_state_change() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    repository
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
            last_event_sequence: event_sequence(5),
        })
    );
}

#[tokio::test]
async fn operation_repository_records_plan_created_without_state_change() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    repository
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
                stage: DeployRunningStage::RouteCutover,
            },
        )
        .await
        .expect("route cutover transition records");
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
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
            last_event_sequence: event_sequence(4),
        })
    );
}

#[tokio::test]
async fn operation_repository_accepts_durable_container_evidence_after_stage_advances() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    repository
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
                stage: DeployRunningStage::RouteCutover,
            },
        )
        .await
        .expect("route cutover transition records");
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
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
                stage: DeployRunningStage::RouteCutover,
            },
        )
        .await
        .expect("route cutover transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: active_service_running(),
            },
        )
        .await
        .expect("committing transition records");
    let waiting = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect_err("waiting-for-health operation is rejected");
    assert!(matches!(
        waiting,
        RecordDeployEvidenceError::ProjectStatus(StatusProjectionError::InvalidTransition { .. })
    ));

    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::CleaningUp,
            },
        )
        .await
        .expect("cleanup transition records");
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Completed)
        .await
        .expect("completed transition records");
    let terminal = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect_err("terminal operation is rejected");
    assert!(matches!(
        terminal,
        RecordDeployEvidenceError::ProjectStatus(StatusProjectionError::InvalidTransition { .. })
    ));
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
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
                stage: DeployRunningStage::RouteCutover,
            },
        )
        .await
        .expect("route cutover transition records");
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: active_service_running(),
            },
        )
        .await
        .expect("committing transition records");
    let committing = repository
        .record_deploy_evidence(&operation_id("op_123"), DeployEvidence::HealthCheckStarted)
        .await
        .expect_err("committing operation is rejected");
    assert!(matches!(
        committing,
        RecordDeployEvidenceError::ProjectStatus(StatusProjectionError::InvalidTransition { .. })
    ));
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
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
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
                stage: DeployRunningStage::StartingContainers,
            },
        )
        .await
        .expect("running transition records");
    let running = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::PlanCreated {
                plan: deploy_plan(),
            },
        )
        .await
        .expect_err("running operation is rejected");
    assert!(matches!(
        running,
        RecordDeployEvidenceError::ProjectStatus(StatusProjectionError::InvalidTransition { .. })
    ));
}
