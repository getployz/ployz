//! Deploy operation status projection: staged transitions, evidence
//! freshness, and terminal rules.

use ployz_core::deploy::{
    DeployOrigin, DeployPlan, DeployPlanStep, DeployServicePlan, ReplicaSlot,
};
use ployz_core::ops::{
    DeployCompletionOutcome, DeployOperationState, DeployRunningStage, DeployTransition,
    OperationEvent, OperationProjection, OperationStatus, ProjectionOperationState,
    StatusProjectionError, project_deploy_transition, project_operation_event,
};
use ployz_test_support::ids::{
    container_id, event_sequence, machine_id, namespace_id, namespace_revision_id, operation_id,
    service_id,
};

#[test]
fn deploy_transition_updates_status_sequence() {
    let accepted = OperationStatus::deploy_accepted(
        operation_id("op_123"),
        namespace_id("default"),
        service_id("svc_api"),
        Some(DeployOrigin::try_new("manual release").expect("valid deploy origin")),
        event_sequence(1),
    );

    let projection =
        project_deploy_transition(&accepted, DeployTransition::Planning, event_sequence(2))
            .expect("planning projects");

    assert_eq!(
        projection,
        OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: Some(DeployOrigin::try_new("manual release").expect("valid deploy origin")),
                state: DeployOperationState::Planning,
                last_event_sequence: event_sequence(2),
            }),
        }
    );
}

#[test]
fn satisfied_deploy_transition_does_not_rewrite_status() {
    let planning = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Planning,
        last_event_sequence: event_sequence(2),
    };

    assert_eq!(
        project_deploy_transition(&planning, DeployTransition::Planning, event_sequence(3)),
        Ok(OperationProjection::AlreadySatisfied)
    );
}

#[test]
fn terminal_operation_status_cannot_return_to_running() {
    let completed = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::completed(),
        last_event_sequence: event_sequence(4),
    };

    assert_eq!(
        project_deploy_transition(&completed, fake_running_transition(), event_sequence(5)),
        Err(StatusProjectionError::TerminalState {
            operation_id: operation_id("op_123"),
            current: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::completed()
            )),
            attempted: Box::new(ProjectionOperationState::Deploy(fake_running_state())),
        })
    );
}

#[test]
fn deploy_completion_is_rejected_before_active_commit_stage() {
    let waiting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
        last_event_sequence: event_sequence(4),
    };

    assert_eq!(
        project_deploy_transition(&waiting, DeployTransition::completed(), event_sequence(5)),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_123"),
            current: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Running {
                    stage: DeployRunningStage::WaitingForHealth,
                }
            )),
            attempted: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::completed()
            )),
        })
    );
}

#[test]
fn deploy_running_stages_reject_unmodeled_large_skips() {
    let planning = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
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
            current: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Planning
            )),
            attempted: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Running {
                    stage: active_service_running(),
                }
            )),
        })
    );

    let starting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
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
            current: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Running {
                    stage: DeployRunningStage::StartingContainers,
                }
            )),
            attempted: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Running {
                    stage: active_service_running(),
                }
            )),
        })
    );
}

#[test]
fn deploy_completion_is_allowed_after_active_commit_stage() {
    let committing = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: active_service_running(),
        },
        last_event_sequence: event_sequence(5),
    };

    assert_eq!(
        project_deploy_transition(
            &committing,
            DeployTransition::completed(),
            event_sequence(6)
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::completed(),
                last_event_sequence: event_sequence(6),
            }),
        })
    );
}

#[test]
fn deploy_completion_can_record_warning_outcome() {
    let committing = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: active_service_running(),
        },
        last_event_sequence: event_sequence(5),
    };

    assert_eq!(
        project_deploy_transition(
            &committing,
            DeployTransition::Completed {
                outcome: DeployCompletionOutcome::CompletedWithWarnings,
            },
            event_sequence(6),
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::Completed {
                    outcome: DeployCompletionOutcome::CompletedWithWarnings,
                },
                last_event_sequence: event_sequence(6),
            }),
        })
    );
}

#[test]
fn deploy_route_cutover_can_precede_active_service_commit() {
    let waiting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
        last_event_sequence: event_sequence(4),
    };

    let route_cutover = project_deploy_transition(
        &waiting,
        DeployTransition::Running {
            stage: DeployRunningStage::RouteCutover,
        },
        event_sequence(5),
    )
    .expect("route cutover follows health");

    let OperationProjection::StatusChanged { status } = route_cutover else {
        panic!("route cutover updates status");
    };

    assert_eq!(
        project_deploy_transition(
            &status,
            DeployTransition::Running {
                stage: active_service_running(),
            },
            event_sequence(6),
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::Running {
                    stage: active_service_running(),
                },
                last_event_sequence: event_sequence(6),
            }),
        })
    );
}

