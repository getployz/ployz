use super::fixtures::*;
use ployz_core::machine::{MachineAddFailure, MachineAddOperationState, MachineName};
use ployz_core::ops::{
    DeployRunningStage, DeployTransition, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayRequest, OperationOwnershipStatus, OperationStatus,
};
use ployz_core::roles::FirstNodeGateway;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    MachineAddOperationSubmission, MachineJoinRedemption, OperationEventAppend,
    OperationLeaseClaim, OperationLeaseClaimError, RedeemMachineJoinTokenError,
    RedeemedMachineJoin, ReplayOperationEventsError, StoredMachineAddJoinToken,
    StoredMachineAddSubmission,
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
async fn operation_repository_records_machine_add_joined_transition() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(
            machine_add_submission("op_machine", "idem_machine", "node_2", "edge_2"),
            default_lease_claim(),
        )
        .await
        .expect("machine add accepted");

    repository
        .record_machine_add_joined(&accepted.operation_id, &accepted.node_id, joined_at(50))
        .await
        .expect("joined transition records");

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
            state: ployz_core::machine::MachineAddOperationState::Joining {
                joined_at: joined_at(50),
            },
            last_event_sequence: event_sequence(2),
        })
    );

    let page = repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_machine"),
            start_sequence: accepted.start_sequence,
            limit: event_replay_limit(10),
        })
        .await
        .expect("machine add replay succeeds");

    assert_eq!(page.events.len(), 2);
    assert_eq!(
        page.events.last().map(|event| &event.event),
        Some(&OperationEvent::MachineAddJoined {
            operation_id: operation_id("op_machine"),
            node_id: node_id("node_2"),
            joined_at: joined_at(50),
        })
    );
}

#[tokio::test]
async fn operation_repository_redeems_machine_join_token_once() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(
            machine_add_submission("op_machine", "idem_machine", "node_2", "edge_2"),
            default_lease_claim(),
        )
        .await
        .expect("machine add accepted");

    let redemption = repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
        .await
        .expect("join token redeems");

    let MachineJoinRedemption::Joined(joined) = redemption else {
        panic!("expected first redemption to join");
    };
    assert_eq!(joined.operation_id, accepted.operation_id);
    assert_eq!(joined.node_id, accepted.node_id);
    assert_eq!(joined.name, accepted.name);
    assert_eq!(joined.gateway, accepted.gateway);
    assert_eq!(joined.joined_at, joined_at(50));
    assert_eq!(joined.last_event_sequence, event_sequence(2));
    assert_eq!(
        repository
            .operation_status(&accepted.operation_id)
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: accepted.operation_id.clone(),
            node_id: accepted.node_id.clone(),
            name: accepted.name.clone(),
            gateway: accepted.gateway,
            state: MachineAddOperationState::Joining {
                joined_at: joined_at(50),
            },
            last_event_sequence: event_sequence(2),
        })
    );
}

#[tokio::test]
async fn operation_repository_machine_join_can_complete_after_local_install() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(
            machine_add_submission("op_machine", "idem_machine", "node_2", "edge_2"),
            default_lease_claim(),
        )
        .await
        .expect("machine add accepted");

    repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
        .await
        .expect("join token redeems");
    repository
        .record_machine_add_completed(&accepted.operation_id, &accepted.node_id)
        .await
        .expect("machine add completes");

    assert_eq!(
        repository
            .operation_status(&accepted.operation_id)
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: accepted.operation_id,
            node_id: accepted.node_id,
            name: accepted.name,
            gateway: accepted.gateway,
            state: MachineAddOperationState::Completed,
            last_event_sequence: event_sequence(3),
        })
    );
}

#[tokio::test]
async fn operation_repository_repeated_machine_join_token_returns_joined_facts() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(
            machine_add_submission("op_machine", "idem_machine", "node_2", "edge_2"),
            default_lease_claim(),
        )
        .await
        .expect("machine add accepted");

    repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
        .await
        .expect("first join token redemption succeeds");
    let second = repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(55))
        .await
        .expect("second join token redemption returns current join facts");

    assert_eq!(
        second,
        MachineJoinRedemption::AlreadyJoined(RedeemedMachineJoin {
            operation_id: accepted.operation_id,
            node_id: accepted.node_id,
            name: accepted.name,
            gateway: accepted.gateway,
            join_bundle: accepted.join_bundle,
            joined_at: joined_at(50),
            last_event_sequence: event_sequence(2),
        })
    );
}

