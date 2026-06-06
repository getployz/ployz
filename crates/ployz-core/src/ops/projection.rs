use super::{
    DeployEvidence, DeployOperationState, DeployRunningStage, DeployTransition, EventSequence,
    OperationEvent, OperationId, OperationStatus,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployProjection {
    Updated { status: OperationStatus },
    AlreadySatisfied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationEventProjection {
    StatusChanged { status: OperationStatus },
    AlreadySatisfied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusProjectionError {
    MissingOperation {
        operation_id: OperationId,
    },
    OperationEventMismatch {
        expected_operation_id: OperationId,
        actual_operation_id: OperationId,
    },
    TerminalState {
        operation_id: OperationId,
        current: Box<DeployOperationState>,
        attempted: Box<DeployOperationState>,
    },
    InvalidTransition {
        operation_id: OperationId,
        current: Box<DeployOperationState>,
        attempted: Box<DeployOperationState>,
    },
}

pub fn validate_fresh_deploy_evidence(
    current: &OperationStatus,
    evidence: &DeployEvidence,
) -> Result<(), StatusProjectionError> {
    let OperationStatus::Deploy { id, state, .. } = current;
    let valid = match evidence {
        DeployEvidence::PlanCreated { .. } => matches!(state, DeployOperationState::Planning),
        DeployEvidence::ContainerStarted { .. } => {
            evidence_is_current_or_past_running_stage(state, DeployRunningStage::StartingContainers)
        }
        DeployEvidence::HealthCheckStarted => {
            evidence_is_current_or_past_running_stage(state, DeployRunningStage::WaitingForHealth)
        }
    };
    if valid {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: id.clone(),
        current: Box::new(state.clone()),
        attempted: Box::new(deploy_evidence_required_state(evidence)),
    })
}

fn evidence_is_current_or_past_running_stage(
    state: &DeployOperationState,
    evidence_stage: DeployRunningStage,
) -> bool {
    let DeployOperationState::Running { stage } = state else {
        return false;
    };

    deploy_stage_rank(*stage) >= deploy_stage_rank(evidence_stage)
}

fn deploy_evidence_required_state(evidence: &DeployEvidence) -> DeployOperationState {
    match evidence {
        DeployEvidence::PlanCreated { .. } => DeployOperationState::Planning,
        DeployEvidence::ContainerStarted { .. } => DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        DeployEvidence::HealthCheckStarted => DeployOperationState::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
    }
}

pub fn project_deploy_transition(
    current: &OperationStatus,
    transition: DeployTransition,
    event_sequence: EventSequence,
) -> Result<DeployProjection, StatusProjectionError> {
    let OperationStatus::Deploy {
        id,
        service_id,
        state: current_state,
        ..
    } = current;
    let attempted = transition.state();

    if deploy_transition_satisfied(current_state, &attempted) {
        return Ok(DeployProjection::AlreadySatisfied);
    }

    validate_deploy_transition(id, current_state, &attempted)?;

    Ok(DeployProjection::Updated {
        status: OperationStatus::Deploy {
            id: id.clone(),
            service_id: service_id.clone(),
            state: attempted,
            last_event_sequence: event_sequence,
        },
    })
}

pub fn project_operation_event(
    current: &OperationStatus,
    event: OperationEvent,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    let OperationStatus::Deploy {
        id,
        state,
        last_event_sequence,
        ..
    } = current;
    let event_operation_id = operation_event_id(&event);
    if event_operation_id != id {
        return Err(StatusProjectionError::OperationEventMismatch {
            expected_operation_id: id.clone(),
            actual_operation_id: event_operation_id.clone(),
        });
    }

    if event_sequence <= *last_event_sequence {
        return Ok(OperationEventProjection::AlreadySatisfied);
    }

    match event {
        OperationEvent::DeployContainerStarted { .. } => {
            if !matches!(
                state,
                DeployOperationState::Running {
                    stage: DeployRunningStage::StartingContainers
                }
            ) {
                return evidence_cursor_after_stage(
                    evidence_is_satisfied_after_stage(
                        state,
                        DeployRunningStage::StartingContainers,
                    ),
                    current,
                    event_sequence,
                );
            }

            Ok(OperationEventProjection::StatusChanged {
                status: evidence_status(current, event_sequence),
            })
        }
        OperationEvent::DeployHealthCheckStarted { .. } => {
            if !matches!(
                state,
                DeployOperationState::Running {
                    stage: DeployRunningStage::WaitingForHealth
                }
            ) {
                return evidence_cursor_after_stage(
                    evidence_is_satisfied_after_stage(state, DeployRunningStage::WaitingForHealth),
                    current,
                    event_sequence,
                );
            }

            Ok(OperationEventProjection::StatusChanged {
                status: evidence_status(current, event_sequence),
            })
        }
        OperationEvent::DeployPlanCreated { .. } => {
            if !matches!(state, DeployOperationState::Planning) {
                return evidence_cursor_after_stage(
                    !matches!(state, DeployOperationState::Accepted),
                    current,
                    event_sequence,
                );
            }

            Ok(OperationEventProjection::StatusChanged {
                status: evidence_status(current, event_sequence),
            })
        }
        OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::DeployFailed { .. }
        | OperationEvent::Cancelled { .. } => {
            match project_deploy_transition(
                current,
                deploy_transition_from_event(event),
                event_sequence,
            )? {
                DeployProjection::Updated { status } => {
                    Ok(OperationEventProjection::StatusChanged { status })
                }
                DeployProjection::AlreadySatisfied => {
                    Ok(OperationEventProjection::AlreadySatisfied)
                }
            }
        }
        OperationEvent::DeploySubmitted { .. } => Ok(OperationEventProjection::AlreadySatisfied),
    }
}

fn evidence_cursor_after_stage(
    satisfied: bool,
    current: &OperationStatus,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    if !satisfied {
        return Ok(OperationEventProjection::AlreadySatisfied);
    }

    Ok(OperationEventProjection::StatusChanged {
        status: evidence_status(current, event_sequence),
    })
}

fn evidence_is_satisfied_after_stage(
    state: &DeployOperationState,
    evidence_stage: DeployRunningStage,
) -> bool {
    match state {
        DeployOperationState::Running { stage } => {
            deploy_stage_rank(*stage) > deploy_stage_rank(evidence_stage)
        }
        DeployOperationState::Completed => true,
        DeployOperationState::Accepted
        | DeployOperationState::Planning
        | DeployOperationState::Failed { .. }
        | DeployOperationState::Cancelled { .. } => false,
    }
}

fn evidence_status(current: &OperationStatus, event_sequence: EventSequence) -> OperationStatus {
    let OperationStatus::Deploy {
        id,
        service_id,
        state,
        ..
    } = current;

    OperationStatus::Deploy {
        id: id.clone(),
        service_id: service_id.clone(),
        state: state.clone(),
        last_event_sequence: event_sequence,
    }
}

fn deploy_transition_from_event(event: OperationEvent) -> DeployTransition {
    match event {
        OperationEvent::DeployPlanningStarted { .. } => DeployTransition::Planning,
        OperationEvent::DeployRunning { stage, .. } => DeployTransition::Running { stage },
        OperationEvent::DeployCompleted { .. } => DeployTransition::Completed,
        OperationEvent::DeployFailed { failure, .. } => DeployTransition::Failed { failure },
        OperationEvent::Cancelled { reason, .. } => DeployTransition::Cancelled { reason },
        OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanCreated { .. }
        | OperationEvent::DeployContainerStarted { .. }
        | OperationEvent::DeployHealthCheckStarted { .. } => {
            unreachable!("non-transition operation event is handled before conversion")
        }
    }
}

fn operation_event_id(event: &OperationEvent) -> &OperationId {
    match event {
        OperationEvent::DeploySubmitted { operation_id, .. }
        | OperationEvent::DeployPlanningStarted { operation_id }
        | OperationEvent::DeployPlanCreated { operation_id, .. }
        | OperationEvent::DeployRunning { operation_id, .. }
        | OperationEvent::DeployContainerStarted { operation_id, .. }
        | OperationEvent::DeployHealthCheckStarted { operation_id }
        | OperationEvent::DeployCompleted { operation_id, .. }
        | OperationEvent::DeployFailed { operation_id, .. }
        | OperationEvent::Cancelled { operation_id, .. } => operation_id,
    }
}

fn deploy_transition_satisfied(
    current: &DeployOperationState,
    attempted: &DeployOperationState,
) -> bool {
    match attempted {
        DeployOperationState::Accepted => matches!(current, DeployOperationState::Accepted),
        DeployOperationState::Planning => !matches!(current, DeployOperationState::Accepted),
        DeployOperationState::Running { stage: attempted } => match current {
            DeployOperationState::Running { stage: current } => {
                let current_rank = deploy_stage_rank(*current);
                let attempted_rank = deploy_stage_rank(*attempted);
                current_rank > attempted_rank
                    || current_rank == attempted_rank && current == attempted
            }
            DeployOperationState::Accepted
            | DeployOperationState::Planning
            | DeployOperationState::Completed
            | DeployOperationState::Failed { .. }
            | DeployOperationState::Cancelled { .. } => false,
        },
        DeployOperationState::Completed => matches!(current, DeployOperationState::Completed),
        DeployOperationState::Failed { failure: attempted } => {
            matches!(current, DeployOperationState::Failed { failure } if failure == attempted)
        }
        DeployOperationState::Cancelled { reason: attempted } => {
            matches!(current, DeployOperationState::Cancelled { reason } if reason == attempted)
        }
    }
}

pub fn validate_deploy_transition(
    operation_id: &OperationId,
    current: &DeployOperationState,
    attempted: &DeployOperationState,
) -> Result<(), StatusProjectionError> {
    if current.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: operation_id.clone(),
            current: Box::new(current.clone()),
            attempted: Box::new(attempted.clone()),
        });
    }

    if deploy_transition_allowed(current, attempted) {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(current.clone()),
        attempted: Box::new(attempted.clone()),
    })
}

