//! The Namespace Remove operation: bounded teardown of one namespace's
//! service containers, route bindings, and serving target entries.

use serde::{Deserialize, Serialize};

use crate::ids::{ContainerId, MachineId, NamespaceId, OperationId};

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, kind_mismatch,
};
use super::routes::RouteTarget;
use super::text::{CancellationReason, FailureMessage, OperatorHint};
use super::{EventSequence, OperationInterruptionEvidence, OperationKind, OperationStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NamespaceRemoveRunningStage {
    RemovingRouteBindings,
    RemovingServingTargets,
    RemovingContainers,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum NamespaceRemoveOperationState {
    Accepted,
    Running {
        stage: NamespaceRemoveRunningStage,
    },
    Completed,
    Failed {
        failure: NamespaceRemoveFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl NamespaceRemoveOperationState {
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
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum NamespaceRemoveFailure {
    IntentReadFailed {
        namespace_id: NamespaceId,
        message: FailureMessage,
    },
    ControlPlaneCommitFailed {
        namespace_id: NamespaceId,
        message: FailureMessage,
    },
    MachineUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    ContainerRemoveFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    Timeout {
        machine_id: MachineId,
        container_id: ContainerId,
        timeout_seconds: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamespaceRemoveTransition {
    Running { stage: NamespaceRemoveRunningStage },
    Completed,
    Failed { failure: NamespaceRemoveFailure },
    Cancelled { reason: CancellationReason },
}

impl NamespaceRemoveTransition {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Running { stage } => OperationEvent::NamespaceRemoveRunning {
                operation_id: operation_id.clone(),
                stage: *stage,
            },
            Self::Completed => OperationEvent::NamespaceRemoveCompleted {
                operation_id: operation_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::NamespaceRemoveFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::NamespaceRemove,
                reason: reason.clone(),
            },
        }
    }

    #[must_use]
    pub fn state(&self) -> NamespaceRemoveOperationState {
        match self {
            Self::Running { stage } => NamespaceRemoveOperationState::Running { stage: *stage },
            Self::Completed => NamespaceRemoveOperationState::Completed,
            Self::Failed { failure } => NamespaceRemoveOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => NamespaceRemoveOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

pub(super) enum NamespaceRemoveEvent {
    Submitted {
        namespace_id: NamespaceId,
    },
    RouteBindingRemoved {
        target: RouteTarget,
    },
    ContainerRemoved {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    Transition(NamespaceRemoveTransition),
    Interrupted(OperationInterruptionEvidence),
}

pub(super) fn project_event(
    id: &OperationId,
    namespace_id: &NamespaceId,
    state: &NamespaceRemoveOperationState,
    event: NamespaceRemoveEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        NamespaceRemoveEvent::Submitted {
            namespace_id: actual,
        } => {
            if namespace_id != &actual {
                return Err(StatusProjectionError::InvalidTransition {
                    operation_id: id.clone(),
                    current: Box::new(ProjectionOperationState::NamespaceRemove(
                        NamespaceRemoveOperationState::Accepted,
                    )),
                    attempted: Box::new(ProjectionOperationState::NamespaceRemove(
                        NamespaceRemoveOperationState::Accepted,
                    )),
                });
            }
            Ok(OperationProjection::AlreadySatisfied)
        }
        NamespaceRemoveEvent::RouteBindingRemoved { target } => {
            let _ = target;
            if matches!(state, NamespaceRemoveOperationState::Running { .. }) {
                return Ok(OperationProjection::StatusChanged {
                    status: Box::new(status(id, namespace_id, state.clone(), event_sequence)),
                });
            }
            Ok(OperationProjection::AlreadySatisfied)
        }
        NamespaceRemoveEvent::ContainerRemoved {
            machine_id,
            container_id,
        } => {
            let _ = (machine_id, container_id);
            if matches!(state, NamespaceRemoveOperationState::Running { .. }) {
                return Ok(OperationProjection::StatusChanged {
                    status: Box::new(status(id, namespace_id, state.clone(), event_sequence)),
                });
            }
            Ok(OperationProjection::AlreadySatisfied)
        }
        NamespaceRemoveEvent::Transition(transition) => {
            project_state(id, namespace_id, state, transition.state(), event_sequence)
        }
        NamespaceRemoveEvent::Interrupted(evidence) => project_state(
            id,
            namespace_id,
            state,
            NamespaceRemoveOperationState::Interrupted { evidence },
            event_sequence,
        ),
    }
}

fn project_state(
    id: &OperationId,
    namespace_id: &NamespaceId,
    current: &NamespaceRemoveOperationState,
    attempted: NamespaceRemoveOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if current == &attempted {
        return Ok(OperationProjection::AlreadySatisfied);
    }
    validate_transition(id, current, &attempted)?;
    Ok(OperationProjection::StatusChanged {
        status: Box::new(status(id, namespace_id, attempted, event_sequence)),
    })
}

fn status(
    id: &OperationId,
    namespace_id: &NamespaceId,
    state: NamespaceRemoveOperationState,
    event_sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::NamespaceRemove {
        id: id.clone(),
        namespace_id: namespace_id.clone(),
        state,
        last_event_sequence: event_sequence,
    }
}

fn validate_transition(
    operation_id: &OperationId,
    current: &NamespaceRemoveOperationState,
    attempted: &NamespaceRemoveOperationState,
) -> Result<(), StatusProjectionError> {
    if current.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: operation_id.clone(),
            current: Box::new(ProjectionOperationState::NamespaceRemove(current.clone())),
            attempted: Box::new(ProjectionOperationState::NamespaceRemove(attempted.clone())),
        });
    }
    if transition_allowed(current, attempted) {
        return Ok(());
    }
    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(ProjectionOperationState::NamespaceRemove(current.clone())),
        attempted: Box::new(ProjectionOperationState::NamespaceRemove(attempted.clone())),
    })
}

fn transition_allowed(
    current: &NamespaceRemoveOperationState,
    attempted: &NamespaceRemoveOperationState,
) -> bool {
    match (current, attempted) {
        (
            NamespaceRemoveOperationState::Accepted,
            NamespaceRemoveOperationState::Running {
                stage: NamespaceRemoveRunningStage::RemovingRouteBindings,
            },
        )
        | (
            NamespaceRemoveOperationState::Running {
                stage: NamespaceRemoveRunningStage::RemovingRouteBindings,
            },
            NamespaceRemoveOperationState::Running {
                stage: NamespaceRemoveRunningStage::RemovingServingTargets,
            },
        )
        | (
            NamespaceRemoveOperationState::Running {
                stage: NamespaceRemoveRunningStage::RemovingServingTargets,
            },
            NamespaceRemoveOperationState::Running {
                stage: NamespaceRemoveRunningStage::RemovingContainers,
            },
        )
        | (
            NamespaceRemoveOperationState::Running { .. },
            NamespaceRemoveOperationState::Completed,
        )
        | (
            NamespaceRemoveOperationState::Accepted | NamespaceRemoveOperationState::Running { .. },
            NamespaceRemoveOperationState::Failed { .. }
            | NamespaceRemoveOperationState::Cancelled { .. }
            | NamespaceRemoveOperationState::Interrupted { .. },
        ) => true,
        (
            NamespaceRemoveOperationState::Accepted
            | NamespaceRemoveOperationState::Completed
            | NamespaceRemoveOperationState::Failed { .. }
            | NamespaceRemoveOperationState::Cancelled { .. }
            | NamespaceRemoveOperationState::Interrupted { .. },
            _,
        )
        | (
            NamespaceRemoveOperationState::Running {
                stage:
                    NamespaceRemoveRunningStage::RemovingRouteBindings
                    | NamespaceRemoveRunningStage::RemovingServingTargets
                    | NamespaceRemoveRunningStage::RemovingContainers,
            },
            NamespaceRemoveOperationState::Accepted | NamespaceRemoveOperationState::Running { .. },
        ) => false,
    }
}

pub fn project_namespace_remove_transition(
    current: &OperationStatus,
    transition: NamespaceRemoveTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let OperationStatus::NamespaceRemove {
        id,
        namespace_id,
        state,
        ..
    } = current
    else {
        return Err(kind_mismatch(current, OperationKind::NamespaceRemove));
    };
    project_state(id, namespace_id, state, transition.state(), event_sequence)
}
