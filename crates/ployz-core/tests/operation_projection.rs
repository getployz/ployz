use ployz_core::deploy::{DeployPlan, DeployPlanStep, ReplicaSlot};
use ployz_core::ids::{ContainerId, NodeId, OperationId, ServiceId};
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenFingerprint, MachineAddFailure,
    MachineAddOperationState, MachineName, MachineReadinessCheck, MachineReadinessEvidence,
};
use ployz_core::ops::{
    DeployOperationState, DeployProjection, DeployRunningStage, DeployTransition, EventSequence,
    FailureMessage, OperationEvent, OperationEventProjection, OperationStatus,
    ProjectionOperationState, StatusProjectionError, project_deploy_transition,
    project_operation_event,
};
use ployz_core::roles::FirstNodeGateway;

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
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
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
            current: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Completed
            )),
            attempted: Box::new(ProjectionOperationState::Deploy(fake_running_state())),
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
            current: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Running {
                    stage: DeployRunningStage::WaitingForHealth,
                }
            )),
            attempted: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Completed
            )),
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
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
                state: DeployOperationState::Completed,
                last_event_sequence: event_sequence(6),
            }),
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
            current: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Accepted
            )),
            attempted: Box::new(ProjectionOperationState::Deploy(
                DeployOperationState::Completed
            )),
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
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
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
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
        last_event_sequence: event_sequence(3),
    };

    assert_eq!(
        project_operation_event(&waiting, health_check_started_event(), event_sequence(4)),
        Ok(OperationEventProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
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
        service_id: service_id("svc_api"),
        state: DeployOperationState::Planning,
        last_event_sequence: event_sequence(2),
    };

    assert_eq!(
        project_operation_event(&planning, plan_created_event(), event_sequence(3)),
        Ok(OperationEventProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
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
        service_id: service_id("svc_api"),
        state: DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        last_event_sequence: event_sequence(4),
    };

    assert_eq!(
        project_operation_event(&executing, plan_created_event(), event_sequence(5)),
        Ok(OperationEventProjection::StatusChanged {
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
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
            status: Box::new(OperationStatus::Deploy {
                id: operation_id("op_123"),
                service_id: service_id("svc_api"),
                state: DeployOperationState::Running {
                    stage: active_service_running(),
                },
                last_event_sequence: event_sequence(6),
            }),
        })
    );
}

#[test]
fn machine_add_submitted_event_is_satisfied_by_accepted_status() {
    let accepted = OperationStatus::machine_add_pending(
        operation_id("op_machine"),
        node_id("node_2"),
        machine_name("edge_2"),
        FirstNodeGateway::Skip,
        issued_join_token(),
        event_sequence(7),
    );

    assert_eq!(
        project_operation_event(
            &accepted,
            machine_add_submitted_event("node_2"),
            event_sequence(8)
        ),
        Ok(OperationEventProjection::AlreadySatisfied)
    );
}

#[test]
fn machine_add_rejects_submitted_event_for_another_node() {
    let accepted = OperationStatus::machine_add_pending(
        operation_id("op_machine"),
        node_id("node_2"),
        machine_name("edge_2"),
        FirstNodeGateway::Skip,
        issued_join_token(),
        event_sequence(7),
    );

    assert_eq!(
        project_operation_event(
            &accepted,
            machine_add_submitted_event("node_3"),
            event_sequence(8)
        ),
        Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id: operation_id("op_machine"),
            expected: ployz_core::ops::OperationSubjectRef::MachineAdd(node_id("node_2")),
            actual: ployz_core::ops::OperationSubjectRef::MachineAdd(node_id("node_3")),
        })
    );
}

#[test]
fn machine_add_cancel_records_terminal_status() {
    let accepted = OperationStatus::machine_add_pending(
        operation_id("op_machine"),
        node_id("node_2"),
        machine_name("edge_2"),
        FirstNodeGateway::Skip,
        issued_join_token(),
        event_sequence(7),
    );
    let reason = ployz_core::ops::CancellationReason::try_new("operator_cancelled")
        .expect("valid cancellation reason");

    assert_eq!(
        project_operation_event(
            &accepted,
            OperationEvent::Cancelled {
                operation_id: operation_id("op_machine"),
                reason: reason.clone(),
            },
            event_sequence(8),
        ),
        Ok(OperationEventProjection::StatusChanged {
            status: Box::new(OperationStatus::MachineAdd {
                id: operation_id("op_machine"),
                node_id: node_id("node_2"),
                name: machine_name("edge_2"),
                gateway: FirstNodeGateway::Skip,
                state: MachineAddOperationState::Cancelled { reason },
                last_event_sequence: event_sequence(8),
            }),
        })
    );
}

