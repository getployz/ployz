//! Managed DNS target reconcile operation state.

use serde::{Deserialize, Serialize};

use crate::certificate::ManagedLeaseName;
use crate::ids::OperationId;
use crate::ingress::IngressEndpointProjectionIdentity;

use super::events::{OperationEvent, OperationSubjectRef};
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
    verify_subject,
};
use super::{EventSequence, FailureMessage, OperationStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "authorization", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedDnsWithdrawAuthorization {
    ProjectionUnavailable {
        projection: IngressEndpointProjectionIdentity,
    },
    TargetDisabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedDnsReconcileSubject {
    Acquire,
    ProjectionApply {
        lease: ManagedLeaseName,
        projection: IngressEndpointProjectionIdentity,
    },
    AuthorizedWithdraw {
        lease: ManagedLeaseName,
        authorization: ManagedDnsWithdrawAuthorization,
    },
    LeaseRenewal {
        lease: ManagedLeaseName,
        projection: Option<IngressEndpointProjectionIdentity>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ManagedDnsReconcileOperationState {
    Accepted,
    Completed,
    Failed { failure: ManagedDnsReconcileFailure },
}

impl ManagedDnsReconcileOperationState {
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
pub struct ManagedDnsReconcileFailure {
    pub class: ManagedDnsReconcileFailureClass,
    pub message: FailureMessage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ManagedDnsReconcileFailureClass {
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
pub enum ManagedDnsReconcileTransition {
    Completed,
    Failed { failure: ManagedDnsReconcileFailure },
}

impl ManagedDnsReconcileTransition {
    #[must_use]
    pub fn state(&self) -> ManagedDnsReconcileOperationState {
        match self {
            Self::Completed => ManagedDnsReconcileOperationState::Completed,
            Self::Failed { failure } => ManagedDnsReconcileOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }

    #[must_use]
    pub fn event(
        &self,
        operation_id: &OperationId,
        subject: &ManagedDnsReconcileSubject,
    ) -> OperationEvent {
        match self {
            Self::Completed => OperationEvent::ManagedDnsReconcileCompleted {
                operation_id: operation_id.clone(),
                subject: subject.clone(),
            },
            Self::Failed { failure } => OperationEvent::ManagedDnsReconcileFailed {
                operation_id: operation_id.clone(),
                subject: subject.clone(),
                failure: failure.clone(),
            },
        }
    }
}

pub(super) enum ManagedDnsReconcileEvent {
    Submitted {
        subject: ManagedDnsReconcileSubject,
    },
    Transition {
        subject: ManagedDnsReconcileSubject,
        transition: ManagedDnsReconcileTransition,
    },
    UnsupportedCancellation,
}

pub(super) fn project_event(
    id: &OperationId,
    subject: &ManagedDnsReconcileSubject,
    state: &ManagedDnsReconcileOperationState,
    event: ManagedDnsReconcileEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        ManagedDnsReconcileEvent::Submitted {
            subject: event_subject,
        } => {
            verify_subject(
                id,
                subject,
                &event_subject,
                OperationSubjectRef::ManagedDnsReconcile,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        ManagedDnsReconcileEvent::Transition {
            subject: event_subject,
            transition,
        } => {
            verify_subject(
                id,
                subject,
                &event_subject,
                OperationSubjectRef::ManagedDnsReconcile,
            )?;
            project_state(id, subject, state, transition.state(), event_sequence)
        }
        ManagedDnsReconcileEvent::UnsupportedCancellation => Err(
            StatusProjectionError::ManagedDnsReconcileCancellationUnsupported {
                operation_id: id.clone(),
            },
        ),
    }
}

fn project_state(
    id: &OperationId,
    subject: &ManagedDnsReconcileSubject,
    state: &ManagedDnsReconcileOperationState,
    attempted: ManagedDnsReconcileOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        state,
        attempted,
        ManagedDnsReconcileOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::ManagedDnsReconcile,
        |state| OperationStatus::ManagedDnsReconcile {
            id: id.clone(),
            subject: subject.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

fn transition_allowed(
    current: &ManagedDnsReconcileOperationState,
    attempted: &ManagedDnsReconcileOperationState,
) -> bool {
    match (current, attempted) {
        (
            ManagedDnsReconcileOperationState::Accepted,
            ManagedDnsReconcileOperationState::Completed
            | ManagedDnsReconcileOperationState::Failed { .. },
        ) => true,
        (
            ManagedDnsReconcileOperationState::Accepted,
            ManagedDnsReconcileOperationState::Accepted,
        )
        | (
            ManagedDnsReconcileOperationState::Completed
            | ManagedDnsReconcileOperationState::Failed { .. },
            _,
        ) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operation_id() -> OperationId {
        OperationId::try_new("op-managed-dns").expect("operation id")
    }

    #[test]
    fn operation_reasons_are_explicit_and_secret_free() {
        let subject = ManagedDnsReconcileSubject::AuthorizedWithdraw {
            lease: ManagedLeaseName::try_new("cluster-one").expect("lease"),
            authorization: ManagedDnsWithdrawAuthorization::TargetDisabled,
        };
        let value = serde_json::to_value(subject).expect("serialize subject");
        assert_eq!(
            value,
            serde_json::json!({
                "reason": "authorized_withdraw",
                "lease": "cluster-one",
                "authorization": { "authorization": "target_disabled" }
            })
        );
        assert!(!value.to_string().contains("token"));
    }

    #[test]
    fn terminal_state_rejects_another_transition() {
        let subject = ManagedDnsReconcileSubject::Acquire;
        let error = project_state(
            &operation_id(),
            &subject,
            &ManagedDnsReconcileOperationState::Completed,
            ManagedDnsReconcileOperationState::Failed {
                failure: ManagedDnsReconcileFailure {
                    class: ManagedDnsReconcileFailureClass::Transport,
                    message: FailureMessage::try_new("too late").expect("message"),
                },
            },
            EventSequence::try_new(3).expect("sequence"),
        )
        .expect_err("terminal transition must fail");
        assert!(matches!(error, StatusProjectionError::TerminalState { .. }));
    }
}
