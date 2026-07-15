//! The Machine Update operation: substrate binaries on one machine move to
//! a target version. States, failures, transitions, and status projection
//! live together here.

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, OperationId};
use crate::install::InstallArtifactVersion;

use super::events::{OperationEvent, OperationSubjectRef};
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
    verify_subject,
};
use super::text::{CancellationReason, FailureMessage};
use super::{
    EventSequence, OperationInterruptionCause, OperationInterruptionEvidence,
    OperationInterruptionStage, OperationKind, OperationStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineUpdateOperationState {
    Accepted,
    Running,
    Completed {
        reported: MachineSubstrateVersions,
    },
    Failed {
        failure: MachineUpdateFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl MachineUpdateOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed { .. }
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => true,
            Self::Accepted | Self::Running => false,
        }
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        let last_durable_stage = match self {
            Self::Accepted => OperationInterruptionStage::MachineUpdateAccepted,
            Self::Running => OperationInterruptionStage::MachineUpdateRunning,
            Self::Completed { .. }
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => return None,
        };
        Some(OperationInterruptionEvidence::new(
            cause,
            last_durable_stage,
        ))
    }

    pub(super) const fn terminal_interruption_evidence(
        &self,
    ) -> Option<&OperationInterruptionEvidence> {
        let Self::Interrupted { evidence } = self else {
            return None;
        };
        Some(evidence)
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
    pub host_runner: Option<InstallArtifactVersion>,
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
/// [`OperationEvent`] stream shape. Subject-bearing variants carry the
/// machine id the event claims, verified against the status record; a
/// cancel names no subject.
pub(super) enum MachineUpdateEvent {
    Submitted {
        machine_id: MachineId,
    },
    Transition {
        machine_id: MachineId,
        transition: MachineUpdateTransition,
    },
    Cancelled(CancellationReason),
    Interrupted(OperationInterruptionEvidence),
}

pub(super) fn project_event(
    id: &OperationId,
    machine_id: &MachineId,
    target_version: &InstallArtifactVersion,
    state: &MachineUpdateOperationState,
    event: MachineUpdateEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        MachineUpdateEvent::Submitted {
            machine_id: event_machine_id,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineUpdate,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        MachineUpdateEvent::Transition {
            machine_id: event_machine_id,
            transition,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineUpdate,
            )?;
            project_state(
                id,
                machine_id,
                target_version,
                state,
                transition.state(),
                event_sequence,
            )
        }
        MachineUpdateEvent::Cancelled(reason) => project_state(
            id,
            machine_id,
            target_version,
            state,
            MachineUpdateOperationState::Cancelled { reason },
            event_sequence,
        ),
        MachineUpdateEvent::Interrupted(evidence) => project_state(
            id,
            machine_id,
            target_version,
            state,
            MachineUpdateOperationState::Interrupted { evidence },
            event_sequence,
        ),
    }
}

pub(super) fn project_state(
    id: &OperationId,
    machine_id: &MachineId,
    target_version: &InstallArtifactVersion,
    state: &MachineUpdateOperationState,
    attempted: MachineUpdateOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        state,
        attempted,
        MachineUpdateOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::MachineUpdate,
        |state| OperationStatus::MachineUpdate {
            id: id.clone(),
            machine_id: machine_id.clone(),
            target_version: target_version.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

fn transition_allowed(
    current: &MachineUpdateOperationState,
    attempted: &MachineUpdateOperationState,
) -> bool {
    match (current, attempted) {
        (
            MachineUpdateOperationState::Accepted,
            MachineUpdateOperationState::Running
            | MachineUpdateOperationState::Cancelled { .. }
            | MachineUpdateOperationState::Interrupted { .. },
        )
        | (
            MachineUpdateOperationState::Running,
            MachineUpdateOperationState::Completed { .. }
            | MachineUpdateOperationState::Failed { .. }
            | MachineUpdateOperationState::Cancelled { .. }
            | MachineUpdateOperationState::Interrupted { .. },
        ) => true,
        (
            MachineUpdateOperationState::Accepted
            | MachineUpdateOperationState::Running
            | MachineUpdateOperationState::Completed { .. }
            | MachineUpdateOperationState::Failed { .. }
            | MachineUpdateOperationState::Cancelled { .. }
            | MachineUpdateOperationState::Interrupted { .. },
            MachineUpdateOperationState::Accepted
            | MachineUpdateOperationState::Running
            | MachineUpdateOperationState::Completed { .. }
            | MachineUpdateOperationState::Failed { .. }
            | MachineUpdateOperationState::Cancelled { .. }
            | MachineUpdateOperationState::Interrupted { .. },
        ) => false,
    }
}
