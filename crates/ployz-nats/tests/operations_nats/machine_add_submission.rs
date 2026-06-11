use super::fixtures::*;
use ployz_core::machine::MachineName;
use ployz_core::ops::{OperationEvent, OperationEventReplayRequest, OperationStatus};
use ployz_core::roles::FirstNodeGateway;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    MachineAddOperationSubmission, OperationEventAppend, RedeemMachineJoinTokenError,
    StoredMachineAddJoinToken, StoredMachineAddSubmission,
};

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
            machine_add_submission("op_other", "idem_machine", "node_2", "edge_2"),
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
                join_token: issued_join_token_for_raw("join_token"),
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
            join_token: issued_join_token_for_raw("join_token"),
        }
    );
}
#[tokio::test]
async fn operation_repository_redeem_before_material_ready_is_typed_not_ready() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(
            machine_add_submission("op_machine", "idem_machine", "node_2", "edge_2"),
            default_lease_claim(),
        )
        .await
        .expect("machine add accepted");

    assert!(matches!(
        repository
            .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
            .await,
        Err(RedeemMachineJoinTokenError::MissingSecretDelivery { operation_id })
            if operation_id == accepted.operation_id
    ));
    // The operation is still Pending: a later redeem (after material) works.
    store_minted_secret(&repository, "op_machine", "idem_machine").await;
    repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(55))
        .await
        .expect("redeem succeeds once material is ready");
}
#[tokio::test]
async fn machine_add_partial_submission_does_not_expose_join_token_before_acceptance() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let idempotency_key = idempotency_key("idem_machine");
    let original = StoredMachineAddSubmission {
        operation_id: operation_id("op_machine"),
        idempotency_key: idempotency_key.clone(),
        start_sequence: None,
        node_id: node_id("node_2"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        gateway: FirstNodeGateway::Skip,
        join_bundle: machine_join_bundle(),
        raw_join_token: raw_join_token("original_raw_join_token"),
        join_token: issued_join_token_for_raw("original_raw_join_token"),
    };
    status_store
        .put_machine_add_submission_if_absent(&idempotency_key, &original)
        .await
        .expect("write partial idempotency record");
    let repository = AsyncNatsOperationRepository::new(event_log, status_store);

    assert!(matches!(
        repository
            .redeem_machine_join_token(&original.raw_join_token, joined_at(50))
            .await,
        Err(RedeemMachineJoinTokenError::UnknownJoinToken)
    ));

    let accepted = repository
        .submit_machine_add(
            MachineAddOperationSubmission {
                operation_id: operation_id("op_other"),
                node_id: original.node_id.clone(),
                name: original.name.clone(),
                gateway: original.gateway,
                join_bundle: machine_join_bundle(),
                raw_join_token: original.raw_join_token.clone(),
                join_token: original.join_token.clone(),
                idempotency_key,
            },
            default_lease_claim(),
        )
        .await
        .expect("retry accepts original machine add");
    store_minted_secret(&repository, "op_machine", "idem_machine").await;
    repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
        .await
        .expect("accepted join token redeems after retry records sequence and index");
}
#[tokio::test]
async fn machine_add_join_token_index_rejects_unaccepted_submission() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let idempotency_key = idempotency_key("idem_machine");
    let raw_join_token = raw_join_token("original_raw_join_token");
    status_store
        .put_machine_add_submission_if_absent(
            &idempotency_key,
            &StoredMachineAddSubmission {
                operation_id: operation_id("op_machine"),
                idempotency_key: idempotency_key.clone(),
                start_sequence: None,
                node_id: node_id("node_2"),
                name: MachineName::try_new("edge_2").expect("valid machine name"),
                gateway: FirstNodeGateway::Skip,
                join_bundle: machine_join_bundle(),
                raw_join_token: raw_join_token.clone(),
                join_token: issued_join_token_for_raw("original_raw_join_token"),
            },
        )
        .await
        .expect("write partial idempotency record");
    status_store
        .put_machine_add_join_token_if_absent(
            &raw_join_token
                .fingerprint()
                .expect("test raw join token fingerprints"),
            &StoredMachineAddJoinToken {
                operation_id: operation_id("op_machine"),
                idempotency_key,
            },
        )
        .await
        .expect("write corrupted join token index");
    let repository = AsyncNatsOperationRepository::new(event_log, status_store);

    assert!(matches!(
        repository
            .redeem_machine_join_token(&raw_join_token, joined_at(50))
            .await,
        Err(RedeemMachineJoinTokenError::UnknownJoinToken)
    ));
}
#[tokio::test]
async fn machine_add_join_token_fingerprint_conflict_fails_before_operation_status() {
    let nats = test_nats().await;
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let raw_join_token = raw_join_token("shared_raw_join_token");
    let existing_idempotency_key = idempotency_key("idem_existing");
    status_store
        .put_machine_add_submission_if_absent(
            &existing_idempotency_key,
            &StoredMachineAddSubmission {
                operation_id: operation_id("op_existing"),
                idempotency_key: existing_idempotency_key.clone(),
                start_sequence: None,
                node_id: node_id("node_existing"),
                name: MachineName::try_new("edge_existing").expect("valid machine name"),
                gateway: FirstNodeGateway::Skip,
                join_bundle: machine_join_bundle(),
                raw_join_token: raw_join_token.clone(),
                join_token: issued_join_token_for_raw("shared_raw_join_token"),
            },
        )
        .await
        .expect("write existing partial idempotency record");
    status_store
        .put_machine_add_join_token_if_absent(
            &raw_join_token
                .fingerprint()
                .expect("test raw join token fingerprints"),
            &StoredMachineAddJoinToken {
                operation_id: operation_id("op_existing"),
                idempotency_key: existing_idempotency_key,
            },
        )
        .await
        .expect("reserve existing join token fingerprint");
    let repository = AsyncNatsOperationRepository::new(
        AsyncNatsOperationEventLog::new(nats.jetstream.clone()),
        status_store,
    );

    assert!(
        repository
            .submit_machine_add(
                MachineAddOperationSubmission {
                    operation_id: operation_id("op_machine"),
                    node_id: node_id("node_2"),
                    name: MachineName::try_new("edge_2").expect("valid machine name"),
                    gateway: FirstNodeGateway::Skip,
                    join_bundle: machine_join_bundle(),
                    raw_join_token,
                    join_token: issued_join_token_for_raw("shared_raw_join_token"),
                    idempotency_key: idempotency_key("idem_machine"),
                },
                default_lease_claim(),
            )
            .await
            .is_err()
    );
    assert!(
        repository
            .operation_status(&operation_id("op_machine"))
            .await
            .expect("status lookup succeeds")
            .is_none()
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
        idempotency_key: idempotency_key.clone(),
        start_sequence: None,
        node_id: node_id("node_2"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        gateway: FirstNodeGateway::Skip,
        join_bundle: machine_join_bundle(),
        raw_join_token: raw_join_token("original_raw_join_token"),
        join_token: issued_join_token_for_raw("original_raw_join_token"),
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
                node_id: original.node_id.clone(),
                name: original.name.clone(),
                gateway: original.gateway,
                join_bundle: machine_join_bundle(),
                raw_join_token: original.raw_join_token.clone(),
                join_token: original.join_token.clone(),
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
            issued_join_token_for_raw("original_raw_join_token"),
            &idempotency_key,
        ))
        .await
        .expect("append original submitted event");
    status_store
        .put_machine_add_submission_if_absent(
            &idempotency_key,
            &StoredMachineAddSubmission {
                operation_id: operation_id("op_machine"),
                idempotency_key: idempotency_key.clone(),
                start_sequence: Some(original_event.sequence),
                node_id: node_id("node_2"),
                name: MachineName::try_new("edge_2").expect("valid machine name"),
                gateway: FirstNodeGateway::Skip,
                join_bundle: machine_join_bundle(),
                raw_join_token: raw_join_token("original_raw_join_token"),
                join_token: issued_join_token_for_raw("original_raw_join_token"),
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
            issued_join_token_for_raw("original_raw_join_token"),
            original_event.sequence,
        ))
        .await
        .expect("write operation status");
    let repository = AsyncNatsOperationRepository::new(event_log, status_store);

    let accepted = repository
        .submit_machine_add(
            MachineAddOperationSubmission {
                operation_id: operation_id("op_other"),
                node_id: node_id("node_2"),
                name: MachineName::try_new("edge_2").expect("valid machine name"),
                gateway: FirstNodeGateway::Skip,
                join_bundle: machine_join_bundle(),
                raw_join_token: raw_join_token("original_raw_join_token"),
                join_token: issued_join_token_for_raw("original_raw_join_token"),
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
