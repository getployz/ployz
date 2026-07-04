//! The Machine Lifecycle operation: drain and resume commit durable
//! operator intent onto the machine record. States, failures, transitions,
//! and status projection live together here.

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, OperationId};
use crate::state::MachineLifecycle;

use super::events::{OperationEvent, OperationSubjectRef};
use super::projection::{OperationProjection, ProjectionOperationState, StatusProjectionError};
use super::text::{CancellationReason, FailureMessage};
use super::{EventSequence, OperationStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineLifecycleOperationState {
    Accepted,
    Completed,
    Failed { failure: MachineLifecycleFailure },
    Cancelled { reason: CancellationReason },
}

impl MachineLifecycleOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineLifecycleFailure {
    NoSuchMachine { machine_id: MachineId },
    EvidenceWriteFailed { message: FailureMessage },
    StateCommitFailed { message: FailureMessage },
}

/// Worker-recorded terminal outcomes. Cancellation is not one: it arrives
/// through the generic [`OperationEvent::Cancelled`] path, never from the
/// lifecycle worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineLifecycleTransition {
    Completed,
    Failed { failure: MachineLifecycleFailure },
}

impl MachineLifecycleTransition {
    #[must_use]
    pub fn state(&self) -> MachineLifecycleOperationState {
        match self {
            Self::Completed => MachineLifecycleOperationState::Completed,
            Self::Failed { failure } => MachineLifecycleOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }

    /// Renders this transition as the operation event it records.
    #[must_use]
    pub fn event(&self, operation_id: &OperationId, machine_id: &MachineId) -> OperationEvent {
        match self {
            Self::Completed => OperationEvent::MachineLifecycleCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::MachineLifecycleFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure: failure.clone(),
            },
        }
    }
}

/// Machine-lifecycle events after classification from the flat
/// [`OperationEvent`] stream shape.
pub(super) enum MachineLifecycleEvent {
    Submitted,
    Transition(MachineLifecycleTransition),
}

/// The destructured fields of [`OperationStatus::MachineLifecycle`].
#[derive(Clone, Copy)]
pub(super) struct MachineLifecycleFields<'status> {
    pub(super) id: &'status OperationId,
    pub(super) machine_id: &'status MachineId,
    pub(super) target: MachineLifecycle,
    pub(super) state: &'status MachineLifecycleOperationState,
}

impl MachineLifecycleFields<'_> {
    fn status_with(
        &self,
        state: MachineLifecycleOperationState,
        event_sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::MachineLifecycle {
            id: self.id.clone(),
            machine_id: self.machine_id.clone(),
            target: self.target,
            state,
            last_event_sequence: event_sequence,
        }
    }
}

pub(super) fn project_event(
    fields: MachineLifecycleFields<'_>,
    subject: OperationSubjectRef,
    event: MachineLifecycleEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if subject != OperationSubjectRef::MachineLifecycle(fields.machine_id.clone()) {
        return Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id: fields.id.clone(),
            expected: OperationSubjectRef::MachineLifecycle(fields.machine_id.clone()),
            actual: subject,
        });
    }
    match event {
        MachineLifecycleEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
        MachineLifecycleEvent::Transition(transition) => {
            project_state(fields, transition.state(), event_sequence)
        }
    }
}

pub(super) fn project_state(
    fields: MachineLifecycleFields<'_>,
    attempted: MachineLifecycleOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if fields.state == &attempted {
        return Ok(OperationProjection::AlreadySatisfied);
    }
    if fields.state.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineLifecycle(
                fields.state.clone(),
            )),
            attempted: Box::new(ProjectionOperationState::MachineLifecycle(attempted)),
        });
    }
    if !transition_allowed(fields.state, &attempted) {
        return Err(StatusProjectionError::InvalidTransition {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineLifecycle(
                fields.state.clone(),
            )),
            attempted: Box::new(ProjectionOperationState::MachineLifecycle(attempted)),
        });
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(fields.status_with(attempted, event_sequence)),
    })
}

fn transition_allowed(
    current: &MachineLifecycleOperationState,
    attempted: &MachineLifecycleOperationState,
) -> bool {
    match (current, attempted) {
        (
            MachineLifecycleOperationState::Accepted,
            MachineLifecycleOperationState::Completed
            | MachineLifecycleOperationState::Failed { .. }
            | MachineLifecycleOperationState::Cancelled { .. },
        ) => true,
        (MachineLifecycleOperationState::Accepted, MachineLifecycleOperationState::Accepted)
        | (
            MachineLifecycleOperationState::Completed
            | MachineLifecycleOperationState::Failed { .. }
            | MachineLifecycleOperationState::Cancelled { .. },
            _,
        ) => false,
    }
}
