use super::fixtures::*;
use ployz_core::machine::{
    MachineAddFailure, MachineAddOperationState, MachineAddOperationStateName, MachineName,
};
use ployz_core::ops::{OperationEvent, OperationEventReplayRequest, OperationStatus};
use ployz_core::roles::InstallRolePolicy;
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    MachineAddOperationSubmission, MachineJoinRedemption, OperationEventAppend,
    RedeemMachineJoinTokenError, RedeemedMachineJoin,
};

#[tokio::test]
async fn operation_repository_records_machine_add_joined_transition() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(machine_add_submission(
            "op_machine",
            "idem_machine",
            "machine_2",
            "edge_2",
        ))
        .await
        .expect("machine add accepted");

    repository
        .record_machine_add_joined(&accepted.operation_id, &accepted.machine_id, joined_at(50))
        .await
        .expect("joined transition records");

    assert_eq!(
        repository
            .records()
            .get(&operation_id("op_machine"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: operation_id("op_machine"),
            machine_id: machine_id("machine_2"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
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
            machine_id: machine_id("machine_2"),
            joined_at: joined_at(50),
        })
    );
}
#[tokio::test]
async fn operation_repository_redeems_machine_join_token_once() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(machine_add_submission(
            "op_machine",
            "idem_machine",
            "machine_2",
            "edge_2",
        ))
        .await
        .expect("machine add accepted");
    store_minted_secret(&repository, "op_machine", "idem_machine").await;

    let redemption = repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
        .await
        .expect("join token redeems");

    let MachineJoinRedemption::Joined(joined) = redemption else {
        panic!("expected first redemption to join");
    };
    assert_eq!(joined.operation_id, accepted.operation_id);
    assert_eq!(joined.machine_id, accepted.machine_id);
    assert_eq!(joined.name, accepted.name);
    assert_eq!(joined.roles, accepted.roles);
    assert_eq!(joined.joined_at, joined_at(50));
    assert_eq!(joined.last_event_sequence, event_sequence(2));
    assert_eq!(
        repository
            .records()
            .get(&accepted.operation_id)
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: accepted.operation_id.clone(),
            machine_id: accepted.machine_id.clone(),
            name: accepted.name.clone(),
            roles: accepted.roles,
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
        .submit_machine_add(machine_add_submission(
            "op_machine",
            "idem_machine",
            "machine_2",
            "edge_2",
        ))
        .await
        .expect("machine add accepted");
    store_minted_secret(&repository, "op_machine", "idem_machine").await;

    repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
        .await
        .expect("join token redeems");
    repository
        .record_machine_add_completed(&accepted.operation_id, &accepted.machine_id)
        .await
        .expect("machine add completes");

    assert_eq!(
        repository
            .records()
            .get(&accepted.operation_id)
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: accepted.operation_id,
            machine_id: accepted.machine_id,
            name: accepted.name,
            roles: accepted.roles,
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
        .submit_machine_add(machine_add_submission(
            "op_machine",
            "idem_machine",
            "machine_2",
            "edge_2",
        ))
        .await
        .expect("machine add accepted");
    store_minted_secret(&repository, "op_machine", "idem_machine").await;

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
            machine_id: accepted.machine_id,
            name: accepted.name,
            roles: accepted.roles,
            join_bundle: accepted.join_bundle,
            secret_delivery: machine_join_secret_delivery(),
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
        .submit_machine_add(machine_add_submission(
            "op_machine",
            "idem_machine",
            "machine_2",
            "edge_2",
        ))
        .await
        .expect("machine add accepted");
    store_minted_secret(&repository, "op_machine", "idem_machine").await;
    event_log
        .append(OperationEventAppend::from_event(
            OperationEvent::MachineAddJoined {
                operation_id: accepted.operation_id.clone(),
                machine_id: accepted.machine_id.clone(),
                joined_at: joined_at(50),
            },
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
            machine_id: accepted.machine_id,
            name: accepted.name,
            roles: accepted.roles,
            join_bundle: accepted.join_bundle,
            secret_delivery: machine_join_secret_delivery(),
            joined_at: joined_at(50),
            last_event_sequence: event_sequence(2),
        })
    );
}
#[tokio::test]
async fn operation_repository_completed_machine_join_redeem_does_not_need_secret_delivery() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(machine_add_submission(
            "op_machine",
            "idem_machine",
            "machine_2",
            "edge_2",
        ))
        .await
        .expect("machine add accepted");
    store_minted_secret(&repository, "op_machine", "idem_machine").await;
    repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
        .await
        .expect("join token redeems");
    repository
        .record_machine_join_completed(&accepted.raw_join_token)
        .await
        .expect("machine join completes and clears secret delivery");

    assert!(matches!(
        repository
            .redeem_machine_join_token(&accepted.raw_join_token, joined_at(55))
            .await,
        Err(RedeemMachineJoinTokenError::OperationNotPending {
            operation_id,
            current: MachineAddOperationStateName::Completed,
        }) if operation_id == accepted.operation_id
    ));
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
async fn operation_repository_expired_machine_join_token_records_failure() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;
    let accepted = repository
        .submit_machine_add(MachineAddOperationSubmission {
            operation_id: operation_id("op_machine"),
            machine_id: machine_id("machine_2"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            join_bundle: machine_join_bundle(),
            raw_join_token: raw_join_token("short_lived_join_token"),
            join_token: issued_join_token_for_raw_with_expiry("short_lived_join_token", 40),
            idempotency_key: idempotency_key("idem_machine"),
        })
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
            .records()
            .get(&accepted.operation_id)
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: accepted.operation_id,
            machine_id: accepted.machine_id,
            name: accepted.name,
            roles: accepted.roles,
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
        .submit_machine_add(machine_add_submission(
            "op_machine",
            "idem_machine",
            "machine_2",
            "edge_2",
        ))
        .await
        .expect("machine add accepted");
    store_minted_secret(&repository, "op_machine", "idem_machine").await;
    repository
        .redeem_machine_join_token(&accepted.raw_join_token, joined_at(50))
        .await
        .expect("join token redeems");

    assert!(
        repository
            .record_machine_add_failed(
                &accepted.operation_id,
                &accepted.machine_id,
                MachineAddFailure::JoinTokenExpired {
                    expired_at: expires_at(40),
                },
            )
            .await
            .is_err()
    );
    assert_eq!(
        repository
            .records()
            .get(&accepted.operation_id)
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::MachineAdd {
            id: accepted.operation_id,
            machine_id: accepted.machine_id,
            name: accepted.name,
            roles: accepted.roles,
            state: MachineAddOperationState::Joining {
                joined_at: joined_at(50),
            },
            last_event_sequence: event_sequence(2),
        })
    );
}
