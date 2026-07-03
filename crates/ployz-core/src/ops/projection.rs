mod machine_update;

use super::classification::{
    CertEvent, ClassifiedOperationEvent, DeployEvent, MachineAddEvent, OperationSubjectRef,
};
use super::{
    CertId, CertOperationState, CertRunningStage, CertTransition, DeployEvidence,
    DeployOperationState, DeployRunningStage, DeployTransition, EventSequence, MachineId,
    MachineUpdateOperationState, OperationEvent, OperationId, OperationKind, OperationStatus,
    ServiceId,
};
use crate::machine::{MachineAddOperationState, MachineName};
use crate::roles::InstallRolePolicy;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationProjection {
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
    MachineUpdate(MachineUpdateOperationState),
}

/// What a piece of deploy evidence requires of the operation state to count
/// as fresh. This is the single source of the evidence→stage mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceRequirement {
    Planning,
    RunningStage(DeployRunningStage),
    Cleanup,
}

const fn evidence_requirement(evidence: &DeployEvidence) -> EvidenceRequirement {
    match evidence {
        DeployEvidence::PlanCreated { .. } => EvidenceRequirement::Planning,
        DeployEvidence::DataplanePrepared { .. } => {
            EvidenceRequirement::RunningStage(DeployRunningStage::PreparingDataplane)
        }
        DeployEvidence::ContainerStarted { .. } => {
            EvidenceRequirement::RunningStage(DeployRunningStage::StartingContainers)
        }
        DeployEvidence::HealthCheckStarted => {
            EvidenceRequirement::RunningStage(DeployRunningStage::WaitingForHealth)
        }
        DeployEvidence::CleanupFinished { .. } => EvidenceRequirement::Cleanup,
    }
}

pub fn project_cert_transition(
    current: &OperationStatus,
    transition: CertTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
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

    project_cert_state(
        id,
        cert_id,
        current_state,
        transition.state(),
        event_sequence,
    )
}

