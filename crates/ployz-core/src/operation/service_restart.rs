//! The Service Restart operation: restart existing service containers in
//! place, one at a time, with a fresh running/health wait after each restart.

use serde::{Deserialize, Serialize};

use crate::ids::{ContainerId, MachineId, NamespaceId, OperationId, ServiceId};

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, kind_mismatch,
};
use super::text::{CancellationReason, FailureMessage, OperatorHint};
use super::{
    EventSequence, OperationInterruptionCause, OperationInterruptionEvidence,
    OperationInterruptionStage, OperationKind, OperationStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum ServiceRestartRunningStage {
    RestartingContainers,
    WaitingForHealth,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum ServiceRestartOperationState {
    Accepted,
    Running {
        stage: ServiceRestartRunningStage,
    },
    Completed,
    Failed {
        failure: ServiceRestartFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl ServiceRestartOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => true,
            Self::Accepted | Self::Running { .. } => false,
        }
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        let last_durable_stage = match self {
            Self::Accepted => OperationInterruptionStage::ServiceRestartAccepted,
            Self::Running { stage } => {
                OperationInterruptionStage::ServiceRestartRunning { stage: *stage }
            }
            Self::Completed
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => return None,
        };
        Some(OperationInterruptionEvidence::new(
            cause,
            last_durable_stage,
        ))
    }

    pub(super) const fn interrupted(evidence: OperationInterruptionEvidence) -> Self {
        Self::Interrupted { evidence }
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
pub enum ServiceRestartFailure {
    NoSuchService {
        namespace_id: NamespaceId,
        service_id: ServiceId,
    },
    NoRunningContainers {
        namespace_id: NamespaceId,
        service_id: ServiceId,
    },
    IntentReadFailed {
        namespace_id: NamespaceId,
        service_id: ServiceId,
        message: FailureMessage,
    },
    MachineUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    ContainerRestartFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    HealthGateFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
    Timeout {
        machine_id: MachineId,
        container_id: ContainerId,
        timeout_seconds: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceRestartTransition {
    Running { stage: ServiceRestartRunningStage },
    Completed,
    Failed { failure: ServiceRestartFailure },
    Cancelled { reason: CancellationReason },
}

impl ServiceRestartTransition {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Running { stage } => OperationEvent::ServiceRestartRunning {
                operation_id: operation_id.clone(),
                stage: *stage,
            },
            Self::Completed => OperationEvent::ServiceRestartCompleted {
                operation_id: operation_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::ServiceRestartFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::ServiceRestart,
                reason: reason.clone(),
            },
        }
    }

    #[must_use]
    pub fn state(&self) -> ServiceRestartOperationState {
        match self {
            Self::Running { stage } => ServiceRestartOperationState::Running { stage: *stage },
            Self::Completed => ServiceRestartOperationState::Completed,
            Self::Failed { failure } => ServiceRestartOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => ServiceRestartOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

pub(super) enum ServiceRestartEvent {
    Submitted {
        namespace_id: NamespaceId,
        service_id: ServiceId,
    },
    ContainerRestarted {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    Transition(ServiceRestartTransition),
}

pub(super) fn project_event(
    id: &OperationId,
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    state: &ServiceRestartOperationState,
    event: ServiceRestartEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        ServiceRestartEvent::Submitted {
            namespace_id: actual_namespace_id,
            service_id: actual_service_id,
        } => {
            verify_target(
                id,
                namespace_id,
                service_id,
                &actual_namespace_id,
                &actual_service_id,
            )?;
            Ok(OperationProjection::AlreadySatisfied)
        }
        ServiceRestartEvent::ContainerRestarted {
            machine_id,
            container_id,
        } => {
            let _ = (machine_id, container_id);
            if matches!(state, ServiceRestartOperationState::Running { .. }) {
                return Ok(OperationProjection::StatusChanged {
                    status: Box::new(status(
                        id,
                        namespace_id,
                        service_id,
                        state.clone(),
                        event_sequence,
                    )),
                });
            }
            Ok(OperationProjection::AlreadySatisfied)
        }
        ServiceRestartEvent::Transition(transition) => project_state(
            id,
            namespace_id,
            service_id,
            state,
            transition.state(),
            event_sequence,
        ),
    }
}

fn verify_target(
    operation_id: &OperationId,
    expected_namespace_id: &NamespaceId,
    expected_service_id: &ServiceId,
    actual_namespace_id: &NamespaceId,
    actual_service_id: &ServiceId,
) -> Result<(), StatusProjectionError> {
    if expected_namespace_id == actual_namespace_id && expected_service_id == actual_service_id {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(ProjectionOperationState::ServiceRestart(
            ServiceRestartOperationState::Accepted,
        )),
        attempted: Box::new(ProjectionOperationState::ServiceRestart(
            ServiceRestartOperationState::Accepted,
        )),
    })
}

fn project_state(
    id: &OperationId,
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    current: &ServiceRestartOperationState,
    attempted: ServiceRestartOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if transition_satisfied(current, &attempted) {
        return Ok(OperationProjection::AlreadySatisfied);
    }
    validate_transition(id, current, &attempted)?;
    Ok(OperationProjection::StatusChanged {
        status: Box::new(status(
            id,
            namespace_id,
            service_id,
            attempted,
            event_sequence,
        )),
    })
}

fn status(
    id: &OperationId,
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    state: ServiceRestartOperationState,
    event_sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::ServiceRestart {
        id: id.clone(),
        namespace_id: namespace_id.clone(),
        service_id: service_id.clone(),
        state,
        last_event_sequence: event_sequence,
    }
}

fn validate_transition(
    operation_id: &OperationId,
    current: &ServiceRestartOperationState,
    attempted: &ServiceRestartOperationState,
) -> Result<(), StatusProjectionError> {
    if current.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: operation_id.clone(),
            current: Box::new(ProjectionOperationState::ServiceRestart(current.clone())),
            attempted: Box::new(ProjectionOperationState::ServiceRestart(attempted.clone())),
        });
    }
    if transition_allowed(current, attempted) {
        return Ok(());
    }
    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(ProjectionOperationState::ServiceRestart(current.clone())),
        attempted: Box::new(ProjectionOperationState::ServiceRestart(attempted.clone())),
    })
}

