//! Core replacement operation: the current core records an explicit
//! operator-owned demotion of a former core.

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, OperationId};
use crate::install::MachineJoinRuntimeNatsUrl;

use super::events::{OperationEvent, OperationSubjectRef};
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
    verify_subject,
};
use super::text::{CancellationReason, FailureMessage};
use super::{EventSequence, OperationStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CoreReplaceOperationState {
    Accepted,
    Completed,
    Failed { failure: CoreReplaceFailure },
    Cancelled { reason: CancellationReason },
}

impl CoreReplaceOperationState {
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
pub enum CoreReplaceFailure {
    DemoteFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreReplaceTransition {
    Completed,
    Failed { failure: CoreReplaceFailure },
}

impl CoreReplaceTransition {
    #[must_use]
    pub fn state(&self) -> CoreReplaceOperationState {
        match self {
            Self::Completed => CoreReplaceOperationState::Completed,
            Self::Failed { failure } => CoreReplaceOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }

    #[must_use]
    pub fn event(&self, operation_id: &OperationId, machine_id: &MachineId) -> OperationEvent {
        match self {
            Self::Completed => OperationEvent::CoreReplaceCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::CoreReplaceFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure: failure.clone(),
            },
        }
    }
}

pub(super) enum CoreReplaceEvent {
    Submitted {
        machine_id: MachineId,
    },
    Transition {
        machine_id: MachineId,
        transition: CoreReplaceTransition,
    },
    Cancelled(CancellationReason),
}

pub(super) fn project_event(
    id: &OperationId,
    machine_id: &MachineId,
    successor_nats_url: &MachineJoinRuntimeNatsUrl,
    state: &CoreReplaceOperationState,
    event: CoreReplaceEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        CoreReplaceEvent::Submitted {
            machine_id: event_machine_id,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::CoreReplace,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        CoreReplaceEvent::Transition {
            machine_id: event_machine_id,
            transition,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::CoreReplace,
            )?;
            project_state(
                id,
                machine_id,
                successor_nats_url,
                state,
                transition.state(),
                event_sequence,
            )
        }
        CoreReplaceEvent::Cancelled(reason) => project_state(
            id,
            machine_id,
            successor_nats_url,
            state,
            CoreReplaceOperationState::Cancelled { reason },
            event_sequence,
        ),
    }
}

fn project_state(
    id: &OperationId,
    machine_id: &MachineId,
    successor_nats_url: &MachineJoinRuntimeNatsUrl,
    state: &CoreReplaceOperationState,
    attempted: CoreReplaceOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        state,
        attempted,
        CoreReplaceOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::CoreReplace,
        |state| OperationStatus::CoreReplace {
            id: id.clone(),
            machine_id: machine_id.clone(),
            successor_nats_url: successor_nats_url.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

fn transition_allowed(
    current: &CoreReplaceOperationState,
    attempted: &CoreReplaceOperationState,
) -> bool {
    match (current, attempted) {
        (
            CoreReplaceOperationState::Accepted,
            CoreReplaceOperationState::Completed
            | CoreReplaceOperationState::Failed { .. }
            | CoreReplaceOperationState::Cancelled { .. },
        ) => true,
        (CoreReplaceOperationState::Accepted, CoreReplaceOperationState::Accepted)
        | (
            CoreReplaceOperationState::Completed
            | CoreReplaceOperationState::Failed { .. }
            | CoreReplaceOperationState::Cancelled { .. },
            _,
        ) => false,
    }
}
