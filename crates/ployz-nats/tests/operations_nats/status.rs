use super::fixtures::*;
use ployz_core::ops::{OperationOwnerLease, OperationStatus};
use ployz_nats::operations::{
    AsyncNatsOperationStatusStore, OperationStatusWrite, StoredDeploySubmission,
};

#[tokio::test]
async fn deploy_submission_index_returns_original_submission() {
    let nats = test_nats().await;
    let store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open operation status store");
    let first = StoredDeploySubmission {
        operation_id: operation_id("op_123"),
        start_sequence: event_sequence(1),
    };
    let duplicate = StoredDeploySubmission {
        operation_id: operation_id("op_456"),
        start_sequence: event_sequence(2),
    };

    assert_eq!(
        store
            .put_deploy_submission_if_absent(&idempotency_key("idem_1"), &first)
            .await
            .expect("first submission stores"),
        first
    );
    assert_eq!(
        store
            .put_deploy_submission_if_absent(&idempotency_key("idem_1"), &duplicate)
            .await
            .expect("duplicate submission returns original"),
        first
    );
    assert_eq!(
        store
            .deploy_submission(&idempotency_key("idem_1"))
            .await
            .expect("submission lookup succeeds"),
        Some(first)
    );
}

#[tokio::test]
async fn status_store_rejects_stale_write_against_real_nats() {
    let nats = test_nats().await;
    let store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open operation status store");
    let operation_id = operation_id("op_123");
    let service_id = service_id("svc_api");
    let newer = OperationStatus::deploy_accepted(
        operation_id.clone(),
        service_id.clone(),
        event_sequence(2),
    );
    let older =
        OperationStatus::deploy_accepted(operation_id.clone(), service_id, event_sequence(1));

    assert!(matches!(
        store
            .put_if_newer(&newer)
            .await
            .expect("newer status stores"),
        OperationStatusWrite::Stored { .. }
    ));
    assert_eq!(
        store
            .put_if_newer(&older)
            .await
            .expect("stale status is classified"),
        OperationStatusWrite::Stale {
            current_sequence: event_sequence(2),
            attempted_sequence: event_sequence(1),
        }
    );
    assert_eq!(
        store
            .get(&operation_id)
            .await
            .expect("status lookup succeeds"),
        Some(newer)
    );
}

#[tokio::test]
async fn status_store_renews_current_owner_lease_against_real_nats() {
    let nats = test_nats().await;
    let store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open operation status store");
    let operation_id = operation_id("op_123");
    store
        .claim_owner_lease(
            &operation_id,
            &owner_id("control_a"),
            lease_time(100),
            lease_time(160),
        )
        .await
        .expect("lease is claimed");

    let renewed = store
        .renew_owner_lease(
            &operation_id,
            &owner_id("control_a"),
            lease_time(120),
            lease_time(190),
        )
        .await
        .expect("lease renewal succeeds");

    assert_eq!(
        renewed,
        Some(OperationOwnerLease::new(
            operation_id,
            owner_id("control_a"),
            lease_time(190),
        ))
    );
}

#[tokio::test]
async fn status_store_rejects_wrong_owner_lease_renewal_against_real_nats() {
    let nats = test_nats().await;
    let store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open operation status store");
    let operation_id = operation_id("op_123");
    let original = store
        .claim_owner_lease(
            &operation_id,
            &owner_id("control_a"),
            lease_time(100),
            lease_time(160),
        )
        .await
        .expect("lease is claimed");

    let renewed = store
        .renew_owner_lease(
            &operation_id,
            &owner_id("control_b"),
            lease_time(120),
            lease_time(190),
        )
        .await
        .expect("wrong owner renewal is classified");

    assert_eq!(renewed, None);
    assert_eq!(
        store
            .operation_ownership(&operation_id, lease_time(120))
            .await
            .expect("ownership reads"),
        ployz_core::ops::OperationOwnershipStatus::Owned { lease: original }
    );
}

#[tokio::test]
async fn status_store_rejects_expired_owner_lease_renewal_against_real_nats() {
    let nats = test_nats().await;
    let store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open operation status store");
    let operation_id = operation_id("op_123");
    store
        .claim_owner_lease(
            &operation_id,
            &owner_id("control_a"),
            lease_time(100),
            lease_time(160),
        )
        .await
        .expect("lease is claimed");

    let renewed = store
        .renew_owner_lease(
            &operation_id,
            &owner_id("control_a"),
            lease_time(161),
            lease_time(220),
        )
        .await
        .expect("expired owner renewal is classified");

    assert_eq!(renewed, None);
}

#[tokio::test]
async fn status_store_missing_owner_lease_renewal_returns_none_against_real_nats() {
    let nats = test_nats().await;
    let store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open operation status store");

    let renewed = store
        .renew_owner_lease(
            &operation_id("op_missing"),
            &owner_id("control_a"),
            lease_time(120),
            lease_time(190),
        )
        .await
        .expect("missing owner renewal is classified");

    assert_eq!(renewed, None);
}

#[tokio::test]
async fn status_store_stale_owner_lease_renewal_does_not_shorten_lease_against_real_nats() {
    let nats = test_nats().await;
    let store = AsyncNatsOperationStatusStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open operation status store");
    let operation_id = operation_id("op_123");
    let original = store
        .claim_owner_lease(
            &operation_id,
            &owner_id("control_a"),
            lease_time(100),
            lease_time(190),
        )
        .await
        .expect("lease is claimed");

    let renewed = store
        .renew_owner_lease(
            &operation_id,
            &owner_id("control_a"),
            lease_time(120),
            lease_time(170),
        )
        .await
        .expect("stale renewal succeeds without shortening");

    assert_eq!(renewed, Some(original));
}