#[test]
fn machine_add_join_and_complete_record_lifecycle_status() {
    let pending = machine_add_pending_status();
    let joined_at =
        ployz_core::machine::JoinTokenRedeemedAt::try_new(650).expect("valid joined at");

    let joined = project_operation_event(
        &pending,
        OperationEvent::MachineAddJoined {
            operation_id: operation_id("op_machine"),
            node_id: node_id("node_2"),
            joined_at,
        },
        event_sequence(8),
    )
    .expect("join projects");

    let OperationEventProjection::StatusChanged {
        status: joined_status,
    } = joined
    else {
        panic!("join should update status");
    };
    assert_eq!(
        joined_status.as_ref(),
        &OperationStatus::MachineAdd {
            id: operation_id("op_machine"),
            node_id: node_id("node_2"),
            name: machine_name("edge_2"),
            gateway: FirstNodeGateway::Skip,
            state: MachineAddOperationState::Joining { joined_at },
            last_event_sequence: event_sequence(8),
        }
    );

    assert_eq!(
        project_operation_event(
            &joined_status,
            OperationEvent::MachineAddCompleted {
                operation_id: operation_id("op_machine"),
                node_id: node_id("node_2"),
            },
            event_sequence(9),
        ),
        Ok(OperationEventProjection::StatusChanged {
            status: Box::new(OperationStatus::MachineAdd {
                id: operation_id("op_machine"),
                node_id: node_id("node_2"),
                name: machine_name("edge_2"),
                gateway: FirstNodeGateway::Skip,
                state: MachineAddOperationState::Completed,
                last_event_sequence: event_sequence(9),
            }),
        })
    );
}

#[test]
fn machine_add_join_token_failure_after_join_is_rejected() {
    let joined_at =
        ployz_core::machine::JoinTokenRedeemedAt::try_new(650).expect("valid joined at");
    let joined = OperationStatus::MachineAdd {
        id: operation_id("op_machine"),
        node_id: node_id("node_2"),
        name: machine_name("edge_2"),
        gateway: FirstNodeGateway::Skip,
        state: MachineAddOperationState::Joining { joined_at },
        last_event_sequence: event_sequence(8),
    };

    assert_eq!(
        project_operation_event(
            &joined,
            OperationEvent::MachineAddFailed {
                operation_id: operation_id("op_machine"),
                node_id: node_id("node_2"),
                failure: MachineAddFailure::JoinTokenExpired {
                    expired_at: JoinTokenExpiresAt::try_new(600).expect("valid expiry"),
                },
            },
            event_sequence(9),
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_machine"),
            current: Box::new(ProjectionOperationState::MachineAdd(
                MachineAddOperationState::Joining { joined_at },
            )),
            attempted: Box::new(ProjectionOperationState::MachineAdd(
                MachineAddOperationState::Failed {
                    failure: MachineAddFailure::JoinTokenExpired {
                        expired_at: JoinTokenExpiresAt::try_new(600).expect("valid expiry"),
                    },
                },
            )),
        })
    );
}

