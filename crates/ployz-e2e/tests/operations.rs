use std::error::Error;

use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{
    DeployOperationState, DeployTransition, EventSequence, OperationIdempotencyKey, OperationStatus,
};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    DeployOperationSubmission, KV_OPS_BUCKET, PLZ_OPS_STREAM,
};

mod support;

use support::nats::TestNats;

#[tokio::test]
async fn e2e_operation_submit_and_transition_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_jetstream().await?;
    let client = async_nats::connect(nats.url()).await?;
    let jetstream = jetstream::new(client);
    bootstrap_operation_resources(&jetstream).await?;
    let repository = AsyncNatsOperationRepository::new(
        AsyncNatsOperationEventLog::new(jetstream.clone()),
        AsyncNatsOperationStatusStore::from_jetstream(&jetstream)
            .await
            .expect("open operation status store"),
    );

    let accepted = repository
        .submit_deploy(DeployOperationSubmission {
            operation_id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            idempotency_key: idempotency_key("idem_1"),
        })
        .await
        .expect("submit deploy over real nats");
    repository
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .await
        .expect("record planning transition over real nats");

    assert_eq!(accepted.operation_id, operation_id("op_123"));
    assert_eq!(accepted.start_sequence, event_sequence(1));
    assert_eq!(
        repository
            .operation_status(&operation_id("op_123"))
            .await
            .expect("operation status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Planning,
            last_event_sequence: event_sequence(2),
        })
    );

    Ok(())
}

async fn bootstrap_operation_resources(
    jetstream: &jetstream::Context,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    jetstream
        .create_stream(jetstream::stream::Config {
            name: PLZ_OPS_STREAM.to_owned(),
            subjects: vec!["plz.v1.op.>".to_owned()],
            storage: StorageType::Memory,
            ..Default::default()
        })
        .await?;
    jetstream
        .create_key_value(jetstream::kv::Config {
            bucket: KV_OPS_BUCKET.to_owned(),
            ..Default::default()
        })
        .await?;

    Ok(())
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
