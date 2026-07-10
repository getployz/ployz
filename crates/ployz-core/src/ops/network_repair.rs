//! Network repair operation: re-apply the cluster dataplane projection to
//! every active machine through one bounded operation.

use serde::{Deserialize, Serialize};

use crate::dataplane::PloyzNativeMeshComponent;
use crate::ids::{MachineId, OperationId};

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, kind_mismatch,
    project_transition,
};
use super::text::{CancellationReason, FailureMessage};
use super::{EventSequence, OperationKind, OperationStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NetworkRepairRunningStage {
    PreparingDataplane,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairOperationState {
    Accepted,
    Running { stage: NetworkRepairRunningStage },
    Completed,
    Failed { failure: NetworkRepairFailure },
    Cancelled { reason: CancellationReason },
}

impl NetworkRepairOperationState {
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
pub enum NetworkRepairFailure {
    NoActiveMachines,
    IntentReadFailed {
        message: FailureMessage,
    },
    DataplaneConvergenceFailed {
        machine_id: MachineId,
        component: PloyzNativeMeshComponent,
        message: FailureMessage,
    },
    DataplaneReportInvalid {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkRepairTransition {
    Running { stage: NetworkRepairRunningStage },
    Completed,
    Failed { failure: NetworkRepairFailure },
    Cancelled { reason: CancellationReason },
}

impl NetworkRepairTransition {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Running { stage } => OperationEvent::NetworkRepairRunning {
                operation_id: operation_id.clone(),
                stage: *stage,
            },
            Self::Completed => OperationEvent::NetworkRepairCompleted {
                operation_id: operation_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::NetworkRepairFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::NetworkRepair,
                reason: reason.clone(),
            },
        }
    }

    #[must_use]
    pub fn state(&self) -> NetworkRepairOperationState {
        match self {
            Self::Running { stage } => NetworkRepairOperationState::Running { stage: *stage },
            Self::Completed => NetworkRepairOperationState::Completed,
            Self::Failed { failure } => NetworkRepairOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => NetworkRepairOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

pub(super) enum NetworkRepairEvent {
    Submitted,
    Transition(NetworkRepairTransition),
}

pub(super) fn project_event(
    id: &OperationId,
    state: &NetworkRepairOperationState,
    event: NetworkRepairEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        NetworkRepairEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
        NetworkRepairEvent::Transition(transition) => {
            project_state(id, state, transition.state(), event_sequence)
        }
    }
}

fn project_state(
    id: &OperationId,
    current: &NetworkRepairOperationState,
    attempted: NetworkRepairOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    project_transition(
        id,
        current,
        attempted,
        NetworkRepairOperationState::is_terminal,
        transition_allowed,
        ProjectionOperationState::NetworkRepair,
        |state| OperationStatus::NetworkRepair {
            id: id.clone(),
            state,
            last_event_sequence: event_sequence,
        },
    )
}

fn transition_allowed(
    current: &NetworkRepairOperationState,
    attempted: &NetworkRepairOperationState,
) -> bool {
    match (current, attempted) {
        (
            NetworkRepairOperationState::Accepted,
            NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::PreparingDataplane,
            }
            | NetworkRepairOperationState::Cancelled { .. },
        )
        | (
            NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::PreparingDataplane,
            },
            NetworkRepairOperationState::Completed
            | NetworkRepairOperationState::Failed { .. }
            | NetworkRepairOperationState::Cancelled { .. },
        ) => true,
        (
            NetworkRepairOperationState::Accepted
            | NetworkRepairOperationState::Completed
            | NetworkRepairOperationState::Failed { .. }
            | NetworkRepairOperationState::Cancelled { .. },
            _,
        )
        | (
            NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::PreparingDataplane,
            },
            NetworkRepairOperationState::Accepted
            | NetworkRepairOperationState::Running {
                stage: NetworkRepairRunningStage::PreparingDataplane,
            },
        ) => false,
    }
}

pub fn project_network_repair_transition(
    current: &OperationStatus,
    transition: NetworkRepairTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let OperationStatus::NetworkRepair { id, state, .. } = current else {
        return Err(kind_mismatch(current, OperationKind::NetworkRepair));
    };
    project_state(id, state, transition.state(), event_sequence)
}