fn transition_allowed(
    current: &ServiceRestartOperationState,
    attempted: &ServiceRestartOperationState,
) -> bool {
    match (current, attempted) {
        (
            ServiceRestartOperationState::Accepted,
            ServiceRestartOperationState::Running {
                stage: ServiceRestartRunningStage::RestartingContainers,
            },
        )
        | (
            ServiceRestartOperationState::Running {
                stage: ServiceRestartRunningStage::RestartingContainers,
            },
            ServiceRestartOperationState::Running {
                stage: ServiceRestartRunningStage::WaitingForHealth,
            },
        )
        | (
            ServiceRestartOperationState::Running { .. },
            ServiceRestartOperationState::Running {
                stage: ServiceRestartRunningStage::RestartingContainers,
            },
        )
        | (ServiceRestartOperationState::Running { .. }, ServiceRestartOperationState::Completed)
        | (
            ServiceRestartOperationState::Accepted | ServiceRestartOperationState::Running { .. },
            ServiceRestartOperationState::Failed { .. }
            | ServiceRestartOperationState::Cancelled { .. }
            | ServiceRestartOperationState::Interrupted { .. },
        ) => true,
        (
            ServiceRestartOperationState::Accepted
            | ServiceRestartOperationState::Completed
            | ServiceRestartOperationState::Failed { .. }
            | ServiceRestartOperationState::Cancelled { .. }
            | ServiceRestartOperationState::Interrupted { .. },
            _,
        )
        | (
            ServiceRestartOperationState::Running {
                stage:
                    ServiceRestartRunningStage::RestartingContainers
                    | ServiceRestartRunningStage::WaitingForHealth,
            },
            ServiceRestartOperationState::Accepted,
        )
        | (
            ServiceRestartOperationState::Running {
                stage: ServiceRestartRunningStage::WaitingForHealth,
            },
            ServiceRestartOperationState::Running {
                stage: ServiceRestartRunningStage::WaitingForHealth,
            },
        ) => false,
    }
}

fn transition_satisfied(
    current: &ServiceRestartOperationState,
    attempted: &ServiceRestartOperationState,
) -> bool {
    current == attempted
}

pub fn project_service_restart_transition(
    current: &OperationStatus,
    transition: ServiceRestartTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let OperationStatus::ServiceRestart {
        id,
        namespace_id,
        service_id,
        state,
        ..
    } = current
    else {
        return Err(kind_mismatch(current, OperationKind::ServiceRestart));
    };
    project_state(
        id,
        namespace_id,
        service_id,
        state,
        transition.state(),
        event_sequence,
    )
}
