//! Managed DNS lease acquire and renewal operation state.

use serde::{Deserialize, Serialize};

use crate::cert::{ManagedLeaseAddressSet, ManagedLeaseName};
use crate::ids::OperationId;

use super::events::{OperationEvent, OperationSubjectRef};
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
    verify_subject,
};
use super::{EventSequence, FailureMessage, OperationStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedLeaseSubject {
    Acquire,
    DownloadBundle {
        lease: ManagedLeaseName,
    },
    Renew {
        lease: ManagedLeaseName,
        addresses: ManagedLeaseAddressSet,
    },
}

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
    pub class: ManagedLeaseFailureClass,
    pub message: FailureMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ManagedLeaseFailureClass {
    WorkerUnauthorized,
    LeaseNotFound,
    WorkerHttp,
    Transport,
    Decode,
    Superseded,
    Storage,
    Interrupted,
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
        subject: &ManagedLeaseSubject,
    ) -> OperationEvent {
        match self {
            Self::Completed => OperationEvent::ManagedLeaseCompleted {
                operation_id: operation_id.clone(),
                subject: subject.clone(),
            },
            Self::Failed { failure } => OperationEvent::ManagedLeaseFailed {
                operation_id: operation_id.clone(),
                subject: subject.clone(),
                failure: failure.clone(),
            },
        }
    }
}

pub(super) enum ManagedLeaseEvent {
    Submitted {
        subject: ManagedLeaseSubject,
    },
    Transition {
        subject: ManagedLeaseSubject,
        transition: ManagedLeaseTransition,
    },
    UnsupportedCancellation,
}

pub(super) fn project_event(
    id: &OperationId,
    subject: &ManagedLeaseSubject,
    state: &ManagedLeaseOperationState,
    event: ManagedLeaseEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        ManagedLeaseEvent::Submitted {
            subject: event_subject,
        } => {
            verify_subject(
                id,
                subject,
                &event_subject,
                OperationSubjectRef::ManagedLease,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        ManagedLeaseEvent::Transition {
            subject: event_subject,
            transition,
        } => {
            verify_subject(
                id,
                subject,
                &event_subject,
                OperationSubjectRef::ManagedLease,
            )?;
            project_state(id, subject, state, transition.state(), event_sequence)
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
    subject: &ManagedLeaseSubject,
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
            subject: subject.clone(),
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

    fn subject() -> ManagedLeaseSubject {
        ManagedLeaseSubject::Renew {
            lease: ManagedLeaseName::try_new("cluster-one").expect("valid lease name"),
            addresses: ManagedLeaseAddressSet::new(
                vec!["203.0.113.8".parse().expect("IPv4")],
                vec!["2001:db8::8".parse().expect("IPv6")],
            ),
        }
    }

    #[test]
    fn renew_subject_records_requested_addresses() {
        let value = serde_json::to_value(subject()).expect("renew subject serializes");

        assert_eq!(
            value,
            serde_json::json!({
                "kind": "renew",
                "lease": "cluster-one",
                "addresses": {
                    "ipv4": ["203.0.113.8"],
                    "ipv6": ["2001:db8::8"]
                }
            })
        );
    }

    #[test]
    fn accepted_can_complete() {
        let projection = project_state(
            &operation_id(),
            &subject(),
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
            &subject(),
            &ManagedLeaseOperationState::Accepted,
            ManagedLeaseOperationState::Failed {
                failure: ManagedLeaseOperationFailure {
                    class: ManagedLeaseFailureClass::Transport,
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
            &subject(),
            &ManagedLeaseOperationState::Completed,
            ManagedLeaseOperationState::Failed {
                failure: ManagedLeaseOperationFailure {
                    class: ManagedLeaseFailureClass::Transport,
                    message: FailureMessage::try_new("too late").expect("non-empty message"),
                },
            },
            EventSequence::try_new(3).expect("positive sequence"),
        )
        .expect_err("terminal transition must fail");

        assert!(matches!(error, StatusProjectionError::TerminalState { .. }));
    }
}
