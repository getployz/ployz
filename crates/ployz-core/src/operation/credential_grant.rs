//! Named client credential grant operation.

use serde::{Deserialize, Serialize};

use crate::ids::OperationId;
use crate::nats_config::{CredentialGrant, CredentialRole, NatsUserPublicKey};

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
};
use super::text::{CancellationReason, FailureMessage};
use super::{
    EventSequence, OperationInterruptionCause, OperationInterruptionEvidence,
    OperationInterruptionNextAction, OperationInterruptionStage,
    OperationInterruptionUncertainWork, OperationStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialGrantAction {
    Add { grant: CredentialGrant },
    Remove { public_key: NatsUserPublicKey },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CredentialGrantOperationState {
    Accepted,
    Completed,
    Failed {
        failure: CredentialGrantFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl CredentialGrantOperationState {
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
                OperationInterruptionStage::CredentialGrantAccepted,
                OperationInterruptionUncertainWork::Intent,
                OperationInterruptionNextAction::InspectThenResubmit,
            )),
            Self::Completed
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => None,
        }
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
pub enum CredentialGrantFailure {
    LastOperator,
    RoleChangeRequiresExplicitOperation {
        current: CredentialRole,
        requested: CredentialRole,
    },
    IntentStoreFailed {
        message: FailureMessage,
        intent_committed: bool,
    },
    AuthorizationRenderFailed {
        message: FailureMessage,
        intent_committed: bool,
    },
    NatsReloadFailed {
        message: FailureMessage,
        intent_committed: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialGrantTransition {
    Completed,
    Failed { failure: CredentialGrantFailure },
}

impl CredentialGrantTransition {
    #[must_use]
    pub fn state(&self) -> CredentialGrantOperationState {
        match self {
            Self::Completed => CredentialGrantOperationState::Completed,
            Self::Failed { failure } => CredentialGrantOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }

    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Completed => OperationEvent::CredentialGrantCompleted {
                operation_id: operation_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::CredentialGrantFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
        }
    }
}

pub(super) enum CredentialGrantEvent {
    Submitted { action: CredentialGrantAction },
    Transition(CredentialGrantTransition),
    Cancelled(CancellationReason),
    Interrupted(OperationInterruptionEvidence),
}

pub(super) fn project_event(
    id: &OperationId,
    action: &CredentialGrantAction,
    state: &CredentialGrantOperationState,
    event: CredentialGrantEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        CredentialGrantEvent::Submitted {
            action: event_action,
        } => {
            if action != &event_action {
                return Err(StatusProjectionError::CredentialGrantActionMismatch {
                    operation_id: id.clone(),
                });
            }
            Ok(OperationProjection::AlreadySatisfied)
        }
        CredentialGrantEvent::Transition(transition) => {
            project_state(id, action, state, transition.state(), event_sequence)
        }
        CredentialGrantEvent::Cancelled(reason) => project_state(
            id,
            action,
            state,
            CredentialGrantOperationState::Cancelled { reason },
            event_sequence,
        ),
        CredentialGrantEvent::Interrupted(evidence) => project_state(
            id,
            action,
            state,
            CredentialGrantOperationState::Interrupted { evidence },
            event_sequence,
        ),
    }
}

fn project_state(
    id: &OperationId,
    action: &CredentialGrantAction,
    state: &CredentialGrantOperationState,
    attempted: CredentialGrantOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        state,
        attempted,
        CredentialGrantOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::CredentialGrant,
        |state| OperationStatus::CredentialGrant {
            id: id.clone(),
            action: action.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

fn transition_allowed(
    current: &CredentialGrantOperationState,
    attempted: &CredentialGrantOperationState,
) -> bool {
    match (current, attempted) {
        (
            CredentialGrantOperationState::Accepted,
            CredentialGrantOperationState::Completed
            | CredentialGrantOperationState::Failed { .. }
            | CredentialGrantOperationState::Cancelled { .. }
            | CredentialGrantOperationState::Interrupted { .. },
        ) => true,
        (CredentialGrantOperationState::Accepted, CredentialGrantOperationState::Accepted)
        | (
            CredentialGrantOperationState::Completed
            | CredentialGrantOperationState::Failed { .. }
            | CredentialGrantOperationState::Cancelled { .. }
            | CredentialGrantOperationState::Interrupted { .. },
            _,
        ) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nats_config::{CredentialName, CredentialRole, NatsUserPublicKey};

    #[test]
    fn completed_credential_grant_is_terminal() {
        assert!(CredentialGrantOperationState::Completed.is_terminal());
    }

    #[test]
    fn terminal_credential_grant_rejects_later_failure() {
        let operation_id = OperationId::try_new("op_credential").expect("operation id");
        let action = add_action();
        let result = project_event(
            &operation_id,
            &action,
            &CredentialGrantOperationState::Completed,
            CredentialGrantEvent::Transition(CredentialGrantTransition::Failed {
                failure: CredentialGrantFailure::LastOperator,
            }),
            EventSequence::try_new(2).expect("event sequence"),
        );

        assert!(matches!(
            result,
            Err(StatusProjectionError::TerminalState { .. })
        ));
    }

    fn add_action() -> CredentialGrantAction {
        CredentialGrantAction::Add {
            grant: CredentialGrant {
                public_key: NatsUserPublicKey::try_new(nkeys::KeyPair::new_user().public_key())
                    .expect("user public key"),
                name: CredentialName::try_new("Ployz Cloud").expect("credential name"),
                role: CredentialRole::Operator,
            },
        }
    }
}
