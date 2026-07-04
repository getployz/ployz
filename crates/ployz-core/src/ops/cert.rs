//! The Cert operation: an ACME renewal publishes a challenge, validates,
//! and commits the active cert. States, failures, transitions, and status
//! projection live together here.

use serde::{Deserialize, Serialize};

use crate::cert::{ActiveCertState, CertBundleRef, CertValidityWindow};
use crate::ids::{CertId, OperationId};

use super::events::OperationSubjectRef;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, verify_subject,
};
use super::text::{CancellationReason, FailureMessage};
use super::{EventSequence, OperationStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum CertRunningStage {
    ChallengePublished,
    ValidationStarted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertOperationState {
    Accepted,
    Running { stage: CertRunningStage },
    Completed,
    Failed { failure: CertOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl CertOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted | Self::Running { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CertOperationFailure {
    ChallengePublishFailed {
        cert_id: CertId,
        message: FailureMessage,
    },
    AcmeValidationFailed {
        cert_id: CertId,
        message: FailureMessage,
        retained_active_cert: Option<ActiveCertState>,
    },
    ActiveCertCommitFailed {
        cert_id: CertId,
        bundle_ref: CertBundleRef,
        validity: CertValidityWindow,
        message: FailureMessage,
        retained_active_cert: Option<ActiveCertState>,
    },
}

impl CertOperationFailure {
    #[must_use]
    pub const fn cert_id(&self) -> &CertId {
        match self {
            Self::ChallengePublishFailed { cert_id, .. }
            | Self::AcmeValidationFailed { cert_id, .. }
            | Self::ActiveCertCommitFailed { cert_id, .. } => cert_id,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertTransition {
    Running { stage: CertRunningStage },
    Completed,
    Failed { failure: CertOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl CertTransition {
    #[must_use]
    pub fn state(&self) -> CertOperationState {
        match self {
            Self::Running { stage } => CertOperationState::Running { stage: *stage },
            Self::Completed => CertOperationState::Completed,
            Self::Failed { failure } => CertOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => CertOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

/// Cert events after classification from the flat [`super::OperationEvent`]
/// stream shape. Subject-bearing variants carry the cert id the event
/// claims, verified against the status record; a cancel names no subject.
pub(super) enum CertEvent {
    Submitted {
        cert_id: CertId,
    },
    Transition {
        cert_id: CertId,
        transition: CertTransition,
    },
    Cancelled(CancellationReason),
}

pub(super) fn project_event(
    id: &OperationId,
    cert_id: &CertId,
    state: &CertOperationState,
    event: CertEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        CertEvent::Submitted {
            cert_id: event_cert_id,
        } => {
            verify_subject(id, cert_id, &event_cert_id, OperationSubjectRef::Cert)?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        CertEvent::Transition {
            cert_id: event_cert_id,
            transition,
        } => {
            verify_subject(id, cert_id, &event_cert_id, OperationSubjectRef::Cert)?;
            project_state(id, cert_id, state, transition.state(), event_sequence)
        }
        CertEvent::Cancelled(reason) => project_state(
            id,
            cert_id,
            state,
            CertOperationState::Cancelled { reason },
            event_sequence,
        ),
    }
}

pub(super) fn project_state(
    id: &OperationId,
    cert_id: &CertId,
    current_state: &CertOperationState,
    attempted: CertOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if transition_satisfied(current_state, &attempted) {
        return Ok(OperationProjection::AlreadySatisfied);
    }

    validate_transition(id, current_state, &attempted)?;

    Ok(OperationProjection::StatusChanged {
        status: Box::new(OperationStatus::Cert {
            id: id.clone(),
            cert_id: cert_id.clone(),
            state: attempted,
            last_event_sequence: event_sequence,
        }),
    })
}

fn validate_transition(
    operation_id: &OperationId,
    current: &CertOperationState,
    attempted: &CertOperationState,
) -> Result<(), StatusProjectionError> {
    if current.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: operation_id.clone(),
            current: Box::new(ProjectionOperationState::Cert(current.clone())),
            attempted: Box::new(ProjectionOperationState::Cert(attempted.clone())),
        });
    }

    if transition_allowed(current, attempted) {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(ProjectionOperationState::Cert(current.clone())),
        attempted: Box::new(ProjectionOperationState::Cert(attempted.clone())),
    })
}

fn transition_allowed(current: &CertOperationState, attempted: &CertOperationState) -> bool {
    match (current, attempted) {
        (
            CertOperationState::Accepted,
            CertOperationState::Running {
                stage: CertRunningStage::ChallengePublished,
            },
        )
        | (CertOperationState::Accepted, CertOperationState::Cancelled { .. })
        | (CertOperationState::Accepted, CertOperationState::Failed { .. })
        | (CertOperationState::Running { .. }, CertOperationState::Cancelled { .. })
        | (CertOperationState::Running { .. }, CertOperationState::Failed { .. }) => true,
        (
            CertOperationState::Running {
                stage: CertRunningStage::ValidationStarted,
            },
            CertOperationState::Completed,
        ) => true,
        (
            CertOperationState::Running { stage: current },
            CertOperationState::Running { stage: attempted },
        ) => stage_is_next(*current, *attempted),
        (CertOperationState::Accepted, _)
        | (CertOperationState::Completed, _)
        | (CertOperationState::Failed { .. }, _)
        | (CertOperationState::Cancelled { .. }, _)
        | (CertOperationState::Running { .. }, _) => false,
    }
}

fn transition_satisfied(current: &CertOperationState, attempted: &CertOperationState) -> bool {
    match attempted {
        CertOperationState::Accepted => matches!(current, CertOperationState::Accepted),
        CertOperationState::Running { stage: attempted } => match current {
            CertOperationState::Running { stage: current } => {
                let current_rank = stage_rank(*current);
                let attempted_rank = stage_rank(*attempted);
                current_rank > attempted_rank
                    || current_rank == attempted_rank && current == attempted
            }
            CertOperationState::Accepted
            | CertOperationState::Completed
            | CertOperationState::Failed { .. }
            | CertOperationState::Cancelled { .. } => false,
        },
        CertOperationState::Completed => matches!(current, CertOperationState::Completed),
        CertOperationState::Failed { failure: attempted } => {
            matches!(current, CertOperationState::Failed { failure } if failure == attempted)
        }
        CertOperationState::Cancelled { reason: attempted } => {
            matches!(current, CertOperationState::Cancelled { reason } if reason == attempted)
        }
    }
}

fn stage_rank(stage: CertRunningStage) -> u8 {
    match stage {
        CertRunningStage::ChallengePublished => 0,
        CertRunningStage::ValidationStarted => 1,
    }
}

fn stage_is_next(current: CertRunningStage, attempted: CertRunningStage) -> bool {
    matches!(
        (current, attempted),
        (
            CertRunningStage::ChallengePublished,
            CertRunningStage::ValidationStarted
        )
    )
}
