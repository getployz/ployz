use super::fixtures::*;
use ployz_core::ops::{
    DeployEvidence, DeployOperationState, DeployRunningStage, DeployTransition, OperationEvent,
    OperationStatus,
};
use ployz_nats::operations::{AsyncNatsOperationEventLog, OperationEventAppend};

#[tokio::test]
async fn operation_repository_retries_container_started_after_stage_advances() {
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
        .expect("starting transition records");

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
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("duplicate remains idempotent after stage advances");

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
        .expect("starting transition records");
    let stored = event_log
        .append(OperationEventAppend::from_event(
            OperationEvent::DeployContainerStarted {
                operation_id: operation_id("op_123"),
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
        ))
        .await
        .expect("container started event appends before status projection");

    let duplicate = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("duplicate retry accepts durable evidence");

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
async fn operation_repository_accepts_durable_container_evidence_after_stage_advances() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
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
        .append(OperationEventAppend::from_event(
            OperationEvent::DeployContainerStarted {
                operation_id: operation_id("op_123"),
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
        ))
        .await
        .expect("container evidence is durable before retry");

    let duplicate = repository
        .record_deploy_evidence(
            &operation_id("op_123"),
            DeployEvidence::ContainerStarted {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
        )
        .await
        .expect("durable evidence after stage advance remains idempotent");

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
                stage: active_service_running(),
            },
            last_event_sequence: event_sequence(7),
        })
    );
}