fn deploy_transition_allowed(
    current: &DeployOperationState,
    attempted: &DeployOperationState,
) -> bool {
    match (current, attempted) {
        (DeployOperationState::Accepted, DeployOperationState::Planning)
        | (DeployOperationState::Accepted, DeployOperationState::Cancelled { .. })
        | (DeployOperationState::Accepted, DeployOperationState::Failed { .. })
        | (DeployOperationState::Planning, DeployOperationState::Cancelled { .. })
        | (DeployOperationState::Planning, DeployOperationState::Failed { .. })
        | (DeployOperationState::Running { .. }, DeployOperationState::Cancelled { .. })
        | (DeployOperationState::Running { .. }, DeployOperationState::Failed { .. }) => true,
        (
            DeployOperationState::Running {
                stage: DeployRunningStage::ActiveServiceCommit,
            },
            DeployOperationState::Completed,
        ) => true,
        (
            DeployOperationState::Planning,
            DeployOperationState::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        ) => true,
        (
            DeployOperationState::Running { stage: current },
            DeployOperationState::Running { stage: attempted },
        ) => deploy_stage_is_next(*current, *attempted),
        (DeployOperationState::Accepted, _)
        | (DeployOperationState::Completed, _)
        | (DeployOperationState::Failed { .. }, _)
        | (DeployOperationState::Cancelled { .. }, _)
        | (DeployOperationState::Planning, _)
        | (DeployOperationState::Running { .. }, _) => false,
    }
}

fn deploy_stage_rank(stage: DeployRunningStage) -> u8 {
    match stage {
        DeployRunningStage::StartingContainers => 0,
        DeployRunningStage::WaitingForHealth => 1,
        DeployRunningStage::ActiveServiceCommit => 2,
    }
}

fn deploy_stage_is_next(current: DeployRunningStage, attempted: DeployRunningStage) -> bool {
    matches!(
        (current, attempted),
        (
            DeployRunningStage::StartingContainers,
            DeployRunningStage::WaitingForHealth
        ) | (
            DeployRunningStage::WaitingForHealth,
            DeployRunningStage::ActiveServiceCommit
        )
    )
}
