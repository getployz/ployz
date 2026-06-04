use ployz_core::ids::{OperationId, ServiceId};
use ployz_core::ops::{
    DeployOperationState, DeployProjection, DeployRunningStage, DeployTransition, EventSequence,
    OperationStatus, StatusProjectionError, project_deploy_transition,
};

#[test]
fn deploy_transition_updates_status_sequence() {
    let accepted = OperationStatus::deploy_accepted(
        operation_id("op_123"),
        service_id("svc_api"),
        event_sequence(1),
    );

    let projection =
        project_deploy_transition(&accepted, DeployTransition::Planning, event_sequence(2))
            .expect("planning projects");

    assert_eq!(
        projection,
        DeployProjection::Updated {
            status: OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
                state: DeployOperationState::Planning,
                last_event_sequence: event_sequence(2),
            },
        }
    );
}

#[test]
fn satisfied_deploy_transition_does_not_rewrite_status() {
    let planning = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Planning,
        last_event_sequence: event_sequence(2),
    };

    assert_eq!(
        project_deploy_transition(&planning, DeployTransition::Planning, event_sequence(3)),
        Ok(DeployProjection::AlreadySatisfied)
    );
}

#[test]
fn terminal_operation_status_cannot_return_to_running() {
    let completed = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Completed,
        last_event_sequence: event_sequence(4),
    };

    assert_eq!(
        project_deploy_transition(&completed, fake_running_transition(), event_sequence(5)),
        Err(StatusProjectionError::TerminalState {
            operation_id: operation_id("op_123"),
            current: Box::new(DeployOperationState::Completed),
            attempted: Box::new(fake_running_state()),
        })
    );
}

#[test]
fn invalid_deploy_transitions_are_rejected() {
    let accepted = OperationStatus::deploy_accepted(
        operation_id("op_123"),
        service_id("svc_api"),
        event_sequence(1),
    );

    assert_eq!(
        project_deploy_transition(&accepted, DeployTransition::Completed, event_sequence(2)),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_123"),
            current: Box::new(DeployOperationState::Accepted),
            attempted: Box::new(DeployOperationState::Completed),
        })
    );
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
