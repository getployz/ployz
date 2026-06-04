use super::fixtures::*;
use ployz_core::ops::OperationStatus;
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
