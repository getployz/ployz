use super::fixtures::*;
use ployz_core::ops::{DeployTransition, OperationEvent, OperationEventReplayCursor};
use ployz_nats::operations::{AsyncNatsOperationEventLog, OperationEventAppend};

#[tokio::test]
async fn operation_event_log_deduplicates_submits_by_operation_id() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());

    let first = event_log
        .append(OperationEventAppend::deploy_submitted(
            operation_id("op_123"),
            deploy_target("svc_api"),
        ))
        .await
        .expect("first submit event stores");
    let second = event_log
        .append(OperationEventAppend::deploy_submitted(
            operation_id("op_123"),
            deploy_target("svc_api"),
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
            target: deploy_target("svc_api"),
        }
    );
}

#[tokio::test]
async fn operation_event_log_deduplicates_submits_across_operation_kinds() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());

    let first = event_log
        .append(OperationEventAppend::deploy_submitted(
            operation_id("op_123"),
            deploy_target("svc_api"),
        ))
        .await
        .expect("first submit event stores");
    let second = event_log
        .append(OperationEventAppend::backup_submitted(
            operation_id("op_123"),
            backup_target("clusters/dev"),
        ))
        .await
        .expect("cross-kind duplicate submit is acknowledged");

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
            target: deploy_target("svc_api"),
        }
    );
}

#[tokio::test]
async fn operation_event_log_replays_only_the_requested_operation() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let requested = operation_id("op_123");
    let other = operation_id("op_456");

    let submitted = event_log
        .append(OperationEventAppend::deploy_submitted(
            requested.clone(),
            deploy_target("svc_api"),
        ))
        .await
        .expect("requested submit event stores");
    event_log
        .append(OperationEventAppend::deploy_submitted(
            other.clone(),
            deploy_target("svc_other"),
        ))
        .await
        .expect("other submit event stores");
    let planning = event_log
        .append(OperationEventAppend::deploy_transition(
            &requested,
            &DeployTransition::Planning,
        ))
        .await
        .expect("requested planning event stores");

    let page = event_log
        .replay_operation(&requested, submitted.sequence, event_replay_limit(10))
        .await
        .expect("operation replay succeeds");

    let [submitted_event, planning_event] = page.events.as_slice() else {
        panic!("expected submitted and planning events, got {page:?}");
    };
    assert_eq!(submitted_event.sequence, submitted.sequence);
    assert_eq!(planning_event.sequence, planning.sequence);
    assert_eq!(page.cursor, OperationEventReplayCursor::CaughtUp);
    assert_eq!(
        page.events
            .into_iter()
            .map(|replayed| replayed.event)
            .collect::<Vec<_>>(),
        vec![
            OperationEvent::DeploySubmitted {
                operation_id: requested.clone(),
                target: deploy_target("svc_api"),
            },
            OperationEvent::DeployPlanningStarted {
                operation_id: requested,
            },
        ]
    );
}

#[tokio::test]
async fn operation_event_log_replay_respects_start_sequence_and_limit() {
    let nats = test_nats().await;
    let event_log = AsyncNatsOperationEventLog::new(nats.jetstream.clone());
    let operation_id = operation_id("op_123");

    event_log
        .append(OperationEventAppend::deploy_submitted(
            operation_id.clone(),
            deploy_target("svc_api"),
        ))
        .await
        .expect("submit event stores");
    let planning = event_log
        .append(OperationEventAppend::deploy_transition(
            &operation_id,
            &DeployTransition::Planning,
        ))
        .await
        .expect("planning event stores");
    event_log
        .append(OperationEventAppend::deploy_plan_created(
            &operation_id,
            &deploy_plan(),
        ))
        .await
        .expect("plan event stores");

    let page = event_log
        .replay_operation(&operation_id, planning.sequence, event_replay_limit(1))
        .await
        .expect("operation replay succeeds");

    let [planning_event] = page.events.as_slice() else {
        panic!("expected one planning event, got {page:?}");
    };
    assert_eq!(planning_event.sequence, planning.sequence);
    assert_eq!(
        page.cursor,
        OperationEventReplayCursor::More {
            next_start_sequence: event_sequence(planning.sequence.get() + 1),
        }
    );
    assert_eq!(
        planning_event.event,
        OperationEvent::DeployPlanningStarted { operation_id }
    );
}
