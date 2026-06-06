use ployz_core::deploy::{DeployPlan, DeployPlanStep, ReplicaSlot};
use ployz_core::ids::{ContainerId, NodeId, OperationId, ServiceId};
use ployz_core::ops::{
    DeployOperationState, DeployProjection, DeployRunningStage, DeployTransition, EventSequence,
    OperationEvent, OperationEventProjection, OperationStatus, StatusProjectionError,
    project_deploy_transition, project_operation_event,
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
fn deploy_completion_is_rejected_before_active_commit_stage() {
    let waiting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
        last_event_sequence: event_sequence(4),
    };

    assert_eq!(
        project_deploy_transition(&waiting, DeployTransition::Completed, event_sequence(5)),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_123"),
            current: Box::new(DeployOperationState::Running {
                stage: DeployRunningStage::WaitingForHealth,
            }),
            attempted: Box::new(DeployOperationState::Completed),
        })
    );
}

#[test]
fn deploy_running_stages_reject_unmodeled_large_skips() {
    let planning = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Planning,
        last_event_sequence: event_sequence(2),
    };

    assert_eq!(
        project_deploy_transition(
            &planning,
            DeployTransition::Running {
                stage: active_service_running(),
            },
            event_sequence(3)
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_123"),
            current: Box::new(DeployOperationState::Planning),
            attempted: Box::new(DeployOperationState::Running {
                stage: active_service_running(),
            }),
        })
    );

    let starting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_deploy_transition(
            &starting,
            DeployTransition::Running {
                stage: active_service_running(),
            },
            event_sequence(4)
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_123"),
            current: Box::new(DeployOperationState::Running {
                stage: DeployRunningStage::StartingContainers,
            }),
            attempted: Box::new(DeployOperationState::Running {
                stage: active_service_running(),
            }),
        })
    );
}

#[test]
fn deploy_completion_is_allowed_after_active_commit_checkpoint() {
    let committing = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: active_service_running(),
        },
        last_event_sequence: event_sequence(5),
    };

    assert_eq!(
        project_deploy_transition(&committing, DeployTransition::Completed, event_sequence(6)),
        Ok(DeployProjection::Updated {
            status: OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
                state: DeployOperationState::Completed,
                last_event_sequence: event_sequence(6),
            },
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

#[test]
fn container_started_event_records_without_changing_status() {
    let starting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_operation_event(&starting, container_started_event(), event_sequence(4)),
        Ok(OperationEventProjection::StatusChanged {
            status: OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
                state: DeployOperationState::Running {
                    stage: DeployRunningStage::StartingContainers,
                },
                last_event_sequence: event_sequence(4),
            },
        })
    );
}

#[test]
fn health_check_started_event_records_without_changing_status() {
    let waiting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_operation_event(&waiting, health_check_started_event(), event_sequence(4)),
        Ok(OperationEventProjection::StatusChanged {
            status: OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
                state: DeployOperationState::Running {
                    stage: DeployRunningStage::WaitingForHealth,
                },
                last_event_sequence: event_sequence(4),
            },
        })
    );
}

#[test]
fn plan_created_event_records_without_changing_status() {
    let planning = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Planning,
        last_event_sequence: event_sequence(2),
    };

    assert_eq!(
        project_operation_event(&planning, plan_created_event(), event_sequence(3)),
        Ok(OperationEventProjection::StatusChanged {
            status: OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
                state: DeployOperationState::Planning,
                last_event_sequence: event_sequence(3),
            },
        })
    );
}

#[test]
fn plan_created_event_after_execution_starts_records_without_changing_status() {
    let executing = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        last_event_sequence: event_sequence(4),
    };

    assert_eq!(
        project_operation_event(&executing, plan_created_event(), event_sequence(5)),
        Ok(OperationEventProjection::StatusChanged {
            status: OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
                state: DeployOperationState::Running {
                    stage: DeployRunningStage::StartingContainers,
                },
                last_event_sequence: event_sequence(5),
            },
        })
    );
}

#[test]
fn older_container_started_event_is_satisfied_after_later_stage() {
    let waiting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: active_service_running(),
        },
        last_event_sequence: event_sequence(5),
    };

    assert_eq!(
        project_operation_event(&waiting, container_started_event(), event_sequence(4)),
        Ok(OperationEventProjection::AlreadySatisfied)
    );
}

#[test]
fn fresh_container_started_event_after_later_stage_records_without_changing_status() {
    let waiting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: active_service_running(),
        },
        last_event_sequence: event_sequence(5),
    };

    assert_eq!(
        project_operation_event(&waiting, container_started_event(), event_sequence(6)),
        Ok(OperationEventProjection::StatusChanged {
            status: OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
                state: DeployOperationState::Running {
                    stage: active_service_running(),
                },
                last_event_sequence: event_sequence(6),
            },
        })
    );
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ActiveServiceCommit
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn container_started_event() -> OperationEvent {
    OperationEvent::DeployContainerStarted {
        operation_id: operation_id("op_123"),
        node_id: node_id("node_a"),
        container_id: container_id("ctr_1"),
    }
}

fn health_check_started_event() -> OperationEvent {
    OperationEvent::DeployHealthCheckStarted {
        operation_id: operation_id("op_123"),
    }
}

fn plan_created_event() -> OperationEvent {
    OperationEvent::DeployPlanCreated {
        operation_id: operation_id("op_123"),
        plan: DeployPlan {
            service_id: service_id("svc_api"),
            target_revision: ployz_core::ids::RevisionId::try_new("rev_2")
                .expect("valid revision id"),
            steps: vec![DeployPlanStep::RunContainer {
                node_id: node_id("node_a"),
                slot: ReplicaSlot::try_new(1).expect("valid replica slot"),
            }],
        },
    }
}

fn fake_running_state() -> DeployOperationState {
    DeployOperationState::Running {
        stage: DeployRunningStage::StartingContainers,
    }
}

fn fake_running_transition() -> DeployTransition {
    DeployTransition::Running {
        stage: DeployRunningStage::StartingContainers,
    }
}
