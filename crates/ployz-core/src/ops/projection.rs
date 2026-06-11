use super::backup;
use super::classification::{
    CertEvent, ClassifiedOperationEvent, DeployEvent, MachineAddEvent, OperationSubjectRef,
};
use super::{
    BackupOperationState, BackupTransition, CertId, CertOperationState, CertRunningStage,
    CertTransition, DeployEvidence, DeployOperationState, DeployRunningStage, DeployTransition,
    EventSequence, NodeId, OperationEvent, OperationId, OperationKind, OperationStatus,
};
use crate::machine::MachineAddOperationState;

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
    MachineAdd(MachineAddOperationState),
    Backup(BackupOperationState),
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
            operation_id: current.id().clone(),
            expected: OperationKind::Cert,
            actual: current.kind(),
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
            operation_id: current.id().clone(),
            expected: OperationKind::Deploy,
            actual: current.kind(),
        });
    };
    let valid = match evidence {
        DeployEvidence::PlanCreated { .. } => matches!(state, DeployOperationState::Planning),
        DeployEvidence::WireGuardEbpfPrepared { .. } => evidence_is_current_or_past_running_stage(
            state,
            DeployRunningStage::PreparingWireGuardEbpf,
        ),
        DeployEvidence::ContainerStarted { .. } => {
            evidence_is_current_or_past_running_stage(state, DeployRunningStage::StartingContainers)
        }
        DeployEvidence::HealthCheckStarted => {
            evidence_is_current_or_past_running_stage(state, DeployRunningStage::WaitingForHealth)
        }
        DeployEvidence::CleanupFinished { .. } => cleanup_evidence_is_valid(state),
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

fn cleanup_evidence_is_valid(state: &DeployOperationState) -> bool {
    matches!(
        state,
        DeployOperationState::Running {
            stage: DeployRunningStage::ActiveServiceCommit
                | DeployRunningStage::RemovingSupersededContainers
        }
    )
}

fn deploy_evidence_required_state(evidence: &DeployEvidence) -> DeployOperationState {
    match evidence {
        DeployEvidence::PlanCreated { .. } => DeployOperationState::Planning,
        DeployEvidence::WireGuardEbpfPrepared { .. } => DeployOperationState::Running {
            stage: DeployRunningStage::PreparingWireGuardEbpf,
        },
        DeployEvidence::ContainerStarted { .. } => DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        DeployEvidence::HealthCheckStarted => DeployOperationState::Running {
            stage: DeployRunningStage::WaitingForHealth,
        },
        DeployEvidence::CleanupFinished { .. } => DeployOperationState::Running {
            stage: DeployRunningStage::RemovingSupersededContainers,
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
            operation_id: current.id().clone(),
            expected: OperationKind::Deploy,
            actual: current.kind(),
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
    let event = ClassifiedOperationEvent::from(event);
    let event_operation_id = event.operation_id();
    let current_operation_id = current.id();
    if event_operation_id != current_operation_id {
        return Err(StatusProjectionError::OperationEventMismatch {
            expected_operation_id: current_operation_id.clone(),
            actual_operation_id: event_operation_id.clone(),
        });
    }

    let last_event_sequence = current.last_event_sequence();
    if event_sequence <= last_event_sequence {
        return Ok(OperationEventProjection::AlreadySatisfied);
    }

    match event {
        ClassifiedOperationEvent::Deploy { event, .. } => {
            let OperationStatus::Deploy { state, .. } = current else {
                return Err(kind_mismatch(current, OperationKind::Deploy));
            };
            project_deploy_event(current, state, event, event_sequence)
        }
        ClassifiedOperationEvent::Cert { subject, event, .. } => {
            let OperationStatus::Cert { cert_id, .. } = current else {
                return Err(kind_mismatch(current, OperationKind::Cert));
            };
            project_cert_event(current, cert_id, subject, event, event_sequence)
        }
        ClassifiedOperationEvent::MachineAdd { subject, event, .. } => {
            let OperationStatus::MachineAdd { node_id, .. } = current else {
                return Err(kind_mismatch(current, OperationKind::MachineAdd));
            };
            project_machine_add_event(current, node_id, subject, event, event_sequence)
        }
        ClassifiedOperationEvent::Backup { event, .. } => {
            let OperationStatus::Backup { state, .. } = current else {
                return Err(kind_mismatch(current, OperationKind::Backup));
            };
            backup::project_event(current, state, event, event_sequence)
        }
        ClassifiedOperationEvent::Cancelled {
            operation_id: _,
            reason,
        } => match current {
            OperationStatus::Deploy { state, .. } => project_deploy_event(
                current,
                state,
                DeployEvent::Transition(DeployTransition::Cancelled { reason }),
                event_sequence,
            ),
            OperationStatus::Cert { .. } => cert_transition_projection(
                current,
                CertTransition::Cancelled { reason },
                event_sequence,
            ),
            OperationStatus::MachineAdd { .. } => project_machine_add_state(
                current,
                MachineAddOperationState::Cancelled { reason },
                event_sequence,
            ),
            OperationStatus::Backup { .. } => backup::transition_projection(
                current,
                BackupTransition::Cancelled { reason },
                event_sequence,
            ),
        },
    }
}

fn kind_mismatch(current: &OperationStatus, actual: OperationKind) -> StatusProjectionError {
    StatusProjectionError::OperationKindMismatch {
        operation_id: current.id().clone(),
        expected: current.kind(),
        actual,
    }
}

fn project_machine_add_state(
    current: &OperationStatus,
    attempted: MachineAddOperationState,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    let OperationStatus::MachineAdd {
        id,
        node_id,
        name,
        gateway,
        state,
        ..
    } = current
    else {
        return Err(kind_mismatch(current, OperationKind::MachineAdd));
    };

    if state == &attempted {
        return Ok(OperationEventProjection::AlreadySatisfied);
    }
    if state.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: id.clone(),
            current: Box::new(ProjectionOperationState::MachineAdd(state.clone())),
            attempted: Box::new(ProjectionOperationState::MachineAdd(attempted)),
        });
    }
    if !machine_add_transition_allowed(state, &attempted) {
        return Err(StatusProjectionError::InvalidTransition {
            operation_id: id.clone(),
            current: Box::new(ProjectionOperationState::MachineAdd(state.clone())),
            attempted: Box::new(ProjectionOperationState::MachineAdd(attempted)),
        });
    }

    Ok(OperationEventProjection::StatusChanged {
        status: Box::new(OperationStatus::MachineAdd {
            id: id.clone(),
            node_id: node_id.clone(),
            name: name.clone(),
            gateway: *gateway,
            state: attempted,
            last_event_sequence: event_sequence,
        }),
    })
}

