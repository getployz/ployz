use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{DeployRunningStage, DeployTransition, EventSequence, OperationEvent};
use ployz_nats::operations::{OperationEventAppend, operation_status_key};
use ployz_nats::streams::{DurableConsumerState, MessageId, OperationEventStream};

#[test]
fn operation_status_key_uses_token_safe_operation_id() {
    let operation_id = OperationId::try_new("op_123").expect("valid operation id");

    assert_eq!(operation_status_key(&operation_id), "ops.op_123");
}

#[test]
fn operation_event_append_carries_nats_message_id() {
    let operation_id = OperationId::try_new("op_123").expect("valid operation id");
    let service_id = ServiceId::try_new("svc_api").expect("valid service id");
    let append = OperationEventAppend::from_event(
        MessageId::new("deploy.submit.idem_1"),
        OperationEvent::DeploySubmitted {
            operation_id,
            service_id,
        },
    );

    assert_eq!(append.subject(), "plz.v1.op.op_123.deploy.submitted");
    assert_eq!(append.message_id().as_str(), "deploy.submit.idem_1");
}

#[test]
fn deploy_transition_append_uses_stable_small_message_id() {
    let append = OperationEventAppend::deploy_transition(
        &operation_id("op_123"),
        &DeployTransition::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
    );

    assert_eq!(
        append.subject(),
        "plz.v1.op.op_123.deploy.running.waiting_for_health"
    );
    assert_eq!(
        append.message_id().as_str(),
        "deploy.event.op_123.waiting_for_health"
    );
}

#[test]
fn operation_stream_replays_matching_operation_events_from_start_sequence() {
    let mut stream = OperationEventStream::default();
    stream.append(
        "plz.v1.op.op_123.deploy.submitted",
        MessageId::new("op_123.submitted"),
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_123"),
            service_id: service_id("svc_api"),
        },
    );
    stream.append(
        "plz.v1.op.op_456.deploy.submitted",
        MessageId::new("op_456.submitted"),
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_456"),
            service_id: service_id("svc_worker"),
        },
    );

    let replay = stream.replay("plz.v1.op.op_123.", event_sequence(1));
    let [event] = replay.as_slice() else {
        panic!("expected one replayed event, got {replay:?}");
    };

    assert_eq!(event.sequence, event_sequence(1));
    assert_eq!(
        event.payload,
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_123"),
            service_id: service_id("svc_api"),
        }
    );
}

#[test]
fn operation_stream_deduplicates_by_message_id() {
    let mut stream = OperationEventStream::default();
    let first = stream.append(
        "plz.v1.op.op_123.deploy.submitted",
        MessageId::new("deploy.submit.idem_1"),
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_123"),
            service_id: service_id("svc_api"),
        },
    );
    let duplicate = stream.append(
        "plz.v1.op.op_456.deploy.submitted",
        MessageId::new("deploy.submit.idem_1"),
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_456"),
            service_id: service_id("svc_worker"),
        },
    );

    assert_eq!(duplicate.sequence(), first.sequence());
    assert_eq!(stream.messages().len(), 1);
}

#[test]
fn durable_consumer_tracks_ack_by_event_sequence() {
    let mut consumer = DurableConsumerState::default();

    assert!(!consumer.is_acked(event_sequence(1)));

    consumer.ack(event_sequence(1));
    consumer.ack(event_sequence(1));

    assert!(consumer.is_acked(event_sequence(1)));
    assert!(!consumer.is_acked(event_sequence(2)));
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