fn project_cert_state(
    id: &OperationId,
    cert_id: &CertId,
    current_state: &CertOperationState,
    attempted: CertOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if cert_transition_satisfied(current_state, &attempted) {
        return Ok(OperationProjection::AlreadySatisfied);
    }

    validate_cert_transition(id, current_state, &attempted)?;

    Ok(OperationProjection::StatusChanged {
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
    let valid = match evidence_requirement(evidence) {
        EvidenceRequirement::Planning => matches!(state, DeployOperationState::Planning),
        EvidenceRequirement::RunningStage(stage) => {
            evidence_is_current_or_past_running_stage(state, stage)
        }
        EvidenceRequirement::Cleanup => cleanup_evidence_is_valid(state),
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
    match evidence_requirement(evidence) {
        EvidenceRequirement::Planning => DeployOperationState::Planning,
        EvidenceRequirement::RunningStage(stage) => DeployOperationState::Running { stage },
        EvidenceRequirement::Cleanup => DeployOperationState::Running {
            stage: DeployRunningStage::RemovingSupersededContainers,
        },
    }
}

pub fn project_deploy_transition(
    current: &OperationStatus,
    transition: DeployTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
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

    project_deploy_state(
        id,
        service_id,
        current_state,
        transition.state(),
        event_sequence,
    )
}

fn project_deploy_state(
    id: &OperationId,
    service_id: &ServiceId,
    current_state: &DeployOperationState,
    attempted: DeployOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if deploy_transition_satisfied(current_state, &attempted) {
        return Ok(OperationProjection::AlreadySatisfied);
    }

    validate_deploy_transition(id, current_state, &attempted)?;

    Ok(OperationProjection::StatusChanged {
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
) -> Result<OperationProjection, StatusProjectionError> {
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
        return Ok(OperationProjection::AlreadySatisfied);
    }

    match event {
        ClassifiedOperationEvent::Deploy { event, .. } => {
            let OperationStatus::Deploy {
                id,
                service_id,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::Deploy));
            };
            project_deploy_event(id, service_id, state, event, event_sequence)
        }
        ClassifiedOperationEvent::Cert { subject, event, .. } => {
            let OperationStatus::Cert {
                id, cert_id, state, ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::Cert));
            };
            project_cert_event(id, cert_id, state, subject, event, event_sequence)
        }
        ClassifiedOperationEvent::MachineAdd { subject, event, .. } => {
            let OperationStatus::MachineAdd {
                id,
                machine_id,
                name,
                roles,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::MachineAdd));
            };
            let fields = MachineAddFields {
                id,
                machine_id,
                name,
                roles: *roles,
                state,
            };
            project_machine_add_event(fields, subject, event, event_sequence)
        }
        ClassifiedOperationEvent::MachineUpdate { subject, event, .. } => {
            let OperationStatus::MachineUpdate {
                id,
                machine_id,
                target_version,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::MachineUpdate));
            };
            let fields = machine_update::MachineUpdateFields {
                id,
                machine_id,
                target_version,
                state,
            };
            machine_update::project_event(fields, subject, event, event_sequence)
        }
        ClassifiedOperationEvent::Cancelled {
            operation_id: _,
            reason,
        } => match current {
            OperationStatus::Deploy {
                id,
                service_id,
                state,
                ..
            } => project_deploy_event(
                id,
                service_id,
                state,
                DeployEvent::Transition(DeployTransition::Cancelled { reason }),
                event_sequence,
            ),
            OperationStatus::Cert {
                id, cert_id, state, ..
            } => project_cert_state(
                id,
                cert_id,
                state,
                CertTransition::Cancelled { reason }.state(),
                event_sequence,
            ),
            OperationStatus::MachineAdd {
                id,
                machine_id,
                name,
                roles,
                state,
                ..
            } => project_machine_add_state(
                MachineAddFields {
                    id,
                    machine_id,
                    name,
                    roles: *roles,
                    state,
                },
                MachineAddOperationState::Cancelled { reason },
                event_sequence,
            ),
            OperationStatus::MachineUpdate {
                id,
                machine_id,
                target_version,
                state,
                ..
            } => machine_update::project_state(
                machine_update::MachineUpdateFields {
                    id,
                    machine_id,
                    target_version,
                    state,
                },
                MachineUpdateOperationState::Cancelled { reason },
                event_sequence,
            ),
        },
    }
}

pub(super) fn kind_mismatch(
    current: &OperationStatus,
    actual: OperationKind,
) -> StatusProjectionError {
    StatusProjectionError::OperationKindMismatch {
        operation_id: current.id().clone(),
        expected: current.kind(),
        actual,
    }
}

pub(super) fn machine_update_mismatch(
    operation_id: &OperationId,
    expected_machine_id: &MachineId,
    actual: OperationSubjectRef,
) -> StatusProjectionError {
    StatusProjectionError::OperationSubjectMismatch {
        operation_id: operation_id.clone(),
        expected: OperationSubjectRef::MachineUpdate(expected_machine_id.clone()),
        actual,
    }
}

/// The destructured fields of [`OperationStatus::MachineAdd`].
#[derive(Clone, Copy)]
struct MachineAddFields<'status> {
    id: &'status OperationId,
    machine_id: &'status MachineId,
    name: &'status MachineName,
    roles: InstallRolePolicy,
    state: &'status MachineAddOperationState,
}

impl MachineAddFields<'_> {
    fn status_with(
        &self,
        state: MachineAddOperationState,
        event_sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::MachineAdd {
            id: self.id.clone(),
            machine_id: self.machine_id.clone(),
            name: self.name.clone(),
            roles: self.roles,
            state,
            last_event_sequence: event_sequence,
        }
    }
}

