use super::fixtures::*;
use ployz_core::ops::{
    DeployEvidence, DeployOperationState, DeployRunningStage, DeployTransition, OperationStatus,
};
use ployz_nats::operations::{RecordDeployEvidenceError, StoredEventMismatchKind};

#[tokio::test]
async fn operation_repository_records_container_started_without_state_change() {
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
    repository
        .record_deploy_transition(
            &operation_id("op_123"),
            DeployTransition::Running {
                stage: DeployRunningStage::PreparingDataplane,
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
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("container started event records");
    let duplicate = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("duplicate container started event is idempotent");

    assert_eq!(duplicate.sequence, stored.sequence);
    assert!(duplicate.duplicate);
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_123"))
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
        .submit_deploy(deploy_submission("op_123", "svc_api"))
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
                stage: DeployRunningStage::PreparingDataplane,
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
            .records()
            .get(&operation_id("op_123"))
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
        .submit_deploy(deploy_submission("op_123", "svc_api"))
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
            .records()
            .get(&operation_id("op_123"))
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
        .submit_deploy(deploy_submission("op_123", "svc_api"))
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
                plan: deploy_plan_on("machine_a"),
            },
        )
        .await
        .expect("first plan records");

    let mismatch = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::PlanCreated {
                plan: deploy_plan_on("machine_b"),
            },
        )
        .await
        .expect_err("changed plan is rejected");

    assert!(matches!(
        mismatch,
        RecordDeployEvidenceError::StoredEventMismatch {
            kind: StoredEventMismatchKind::DeployPlan,
            ..
        }
    ));
}
