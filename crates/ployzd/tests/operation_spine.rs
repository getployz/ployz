use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{
    DeployOperationState, DeployRunningStage, DeployTransition, EventSequence, OperationEvent,
};
use ployzd::controllers::{
    DeploySubmitCommand, DeployWorker, IdempotencyKey, OperationSpine, StatusProjectionError,
};

#[test]
fn duplicate_submit_with_same_idempotency_key_returns_same_operation() {
    let mut spine = OperationSpine::new();
    let first = spine.submit_deploy(deploy_submit("op_123", "idem_1", "svc_api"));
    let second = spine.submit_deploy(deploy_submit("op_456", "idem_1", "svc_other"));

    assert_eq!(first, second);
    assert_eq!(first.operation_id, operation_id("op_123"));
    assert_eq!(first.start_sequence, event_sequence(1));
}

#[test]
fn deploy_submit_writes_status_with_last_event_sequence() {
    let mut spine = OperationSpine::new();
    let accepted = spine.submit_deploy(deploy_submit("op_123", "idem_1", "svc_api"));

    assert_eq!(accepted.start_sequence, event_sequence(1));
    assert_eq!(
        spine.status(&operation_id("op_123")),
        Some(&ployz_core::ops::OperationStatus::deploy_accepted(
            operation_id("op_123"),
            service_id("svc_api"),
            event_sequence(1),
        ))
    );
}

#[test]
fn watch_replays_operation_events_from_start_sequence() {
    let mut spine = OperationSpine::new();
    spine.submit_deploy(deploy_submit("op_123", "idem_1", "svc_api"));
    spine.submit_deploy(deploy_submit("op_456", "idem_2", "svc_other"));

    let events = spine.watch(&operation_id("op_123"), event_sequence(1));

    assert_eq!(events.len(), 1);
    assert_eq!(events[0].sequence, event_sequence(1));
    assert_eq!(
        events[0].payload,
        OperationEvent::DeploySubmitted {
            operation_id: operation_id("op_123"),
            service_id: service_id("svc_api"),
        }
    );
}

#[test]
fn unacked_submitted_event_is_redelivered_until_ack() {
    let mut spine = OperationSpine::new();
    spine.submit_deploy(deploy_submit("op_123", "idem_1", "svc_api"));
    let consumer = spine.deploy_submitted_consumer();

    let first = consumer
        .next()
        .expect("submitted event is pending")
        .sequence;
    let redelivered = consumer
        .next()
        .expect("unacked submitted event is still pending")
        .sequence;
    assert_eq!(first, redelivered);

    spine.ack_deploy_submitted(first);
    assert!(spine.deploy_submitted_consumer().next().is_none());
}

#[test]
fn submitted_ack_is_per_event_sequence() {
    let mut spine = OperationSpine::new();
    spine.submit_deploy(deploy_submit("op_123", "idem_1", "svc_api"));
    spine.submit_deploy(deploy_submit("op_456", "idem_2", "svc_worker"));

    spine.ack_deploy_submitted(event_sequence(2));

    let pending = spine
        .deploy_submitted_consumer()
        .next()
        .expect("first submitted event remains pending")
        .sequence;
    assert_eq!(pending, event_sequence(1));
}

#[test]
fn worker_moves_submitted_operation_to_planning() {
    let mut spine = OperationSpine::new();
    spine.submit_deploy(deploy_submit("op_123", "idem_1", "svc_api"));
    let message = spine
        .deploy_submitted_consumer()
        .next()
        .expect("submitted event is pending")
        .clone();

    DeployWorker::mark_planning(&mut spine, &message).expect("planning status projects");
    DeployWorker::mark_planning(&mut spine, &message).expect("redelivered planning is idempotent");

    assert_eq!(
        spine.status(&operation_id("op_123")),
        Some(&ployz_core::ops::OperationStatus::Deploy {
            id: operation_id("op_123"),
            service_id: service_id("svc_api"),
            state: DeployOperationState::Planning,
            last_event_sequence: event_sequence(2),
        })
    );

    let events = spine.watch(&operation_id("op_123"), event_sequence(1));
    assert_eq!(events.len(), 2);
    assert_eq!(
        events[1].payload,
        OperationEvent::DeployPlanningStarted {
            operation_id: operation_id("op_123"),
        }
    );
}

#[test]
fn terminal_operation_status_cannot_return_to_running() {
    let mut spine = OperationSpine::new();
    spine.submit_deploy(deploy_submit("op_123", "idem_1", "svc_api"));
    spine
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Planning)
        .expect("planning status projects");
    spine
        .record_deploy_transition(&operation_id("op_123"), fake_running_transition())
        .expect("running status projects");
    spine
        .record_deploy_transition(&operation_id("op_123"), DeployTransition::Completed)
        .expect("completed status projects");

    assert_eq!(
        spine.record_deploy_transition(&operation_id("op_123"), fake_running_transition()),
        Err(StatusProjectionError::TerminalState {
            operation_id: operation_id("op_123"),
            current: DeployOperationState::Completed,
            attempted: fake_running_state(),
        })
    );
    assert_eq!(
        spine.record_deploy_transition(&operation_id("op_123"), DeployTransition::Completed),
        Ok(())
    );
}

#[test]
fn invalid_deploy_transitions_are_rejected() {
    let mut spine = OperationSpine::new();
    spine.submit_deploy(deploy_submit("op_123", "idem_1", "svc_api"));

    assert_eq!(
        spine.record_deploy_transition(&operation_id("op_123"), DeployTransition::Completed),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_123"),
            current: DeployOperationState::Accepted,
            attempted: DeployOperationState::Completed,
        })
    );
}

fn deploy_submit(
    operation_id: &str,
    idempotency_key: &str,
    service_id: &str,
) -> DeploySubmitCommand {
    DeploySubmitCommand {
        operation_id: self::operation_id(operation_id),
        idempotency_key: IdempotencyKey::try_new(idempotency_key).expect("valid idempotency key"),
        service_id: self::service_id(service_id),
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

fn fake_running_state() -> DeployOperationState {
    DeployOperationState::Running {
        stage: DeployRunningStage::WaitingForHealth,
    }
}

fn fake_running_transition() -> DeployTransition {
    DeployTransition::Running {
        stage: DeployRunningStage::WaitingForHealth,
    }
}
