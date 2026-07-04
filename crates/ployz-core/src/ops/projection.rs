//! Status projection spine: the projection result and error types shared by
//! every operation kind, and the single dispatcher that routes a classified
//! event to its kind's projection. Each kind's projection logic lives in
//! that kind's module.

use super::cert::{self, CertOperationState};
use super::deploy::{self, DeployEvent, DeployOperationState, DeployTransition};
use super::events::{ClassifiedOperationEvent, OperationSubjectRef};
use super::machine_add::{self, MachineAddFields, MachineAddOperationState};
use super::machine_lifecycle::{
    self, MachineLifecycleFields, MachineLifecycleOperationState,
};
use super::machine_update::{self, MachineUpdateFields, MachineUpdateOperationState};
use super::{EventSequence, OperationEvent, OperationId, OperationKind, OperationStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationProjection {
    StatusChanged { status: Box<OperationStatus> },
    AlreadySatisfied,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StatusProjectionError {
    #[error("missing operation {}", .operation_id.as_str())]
    MissingOperation { operation_id: OperationId },
    #[error(
        "operation {} kind mismatch: expected {}, found {}",
        .operation_id.as_str(),
        operation_kind_name(*.expected),
        operation_kind_name(*.actual)
    )]
    OperationKindMismatch {
        operation_id: OperationId,
        expected: OperationKind,
        actual: OperationKind,
    },
    #[error(
        "operation {} subject mismatch: expected {}, found {}",
        .operation_id.as_str(),
        subject_ref_text(.expected),
        subject_ref_text(.actual)
    )]
    OperationSubjectMismatch {
        operation_id: OperationId,
        expected: OperationSubjectRef,
        actual: OperationSubjectRef,
    },
    #[error(
        "operation event mismatch: expected {}, found {}",
        .expected_operation_id.as_str(),
        .actual_operation_id.as_str()
    )]
    OperationEventMismatch {
        expected_operation_id: OperationId,
        actual_operation_id: OperationId,
    },
    #[error(
        "operation {} is terminal in its {} state; a {} transition was attempted",
        .operation_id.as_str(),
        operation_kind_name(.current.kind()),
        operation_kind_name(.attempted.kind())
    )]
    TerminalState {
        operation_id: OperationId,
        current: Box<ProjectionOperationState>,
        attempted: Box<ProjectionOperationState>,
    },
    #[error(
        "operation {} cannot transition from its {} state to the attempted {} state",
        .operation_id.as_str(),
        operation_kind_name(.current.kind()),
        operation_kind_name(.attempted.kind())
    )]
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
    MachineLifecycle(MachineLifecycleOperationState),
}

impl ProjectionOperationState {
    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        match self {
            Self::Deploy(_) => OperationKind::Deploy,
            Self::Cert(_) => OperationKind::Cert,
            Self::MachineAdd(_) => OperationKind::MachineAdd,
            Self::MachineUpdate(_) => OperationKind::MachineUpdate,
            Self::MachineLifecycle(_) => OperationKind::MachineLifecycle,
        }
    }
}

pub(crate) const fn operation_kind_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Deploy => "deploy",
        OperationKind::Cert => "cert",
        OperationKind::MachineAdd => "machine-add",
        OperationKind::MachineUpdate => "machine-update",
        OperationKind::MachineLifecycle => "machine-lifecycle",
    }
}

fn subject_ref_text(subject: &OperationSubjectRef) -> String {
    match subject {
        OperationSubjectRef::Cert(cert_id) => format!("cert {}", cert_id.as_str()),
        OperationSubjectRef::MachineAdd(machine_id) => {
            format!("machine-add {}", machine_id.as_str())
        }
        OperationSubjectRef::MachineUpdate(machine_id) => {
            format!("machine-update {}", machine_id.as_str())
        }
        OperationSubjectRef::MachineLifecycle(machine_id) => {
            format!("machine-lifecycle {}", machine_id.as_str())
        }
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
            deploy::project_deploy_event(id, service_id, state, event, event_sequence)
        }
        ClassifiedOperationEvent::Cert { subject, event, .. } => {
            let OperationStatus::Cert {
                id, cert_id, state, ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::Cert));
            };
            cert::project_cert_event(id, cert_id, state, subject, event, event_sequence)
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
            machine_add::project_machine_add_event(fields, subject, event, event_sequence)
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
            let fields = MachineUpdateFields {
                id,
                machine_id,
                target_version,
                state,
            };
            machine_update::project_event(fields, subject, event, event_sequence)
        }
        ClassifiedOperationEvent::MachineLifecycle { subject, event, .. } => {
            let OperationStatus::MachineLifecycle {
                id,
                machine_id,
                target,
                state,
                ..
            } = current
            else {
                return Err(kind_mismatch(current, OperationKind::MachineLifecycle));
            };
            let fields = MachineLifecycleFields {
                id,
                machine_id,
                target: *target,
                state,
            };
            machine_lifecycle::project_event(fields, subject, event, event_sequence)
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
            } => deploy::project_deploy_event(
                id,
                service_id,
                state,
                DeployEvent::Transition(DeployTransition::Cancelled { reason }),
                event_sequence,
            ),
            OperationStatus::Cert {
                id, cert_id, state, ..
            } => cert::project_cert_state(
                id,
                cert_id,
                state,
                CertOperationState::Cancelled { reason },
                event_sequence,
            ),
            OperationStatus::MachineAdd {
                id,
                machine_id,
                name,
                roles,
                state,
                ..
            } => machine_add::project_machine_add_state(
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
                MachineUpdateFields {
                    id,
                    machine_id,
                    target_version,
                    state,
                },
                MachineUpdateOperationState::Cancelled { reason },
                event_sequence,
            ),
            OperationStatus::MachineLifecycle {
                id,
                machine_id,
                target,
                state,
                ..
            } => machine_lifecycle::project_state(
                MachineLifecycleFields {
                    id,
                    machine_id,
                    target: *target,
                    state,
                },
                MachineLifecycleOperationState::Cancelled { reason },
                event_sequence,
            ),
        },
    }
}