fn machine_add_transition_allowed(
    current: &MachineAddOperationState,
    attempted: &MachineAddOperationState,
) -> bool {
    match (current, attempted) {
        (
            MachineAddOperationState::Pending { .. },
            MachineAddOperationState::Joining { .. } | MachineAddOperationState::Cancelled { .. },
        )
        | (
            MachineAddOperationState::Joining { .. },
            MachineAddOperationState::Completed | MachineAddOperationState::Cancelled { .. },
        ) => true,
        (
            MachineAddOperationState::Pending { .. } | MachineAddOperationState::Joining { .. },
            MachineAddOperationState::Failed { failure },
        ) => machine_add_failure_allowed(current, failure),
        (
            MachineAddOperationState::Pending { .. } | MachineAddOperationState::Joining { .. },
            MachineAddOperationState::Pending { .. },
        )
        | (MachineAddOperationState::Joining { .. }, MachineAddOperationState::Joining { .. })
        | (MachineAddOperationState::Pending { .. }, MachineAddOperationState::Completed)
        | (
            MachineAddOperationState::Completed
            | MachineAddOperationState::Failed { .. }
            | MachineAddOperationState::Cancelled { .. },
            _,
        ) => false,
    }
}

fn machine_add_failure_allowed(
    current: &MachineAddOperationState,
    failure: &crate::machine::MachineAddFailure,
) -> bool {
    match (current, failure) {
        (
            MachineAddOperationState::Pending { .. },
            crate::machine::MachineAddFailure::InvalidJoinToken
            | crate::machine::MachineAddFailure::JoinTokenExpired { .. }
            | crate::machine::MachineAddFailure::AuthorizationRenderFailed { .. }
            | crate::machine::MachineAddFailure::NatsReloadFailed { .. }
            | crate::machine::MachineAddFailure::MintedCredentialUnusable { .. },
        )
        | (
            MachineAddOperationState::Joining { .. },
            crate::machine::MachineAddFailure::BootstrapFailed { .. }
            | crate::machine::MachineAddFailure::ReadinessFailed { .. },
        ) => true,
        (
            MachineAddOperationState::Pending { .. },
            crate::machine::MachineAddFailure::BootstrapFailed { .. }
            | crate::machine::MachineAddFailure::ReadinessFailed { .. },
        )
        | (
            MachineAddOperationState::Joining { .. },
            crate::machine::MachineAddFailure::InvalidJoinToken
            | crate::machine::MachineAddFailure::JoinTokenExpired { .. }
            | crate::machine::MachineAddFailure::AuthorizationRenderFailed { .. }
            | crate::machine::MachineAddFailure::NatsReloadFailed { .. }
            | crate::machine::MachineAddFailure::MintedCredentialUnusable { .. },
        )
        | (
            MachineAddOperationState::Completed
            | MachineAddOperationState::Failed { .. }
            | MachineAddOperationState::Cancelled { .. },
            _,
        ) => false,
    }
}

