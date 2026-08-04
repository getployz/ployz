//! The visible bounded operation that clears one machine's disposable build cache.

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, OperationId};

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
#[serde(deny_unknown_fields)]
pub struct BuildCachePruneEvidence {
    pub before_available_bytes: u64,
    pub reclaimed_bytes: u64,
    pub after_available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineBuildCachePruneOperationState {
    Accepted,
    Pruning,
    Completed {
        evidence: BuildCachePruneEvidence,
    },
    Failed {
        failure: MachineBuildCachePruneFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl MachineBuildCachePruneOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed { .. }
                | Self::Failed { .. }
                | Self::Cancelled { .. }
                | Self::Interrupted { .. }
        )
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        let stage = match self {
            Self::Accepted => OperationInterruptionStage::MachineBuildCachePruneAccepted,
            Self::Pruning => OperationInterruptionStage::MachineBuildCachePrunePruning,
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
pub enum MachineBuildCachePruneFailure {
    MachineUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    PruneRejected {
        machine_id: MachineId,
        message: FailureMessage,
    },
    StateCommitFailed {
        machine_id: MachineId,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineBuildCachePruneTransition {
    Pruning,
    Completed {
        evidence: BuildCachePruneEvidence,
    },
    Failed {
        failure: MachineBuildCachePruneFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
}

impl MachineBuildCachePruneTransition {
    #[must_use]
    pub fn state(&self) -> MachineBuildCachePruneOperationState {
        match self {
            Self::Pruning => MachineBuildCachePruneOperationState::Pruning,
            Self::Completed { evidence } => MachineBuildCachePruneOperationState::Completed {
                evidence: evidence.clone(),
            },
            Self::Failed { failure } => MachineBuildCachePruneOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => MachineBuildCachePruneOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }

    #[must_use]
    pub fn event(&self, operation_id: &OperationId, machine_id: &MachineId) -> OperationEvent {
        match self {
            Self::Pruning => OperationEvent::MachineBuildCachePrunePruning {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            },
            Self::Completed { evidence } => OperationEvent::MachineBuildCachePruneCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                evidence: evidence.clone(),
            },
            Self::Failed { failure } => OperationEvent::MachineBuildCachePruneFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::MachineBuildCachePrune,
                reason: reason.clone(),
            },
        }
    }
}

pub(super) enum MachineBuildCachePruneEvent {
    Submitted {
        machine_id: MachineId,
    },
    Transition {
        machine_id: MachineId,
        transition: MachineBuildCachePruneTransition,
    },
    Cancelled(CancellationReason),
}

pub(super) fn project_event(
    id: &OperationId,
    machine_id: &MachineId,
    state: &MachineBuildCachePruneOperationState,
    event: MachineBuildCachePruneEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        MachineBuildCachePruneEvent::Submitted {
            machine_id: event_machine_id,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineBuildCachePrune,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        MachineBuildCachePruneEvent::Transition {
            machine_id: event_machine_id,
            transition,
        } => {
            verify_subject(
                id,
                machine_id,
                &event_machine_id,
                OperationSubjectRef::MachineBuildCachePrune,
            )?;
            project_state(id, machine_id, state, transition.state(), event_sequence)
        }
        MachineBuildCachePruneEvent::Cancelled(reason) => project_state(
            id,
            machine_id,
            state,
            MachineBuildCachePruneOperationState::Cancelled { reason },
            event_sequence,
        ),
    }
}

fn project_state(
    id: &OperationId,
    machine_id: &MachineId,
    state: &MachineBuildCachePruneOperationState,
    attempted: MachineBuildCachePruneOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        state,
        attempted,
        MachineBuildCachePruneOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::MachineBuildCachePrune,
        |state| OperationStatus::MachineBuildCachePrune {
            id: id.clone(),
            machine_id: machine_id.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

fn transition_allowed(
    current: &MachineBuildCachePruneOperationState,
    attempted: &MachineBuildCachePruneOperationState,
) -> bool {
    matches!(
        (current, attempted),
        (
            MachineBuildCachePruneOperationState::Accepted,
            MachineBuildCachePruneOperationState::Pruning
                | MachineBuildCachePruneOperationState::Failed { .. }
                | MachineBuildCachePruneOperationState::Cancelled { .. }
        ) | (
            MachineBuildCachePruneOperationState::Pruning,
            MachineBuildCachePruneOperationState::Completed { .. }
                | MachineBuildCachePruneOperationState::Failed { .. }
                | MachineBuildCachePruneOperationState::Cancelled { .. }
        )
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::project_operation_event;

    #[test]
    fn prune_projects_accepted_pruning_completed() {
        let operation_id = OperationId::try_new("op_prune").unwrap();
        let machine_id = MachineId::try_new("machine-a").unwrap();
        let mut status = OperationStatus::machine_build_cache_prune_accepted(
            operation_id.clone(),
            machine_id.clone(),
            EventSequence::first(),
        );
        for (sequence, event) in [
            (
                2,
                OperationEvent::MachineBuildCachePrunePruning {
                    operation_id: operation_id.clone(),
                    machine_id: machine_id.clone(),
                },
            ),
            (
                3,
                OperationEvent::MachineBuildCachePruneCompleted {
                    operation_id: operation_id.clone(),
                    machine_id: machine_id.clone(),
                    evidence: BuildCachePruneEvidence {
                        before_available_bytes: 10,
                        reclaimed_bytes: 5,
                        after_available_bytes: 15,
                    },
                },
            ),
        ] {
            let OperationProjection::StatusChanged { status: next } =
                project_operation_event(&status, event, EventSequence::try_new(sequence).unwrap())
                    .unwrap()
            else {
                panic!("transition must change status")
            };
            status = *next;
        }
        assert!(matches!(
            status,
            OperationStatus::MachineBuildCachePrune {
                state: MachineBuildCachePruneOperationState::Completed { .. },
                ..
            }
        ));
    }

    #[test]
    fn initial_evidence_failure_can_fail_from_accepted() {
        let operation_id = OperationId::try_new("op_prune").unwrap();
        let machine_id = MachineId::try_new("machine-a").unwrap();
        let status = OperationStatus::machine_build_cache_prune_accepted(
            operation_id.clone(),
            machine_id.clone(),
            EventSequence::first(),
        );
        let projected = project_operation_event(
            &status,
            OperationEvent::MachineBuildCachePruneFailed {
                operation_id,
                machine_id: machine_id.clone(),
                failure: MachineBuildCachePruneFailure::StateCommitFailed {
                    machine_id,
                    message: FailureMessage::try_new("initial transition failed").unwrap(),
                },
            },
            EventSequence::try_new(2).unwrap(),
        )
        .unwrap();

        assert!(matches!(
            projected,
            OperationProjection::StatusChanged {
                status
            } if matches!(
                *status,
                OperationStatus::MachineBuildCachePrune {
                    state: MachineBuildCachePruneOperationState::Failed { .. },
                    ..
                }
            )
        ));
    }
}
