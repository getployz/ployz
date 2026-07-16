//! The Machine Lifecycle operation: drain and resume commit durable
//! operator intent onto the machine record. States, failures, transitions,
//! and status projection live together here.

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, OperationId};
use crate::machine::MachineLifecycle;

use super::events::{OperationEvent, OperationSubjectRef};
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
    verify_subject,
};
use super::text::{CancellationReason, FailureMessage};
use super::{
    EventSequence, OperationInterruptionCause, OperationInterruptionEvidence,
    OperationInterruptionStage, OperationStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineLifecycleOperationState {
    Accepted,
    Completed,
    Failed {
        failure: MachineLifecycleFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl MachineLifecycleOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => true,
            Self::Accepted => false,
        }
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        match self {
            Self::Accepted => Some(OperationInterruptionEvidence::new(
                cause,
                OperationInterruptionStage::MachineLifecycleAccepted,
            )),
            Self::Completed
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => None,
        }
    }

    pub(super) const fn interrupted(evidence: OperationInterruptionEvidence) -> Self {
        Self::Interrupted { evidence }
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
/// [`OperationEvent`] stream shape. Subject-bearing variants carry the
/// machine id the event claims, verified against the status record; a
/// cancel names no subject.
pub(super) enum MachineLifecycleEvent {
    Submitted {
        machine_id: MachineId,
    },
    Transition {
        machine_id: MachineId,
        transition: MachineLifecycleTransition,
    },
    Cancelled(CancellationReason),
}

pub(super) fn project_event(
    id: &OperationId,
    machine_id: &MachineId,
    target: MachineLifecycle,
    state: &MachineLifecycleOperationState,
    event: MachineLifecycleEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        MachineLifecycleEvent::Submitted {
            machine_id: event_machine_id,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineLifecycle,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        MachineLifecycleEvent::Transition {
            machine_id: event_machine_id,
            transition,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineLifecycle,
            )?;
            project_state(
                id,
                machine_id,
                target,
                state,
                transition.state(),
                event_sequence,
            )
        }
        MachineLifecycleEvent::Cancelled(reason) => project_state(
            id,
            machine_id,
            target,
            state,
            MachineLifecycleOperationState::Cancelled { reason },
            event_sequence,
        ),
    }
}

pub(super) fn project_state(
    id: &OperationId,
    machine_id: &MachineId,
    target: MachineLifecycle,
    state: &MachineLifecycleOperationState,
    attempted: MachineLifecycleOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        state,
        attempted,
        MachineLifecycleOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::MachineLifecycle,
        |state| OperationStatus::MachineLifecycle {
            id: id.clone(),
            machine_id: machine_id.clone(),
            target,
            state,
            last_event_sequence: event_sequence,
        },
    )
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
        (_, MachineLifecycleOperationState::Interrupted { .. })
        | (MachineLifecycleOperationState::Accepted, MachineLifecycleOperationState::Accepted)
        | (
            MachineLifecycleOperationState::Completed
            | MachineLifecycleOperationState::Failed { .. }
            | MachineLifecycleOperationState::Cancelled { .. }
            | MachineLifecycleOperationState::Interrupted { .. },
            _,
        ) => false,
    }
}