fn project_deploy_event(
    current: &OperationStatus,
    state: &DeployOperationState,
    event: DeployEvent,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    match event {
        DeployEvent::Evidence(DeployEvidence::ContainerStarted { .. }) => {
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
        DeployEvent::Evidence(DeployEvidence::WireGuardEbpfPrepared { .. }) => {
            if !matches!(
                state,
                DeployOperationState::Running {
                    stage: DeployRunningStage::PreparingWireGuardEbpf
                }
            ) {
                return evidence_cursor_after_stage(
                    evidence_is_satisfied_after_stage(
                        state,
                        DeployRunningStage::PreparingWireGuardEbpf,
                    ),
                    current,
                    event_sequence,
                );
            }

            Ok(OperationEventProjection::StatusChanged {
                status: Box::new(evidence_status(current, event_sequence)),
            })
        }
        DeployEvent::Evidence(DeployEvidence::HealthCheckStarted) => {
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
        DeployEvent::Evidence(DeployEvidence::CleanupFinished { .. }) => {
            if !cleanup_evidence_is_valid(state) {
                return evidence_cursor_after_stage(
                    evidence_is_satisfied_after_stage(
                        state,
                        DeployRunningStage::RemovingSupersededContainers,
                    ),
                    current,
                    event_sequence,
                );
            }

            Ok(OperationEventProjection::StatusChanged {
                status: Box::new(evidence_status(current, event_sequence)),
            })
        }
        DeployEvent::Evidence(DeployEvidence::PlanCreated { .. }) => {
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
        DeployEvent::Transition(transition) => {
            project_deploy_transition(current, transition, event_sequence)
                .map(deploy_projection_to_event_projection)
        }
        DeployEvent::Submitted => Ok(OperationEventProjection::AlreadySatisfied),
    }
}

fn project_cert_event(
    current: &OperationStatus,
    cert_id: &CertId,
    event_subject: OperationSubjectRef,
    event: CertEvent,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    if event_subject != OperationSubjectRef::Cert(cert_id.clone()) {
        return Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id: current.id().clone(),
            expected: OperationSubjectRef::Cert(cert_id.clone()),
            actual: event_subject,
        });
    }

    match event {
        CertEvent::Transition(transition) => {
            cert_transition_projection(current, transition, event_sequence)
        }
        CertEvent::Submitted => Ok(OperationEventProjection::AlreadySatisfied),
    }
}

fn project_machine_add_event(
    current: &OperationStatus,
    expected_node_id: &NodeId,
    event_subject: OperationSubjectRef,
    event: MachineAddEvent,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    if event_subject != OperationSubjectRef::MachineAdd(expected_node_id.clone()) {
        return Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id: current.id().clone(),
            expected: OperationSubjectRef::MachineAdd(expected_node_id.clone()),
            actual: event_subject,
        });
    }

    match event {
        MachineAddEvent::Submitted => Ok(OperationEventProjection::AlreadySatisfied),
        MachineAddEvent::CredentialProvisioned => {
            project_machine_add_credential_evidence(current, event_sequence)
        }
        MachineAddEvent::Transition(attempted) => {
            project_machine_add_state(current, attempted, event_sequence)
        }
    }
}

