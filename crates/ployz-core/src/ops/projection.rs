use super::{
    CertId, CertOperationState, CertRunningStage, CertTransition, DeployEvidence,
    DeployOperationState, DeployRunningStage, DeployTransition, EventSequence, OperationEvent,
    OperationId, OperationStatus,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployProjection {
    Updated { status: Box<OperationStatus> },
    AlreadySatisfied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertProjection {
    Updated { status: Box<OperationStatus> },
    AlreadySatisfied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationEventProjection {
    StatusChanged { status: Box<OperationStatus> },
    AlreadySatisfied,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusProjectionError {
    MissingOperation {
        operation_id: OperationId,
    },
    OperationKindMismatch {
        operation_id: OperationId,
        expected: OperationKind,
        actual: OperationKind,
    },
    OperationSubjectMismatch {
        operation_id: OperationId,
        expected: OperationSubjectRef,
        actual: OperationSubjectRef,
    },
    OperationEventMismatch {
        expected_operation_id: OperationId,
        actual_operation_id: OperationId,
    },
    TerminalState {
        operation_id: OperationId,
        current: Box<ProjectionOperationState>,
        attempted: Box<ProjectionOperationState>,
    },
    InvalidTransition {
        operation_id: OperationId,
        current: Box<ProjectionOperationState>,
        attempted: Box<ProjectionOperationState>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionOperationState {
    Deploy(DeployOperationState),
    Cert(CertOperationState),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationKind {
    Deploy,
    Cert,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationSubjectRef {
    Cert(CertId),
}

pub fn project_cert_transition(
    current: &OperationStatus,
    transition: CertTransition,
    event_sequence: EventSequence,
) -> Result<CertProjection, StatusProjectionError> {
    let OperationStatus::Cert {
        id,
        cert_id,
        state: current_state,
        ..
    } = current
    else {
        return Err(StatusProjectionError::OperationKindMismatch {
            operation_id: operation_status_id(current).clone(),
            expected: OperationKind::Cert,
            actual: operation_status_kind(current),
        });
    };
    let attempted = transition.state();

    if cert_transition_satisfied(current_state, &attempted) {
        return Ok(CertProjection::AlreadySatisfied);
    }

    validate_cert_transition(id, current_state, &attempted)?;

    Ok(CertProjection::Updated {
        status: Box::new(OperationStatus::Cert {
            id: id.clone(),
            cert_id: cert_id.clone(),
            state: attempted,
            last_event_sequence: event_sequence,
        }),
    })
}

pub fn validate_fresh_deploy_evidence(
    current: &OperationStatus,
    evidence: &DeployEvidence,
) -> Result<(), StatusProjectionError> {
    let OperationStatus::Deploy { id, state, .. } = current else {
        return Err(StatusProjectionError::OperationKindMismatch {
            operation_id: operation_status_id(current).clone(),
            expected: OperationKind::Deploy,
            actual: operation_status_kind(current),
        });
    };
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
        current: Box::new(ProjectionOperationState::Deploy(state.clone())),
        attempted: Box::new(ProjectionOperationState::Deploy(
            deploy_evidence_required_state(evidence),
        )),
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
    } = current
    else {
        return Err(StatusProjectionError::OperationKindMismatch {
            operation_id: operation_status_id(current).clone(),
            expected: OperationKind::Deploy,
            actual: operation_status_kind(current),
        });
    };
    let attempted = transition.state();

    if deploy_transition_satisfied(current_state, &attempted) {
        return Ok(DeployProjection::AlreadySatisfied);
    }

    validate_deploy_transition(id, current_state, &attempted)?;

    Ok(DeployProjection::Updated {
        status: Box::new(OperationStatus::Deploy {
            id: id.clone(),
            service_id: service_id.clone(),
            state: attempted,
            last_event_sequence: event_sequence,
        }),
    })
}

pub fn project_operation_event(
    current: &OperationStatus,
    event: OperationEvent,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    let event_operation_id = operation_event_id(&event);
    let current_operation_id = operation_status_id(current);
    if event_operation_id != current_operation_id {
        return Err(StatusProjectionError::OperationEventMismatch {
            expected_operation_id: current_operation_id.clone(),
            actual_operation_id: event_operation_id.clone(),
        });
    }

    let last_event_sequence = operation_status_sequence(current);
    if event_sequence <= last_event_sequence {
        return Ok(OperationEventProjection::AlreadySatisfied);
    }

    match (current, operation_event_kind(&event)) {
        (OperationStatus::Deploy { state, .. }, Some(OperationKind::Deploy) | None) => {
            project_deploy_event(current, state, event, event_sequence)
        }
        (OperationStatus::Cert { cert_id, .. }, Some(OperationKind::Cert) | None) => {
            project_cert_event(current, cert_id, event, event_sequence)
        }
        (OperationStatus::Deploy { id, .. }, Some(OperationKind::Cert)) => {
            Err(StatusProjectionError::OperationKindMismatch {
                operation_id: id.clone(),
                expected: OperationKind::Deploy,
                actual: OperationKind::Cert,
            })
        }
        (OperationStatus::Cert { id, .. }, Some(OperationKind::Deploy)) => {
            Err(StatusProjectionError::OperationKindMismatch {
                operation_id: id.clone(),
                expected: OperationKind::Cert,
                actual: OperationKind::Deploy,
            })
        }
    }
}

fn project_deploy_event(
    current: &OperationStatus,
    state: &DeployOperationState,
    event: OperationEvent,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
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
                status: Box::new(evidence_status(current, event_sequence)),
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
                status: Box::new(evidence_status(current, event_sequence)),
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
                status: Box::new(evidence_status(current, event_sequence)),
            })
        }
        OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::DeployFailed { .. }
        | OperationEvent::Cancelled { .. } => {
            project_deploy_transition(current, deploy_transition_from_event(event), event_sequence)
                .map(deploy_projection_to_event_projection)
        }
        OperationEvent::DeploySubmitted { .. } => Ok(OperationEventProjection::AlreadySatisfied),
        OperationEvent::CertRenewalSubmitted { .. }
        | OperationEvent::CertChallengePublished { .. }
        | OperationEvent::CertValidationStarted { .. }
        | OperationEvent::CertCompleted { .. }
        | OperationEvent::CertFailed { .. } => {
            unreachable!("operation kind is checked before deploy projection")
        }
    }
}

fn project_cert_event(
    current: &OperationStatus,
    cert_id: &CertId,
    event: OperationEvent,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    if let Some(actual_subject) = cert_event_subject(&event)
        && actual_subject != OperationSubjectRef::Cert(cert_id.clone())
    {
        return Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id: operation_status_id(current).clone(),
            expected: OperationSubjectRef::Cert(cert_id.clone()),
            actual: actual_subject,
        });
    }

    match event {
        OperationEvent::CertChallengePublished { .. } => cert_transition_projection(
            current,
            CertTransition::Running {
                stage: CertRunningStage::ChallengePublished,
            },
            event_sequence,
        ),
        OperationEvent::CertValidationStarted { .. } => cert_transition_projection(
            current,
            CertTransition::Running {
                stage: CertRunningStage::ValidationStarted,
            },
            event_sequence,
        ),
        OperationEvent::CertCompleted { .. } => {
            cert_transition_projection(current, CertTransition::Completed, event_sequence)
        }
        OperationEvent::CertFailed { failure, .. } => {
            cert_transition_projection(current, CertTransition::Failed { failure }, event_sequence)
        }
        OperationEvent::CertRenewalSubmitted { .. } => {
            Ok(OperationEventProjection::AlreadySatisfied)
        }
        OperationEvent::Cancelled { reason, .. } => cert_transition_projection(
            current,
            CertTransition::Cancelled { reason },
            event_sequence,
        ),
        OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployPlanCreated { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployContainerStarted { .. }
        | OperationEvent::DeployHealthCheckStarted { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::DeployFailed { .. } => {
            unreachable!("operation kind is checked before cert projection")
        }
    }
}

fn cert_event_subject(event: &OperationEvent) -> Option<OperationSubjectRef> {
    match event {
        OperationEvent::CertRenewalSubmitted { cert_id, .. }
        | OperationEvent::CertChallengePublished { cert_id, .. }
        | OperationEvent::CertValidationStarted { cert_id, .. } => {
            Some(OperationSubjectRef::Cert(cert_id.clone()))
        }
        OperationEvent::CertCompleted { active_cert, .. } => {
            Some(OperationSubjectRef::Cert(active_cert.cert_id.clone()))
        }
        OperationEvent::CertFailed { failure, .. } => {
            Some(OperationSubjectRef::Cert(failure.cert_id().clone()))
        }
        OperationEvent::Cancelled { .. }
        | OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployPlanCreated { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployContainerStarted { .. }
        | OperationEvent::DeployHealthCheckStarted { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::DeployFailed { .. } => None,
    }
}

fn deploy_projection_to_event_projection(projection: DeployProjection) -> OperationEventProjection {
    match projection {
        DeployProjection::Updated { status } => OperationEventProjection::StatusChanged { status },
        DeployProjection::AlreadySatisfied => OperationEventProjection::AlreadySatisfied,
    }
}

fn cert_transition_projection(
    current: &OperationStatus,
    transition: CertTransition,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    match project_cert_transition(current, transition, event_sequence)? {
        CertProjection::Updated { status } => {
            Ok(OperationEventProjection::StatusChanged { status })
        }
        CertProjection::AlreadySatisfied => Ok(OperationEventProjection::AlreadySatisfied),
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
        status: Box::new(evidence_status(current, event_sequence)),
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
    } = current
    else {
        return current.clone();
    };

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
        | OperationEvent::DeployHealthCheckStarted { .. }
        | OperationEvent::CertRenewalSubmitted { .. }
        | OperationEvent::CertChallengePublished { .. }
        | OperationEvent::CertValidationStarted { .. }
        | OperationEvent::CertCompleted { .. }
        | OperationEvent::CertFailed { .. } => {
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
        | OperationEvent::DeployCompleted { operation_id }
        | OperationEvent::DeployFailed { operation_id, .. }
        | OperationEvent::CertRenewalSubmitted { operation_id, .. }
        | OperationEvent::CertChallengePublished { operation_id, .. }
        | OperationEvent::CertValidationStarted { operation_id, .. }
        | OperationEvent::CertCompleted { operation_id, .. }
        | OperationEvent::CertFailed { operation_id, .. }
        | OperationEvent::Cancelled { operation_id, .. } => operation_id,
    }
}

fn operation_event_kind(event: &OperationEvent) -> Option<OperationKind> {
    match event {
        OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployPlanCreated { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployContainerStarted { .. }
        | OperationEvent::DeployHealthCheckStarted { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::DeployFailed { .. } => Some(OperationKind::Deploy),
        OperationEvent::CertRenewalSubmitted { .. }
        | OperationEvent::CertChallengePublished { .. }
        | OperationEvent::CertValidationStarted { .. }
        | OperationEvent::CertCompleted { .. }
        | OperationEvent::CertFailed { .. } => Some(OperationKind::Cert),
        OperationEvent::Cancelled { .. } => None,
    }
}

fn operation_status_id(status: &OperationStatus) -> &OperationId {
    match status {
        OperationStatus::Deploy { id, .. } | OperationStatus::Cert { id, .. } => id,
    }
}

fn operation_status_kind(status: &OperationStatus) -> OperationKind {
    match status {
        OperationStatus::Deploy { .. } => OperationKind::Deploy,
        OperationStatus::Cert { .. } => OperationKind::Cert,
    }
}

fn operation_status_sequence(status: &OperationStatus) -> EventSequence {
    match status {
        OperationStatus::Deploy {
            last_event_sequence,
            ..
        }
        | OperationStatus::Cert {
            last_event_sequence,
            ..
        } => *last_event_sequence,
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
            current: Box::new(ProjectionOperationState::Deploy(current.clone())),
            attempted: Box::new(ProjectionOperationState::Deploy(attempted.clone())),
        });
    }

    if deploy_transition_allowed(current, attempted) {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(ProjectionOperationState::Deploy(current.clone())),
        attempted: Box::new(ProjectionOperationState::Deploy(attempted.clone())),
    })
}

pub fn validate_cert_transition(
    operation_id: &OperationId,
    current: &CertOperationState,
    attempted: &CertOperationState,
) -> Result<(), StatusProjectionError> {
    if current.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: operation_id.clone(),
            current: Box::new(ProjectionOperationState::Cert(current.clone())),
            attempted: Box::new(ProjectionOperationState::Cert(attempted.clone())),
        });
    }

    if cert_transition_allowed(current, attempted) {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(ProjectionOperationState::Cert(current.clone())),
        attempted: Box::new(ProjectionOperationState::Cert(attempted.clone())),
    })
}

