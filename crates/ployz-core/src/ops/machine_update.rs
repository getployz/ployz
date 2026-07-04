//! The Machine Update operation: substrate binaries on one machine move to
//! a target version. States, failures, transitions, and status projection
//! live together here.

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, OperationId};
use crate::install::InstallArtifactVersion;

use super::events::{OperationEvent, OperationSubjectRef};
use super::projection::{OperationProjection, ProjectionOperationState, StatusProjectionError};
use super::text::{CancellationReason, FailureMessage};
use super::{EventSequence, OperationKind, OperationStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUpdateOperationState {
    Accepted,
    Running,
    Completed { reported: MachineSubstrateVersions },
    Failed { failure: MachineUpdateFailure },
    Cancelled { reason: CancellationReason },
}

impl MachineUpdateOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted | Self::Running => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUpdateFailure {
    MachineUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    UpdateRejected {
        machine_id: MachineId,
        message: FailureMessage,
    },
    VersionNotReported {
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
        reported: MachineSubstrateVersions,
    },
    StateCommitFailed {
        machine_id: MachineId,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineSubstrateVersions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ployzd: Option<InstallArtifactVersion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keeper: Option<InstallArtifactVersion>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineUpdateTransition {
    Running,
    Completed { reported: MachineSubstrateVersions },
    Failed { failure: MachineUpdateFailure },
    Cancelled { reason: CancellationReason },
}

impl MachineUpdateTransition {
    #[must_use]
    pub fn state(&self) -> MachineUpdateOperationState {
        match self {
            Self::Running => MachineUpdateOperationState::Running,
            Self::Completed { reported } => MachineUpdateOperationState::Completed {
                reported: reported.clone(),
            },
            Self::Failed { failure } => MachineUpdateOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => MachineUpdateOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }

    /// Renders this transition as the operation event it records.
    #[must_use]
    pub fn event(&self, operation_id: &OperationId, machine_id: &MachineId) -> OperationEvent {
        match self {
            Self::Running => OperationEvent::MachineUpdateRunning {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
            Self::Completed { reported } => OperationEvent::MachineUpdateCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                reported: reported.clone(),
            },
            Self::Failed { failure } => OperationEvent::MachineUpdateFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::MachineUpdate,
                reason: reason.clone(),
            },
        }
    }
}

/// Machine-update events after classification from the flat
/// [`OperationEvent`] stream shape.
pub(super) enum MachineUpdateEvent {
    Submitted,
    Transition(MachineUpdateTransition),
}

/// The destructured fields of [`OperationStatus::MachineUpdate`].
#[derive(Clone, Copy)]
pub(super) struct MachineUpdateFields<'status> {
    pub(super) id: &'status OperationId,
    pub(super) machine_id: &'status MachineId,
    pub(super) target_version: &'status InstallArtifactVersion,
    pub(super) state: &'status MachineUpdateOperationState,
}

impl MachineUpdateFields<'_> {
    fn status_with(
        &self,
        state: MachineUpdateOperationState,
        event_sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::MachineUpdate {
            id: self.id.clone(),
            machine_id: self.machine_id.clone(),
            target_version: self.target_version.clone(),
            state,
            last_event_sequence: event_sequence,
        }
    }
}

pub(super) fn project_event(
    fields: MachineUpdateFields<'_>,
    subject: OperationSubjectRef,
    event: MachineUpdateEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if subject != OperationSubjectRef::MachineUpdate(fields.machine_id.clone()) {
        return Err(StatusProjectionError::OperationSubjectMismatch {
            operation_id: fields.id.clone(),
            expected: OperationSubjectRef::MachineUpdate(fields.machine_id.clone()),
            actual: subject,
        });
    }
    match event {
        MachineUpdateEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
        MachineUpdateEvent::Transition(transition) => {
            project_state(fields, transition.state(), event_sequence)
        }
    }
}

pub(super) fn project_state(
    fields: MachineUpdateFields<'_>,
    attempted: MachineUpdateOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if fields.state == &attempted {
        return Ok(OperationProjection::AlreadySatisfied);
    }
    if fields.state.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineUpdate(
                fields.state.clone(),
            )),
            attempted: Box::new(ProjectionOperationState::MachineUpdate(attempted)),
        });
    }
    if !transition_allowed(fields.state, &attempted) {
        return Err(StatusProjectionError::InvalidTransition {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineUpdate(
                fields.state.clone(),
            )),
            attempted: Box::new(ProjectionOperationState::MachineUpdate(attempted)),
        });
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(fields.status_with(attempted, event_sequence)),
    })
}

fn transition_allowed(
    current: &MachineUpdateOperationState,
    attempted: &MachineUpdateOperationState,
) -> bool {
    match (current, attempted) {
        (
            MachineUpdateOperationState::Accepted,
            MachineUpdateOperationState::Running | MachineUpdateOperationState::Cancelled { .. },
        )
        | (
            MachineUpdateOperationState::Running,
            MachineUpdateOperationState::Completed { .. }
            | MachineUpdateOperationState::Failed { .. }
            | MachineUpdateOperationState::Cancelled { .. },
        ) => true,
        (
            MachineUpdateOperationState::Accepted
            | MachineUpdateOperationState::Running
            | MachineUpdateOperationState::Completed { .. }
            | MachineUpdateOperationState::Failed { .. }
            | MachineUpdateOperationState::Cancelled { .. },
            MachineUpdateOperationState::Accepted
            | MachineUpdateOperationState::Running
            | MachineUpdateOperationState::Completed { .. }
            | MachineUpdateOperationState::Failed { .. }
            | MachineUpdateOperationState::Cancelled { .. },
        ) => false,
    }
}