/// Credential-provisioning steps are evidence: they advance the status
/// cursor without changing the machine-add state. They are only recorded
/// while the operation is live; once terminal, the evidence is satisfied.
fn project_machine_add_credential_evidence(
    current: &OperationStatus,
    event_sequence: EventSequence,
) -> Result<OperationEventProjection, StatusProjectionError> {
    let OperationStatus::MachineAdd {
        id,
        node_id,
        name,
        gateway,
        state,
        ..
    } = current
    else {
        return Err(kind_mismatch(current, OperationKind::MachineAdd));
    };
    if state.is_terminal() {
        return Ok(OperationEventProjection::AlreadySatisfied);
    }

    Ok(OperationEventProjection::StatusChanged {
        status: Box::new(OperationStatus::MachineAdd {
            id: id.clone(),
            node_id: node_id.clone(),
            name: name.clone(),
            gateway: *gateway,
            state: state.clone(),
            last_event_sequence: event_sequence,
        }),
    })
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
        DeployOperationState::Completed { .. } => true,
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
            | DeployOperationState::Completed { .. }
            | DeployOperationState::Failed { .. }
            | DeployOperationState::Cancelled { .. } => false,
        },
        DeployOperationState::Completed { outcome: attempted } => {
            matches!(current, DeployOperationState::Completed { outcome } if outcome == attempted)
        }
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
            DeployOperationState::Completed { .. },
        )
        | (
            DeployOperationState::Running {
                stage: DeployRunningStage::RemovingSupersededContainers,
            },
            DeployOperationState::Completed { .. },
        ) => true,
        (
            DeployOperationState::Planning,
            DeployOperationState::Running {
                stage: DeployRunningStage::PreparingWireGuardEbpf,
            },
        ) => true,
        (
            DeployOperationState::Running { stage: current },
            DeployOperationState::Running { stage: attempted },
        ) => deploy_stage_is_next(*current, *attempted),
        (DeployOperationState::Accepted, _)
        | (DeployOperationState::Completed { .. }, _)
        | (DeployOperationState::Failed { .. }, _)
        | (DeployOperationState::Cancelled { .. }, _)
        | (DeployOperationState::Planning, _)
        | (DeployOperationState::Running { .. }, _) => false,
    }
}

fn deploy_stage_rank(stage: DeployRunningStage) -> u8 {
    match stage {
        DeployRunningStage::PreparingWireGuardEbpf => 0,
        DeployRunningStage::StartingContainers => 1,
        DeployRunningStage::WaitingForHealth => 2,
        DeployRunningStage::RouteCutover => 3,
        DeployRunningStage::ActiveServiceCommit => 4,
        DeployRunningStage::RemovingSupersededContainers => 5,
    }
}

fn deploy_stage_is_next(current: DeployRunningStage, attempted: DeployRunningStage) -> bool {
    matches!(
        (current, attempted),
        (
            DeployRunningStage::PreparingWireGuardEbpf,
            DeployRunningStage::StartingContainers
        ) | (
            DeployRunningStage::StartingContainers,
            DeployRunningStage::WaitingForHealth
        ) | (
            DeployRunningStage::WaitingForHealth,
            DeployRunningStage::RouteCutover
        ) | (
            DeployRunningStage::RouteCutover,
            DeployRunningStage::ActiveServiceCommit
        ) | (
            DeployRunningStage::ActiveServiceCommit,
            DeployRunningStage::RemovingSupersededContainers
        ) | (
            DeployRunningStage::WaitingForHealth,
            DeployRunningStage::ActiveServiceCommit
        )
    )
}