fn cert_transition_allowed(current: &CertOperationState, attempted: &CertOperationState) -> bool {
    match (current, attempted) {
        (
            CertOperationState::Accepted,
            CertOperationState::Running {
                stage: CertRunningStage::ChallengePublished,
            },
        )
        | (CertOperationState::Accepted, CertOperationState::Cancelled { .. })
        | (CertOperationState::Accepted, CertOperationState::Failed { .. })
        | (CertOperationState::Running { .. }, CertOperationState::Cancelled { .. })
        | (CertOperationState::Running { .. }, CertOperationState::Failed { .. }) => true,
        (
            CertOperationState::Running {
                stage: CertRunningStage::ValidationStarted,
            },
            CertOperationState::Completed,
        ) => true,
        (
            CertOperationState::Running { stage: current },
            CertOperationState::Running { stage: attempted },
        ) => cert_stage_is_next(*current, *attempted),
        (CertOperationState::Accepted, _)
        | (CertOperationState::Completed, _)
        | (CertOperationState::Failed { .. }, _)
        | (CertOperationState::Cancelled { .. }, _)
        | (CertOperationState::Running { .. }, _) => false,
    }
}

fn cert_transition_satisfied(current: &CertOperationState, attempted: &CertOperationState) -> bool {
    match attempted {
        CertOperationState::Accepted => matches!(current, CertOperationState::Accepted),
        CertOperationState::Running { stage: attempted } => match current {
            CertOperationState::Running { stage: current } => {
                let current_rank = cert_stage_rank(*current);
                let attempted_rank = cert_stage_rank(*attempted);
                current_rank > attempted_rank
                    || current_rank == attempted_rank && current == attempted
            }
            CertOperationState::Accepted
            | CertOperationState::Completed
            | CertOperationState::Failed { .. }
            | CertOperationState::Cancelled { .. } => false,
        },
        CertOperationState::Completed => matches!(current, CertOperationState::Completed),
        CertOperationState::Failed { failure: attempted } => {
            matches!(current, CertOperationState::Failed { failure } if failure == attempted)
        }
        CertOperationState::Cancelled { reason: attempted } => {
            matches!(current, CertOperationState::Cancelled { reason } if reason == attempted)
        }
    }
}

fn cert_stage_rank(stage: CertRunningStage) -> u8 {
    match stage {
        CertRunningStage::ChallengePublished => 0,
        CertRunningStage::ValidationStarted => 1,
    }
}

fn cert_stage_is_next(current: CertRunningStage, attempted: CertRunningStage) -> bool {
    matches!(
        (current, attempted),
        (
            CertRunningStage::ChallengePublished,
            CertRunningStage::ValidationStarted
        )
    )
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
