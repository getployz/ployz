use super::fixtures::*;
use ployz_core::machine::MachineName;
use ployz_core::ops::{OperationEvent, OperationEventReplayRequest, OperationStatus};
use ployz_core::roles::InstallRolePolicy;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    MachineAddOperationSubmission, OperationEventAppend, RedeemMachineJoinTokenError,
    StoredMachineAddClaim, StoredMachineAddJoinToken, StoredMachineAddSubmission,
};

#[tokio::test]
async fn operation_repository_machine_add_submit_is_durable_and_rejects_duplicate_key() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_machine_add(machine_add_submission(
            "op_machine",
            "idem_machine",
            "node_2",
            "edge_2",
        ))
        .await
        .expect("first machine add accepted");
    let _duplicate = repository
        .submit_machine_add(machine_add_submission(
            "op_other",
            "idem_machine",
            "node_2",
            "edge_2",
        ))
        .await
        .expect_err("duplicate machine add key is rejected");

    assert_eq!(first.operation_id, operation_id("op_machine"));
    assert_eq!(first.node_id, node_id("node_2"));
    assert_eq!(
        first.name,
        MachineName::try_new("edge_2").expect("valid machine name")
    );
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_machine"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: operation_id("op_machine"),
            node_id: node_id("node_2"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
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
            roles: InstallRolePolicy::install_all().without_gateway(),
            join_token: issued_join_token_for_raw("join_token"),
        }
    );
}
#[tokio::test]
async fn operation_repository_redeem_before_material_ready_is_typed_not_ready() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(machine_add_submission(
            "op_machine",
            "idem_machine",
            "node_2",
            "edge_2",
        ))
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
async fn machine_add_claim_does_not_expose_join_token_before_acceptance() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let idempotency_key = idempotency_key("idem_machine");
    let claimed_operation_id = operation_id("op_machine");
    let raw_join_token = raw_join_token("original_raw_join_token");
    status_store
        .put_machine_add_claim_if_absent(
            &idempotency_key,
            &stored_machine_add_claim(claimed_operation_id.clone(), raw_join_token.clone()),
        )
        .await
        .expect("write idempotency claim");
    status_store
        .put_machine_add_join_token_if_absent(
            &raw_join_token
                .fingerprint()
                .expect("test raw join token fingerprints"),
            &StoredMachineAddJoinToken {
                operation_id: claimed_operation_id,
                idempotency_key: idempotency_key.clone(),
            },
        )
        .await
        .expect("write join token claim");
    let original = MachineAddOperationSubmission {
        operation_id: operation_id("op_other"),
        node_id: node_id("node_2"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        roles: InstallRolePolicy::install_all().without_gateway(),
        join_bundle: machine_join_bundle(),
        raw_join_token: raw_join_token.clone(),
        join_token: issued_join_token_for_raw("original_raw_join_token"),
        idempotency_key,
    };
    let repository = AsyncNatsOperationRepository::new(event_log, status_store);

    assert!(matches!(
        repository
            .redeem_machine_join_token(&raw_join_token, joined_at(50))
            .await,
        Err(RedeemMachineJoinTokenError::UnknownJoinToken)
    ));

    repository
        .submit_machine_add(original)
        .await
        .expect_err("claim blocks a duplicate submit");
}

#[tokio::test]
async fn machine_add_retry_adopts_claimed_material_after_claim_only() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let idempotency_key = idempotency_key("idem_machine");
    let claimed_raw_join_token = raw_join_token("first_raw_join_token");
    status_store
        .put_machine_add_claim_if_absent(
            &idempotency_key,
            &stored_machine_add_claim(operation_id("op_machine"), claimed_raw_join_token.clone()),
        )
        .await
        .expect("write claim");
    let repository = AsyncNatsOperationRepository::new(event_log, status_store);

    let accepted = repository
        .submit_machine_add(machine_add_submission_with_raw(
            "op_machine",
            "idem_machine",
            "second_raw_join_token",
        ))
        .await
        .expect("retry adopts claimed material");

    assert_eq!(accepted.raw_join_token, claimed_raw_join_token);
    assert_eq!(
        accepted.join_token,
        issued_join_token_for_raw("first_raw_join_token")
    );
}

