//! The visible bounded operation that prepares machine-local ZFS storage.

use serde::{Deserialize, Serialize};

use crate::deploy::ZfsPoolName;
use crate::ids::{MachineId, OperationId};
use crate::storage::StorageEffectFailure;

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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineStoragePrepareOperationState {
    Accepted,
    Preparing,
    Completed {
        pool: ZfsPoolName,
    },
    Failed {
        failure: MachineStoragePrepareFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl MachineStoragePrepareOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed { .. }
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => true,
            Self::Accepted | Self::Preparing => false,
        }
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        let stage = match self {
            Self::Accepted => OperationInterruptionStage::MachineStoragePrepareAccepted,
            Self::Preparing => OperationInterruptionStage::MachineStoragePreparePreparing,
            Self::Completed { .. }
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => return None,
        };
        Some(OperationInterruptionEvidence::new(cause, stage))
    }

    pub(super) const fn interrupted(evidence: OperationInterruptionEvidence) -> Self {
        Self::Interrupted { evidence }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineStoragePrepareFailure {
    MachineUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    PreparationRejected {
        machine_id: MachineId,
        failure: StorageEffectFailure,
    },
    MachineSubstrateBusy {
        machine_id: MachineId,
        owner_operation_id: OperationId,
    },
    EvidenceUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    StateCommitFailed {
        machine_id: MachineId,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineStoragePrepareTransition {
    Preparing,
    Completed {
        pool: ZfsPoolName,
    },
    Failed {
        failure: MachineStoragePrepareFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
}

impl MachineStoragePrepareTransition {
    #[must_use]
    pub fn state(&self) -> MachineStoragePrepareOperationState {
        match self {
            Self::Preparing => MachineStoragePrepareOperationState::Preparing,
            Self::Completed { pool } => {
                MachineStoragePrepareOperationState::Completed { pool: pool.clone() }
            }
            Self::Failed { failure } => MachineStoragePrepareOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => MachineStoragePrepareOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }

    #[must_use]
    pub fn event(&self, operation_id: &OperationId, machine_id: &MachineId) -> OperationEvent {
        match self {
            Self::Preparing => OperationEvent::MachineStoragePreparePreparing {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
            Self::Completed { pool } => OperationEvent::MachineStoragePrepareCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                pool: pool.clone(),
            },
            Self::Failed { failure } => OperationEvent::MachineStoragePrepareFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::MachineStoragePrepare,
                reason: reason.clone(),
            },
        }
    }
}

pub(super) enum MachineStoragePrepareEvent {
    Submitted {
        machine_id: MachineId,
    },
    Transition {
        machine_id: MachineId,
        transition: MachineStoragePrepareTransition,
    },
    Cancelled(CancellationReason),
}

pub(super) fn project_event(
    id: &OperationId,
    machine_id: &MachineId,
    requested_pool: &Option<ZfsPoolName>,
    state: &MachineStoragePrepareOperationState,
    event: MachineStoragePrepareEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        MachineStoragePrepareEvent::Submitted {
            machine_id: event_machine_id,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineStoragePrepare,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        MachineStoragePrepareEvent::Transition {
            machine_id: event_machine_id,
            transition,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineStoragePrepare,
            )?;
            project_state(
                id,
                machine_id,
                requested_pool,
                state,
                transition.state(),
                event_sequence,
            )
        }
        MachineStoragePrepareEvent::Cancelled(reason) => project_state(
            id,
            machine_id,
            requested_pool,
            state,
            MachineStoragePrepareOperationState::Cancelled { reason },
            event_sequence,
        ),
    }
}

fn project_state(
    id: &OperationId,
    machine_id: &MachineId,
    requested_pool: &Option<ZfsPoolName>,
    state: &MachineStoragePrepareOperationState,
    attempted: MachineStoragePrepareOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        state,
        attempted,
        MachineStoragePrepareOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::MachineStoragePrepare,
        |state| OperationStatus::MachineStoragePrepare {
            id: id.clone(),
            machine_id: machine_id.clone(),
            requested_pool: requested_pool.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

fn transition_allowed(
    current: &MachineStoragePrepareOperationState,
    attempted: &MachineStoragePrepareOperationState,
) -> bool {
    matches!(
        (current, attempted),
        (
            MachineStoragePrepareOperationState::Accepted,
            MachineStoragePrepareOperationState::Preparing
                | MachineStoragePrepareOperationState::Cancelled { .. }
        ) | (
            MachineStoragePrepareOperationState::Preparing,
            MachineStoragePrepareOperationState::Completed { .. }
                | MachineStoragePrepareOperationState::Failed { .. }
                | MachineStoragePrepareOperationState::Cancelled { .. }
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::project_operation_event;

    fn operation_id() -> OperationId {
        OperationId::try_new("op_storage_prepare").unwrap()
    }

    fn machine_id() -> MachineId {
        MachineId::try_new("machine-a").unwrap()
    }

    #[test]
    fn preparation_projects_only_the_accepted_preparing_terminal_path() {
        let mut status = OperationStatus::machine_storage_prepare_accepted(
            operation_id(),
            machine_id(),
            None,
            EventSequence::first(),
        );
        for (sequence, event) in [
            (
                2,
                OperationEvent::MachineStoragePreparePreparing {
                    operation_id: operation_id(),
                    machine_id: machine_id(),
                },
            ),
            (
                3,
                OperationEvent::MachineStoragePrepareCompleted {
                    operation_id: operation_id(),
                    machine_id: machine_id(),
                    pool: ZfsPoolName::try_new("tank").unwrap(),
                },
            ),
        ] {
            let projected =
                project_operation_event(&status, event, EventSequence::try_new(sequence).unwrap())
                    .unwrap();
            let OperationProjection::StatusChanged { status: next } = projected else {
                panic!("transition must change status")
            };
            status = *next;
        }
        assert!(matches!(
            status,
            OperationStatus::MachineStoragePrepare {
                state: MachineStoragePrepareOperationState::Completed { .. },
                ..
            }
        ));
        assert!(matches!(
            project_operation_event(
                &status,
                OperationEvent::MachineStoragePrepareFailed {
                    operation_id: operation_id(),
                    machine_id: machine_id(),
                    failure: MachineStoragePrepareFailure::EvidenceUnavailable {
                        machine_id: machine_id(),
                        message: FailureMessage::try_new("late failure").unwrap(),
                    },
                },
                EventSequence::try_new(4).unwrap(),
            ),
            Err(StatusProjectionError::TerminalState { .. })
        ));
    }

    #[test]
    fn completion_before_preparing_is_rejected() {
        let status = OperationStatus::machine_storage_prepare_accepted(
            operation_id(),
            machine_id(),
            None,
            EventSequence::first(),
        );
        assert!(matches!(
            project_operation_event(
                &status,
                OperationEvent::MachineStoragePrepareCompleted {
                    operation_id: operation_id(),
                    machine_id: machine_id(),
                    pool: ZfsPoolName::try_new("tank").unwrap(),
                },
                EventSequence::try_new(2).unwrap(),
            ),
            Err(StatusProjectionError::InvalidTransition { .. })
        ));
    }
}
