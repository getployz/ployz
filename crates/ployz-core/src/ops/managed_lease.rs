//! Managed DNS lease acquire and renewal operation state.

use serde::{Deserialize, Serialize};

use crate::cert::ManagedLeaseName;
use crate::ids::OperationId;

use super::events::{OperationEvent, OperationSubjectRef};
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
    verify_subject,
};
use super::{EventSequence, FailureMessage, OperationStatus};

pub const MANAGED_LEASE_ACQUISITION_SUBJECT: &str = "acquire";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedLeaseOperationState {
    Accepted,
    Completed,
    Failed {
        failure: ManagedLeaseOperationFailure,
    },
}

impl ManagedLeaseOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } => true,
            Self::Accepted => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ManagedLeaseOperationFailure {
    pub message: FailureMessage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedLeaseTransition {
    Completed,
    Failed {
        failure: ManagedLeaseOperationFailure,
    },
}

impl ManagedLeaseTransition {
    #[must_use]
    pub fn state(&self) -> ManagedLeaseOperationState {
        match self {
            Self::Completed => ManagedLeaseOperationState::Completed,
            Self::Failed { failure } => ManagedLeaseOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }

    #[must_use]
    pub fn event(
        &self,
        operation_id: &OperationId,
        lease_name: &ManagedLeaseName,
    ) -> OperationEvent {
        match self {
            Self::Completed => OperationEvent::ManagedLeaseCompleted {
                operation_id: operation_id.clone(),
                lease_name: lease_name.clone(),
            },
            Self::Failed { failure } => OperationEvent::ManagedLeaseFailed {
                operation_id: operation_id.clone(),
                lease_name: lease_name.clone(),
                failure: failure.clone(),
            },
        }
    }
}

pub(super) enum ManagedLeaseEvent {
    Submitted {
        lease_name: ManagedLeaseName,
    },
    Transition {
        lease_name: ManagedLeaseName,
        transition: ManagedLeaseTransition,
    },
    UnsupportedCancellation,
}

pub(super) fn project_event(
    id: &OperationId,
    lease_name: &ManagedLeaseName,
    state: &ManagedLeaseOperationState,
    event: ManagedLeaseEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        ManagedLeaseEvent::Submitted {
            lease_name: event_lease_name,
        } => {
            verify_subject(
                id,
                lease_name,
                &event_lease_name,
                OperationSubjectRef::ManagedLease,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        ManagedLeaseEvent::Transition {
            lease_name: event_lease_name,
            transition,
        } => {
            verify_subject(
                id,
                lease_name,
                &event_lease_name,
                OperationSubjectRef::ManagedLease,
            )?;
            project_state(id, lease_name, state, transition.state(), event_sequence)
        }
        ManagedLeaseEvent::UnsupportedCancellation => {
            Err(StatusProjectionError::ManagedLeaseCancellationUnsupported {
                operation_id: id.clone(),
            })
        }
    }
}

fn project_state(
    id: &OperationId,
    lease_name: &ManagedLeaseName,
    state: &ManagedLeaseOperationState,
    attempted: ManagedLeaseOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        state,
        attempted,
        ManagedLeaseOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::ManagedLease,
        |state| OperationStatus::ManagedLease {
            id: id.clone(),
            lease_name: lease_name.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

fn transition_allowed(
    current: &ManagedLeaseOperationState,
    attempted: &ManagedLeaseOperationState,
) -> bool {
    match (current, attempted) {
        (
            ManagedLeaseOperationState::Accepted,
            ManagedLeaseOperationState::Completed | ManagedLeaseOperationState::Failed { .. },
        ) => true,
        (ManagedLeaseOperationState::Accepted, ManagedLeaseOperationState::Accepted)
        | (ManagedLeaseOperationState::Completed | ManagedLeaseOperationState::Failed { .. }, _) => {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation_id() -> OperationId {
        OperationId::try_new("op-managed-lease").expect("valid operation id")
    }

    fn lease_name() -> ManagedLeaseName {
        ManagedLeaseName::try_new("cluster-one").expect("valid lease name")
    }

    #[test]
    fn accepted_can_complete() {
        let projection = project_state(
            &operation_id(),
            &lease_name(),
            &ManagedLeaseOperationState::Accepted,
            ManagedLeaseOperationState::Completed,
            EventSequence::try_new(2).expect("positive sequence"),
        )
        .expect("valid transition");

        assert!(matches!(
            projection,
            OperationProjection::StatusChanged { .. }
        ));
    }

    #[test]
    fn accepted_can_fail() {
        let projection = project_state(
            &operation_id(),
            &lease_name(),
            &ManagedLeaseOperationState::Accepted,
            ManagedLeaseOperationState::Failed {
                failure: ManagedLeaseOperationFailure {
                    message: FailureMessage::try_new("worker unavailable")
                        .expect("non-empty message"),
                },
            },
            EventSequence::try_new(2).expect("positive sequence"),
        )
        .expect("valid transition");

        assert!(matches!(
            projection,
            OperationProjection::StatusChanged { .. }
        ));
    }

    #[test]
    fn terminal_state_rejects_another_transition() {
        let error = project_state(
            &operation_id(),
            &lease_name(),
            &ManagedLeaseOperationState::Completed,
            ManagedLeaseOperationState::Failed {
                failure: ManagedLeaseOperationFailure {
                    message: FailureMessage::try_new("too late").expect("non-empty message"),
                },
            },
            EventSequence::try_new(3).expect("positive sequence"),
        )
        .expect_err("terminal transition must fail");

        assert!(matches!(error, StatusProjectionError::TerminalState { .. }));
    }
}