#[test]
fn deploy_cleanup_can_follow_active_service_commit() {
    let committing = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: active_service_running(),
        },
        last_event_sequence: event_sequence(6),
    };

    assert_eq!(
        project_deploy_transition(
            &committing,
            DeployTransition::Running {
                stage: DeployRunningStage::RemovingSupersededContainers,
            },
            event_sequence(7),
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::Running {
                    stage: DeployRunningStage::RemovingSupersededContainers,
                },
                last_event_sequence: event_sequence(7),
            }),
        })
    );
}

#[test]
fn deploy_cleanup_evidence_can_record_from_active_service_commit() {
    let committing = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: active_service_running(),
        },
        last_event_sequence: event_sequence(6),
    };

    assert_eq!(
        project_operation_event(
            &committing,
            OperationEvent::DeployCleanupFinished {
                operation_id: operation_id("op_123"),
                removed: Vec::new(),
                failed: Vec::new(),
            },
            event_sequence(7),
        ),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::Running {
                    stage: active_service_running(),
                },
                last_event_sequence: event_sequence(7),
            }),
        })
    );
}

#[test]
fn invalid_deploy_transitions_are_rejected() {
    let accepted = OperationStatus::deploy_accepted(
        operation_id("op_123"),
        namespace_id("default"),
        service_id("svc_api"),
        None,
        event_sequence(1),
    );

    assert_eq!(
        project_deploy_transition(&accepted, DeployTransition::completed(), event_sequence(2)),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_123"),
            current: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Accepted
            )),
            attempted: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::completed()
            )),
        })
    );
}

#[test]
fn container_started_event_records_without_changing_status() {
    let starting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_operation_event(&starting, container_started_event(), event_sequence(4)),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::Running {
                    stage: DeployRunningStage::StartingContainers,
                },
                last_event_sequence: event_sequence(4),
            }),
        })
    );
}

#[test]
fn health_check_started_event_records_without_changing_status() {
    let waiting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_operation_event(&waiting, health_check_started_event(), event_sequence(4)),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::Running {
                    stage: DeployRunningStage::WaitingForHealth,
                },
                last_event_sequence: event_sequence(4),
            }),
        })
    );
}

#[test]
fn plan_created_event_records_without_changing_status() {
    let planning = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Planning,
        last_event_sequence: event_sequence(2),
    };

    assert_eq!(
        project_operation_event(&planning, plan_created_event(), event_sequence(3)),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::Planning,
                last_event_sequence: event_sequence(3),
            }),
        })
    );
}

#[test]
fn plan_created_event_after_execution_starts_records_without_changing_status() {
    let executing = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        last_event_sequence: event_sequence(4),
    };

    assert_eq!(
        project_operation_event(&executing, plan_created_event(), event_sequence(5)),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::Running {
                    stage: DeployRunningStage::StartingContainers,
                },
                last_event_sequence: event_sequence(5),
            }),
        })
    );
}

#[test]
fn older_container_started_event_is_satisfied_after_later_stage() {
    let waiting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: active_service_running(),
        },
        last_event_sequence: event_sequence(5),
    };

    assert_eq!(
        project_operation_event(&waiting, container_started_event(), event_sequence(4)),
        Ok(OperationProjection::AlreadySatisfied)
    );
}

#[test]
fn fresh_container_started_event_after_later_stage_records_without_changing_status() {
    let waiting = OperationStatus::Deploy {
        id: operation_id("op_123"),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        origin: None,
        state: DeployOperationState::Running {
            stage: active_service_running(),
        },
        last_event_sequence: event_sequence(5),
    };

    assert_eq!(
        project_operation_event(&waiting, container_started_event(), event_sequence(6)),
        Ok(OperationProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                origin: None,
                state: DeployOperationState::Running {
                    stage: active_service_running(),
                },
                last_event_sequence: event_sequence(6),
            }),
        })
    );
}

fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ServingTargetCommit
}

fn container_started_event() -> OperationEvent {
    OperationEvent::DeployContainerStarted {
        operation_id: operation_id("op_123"),
        machine_id: machine_id("machine_a"),
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
            namespace_id: namespace_id("default"),
            namespace_revision_id: namespace_revision_id("rev_2"),
            services: vec![DeployServicePlan {
                service_id: service_id("svc_api"),
                steps: vec![DeployPlanStep::RunContainer {
                    machine_id: machine_id("machine_a"),
                    slot: ReplicaSlot::try_new(1).expect("valid replica slot"),
                }],
                pre_start: None,
            }],
            volume_pin_commits: Vec::new(),
            cleanup_containers: Vec::new(),
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
