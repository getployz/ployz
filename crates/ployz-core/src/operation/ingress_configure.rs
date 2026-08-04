//! Cluster ingress configuration operation state and evidence.

use serde::{Deserialize, Serialize};

use crate::ids::OperationId;
use crate::ingress::IngressConfiguration;

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_transition,
};
use super::{
    EventSequence, FailureMessage, OperationInterruptionCause, OperationInterruptionEvidence,
    OperationInterruptionStage, OperationStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressConfigureOperationState {
    Accepted,
    Completed,
    Failed {
        failure: IngressConfigureFailure,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl IngressConfigureOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        !matches!(self, Self::Accepted)
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        match self {
            Self::Accepted => Some(OperationInterruptionEvidence::new(
                cause,
                OperationInterruptionStage::IngressConfigureAccepted,
            )),
            Self::Completed | Self::Failed { .. } | Self::Interrupted { .. } => None,
        }
    }

    pub(super) const fn interrupted(evidence: OperationInterruptionEvidence) -> Self {
        Self::Interrupted { evidence }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum IngressConfigureFailure {
    InvalidConfiguration { message: FailureMessage },
    IntentStoreFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IngressConfigureTransition {
    Completed,
    Failed { failure: IngressConfigureFailure },
}

impl IngressConfigureTransition {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Completed => OperationEvent::IngressConfigureCompleted {
                operation_id: operation_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::IngressConfigureFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
        }
    }

    fn state(&self) -> IngressConfigureOperationState {
        match self {
            Self::Completed => IngressConfigureOperationState::Completed,
            Self::Failed { failure } => IngressConfigureOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }
}

pub(super) enum IngressConfigureEvent {
    Submitted { configuration: IngressConfiguration },
    Transition(IngressConfigureTransition),
    UnsupportedCancellation,
}

pub(super) fn project_event(
    id: &OperationId,
    configuration: &IngressConfiguration,
    state: &IngressConfigureOperationState,
    event: IngressConfigureEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let attempted = match event {
        IngressConfigureEvent::Submitted {
            configuration: event_configuration,
        } => {
            if configuration != &event_configuration {
                return Err(StatusProjectionError::IngressConfigurationMismatch {
                    operation_id: id.clone(),
                });
            }
            return Ok(OperationProjection::AlreadySatisfied);
        }
        IngressConfigureEvent::Transition(transition) => transition.state(),
        IngressConfigureEvent::UnsupportedCancellation => {
            return Ok(OperationProjection::AlreadySatisfied);
        }
    };
    project_transition(
        id,
        state,
        attempted,
        IngressConfigureOperationState::is_terminal,
        |current, attempted| {
            matches!(current, IngressConfigureOperationState::Accepted) && attempted.is_terminal()
        },
        ProjectionOperationState::IngressConfigure,
        |state| OperationStatus::IngressConfigure {
            id: id.clone(),
            configuration: configuration.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::{AutomaticHostnameConfiguration, PloyzDnsTargetIntent};

    #[test]
    fn completed_configuration_is_terminal() {
        assert!(IngressConfigureOperationState::Completed.is_terminal());
    }

    #[test]
    fn submitted_configuration_must_match_status() {
        let operation_id = OperationId::try_new("op_ingress_configure").expect("operation id");
        let configuration = IngressConfiguration::try_new(
            AutomaticHostnameConfiguration::Disabled,
            PloyzDnsTargetIntent::Disabled,
        )
        .expect("valid configuration");
        let result = project_event(
            &operation_id,
            &configuration,
            &IngressConfigureOperationState::Accepted,
            IngressConfigureEvent::Submitted {
                configuration: IngressConfiguration::try_new(
                    AutomaticHostnameConfiguration::Ployz,
                    PloyzDnsTargetIntent::Enabled,
                )
                .expect("valid configuration"),
            },
            EventSequence::try_new(1).expect("sequence"),
        );

        assert!(matches!(
            result,
            Err(StatusProjectionError::IngressConfigurationMismatch { .. })
        ));
    }
}
