use std::error::Error;

use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{
    DeployOperationState, DeployTransition, EventSequence, OperationEvent,
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayRequest,
    OperationIdempotencyKey, OperationStatus,
};
use ployz_core::subjects::{API_DEPLOY_SUBMIT, API_OPS_STATUS, API_OPS_WATCH};
use ployz_nats::operations::{
    AsyncNatsOperationEventLog, AsyncNatsOperationRepository, AsyncNatsOperationStatusStore,
    DeployOperationSubmission, KV_OPS_BUCKET, PLZ_OPS_STREAM,
};
use ployz_sdk_types::{
    DeploySubmitRequest, DeploySubmitResponse, OperationApiResponse, OperationDispatch,
    OpsStatusRequest, OpsStatusResponse, OpsWatchResponse,
};
use ployzd::controllers::OperationControllers;

mod support;

use support::nats::TestNats;

#[tokio::test]
async fn e2e_operations_over_real_nats() -> Result<(), Box<dyn Error + Send + Sync>> {
    e2e_repository_submit_and_transition_over_real_nats().await?;
    e2e_deploy_submit_service_accepts_operation_over_real_nats().await?;

    Ok(())
}

async fn e2e_repository_submit_and_transition_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_jetstream().await?;
    let client = async_nats::connect(nats.url()).await?;
    let jetstream = jetstream::new(client);
    bootstrap_operation_resources(&jetstream).await?;
    let event_log = AsyncNatsOperationEventLog::new(jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&jetstream)
        .await
        .expect("open operation status store");
    let repository = AsyncNatsOperationRepository::new(event_log.clone(), status_store.clone());

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
    assert_eq!(
        operation_replay_page(&repository, accepted.start_sequence)
            .await
            .events
            .into_iter()
            .map(|event| event.event)
            .collect::<Vec<_>>(),
        vec![
            OperationEvent::DeploySubmitted {
                operation_id: operation_id("op_123"),
                service_id: service_id("svc_api"),
            },
            OperationEvent::DeployPlanningStarted {
                operation_id: operation_id("op_123"),
            },
        ]
    );
    assert_eq!(
        operation_replay_page(&repository, accepted.start_sequence)
            .await
            .cursor,
        OperationEventReplayCursor::CaughtUp
    );

    Ok(())
}

async fn e2e_deploy_submit_service_accepts_operation_over_real_nats()
-> Result<(), Box<dyn Error + Send + Sync>> {
    let nats = TestNats::start_jetstream().await?;
    let client = async_nats::connect(nats.url()).await?;
    let jetstream = jetstream::new(client.clone());
    bootstrap_operation_resources(&jetstream).await?;
    let event_log = AsyncNatsOperationEventLog::new(jetstream.clone());
    let status_store = AsyncNatsOperationStatusStore::from_jetstream(&jetstream)
        .await
        .expect("open operation status store");
    let repository = AsyncNatsOperationRepository::new(event_log.clone(), status_store.clone());
    let controllers = OperationControllers::new(event_log, status_store);
    let _runtime = ployzd::api_runtime::start_operation_api_service(client.clone(), controllers)
        .await
        .expect("api service starts");
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_api_123"),
        service_id: service_id("svc_api"),
        idempotency_key: idempotency_key("idem_api_1"),
    };

    let response = client
        .request(API_DEPLOY_SUBMIT, serde_json::to_vec(&request)?.into())
        .await?;
    let accepted = match serde_json::from_slice::<DeploySubmitResponse>(&response.payload)? {
        OperationApiResponse::Ok { value } => value,
        OperationApiResponse::DomainError { error } => {
            panic!("deploy submit failed: {error:?}");
        }
    };

    assert_eq!(accepted.operation_id, operation_id("op_api_123"));
    assert_eq!(
        accepted.dispatch,
        OperationDispatch::Queued {
            watch_subject: "plz.v1.op.op_api_123.>".to_owned(),
            start_sequence: event_sequence(1),
        }
    );
    assert_eq!(
        repository
            .operation_status(&operation_id("op_api_123"))
            .await
            .expect("operation status lookup succeeds"),
        Some(OperationStatus::Deploy {
            id: operation_id("op_api_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Accepted,
            last_event_sequence: event_sequence(1),
        })
    );
    let status_request = OpsStatusRequest {
        operation_id: operation_id("op_api_123"),
    };
    let status_response = client
        .request(API_OPS_STATUS, serde_json::to_vec(&status_request)?.into())
        .await?;
    let status = match serde_json::from_slice::<OpsStatusResponse>(&status_response.payload)? {
        OperationApiResponse::Ok { value } => value,
        OperationApiResponse::DomainError { error } => {
            panic!("ops status failed: {error:?}");
        }
    };
    assert_eq!(
        status,
        OperationStatus::Deploy {
            id: operation_id("op_api_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Accepted,
            last_event_sequence: event_sequence(1),
        }
    );

    let watch_request = OperationEventReplayRequest {
        operation_id: operation_id("op_api_123"),
        start_sequence: event_sequence(1),
        limit: event_replay_limit(10),
    };
    let watch_response = client
        .request(API_OPS_WATCH, serde_json::to_vec(&watch_request)?.into())
        .await?;
    let page = match serde_json::from_slice::<OpsWatchResponse>(&watch_response.payload)? {
        OperationApiResponse::Ok { value } => value,
        OperationApiResponse::DomainError { error } => {
            panic!("ops watch failed: {error:?}");
        }
    };
    assert_eq!(
        page.events
            .into_iter()
            .map(|event| event.event)
            .collect::<Vec<_>>(),
        vec![OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_api_123"),
            service_id: service_id("svc_api"),
        }]
    );
    assert_eq!(page.cursor, OperationEventReplayCursor::CaughtUp);

    Ok(())
}

async fn operation_replay_page(
    repository: &AsyncNatsOperationRepository,
    start_sequence: EventSequence,
) -> ployz_core::ops::OperationEventReplayPage {
    repository
        .replay_operation_events(OperationEventReplayRequest {
            operation_id: operation_id("op_123"),
            start_sequence,
            limit: event_replay_limit(10),
        })
        .await
        .expect("operation event replay succeeds")
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

fn event_replay_limit(value: u16) -> OperationEventReplayLimit {
    OperationEventReplayLimit::try_new(value).expect("valid event replay limit")
}

fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}
