//! The Volume Remove operation: explicit destruction of one pinned volume.

use serde::{Deserialize, Serialize};

use crate::deploy::VolumeName;
use crate::ids::{MachineId, NamespaceId, OperationId, ServiceId};

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, kind_mismatch,
    project_transition,
};
use super::text::CancellationReason;
use super::text::FailureMessage;
use super::{
    EventSequence, OperationInterruptionCause, OperationInterruptionEvidence,
    OperationInterruptionNextAction, OperationInterruptionStage,
    OperationInterruptionUncertainWork, OperationKind, OperationStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum VolumeRemoveRunningStage {
    RemovingVolumeData,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeRemoveOperationState {
    Accepted,
    Running {
        stage: VolumeRemoveRunningStage,
    },
    Completed,
    Failed {
        failure: VolumeRemoveFailure,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl VolumeRemoveOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } | Self::Interrupted { .. } => true,
            Self::Accepted | Self::Running { .. } => false,
        }
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        let last_durable_stage = match self {
            Self::Accepted => OperationInterruptionStage::VolumeRemoveAccepted,
            Self::Running { stage } => {
                OperationInterruptionStage::VolumeRemoveRunning { stage: *stage }
            }
            Self::Completed | Self::Failed { .. } | Self::Interrupted { .. } => return None,
        };
        Some(OperationInterruptionEvidence::new(
            cause,
            last_durable_stage,
            OperationInterruptionUncertainWork::IntentAndRuntime,
            OperationInterruptionNextAction::InspectThenResubmit,
        ))
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
pub enum VolumeRemoveFailure {
    VolumeNotFound {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
    },
    VolumeInUse {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
        referencing_services: Vec<ServiceId>,
    },
    IntentReadFailed {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
        message: FailureMessage,
    },
    MachineUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
    },
    VolumeRemoveFailed {
        machine_id: MachineId,
        volume: VolumeName,
        message: FailureMessage,
    },
    ControlPlaneCommitFailed {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeRemoveTransition {
    Running { stage: VolumeRemoveRunningStage },
    Completed,
    Failed { failure: VolumeRemoveFailure },
}

impl VolumeRemoveTransition {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Running { stage } => OperationEvent::VolumeRemoveRunning {
                operation_id: operation_id.clone(),
                stage: *stage,
            },
            Self::Completed => OperationEvent::VolumeRemoveCompleted {
                operation_id: operation_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::VolumeRemoveFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
        }
    }

    #[must_use]
    pub fn state(&self) -> VolumeRemoveOperationState {
        match self {
            Self::Running { stage } => VolumeRemoveOperationState::Running { stage: *stage },
            Self::Completed => VolumeRemoveOperationState::Completed,
            Self::Failed { failure } => VolumeRemoveOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }
}

pub(super) enum VolumeRemoveEvent {
    Submitted {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
    },
    Transition(VolumeRemoveTransition),
    Cancelled(CancellationReason),
    Interrupted(OperationInterruptionEvidence),
}

pub(super) fn project_event(
    id: &OperationId,
    namespace_id: &NamespaceId,
    volume_name: &VolumeName,
    state: &VolumeRemoveOperationState,
    event: VolumeRemoveEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        VolumeRemoveEvent::Submitted {
            namespace_id: actual_namespace_id,
            volume_name: actual_volume_name,
        } => {
            if namespace_id != &actual_namespace_id || volume_name != &actual_volume_name {
                return Err(StatusProjectionError::InvalidTransition {
                    operation_id: id.clone(),
                    current: Box::new(ProjectionOperationState::VolumeRemove(state.clone())),
                    attempted: Box::new(ProjectionOperationState::VolumeRemove(state.clone())),
                });
            }
            Ok(OperationProjection::AlreadySatisfied)
        }
        VolumeRemoveEvent::Transition(transition) => project_transition(
            id,
            state,
            transition.state(),
            VolumeRemoveOperationState::is_terminal,
            transition_allowed,
            ProjectionOperationState::VolumeRemove,
            |state| status(id, namespace_id, volume_name, state, event_sequence),
        ),
        VolumeRemoveEvent::Cancelled(reason) => {
            let _ = reason;
            Err(StatusProjectionError::InvalidTransition {
                operation_id: id.clone(),
                current: Box::new(ProjectionOperationState::VolumeRemove(state.clone())),
                attempted: Box::new(ProjectionOperationState::VolumeRemove(state.clone())),
            })
        }
        VolumeRemoveEvent::Interrupted(evidence) => project_transition(
            id,
            state,
            VolumeRemoveOperationState::Interrupted { evidence },
            VolumeRemoveOperationState::is_terminal,
            transition_allowed,
            ProjectionOperationState::VolumeRemove,
            |state| status(id, namespace_id, volume_name, state, event_sequence),
        ),
    }
}

fn status(
    id: &OperationId,
    namespace_id: &NamespaceId,
    volume_name: &VolumeName,
    state: VolumeRemoveOperationState,
    event_sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::VolumeRemove {
        id: id.clone(),
        namespace_id: namespace_id.clone(),
        volume_name: volume_name.clone(),
        state,
        last_event_sequence: event_sequence,
    }
}

fn transition_allowed(
    current: &VolumeRemoveOperationState,
    attempted: &VolumeRemoveOperationState,
) -> bool {
    match (current, attempted) {
        (
            VolumeRemoveOperationState::Accepted,
            VolumeRemoveOperationState::Running {
                stage: VolumeRemoveRunningStage::RemovingVolumeData,
            },
        )
        | (
            VolumeRemoveOperationState::Running {
                stage: VolumeRemoveRunningStage::RemovingVolumeData,
            },
            VolumeRemoveOperationState::Completed,
        )
        | (
            VolumeRemoveOperationState::Accepted | VolumeRemoveOperationState::Running { .. },
            VolumeRemoveOperationState::Failed { .. }
            | VolumeRemoveOperationState::Interrupted { .. },
        ) => true,
        (
            VolumeRemoveOperationState::Accepted
            | VolumeRemoveOperationState::Running { .. }
            | VolumeRemoveOperationState::Completed
            | VolumeRemoveOperationState::Failed { .. }
            | VolumeRemoveOperationState::Interrupted { .. },
            VolumeRemoveOperationState::Accepted
            | VolumeRemoveOperationState::Running { .. }
            | VolumeRemoveOperationState::Completed,
        )
        | (
            VolumeRemoveOperationState::Completed
            | VolumeRemoveOperationState::Failed { .. }
            | VolumeRemoveOperationState::Interrupted { .. },
            VolumeRemoveOperationState::Failed { .. }
            | VolumeRemoveOperationState::Interrupted { .. },
        ) => false,
    }
}

pub fn project_volume_remove_transition(
    current: &OperationStatus,
    transition: VolumeRemoveTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let OperationStatus::VolumeRemove {
        id,
        namespace_id,
        volume_name,
        state,
        ..
    } = current
    else {
        return Err(kind_mismatch(current, OperationKind::VolumeRemove));
    };
    project_event(
        id,
        namespace_id,
        volume_name,
        state,
        VolumeRemoveEvent::Transition(transition),
        event_sequence,
    )
}