#[tokio::test]
async fn machine_add_retry_recovers_after_submit_event_without_submission() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let idempotency_key = idempotency_key("idem_machine");
    let claim = stored_machine_add_claim(
        operation_id("op_machine"),
        raw_join_token("first_raw_join_token"),
    );
    status_store
        .put_machine_add_claim_if_absent(&idempotency_key, &claim)
        .await
        .expect("write claim");
    let stored = event_log
        .append(OperationEventAppend::machine_add_submitted(
            claim.operation_id.clone(),
            claim.node_id.clone(),
            claim.name.clone(),
            claim.roles,
            claim.join_token.clone(),
        ))
        .await
        .expect("write submitted event");
    let repository = AsyncNatsOperationRepository::new(event_log, status_store);

    let accepted = repository
        .submit_machine_add(machine_add_submission_with_raw(
            "op_machine",
            "idem_machine",
            "second_raw_join_token",
        ))
        .await
        .expect("retry recovers submitted event");

    assert_eq!(accepted.start_sequence, stored.sequence);
    assert_machine_add_pending(&repository, accepted.start_sequence).await;
}

#[tokio::test]
async fn machine_add_retry_recovers_after_submission_without_status() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let idempotency_key = idempotency_key("idem_machine");
    let claim = stored_machine_add_claim(
        operation_id("op_machine"),
        raw_join_token("first_raw_join_token"),
    );
    status_store
        .put_machine_add_claim_if_absent(&idempotency_key, &claim)
        .await
        .expect("write claim");
    let stored = event_log
        .append(OperationEventAppend::machine_add_submitted(
            claim.operation_id.clone(),
            claim.node_id.clone(),
            claim.name.clone(),
            claim.roles,
            claim.join_token.clone(),
        ))
        .await
        .expect("write submitted event");
    status_store
        .put_machine_add_submission_if_absent(
            &idempotency_key,
            &StoredMachineAddSubmission {
                operation_id: claim.operation_id,
                idempotency_key: idempotency_key.clone(),
                start_sequence: stored.sequence,
                node_id: claim.node_id,
                name: claim.name,
                roles: claim.roles,
                join_bundle: claim.join_bundle,
                join_token: claim.join_token,
                raw_join_token: claim.raw_join_token,
            },
        )
        .await
        .expect("write accepted submission");
    let repository = AsyncNatsOperationRepository::new(event_log, status_store);

    let accepted = repository
        .submit_machine_add(machine_add_submission_with_raw(
            "op_machine",
            "idem_machine",
            "second_raw_join_token",
        ))
        .await
        .expect("retry recovers status");

    assert_eq!(accepted.start_sequence, stored.sequence);
    assert_machine_add_pending(&repository, accepted.start_sequence).await;
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
        .expect("write join token claim");
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
        .put_machine_add_claim_if_absent(
            &existing_idempotency_key,
            &stored_machine_add_claim(operation_id("op_existing"), raw_join_token.clone()),
        )
        .await
        .expect("write existing idempotency claim");
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
            .submit_machine_add(MachineAddOperationSubmission {
                operation_id: operation_id("op_machine"),
                node_id: node_id("node_2"),
                name: MachineName::try_new("edge_2").expect("valid machine name"),
                roles: InstallRolePolicy::install_all().without_gateway(),
                join_bundle: machine_join_bundle(),
                raw_join_token,
                join_token: issued_join_token_for_raw("shared_raw_join_token"),
                idempotency_key: idempotency_key("idem_machine"),
            },)
            .await
            .is_err()
    );
    assert!(
        repository
            .records()
            .get(&operation_id("op_machine"))
            .await
            .expect("status lookup succeeds")
            .is_none()
    );
}

fn stored_machine_add_claim(
    operation_id: ployz_core::ids::OperationId,
    raw_join_token: ployz_core::machine::RawJoinToken,
) -> StoredMachineAddClaim {
    let token = raw_join_token.as_str().to_owned();
    StoredMachineAddClaim {
        operation_id,
        node_id: node_id("node_2"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        roles: InstallRolePolicy::install_all().without_gateway(),
        join_bundle: machine_join_bundle(),
        join_token: issued_join_token_for_raw(&token),
        raw_join_token,
    }
}

fn machine_add_submission_with_raw(
    operation_id_value: &str,
    idempotency_key_value: &str,
    raw_join_token_value: &str,
) -> MachineAddOperationSubmission {
    MachineAddOperationSubmission {
        operation_id: operation_id(operation_id_value),
        node_id: node_id("node_2"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        roles: InstallRolePolicy::install_all().without_gateway(),
        join_bundle: machine_join_bundle(),
        raw_join_token: raw_join_token(raw_join_token_value),
        join_token: issued_join_token_for_raw(raw_join_token_value),
        idempotency_key: idempotency_key(idempotency_key_value),
    }
}

async fn assert_machine_add_pending(
    repository: &AsyncNatsOperationRepository,
    start_sequence: ployz_core::ops::EventSequence,
) {
    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_machine"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: operation_id("op_machine"),
            node_id: node_id("node_2"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            state: ployz_core::machine::MachineAddOperationState::Pending {
                join_token: issued_join_token_for_raw("first_raw_join_token"),
            },
            last_event_sequence: start_sequence,
        })
    );
}