#[test]
fn machine_add_readiness_failure_after_join_is_allowed() {
    let joined_at =
        ployz_core::machine::JoinTokenRedeemedAt::try_new(650).expect("valid joined at");
    let joined = OperationStatus::MachineAdd {
        id: operation_id("op_machine"),
        node_id: node_id("node_2"),
        name: machine_name("edge_2"),
        gateway: FirstNodeGateway::Skip,
        state: MachineAddOperationState::Joining { joined_at },
        last_event_sequence: event_sequence(8),
    };
    let failure = MachineAddFailure::ReadinessFailed {
        evidence: missing_heartbeat_readiness(),
    };

    assert_eq!(
        project_operation_event(
            &joined,
            OperationEvent::MachineAddFailed {
                operation_id: operation_id("op_machine"),
                node_id: node_id("node_2"),
                failure: failure.clone(),
            },
            event_sequence(9),
        ),
        Ok(OperationEventProjection::StatusChanged {
            status: Box::new(OperationStatus::MachineAdd {
                id: operation_id("op_machine"),
                node_id: node_id("node_2"),
                name: machine_name("edge_2"),
                gateway: FirstNodeGateway::Skip,
                state: MachineAddOperationState::Failed { failure },
                last_event_sequence: event_sequence(9),
            }),
        })
    );
}

#[test]
fn machine_add_bootstrap_failure_after_join_is_allowed() {
    let joined_at =
        ployz_core::machine::JoinTokenRedeemedAt::try_new(650).expect("valid joined at");
    let joined = OperationStatus::MachineAdd {
        id: operation_id("op_machine"),
        node_id: node_id("node_2"),
        name: machine_name("edge_2"),
        gateway: FirstNodeGateway::Skip,
        state: MachineAddOperationState::Joining { joined_at },
        last_event_sequence: event_sequence(8),
    };
    let failure = MachineAddFailure::BootstrapFailed {
        message: FailureMessage::try_new("artifact install failed").expect("valid failure message"),
    };

    assert_eq!(
        project_operation_event(
            &joined,
            OperationEvent::MachineAddFailed {
                operation_id: operation_id("op_machine"),
                node_id: node_id("node_2"),
                failure: failure.clone(),
            },
            event_sequence(9),
        ),
        Ok(OperationEventProjection::StatusChanged {
            status: Box::new(OperationStatus::MachineAdd {
                id: operation_id("op_machine"),
                node_id: node_id("node_2"),
                name: machine_name("edge_2"),
                gateway: FirstNodeGateway::Skip,
                state: MachineAddOperationState::Failed { failure },
                last_event_sequence: event_sequence(9),
            }),
        })
    );
}

#[test]
fn machine_add_completed_before_join_is_rejected() {
    let pending = machine_add_pending_status();

    assert_eq!(
        project_operation_event(
            &pending,
            OperationEvent::MachineAddCompleted {
                operation_id: operation_id("op_machine"),
                node_id: node_id("node_2"),
            },
            event_sequence(8),
        ),
        Err(StatusProjectionError::InvalidTransition {
            operation_id: operation_id("op_machine"),
            current: Box::new(ProjectionOperationState::MachineAdd(
                MachineAddOperationState::Pending {
                    join_token: issued_join_token(),
                }
            )),
            attempted: Box::new(ProjectionOperationState::MachineAdd(
                MachineAddOperationState::Completed
            )),
        })
    );
}

fn missing_heartbeat_readiness() -> MachineReadinessEvidence {
    MachineReadinessEvidence {
        nats_tunnel: MachineReadinessCheck::Confirmed,
        heartbeat: MachineReadinessCheck::Missing {
            reason: FailureMessage::try_new("heartbeat missing").expect("valid failure message"),
        },
        node_inspect: MachineReadinessCheck::Confirmed,
    }
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

fn machine_name(value: &str) -> MachineName {
    MachineName::try_new(value).expect("valid machine name")
}

fn issued_join_token() -> IssuedJoinToken {
    IssuedJoinToken::new(
        JoinTokenFingerprint::try_new("join_hash").expect("valid join token fingerprint"),
        JoinTokenExpiresAt::try_new(700).expect("valid join token expiry"),
    )
}

fn machine_add_pending_status() -> OperationStatus {
    OperationStatus::machine_add_pending(
        operation_id("op_machine"),
        node_id("node_2"),
        machine_name("edge_2"),
        FirstNodeGateway::Skip,
        issued_join_token(),
        event_sequence(7),
    )
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

fn machine_add_submitted_event(node_id: &str) -> OperationEvent {
    OperationEvent::MachineAddSubmitted {
        operation_id: operation_id("op_machine"),
        node_id: self::node_id(node_id),
        name: machine_name("edge_2"),
        gateway: FirstNodeGateway::Skip,
        join_token: issued_join_token(),
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
