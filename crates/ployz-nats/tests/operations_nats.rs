use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{
    DeployOperationState, DeployTransition, EventSequence, OperationEvent, OperationIdempotencyKey,
    OperationStatus,
};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    DeployOperationSubmission, KV_OPS_BUCKET, OperationEventAppend, OperationStatusWrite,
    PLZ_OPS_STREAM, StoredDeploySubmission,
};

#[tokio::test]
async fn operation_event_log_deduplicates_submits_by_message_id() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());

    let first = event_log
        .append(OperationEventAppend::deploy_submitted(
            operation_id("op_123"),
            service_id("svc_api"),
            &idempotency_key("idem_1"),
        ))
        .await
        .expect("first submit event stores");
    let second = event_log
        .append(OperationEventAppend::deploy_submitted(
            operation_id("op_456"),
            service_id("svc_other"),
            &idempotency_key("idem_1"),
        ))
        .await
        .expect("duplicate submit event is acknowledged");

    assert!(!first.duplicate);
    assert!(second.duplicate);
    assert_eq!(second.sequence, first.sequence);
    assert_eq!(
        event_log
            .event_at_sequence(second.sequence)
            .await
            .expect("original event can be loaded"),
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_123"),
            service_id: service_id("svc_api"),
        }
    );
}

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
async fn operation_repository_duplicate_submit_returns_original_operation() {
    let nats = test_nats().await;
    let repository = operation_repository(&nats.jetstream).await;

    let first = repository
        .submit_deploy(deploy_submission("op_123", "idem_1", "svc_api"))
        .await
        .expect("first submit accepted");
    let second = repository
        .submit_deploy(deploy_submission("op_456", "idem_1", "svc_other"))
        .await
        .expect("duplicate submit accepted");

    assert_eq!(first, second);
    assert_eq!(first.operation_id, operation_id("op_123"));
    assert!(
        repository
            .operation_status(&operation_id("op_456"))
            .await
            .expect("status lookup succeeds")
            .is_none()
    );
}

#[tokio::test]
async fn operation_repository_records_transition_status_against_real_nats() {
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
    let duplicate = repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("duplicate planning is satisfied");

    assert!(matches!(
        duplicate,
        OperationStatusWrite::AlreadySatisfied {
            current_sequence
        } if current_sequence == event_sequence(2)
    ));
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Planning,
            last_event_sequence: event_sequence(2),
        })
    );
}

struct TestNats {
    _server: nats_server::Server,
    jetstream: jetstream::Context,
}

async fn test_nats() -> TestNats {
    let server = nats_server::run_server("tests/configs/jetstream.conf");
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");
    let jetstream = jetstream::new(client);
    bootstrap_operation_resources(&jetstream).await;

    TestNats {
        _server: server,
        jetstream,
    }
}

async fn bootstrap_operation_resources(jetstream: &jetstream::Context) {
    jetstream
        .create_stream(jetstream::stream::Config {
            name: PLZ_OPS_STREAM.to_owned(),
            subjects: vec!["plz.v1.op.>".to_owned()],
            storage: StorageType::Memory,
            ..Default::default()
        })
        .await
        .expect("create PLZ_OPS stream");
    jetstream
        .create_key_value(jetstream::kv::Config {
            bucket: KV_OPS_BUCKET.to_owned(),
            ..Default::default()
        })
        .await
        .expect("create KV_OPS bucket");
}

async fn operation_repository(jetstream: &jetstream::Context) -> AsyncNatsOperationRepository {
    AsyncNatsOperationRepository::new(
        AsyncNatsOperationEventLog::new(jetstream.clone()),
        AsyncNatsOperationStatusStore::from_jetstream(jetstream)
            .await
            .expect("open operation status store"),
    )
}

fn deploy_submission(
    operation_id: &str,
    idempotency_key: &str,
    service_id: &str,
) -> DeployOperationSubmission {
    DeployOperationSubmission {
        operation_id: self::operation_id(operation_id),
        service_id: self::service_id(service_id),
        idempotency_key: self::idempotency_key(idempotency_key),
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}
