//! Canonical ingress endpoint refresh operation state and evidence.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::ids::{MachineId, OperationId};
use crate::ingress::IngressEndpointProjectionIdentity;

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
};
use super::{EventSequence, FailureMessage, OperationStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressRefreshOperationState {
    Accepted,
    Completed { evidence: IngressRefreshEvidence },
    Failed { failure: IngressRefreshFailure },
}

impl IngressRefreshOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        !matches!(self, Self::Accepted)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "class", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressRefreshFailure {
    IntentUnavailable {
        message: FailureMessage,
    },
    GatherFailed {
        message: FailureMessage,
        candidates: Vec<IngressRefreshCandidateEvidence>,
    },
    StorageFailed {
        message: FailureMessage,
        candidates: Vec<IngressRefreshCandidateEvidence>,
    },
    ConcurrentWriter {
        message: FailureMessage,
        candidates: Vec<IngressRefreshCandidateEvidence>,
    },
    Interrupted {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IngressRefreshEvidence {
    pub candidates: Vec<IngressRefreshCandidateEvidence>,
    pub publishable_gateway_ids: Vec<MachineId>,
    pub deadline_seconds: u64,
    pub before: IngressEndpointProjectionIdentity,
    pub after: IngressEndpointProjectionIdentity,
    pub invalidation: IngressRefreshInvalidationEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IngressRefreshCandidateEvidence {
    pub machine_id: MachineId,
    pub gateway: IngressRefreshGatewayOutcome,
    pub facts: IngressRefreshFactsOutcome,
    pub publication: IngressRefreshCandidatePublication,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressRefreshCandidatePublication {
    Published {
        addresses: Vec<IpAddr>,
    },
    Excluded {
        reason: IngressRefreshExclusionReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum IngressRefreshExclusionReason {
    GatewayUnavailable,
    GatewayTestimonyFailed,
    FactsTestimonyFailed,
    MissingFacts,
    Addressless,
    NonPublicAddresses,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressRefreshInvalidationEvidence {
    Published,
    Failed { message: FailureMessage },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressRefreshGatewayOutcome {
    Current,
    LastKnownGood,
    Unavailable,
    TimedOut,
    NoResponder,
    WrongResponder,
    Rejected,
    Malformed,
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressRefreshFactsOutcome {
    Responded {
        public_control_endpoints: Vec<IpAddr>,
    },
    Missing,
    Addressless,
    NonPublic,
    TimedOut,
    NoResponder,
    WrongResponder,
    Rejected,
    Malformed,
    Transport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressRefreshTransition {
    Completed { evidence: IngressRefreshEvidence },
    Failed { failure: IngressRefreshFailure },
}

impl IngressRefreshTransition {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Completed { evidence } => OperationEvent::IngressRefreshCompleted {
                operation_id: operation_id.clone(),
                evidence: evidence.clone(),
            },
            Self::Failed { failure } => OperationEvent::IngressRefreshFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
        }
    }

    fn state(&self) -> IngressRefreshOperationState {
        match self {
            Self::Completed { evidence } => IngressRefreshOperationState::Completed {
                evidence: evidence.clone(),
            },
            Self::Failed { failure } => IngressRefreshOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }
}

pub(super) enum IngressRefreshEvent {
    Submitted,
    Transition(IngressRefreshTransition),
    UnsupportedCancellation,
}

pub(super) fn project_event(
    id: &OperationId,
    state: &IngressRefreshOperationState,
    event: IngressRefreshEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let attempted = match event {
        IngressRefreshEvent::Submitted => return Ok(OperationProjection::AlreadySatisfied),
        IngressRefreshEvent::Transition(transition) => transition.state(),
        IngressRefreshEvent::UnsupportedCancellation => {
            return Ok(OperationProjection::AlreadySatisfied);
        }
    };
    project_transition(
        id,
        state,
        attempted,
        IngressRefreshOperationState::is_terminal,
        |current, attempted| {
            matches!(current, IngressRefreshOperationState::Accepted) && attempted.is_terminal()
        },
        ProjectionOperationState::IngressRefresh,
        |state| OperationStatus::IngressRefresh {
            id: id.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ControlPlaneEpoch;

    #[test]
    fn completed_refresh_is_terminal_and_preserves_identity_evidence() {
        let id = OperationId::try_new("op_ingress_refresh_test").expect("operation id");
        let identity = IngressEndpointProjectionIdentity {
            control_plane_epoch: ControlPlaneEpoch::initial(),
            revision: 1,
        };
        let state = IngressRefreshOperationState::Accepted;
        let evidence = IngressRefreshEvidence {
            candidates: Vec::new(),
            publishable_gateway_ids: Vec::new(),
            deadline_seconds: 30,
            before: IngressEndpointProjectionIdentity {
                control_plane_epoch: ControlPlaneEpoch::initial(),
                revision: 0,
            },
            after: identity,
            invalidation: IngressRefreshInvalidationEvidence::Published,
        };

        let projected = project_event(
            &id,
            &state,
            IngressRefreshEvent::Transition(IngressRefreshTransition::Completed {
                evidence: evidence.clone(),
            }),
            EventSequence::try_new(2).expect("sequence"),
        )
        .expect("project completion");

        assert!(matches!(
            projected,
            OperationProjection::StatusChanged { status }
                if matches!(status.as_ref(), OperationStatus::IngressRefresh {
                    state: IngressRefreshOperationState::Completed { evidence: stored },
                    ..
                } if stored == &evidence)
        ));
    }

    #[test]
    fn failed_refresh_preserves_typed_message_and_candidate_evidence() {
        let id = OperationId::try_new("op_ingress_refresh_failed").expect("operation id");
        let failure = IngressRefreshFailure::StorageFailed {
            message: FailureMessage::try_new("projection write failed").expect("message"),
            candidates: vec![IngressRefreshCandidateEvidence {
                machine_id: MachineId::try_new("machine_a").expect("machine id"),
                gateway: IngressRefreshGatewayOutcome::Current,
                facts: IngressRefreshFactsOutcome::NonPublic,
                publication: IngressRefreshCandidatePublication::Excluded {
                    reason: IngressRefreshExclusionReason::NonPublicAddresses,
                },
            }],
        };

        let projected = project_event(
            &id,
            &IngressRefreshOperationState::Accepted,
            IngressRefreshEvent::Transition(IngressRefreshTransition::Failed {
                failure: failure.clone(),
            }),
            EventSequence::try_new(2).expect("sequence"),
        )
        .expect("project failure");

        assert!(matches!(
            projected,
            OperationProjection::StatusChanged { status }
                if matches!(status.as_ref(), OperationStatus::IngressRefresh {
                    state: IngressRefreshOperationState::Failed { failure: stored },
                    ..
                } if stored == &failure)
        ));
    }
}