fn project_machine_add_state(
    fields: MachineAddFields<'_>,
    attempted: MachineAddOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if fields.state == &attempted {
        return Ok(OperationProjection::AlreadySatisfied);
    }
    if fields.state.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineAdd(fields.state.clone())),
            attempted: Box::new(ProjectionOperationState::MachineAdd(attempted)),
        });
    }
    if !machine_add_transition_allowed(fields.state, &attempted) {
        return Err(StatusProjectionError::InvalidTransition {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineAdd(fields.state.clone())),
            attempted: Box::new(ProjectionOperationState::MachineAdd(attempted)),
        });
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(fields.status_with(attempted, event_sequence)),
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
            | crate::machine::MachineAddFailure::MintedCredentialUnusable { .. }
            | crate::machine::MachineAddFailure::CredentialEvidenceWriteFailed { .. },
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
            | crate::machine::MachineAddFailure::MintedCredentialUnusable { .. }
            | crate::machine::MachineAddFailure::CredentialEvidenceWriteFailed { .. },
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
    id: &OperationId,
    service_id: &ServiceId,
    state: &DeployOperationState,
    event: DeployEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        DeployEvent::Evidence(evidence) => {
            let requirement = evidence_requirement(&evidence);
            let fresh = match requirement {
                EvidenceRequirement::Planning => {
                    matches!(state, DeployOperationState::Planning)
                }
                EvidenceRequirement::RunningStage(required) => matches!(
                    state,
                    DeployOperationState::Running { stage } if *stage == required
                ),
                EvidenceRequirement::Cleanup => cleanup_evidence_is_valid(state),
            };
            if fresh {
                return Ok(OperationProjection::StatusChanged {
                    status: Box::new(evidence_status(id, service_id, state, event_sequence)),
                });
            }

            let satisfied = match requirement {
                EvidenceRequirement::Planning => !matches!(state, DeployOperationState::Accepted),
                EvidenceRequirement::RunningStage(required) => {
                    evidence_is_satisfied_after_stage(state, required)
                }
                EvidenceRequirement::Cleanup => evidence_is_satisfied_after_stage(
                    state,
                    DeployRunningStage::RemovingSupersededContainers,
                ),
            };
            if !satisfied {
                return Ok(OperationProjection::AlreadySatisfied);
            }

            Ok(OperationProjection::StatusChanged {
                status: Box::new(evidence_status(id, service_id, state, event_sequence)),
            })
        }
        DeployEvent::Transition(transition) => {
            project_deploy_state(id, service_id, state, transition.state(), event_sequence)
        }
        DeployEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
    }
}

fn project_cert_event(
    id: &OperationId,
    cert_id: &CertId,
    state: &CertOperationState,
    event_subject: OperationSubjectRef,
    event: CertEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if event_subject != OperationSubjectRef::Cert(cert_id.clone()) {
        return Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id: id.clone(),
            expected: OperationSubjectRef::Cert(cert_id.clone()),
            actual: event_subject,
        });
    }

    match event {
        CertEvent::Transition(transition) => {
            project_cert_state(id, cert_id, state, transition.state(), event_sequence)
        }
        CertEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
    }
}

fn project_machine_add_event(
    fields: MachineAddFields<'_>,
    event_subject: OperationSubjectRef,
    event: MachineAddEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if event_subject != OperationSubjectRef::MachineAdd(fields.machine_id.clone()) {
        return Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id: fields.id.clone(),
            expected: OperationSubjectRef::MachineAdd(fields.machine_id.clone()),
            actual: event_subject,
        });
    }

    match event {
        MachineAddEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
        MachineAddEvent::CredentialProvisioned => {
            project_machine_add_credential_evidence(fields, event_sequence)
        }
        MachineAddEvent::Transition(attempted) => {
            project_machine_add_state(fields, attempted, event_sequence)
        }
    }
}

/// Credential-provisioning steps are evidence: they advance the status
/// cursor without changing the machine-add state. They are only recorded
/// while the operation is live; once terminal, the evidence is satisfied.
fn project_machine_add_credential_evidence(
    fields: MachineAddFields<'_>,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if fields.state.is_terminal() {
        return Ok(OperationProjection::AlreadySatisfied);
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(fields.status_with(fields.state.clone(), event_sequence)),
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

fn evidence_status(
    id: &OperationId,
    service_id: &ServiceId,
    state: &DeployOperationState,
    event_sequence: EventSequence,
) -> OperationStatus {
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
                stage: DeployRunningStage::PreparingDataplane,
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
        DeployRunningStage::PreparingDataplane => 0,
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
            DeployRunningStage::PreparingDataplane,
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