#[tokio::test]
async fn operation_repository_duplicate_join_event_returns_original_joined_facts() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open status store");
    let repository = AsyncNatsOperationRepository::new(event_log.clone(), status_store);
    let accepted = repository
        .submit_machine_add(
            machine_add_submission("op_machine", "idem_machine", "node_2", "edge_2"),
            default_lease_claim(),
        )
        .await
        .expect("machine add accepted");
    event_log
        .append(OperationEventAppend::machine_add_joined(
            &accepted.operation_id,
            &accepted.node_id,
            joined_at(50),
        ))
        .await
        .expect("joined event append succeeds");

    let redemption = repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(55))
        .await
        .expect("duplicate joined event projects original join facts");

    assert_eq!(
        redemption,
        MachineJoinRedemption::Joined(RedeemedMachineJoin {
            operation_id: accepted.operation_id,
            node_id: accepted.node_id,
            name: accepted.name,
            gateway: accepted.gateway,
            join_bundle: accepted.join_bundle,
            joined_at: joined_at(50),
            last_event_sequence: event_sequence(2),
        })
    );
}

#[tokio::test]
async fn operation_repository_unknown_machine_join_token_has_no_operation_to_mutate() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    assert!(matches!(
        repository
            .redeem_machine_join_token(&raw_join_token("unknown_join_token"), joined_at(50))
            .await,
        Err(RedeemMachineJoinTokenError::UnknownJoinToken)
    ));
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
                node_id: node_id("node_3"),
                name: MachineName::try_new("edge_3").expect("valid machine name"),
                gateway: FirstNodeGateway::Install,
                join_bundle: machine_join_bundle(),
                raw_join_token: raw_join_token("new_raw_join_token"),
                join_token: issued_join_token_for_raw("new_raw_join_token"),
                idempotency_key,
            },
            default_lease_claim(),
        )
        .await
        .expect("retry accepts original machine add");
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
async fn operation_repository_expired_machine_join_token_records_failure() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(
            MachineAddOperationSubmission {
                operation_id: operation_id("op_machine"),
                node_id: node_id("node_2"),
                name: MachineName::try_new("edge_2").expect("valid machine name"),
                gateway: FirstNodeGateway::Skip,
                join_bundle: machine_join_bundle(),
                raw_join_token: raw_join_token("short_lived_join_token"),
                join_token: issued_join_token_for_raw_with_expiry("short_lived_join_token", 40),
                idempotency_key: idempotency_key("idem_machine"),
            },
            default_lease_claim(),
        )
        .await
        .expect("machine add accepted");

    assert!(matches!(
        repository
            .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
            .await,
        Err(RedeemMachineJoinTokenError::JoinRejected {
            operation_id,
            failure: MachineAddFailure::JoinTokenExpired { .. },
        }) if operation_id == accepted.operation_id
    ));
    assert_eq!(
        repository
            .operation_status(&accepted.operation_id)
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: accepted.operation_id,
            node_id: accepted.node_id,
            name: accepted.name,
            gateway: accepted.gateway,
            state: MachineAddOperationState::Failed {
                failure: MachineAddFailure::JoinTokenExpired {
                    expired_at: expires_at(40),
                },
            },
            last_event_sequence: event_sequence(2),
        })
    );
}

#[tokio::test]
async fn operation_repository_late_expired_join_token_cannot_fail_joined_machine_add() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(
            machine_add_submission("op_machine", "idem_machine", "node_2", "edge_2"),
            default_lease_claim(),
        )
        .await
        .expect("machine add accepted");
    repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
        .await
        .expect("join token redeems");

    assert!(
        repository
            .record_machine_add_failed(
                &accepted.operation_id,
                &accepted.node_id,
                MachineAddFailure::JoinTokenExpired {
                    expired_at: expires_at(40),
                },
            )
            .await
            .is_err()
    );
    assert_eq!(
        repository
            .operation_status(&accepted.operation_id)
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: accepted.operation_id,
            node_id: accepted.node_id,
            name: accepted.name,
            gateway: accepted.gateway,
            state: MachineAddOperationState::Joining {
                joined_at: joined_at(50),
            },
            last_event_sequence: event_sequence(2),
        })
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
                node_id: node_id("node_3"),
                name: MachineName::try_new("edge_3").expect("valid machine name"),
                gateway: FirstNodeGateway::Install,
                join_bundle: machine_join_bundle(),
                raw_join_token: raw_join_token("new_raw_join_token"),
                join_token: issued_join_token_for_raw("new_raw_join_token"),
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
                node_id: node_id("node_3"),
                name: MachineName::try_new("edge_3").expect("valid machine name"),
                gateway: FirstNodeGateway::Install,
                join_bundle: machine_join_bundle(),
                raw_join_token: raw_join_token("new_raw_join_token"),
                join_token: issued_join_token_for_raw("new_raw_join_token"),
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
        DeployRunningStage::PreparingWireGuardEbpf,
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
