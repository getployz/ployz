use super::fixtures::*;
use ployz_core::ops::OperationEvent;
use ployz_nats::operations::{AsyncNatsOperationEventLog, OperationEventAppend};

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
