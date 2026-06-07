use super::fixtures::*;
use ployz_core::machine::MachineName;
use ployz_core::ops::{
    DeployRunningStage, DeployTransition, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayRequest, OperationOwnershipStatus, OperationStatus,
};
use ployz_core::roles::FirstNodeGateway;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    MachineAddOperationSubmission, OperationEventAppend, OperationLeaseClaim,
    OperationLeaseClaimError, ReplayOperationEventsError, StoredMachineAddSubmission,
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

#[tokio::test]
async fn operation_repository_machine_add_submit_is_durable_and_idempotent() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_machine_add(
            machine_add_submission("op_machine", "idem_machine", "node_2", "edge_2"),
            default_lease_claim(),
        )
        .await
        .expect("first machine add accepted");
    let second = repository
        .submit_machine_add(
            machine_add_submission("op_other", "idem_machine", "node_3", "edge_3"),
            default_lease_claim(),
        )
        .await
        .expect("duplicate machine add accepted");

    assert_eq!(first, second);
    assert_eq!(first.operation_id, operation_id("op_machine"));
    assert_eq!(first.node_id, node_id("node_2"));
    assert_eq!(
        first.name,
        MachineName::try_new("edge_2").expect("valid machine name")
    );
    assert_eq!(
        repository
            .operation_status(&operation_id("op_machine"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: operation_id("op_machine"),
            node_id: node_id("node_2"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: FirstNodeGateway::Skip,
            state: ployz_core::machine::MachineAddOperationState::Pending {
                join_token: issued_join_token("join_hash"),
            },
            last_event_sequence: first.start_sequence,
        })
    );

    let page = repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_machine"),
            start_sequence: first.start_sequence,
            limit: event_replay_limit(10),
        })
        .await
        .expect("machine add replay succeeds");

    let [event] = page.events.as_slice() else {
        panic!("expected one machine add event");
    };
    assert_eq!(
        event.event,
        OperationEvent::MachineAddSubmitted {
            operation_id: operation_id("op_machine"),
            node_id: node_id("node_2"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: FirstNodeGateway::Skip,
            join_token: issued_join_token("join_hash"),
        }
    );
}

#[tokio::test]
async fn machine_add_retry_recovers_original_join_material_after_partial_submit() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let idempotency_key = idempotency_key("idem_machine");
    let original = StoredMachineAddSubmission {
        operation_id: operation_id("op_machine"),
        start_sequence: None,
        node_id: node_id("node_2"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        gateway: FirstNodeGateway::Skip,
        join_token: issued_join_token("original_hash"),
        raw_join_token: raw_join_token("original_raw_join_token"),
    };
    status_store
        .put_machine_add_submission_if_absent(&idempotency_key, &original)
        .await
        .expect("write idempotency record first");
    let stored = event_log
        .append(OperationEventAppend::machine_add_submitted(
            original.operation_id.clone(),
            original.node_id.clone(),
            original.name.clone(),
            original.gateway,
            original.join_token.clone(),
            &idempotency_key,
        ))
        .await
        .expect("append submitted event");
    let repository = AsyncNatsOperationRepository::new(event_log, status_store);

    let accepted = repository
        .submit_machine_add(
            MachineAddOperationSubmission {
                operation_id: operation_id("op_other"),
                node_id: node_id("node_3"),
                name: MachineName::try_new("edge_3").expect("valid machine name"),
                gateway: FirstNodeGateway::Install,
                join_token: issued_join_token("new_hash"),
                raw_join_token: raw_join_token("new_raw_join_token"),
                idempotency_key,
            },
            default_lease_claim(),
        )
        .await
        .expect("retry recovers original machine add");

    assert_eq!(accepted.operation_id, original.operation_id);
    assert_eq!(accepted.start_sequence, stored.sequence);
    assert_eq!(accepted.node_id, original.node_id);
    assert_eq!(accepted.name, original.name);
    assert_eq!(accepted.gateway, original.gateway);
    assert_eq!(accepted.join_token, original.join_token);
    assert_eq!(accepted.raw_join_token, original.raw_join_token);
}

#[tokio::test]
async fn machine_add_retry_with_recorded_sequence_does_not_append_again() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let idempotency_key = idempotency_key("idem_machine");
    let original_event = event_log
        .append(OperationEventAppend::machine_add_submitted(
            operation_id("op_machine"),
            node_id("node_2"),
            MachineName::try_new("edge_2").expect("valid machine name"),
            FirstNodeGateway::Skip,
            issued_join_token("original_hash"),
            &idempotency_key,
        ))
        .await
        .expect("append original submitted event");
    status_store
        .put_machine_add_submission_if_absent(
            &idempotency_key,
            &StoredMachineAddSubmission {
                operation_id: operation_id("op_machine"),
                start_sequence: Some(original_event.sequence),
                node_id: node_id("node_2"),
                name: MachineName::try_new("edge_2").expect("valid machine name"),
                gateway: FirstNodeGateway::Skip,
                join_token: issued_join_token("original_hash"),
                raw_join_token: raw_join_token("original_raw_join_token"),
            },
        )
        .await
        .expect("write recorded idempotency record");
    status_store
        .put_if_newer(&OperationStatus::machine_add_pending(
            operation_id("op_machine"),
            node_id("node_2"),
            MachineName::try_new("edge_2").expect("valid machine name"),
            FirstNodeGateway::Skip,
            issued_join_token("original_hash"),
            original_event.sequence,
        ))
        .await
        .expect("write operation status");
    let repository = AsyncNatsOperationRepository::new(event_log, status_store);

    let accepted = repository
        .submit_machine_add(
            MachineAddOperationSubmission {
                operation_id: operation_id("op_other"),
                node_id: node_id("node_3"),
                name: MachineName::try_new("edge_3").expect("valid machine name"),
                gateway: FirstNodeGateway::Install,
                join_token: issued_join_token("new_hash"),
                raw_join_token: raw_join_token("new_raw_join_token"),
                idempotency_key,
            },
            default_lease_claim(),
        )
        .await
        .expect("retry returns original machine add");

    assert_eq!(accepted.operation_id, operation_id("op_machine"));
    assert_eq!(accepted.start_sequence, original_event.sequence);
    let replay = repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_machine"),
            start_sequence: event_sequence(1),
            limit: event_replay_limit(10),
        })
        .await
        .expect("replay succeeds");
    assert_eq!(replay.events.len(), 1);
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
