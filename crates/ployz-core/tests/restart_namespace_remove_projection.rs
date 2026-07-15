use ployz_core::operation::{
    NamespaceRemoveOperationState, NamespaceRemoveRunningStage, NamespaceRemoveTransition,
    OperationEvent, OperationProjection, OperationStatus, ProjectionOperationState, RouteHostname,
    RouteTarget, ServiceRestartOperationState, ServiceRestartRunningStage,
    ServiceRestartTransition, StatusProjectionError, project_namespace_remove_transition,
    project_operation_event, project_service_restart_transition,
};
use ployz_test_support::ids::{
    container_id, event_sequence, machine_id, namespace_id, operation_id, service_id,
};

#[test]
fn service_restart_terminal_state_is_final() {
    let completed = OperationStatus::ServiceRestart {
        id: operation_id("op_restart_svc"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        state: ServiceRestartOperationState::Completed,
        last_event_sequence: event_sequence(4),
    };

    assert_eq!(
        project_service_restart_transition(
            &completed,
            ServiceRestartTransition::Running {
                stage: ServiceRestartRunningStage::RestartingContainers,
            },
            event_sequence(5),
        ),
        Err(StatusProjectionError::TerminalState {
            operation_id: operation_id("op_restart_svc"),
            current: Box::new(ProjectionOperationState::ServiceRestart(
                ServiceRestartOperationState::Completed
            )),
            attempted: Box::new(ProjectionOperationState::ServiceRestart(
                ServiceRestartOperationState::Running {
                    stage: ServiceRestartRunningStage::RestartingContainers,
                }
            )),
        })
    );
}

#[test]
fn service_restart_container_event_advances_running_status_sequence() {
    let running = OperationStatus::ServiceRestart {
        id: operation_id("op_restart_svc"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        state: ServiceRestartOperationState::Running {
            stage: ServiceRestartRunningStage::WaitingForHealth,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_operation_event(
            &running,
            OperationEvent::ServiceRestartContainerRestarted {
                operation_id: operation_id("op_restart_svc"),
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_1"),
            },
            event_sequence(4),
        )
        .expect("container event projects"),
        OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::ServiceRestart {
                id: operation_id("op_restart_svc"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                state: ServiceRestartOperationState::Running {
                    stage: ServiceRestartRunningStage::WaitingForHealth,
                },
                last_event_sequence: event_sequence(4),
            }),
        }
    );
}

#[test]
fn namespace_remove_terminal_state_is_final() {
    let completed = OperationStatus::NamespaceRemove {
        id: operation_id("op_namespace_rm_default"),
        namespace_id: namespace_id("default"),
        state: NamespaceRemoveOperationState::Completed,
        last_event_sequence: event_sequence(6),
    };

    assert_eq!(
        project_namespace_remove_transition(
            &completed,
            NamespaceRemoveTransition::Running {
                stage: NamespaceRemoveRunningStage::RemovingContainers,
            },
            event_sequence(7),
        ),
        Err(StatusProjectionError::TerminalState {
            operation_id: operation_id("op_namespace_rm_default"),
            current: Box::new(ProjectionOperationState::NamespaceRemove(
                NamespaceRemoveOperationState::Completed
            )),
            attempted: Box::new(ProjectionOperationState::NamespaceRemove(
                NamespaceRemoveOperationState::Running {
                    stage: NamespaceRemoveRunningStage::RemovingContainers,
                }
            )),
        })
    );
}

#[test]
fn namespace_remove_route_event_advances_running_status_sequence() {
    let running = OperationStatus::NamespaceRemove {
        id: operation_id("op_namespace_rm_default"),
        namespace_id: namespace_id("default"),
        state: NamespaceRemoveOperationState::Running {
            stage: NamespaceRemoveRunningStage::RemovingRouteBindings,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_operation_event(
            &running,
            OperationEvent::NamespaceRemoveRouteBindingRemoved {
                operation_id: operation_id("op_namespace_rm_default"),
                target: route_target("api.example.com"),
            },
            event_sequence(4),
        )
        .expect("route removed event projects"),
        OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::NamespaceRemove {
                id: operation_id("op_namespace_rm_default"),
                namespace_id: namespace_id("default"),
                state: NamespaceRemoveOperationState::Running {
                    stage: NamespaceRemoveRunningStage::RemovingRouteBindings,
                },
                last_event_sequence: event_sequence(4),
            }),
        }
    );
}

fn route_target(hostname: &str) -> RouteTarget {
    RouteTarget::new(RouteHostname::try_new(hostname).expect("valid hostname"))
}
