//! The Volume Create operation: admit and pin one operator-directed volume,
//! then ensure its machine-local durable shape.

use serde::{Deserialize, Serialize};

use crate::deploy::{VolumeName, VolumeSpec};
use crate::ids::{MachineId, NamespaceId, OperationId};
use crate::intent::VolumePinState;
use crate::machine::VolumeEnsureFailure;

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, kind_mismatch,
    project_transition,
};
use super::{
    EventSequence, OperationInterruptionCause, OperationInterruptionEvidence,
    OperationInterruptionStage, OperationKind, OperationStatus,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct VolumeCreateRequest {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
    pub machine_id: MachineId,
    pub spec: VolumeSpec,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum VolumeCreateRunningStage {
    CommittingPin,
    EnsuringVolume,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeCreateOperationState {
    Accepted,
    Planning,
    Running {
        stage: VolumeCreateRunningStage,
    },
    Completed,
    Failed {
        failure: VolumeCreateFailure,
    },
    Interrupted {
        evidence: OperationInterruptionEvidence,
    },
}

impl VolumeCreateOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed | Self::Failed { .. } | Self::Interrupted { .. } => true,
            Self::Accepted | Self::Planning | Self::Running { .. } => false,
        }
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: OperationInterruptionCause,
    ) -> Option<OperationInterruptionEvidence> {
        let stage = match self {
            Self::Accepted => OperationInterruptionStage::VolumeCreateAccepted,
            Self::Planning => OperationInterruptionStage::VolumeCreatePlanning,
            Self::Running { stage } => {
                OperationInterruptionStage::VolumeCreateRunning { stage: *stage }
            }
            Self::Completed | Self::Failed { .. } | Self::Interrupted { .. } => return None,
        };
        Some(OperationInterruptionEvidence::new(cause, stage))
    }

    pub(super) const fn interrupted(evidence: OperationInterruptionEvidence) -> Self {
        Self::Interrupted { evidence }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VolumeCreateFailure {
    IntentReadFailed {
        message: super::FailureMessage,
    },
    MachineNotAccepted {
        machine_id: MachineId,
    },
    AdmissionFailed {
        failure: crate::deploy::VolumeAdmissionFailure,
    },
    PinCommitFailed {
        pin: VolumePinState,
        message: super::FailureMessage,
    },
    MachineUnavailable {
        machine_id: MachineId,
        message: super::FailureMessage,
    },
    EnsureFailed {
        machine_id: MachineId,
        volume_name: VolumeName,
        failure: VolumeEnsureFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VolumeCreateTransition {
    Planning,
    Running { stage: VolumeCreateRunningStage },
    Completed,
    Failed { failure: VolumeCreateFailure },
}

impl VolumeCreateTransition {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Planning => OperationEvent::VolumeCreatePlanningStarted {
                operation_id: operation_id.clone(),
            },
            Self::Running { stage } => OperationEvent::VolumeCreateRunning {
                operation_id: operation_id.clone(),
                stage: *stage,
            },
            Self::Completed => OperationEvent::VolumeCreateCompleted {
                operation_id: operation_id.clone(),
            },
            Self::Failed { failure } => OperationEvent::VolumeCreateFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
        }
    }

    #[must_use]
    pub fn state(&self) -> VolumeCreateOperationState {
        match self {
            Self::Planning => VolumeCreateOperationState::Planning,
            Self::Running { stage } => VolumeCreateOperationState::Running { stage: *stage },
            Self::Completed => VolumeCreateOperationState::Completed,
            Self::Failed { failure } => VolumeCreateOperationState::Failed {
                failure: failure.clone(),
            },
        }
    }
}

pub(super) enum VolumeCreateEvent {
    Submitted { request: VolumeCreateRequest },
    Transition(VolumeCreateTransition),
    Cancelled,
}

pub(super) fn project_event(
    request: &VolumeCreateRequest,
    state: &VolumeCreateOperationState,
    event: VolumeCreateEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        VolumeCreateEvent::Submitted { request: actual } => {
            if request != &actual {
                return Err(StatusProjectionError::InvalidTransition {
                    operation_id: request.operation_id.clone(),
                    current: Box::new(ProjectionOperationState::VolumeCreate(state.clone())),
                    attempted: Box::new(ProjectionOperationState::VolumeCreate(state.clone())),
                });
            }
            Ok(OperationProjection::AlreadySatisfied)
        }
        VolumeCreateEvent::Transition(transition) => project_transition(
            &request.operation_id,
            state,
            transition.state(),
            VolumeCreateOperationState::is_terminal,
            transition_allowed,
            ProjectionOperationState::VolumeCreate,
            |state| status(request, state, event_sequence),
        ),
        VolumeCreateEvent::Cancelled => Err(StatusProjectionError::InvalidTransition {
            operation_id: request.operation_id.clone(),
            current: Box::new(ProjectionOperationState::VolumeCreate(state.clone())),
            attempted: Box::new(ProjectionOperationState::VolumeCreate(state.clone())),
        }),
    }
}

fn status(
    request: &VolumeCreateRequest,
    state: VolumeCreateOperationState,
    event_sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::VolumeCreate {
        request: request.clone(),
        state,
        last_event_sequence: event_sequence,
    }
}

fn transition_allowed(
    current: &VolumeCreateOperationState,
    attempted: &VolumeCreateOperationState,
) -> bool {
    match (current, attempted) {
        (VolumeCreateOperationState::Accepted, VolumeCreateOperationState::Planning)
        | (
            VolumeCreateOperationState::Planning,
            VolumeCreateOperationState::Running {
                stage: VolumeCreateRunningStage::CommittingPin,
            },
        )
        | (
            VolumeCreateOperationState::Running {
                stage: VolumeCreateRunningStage::CommittingPin,
            },
            VolumeCreateOperationState::Running {
                stage: VolumeCreateRunningStage::EnsuringVolume,
            },
        )
        | (
            VolumeCreateOperationState::Running {
                stage: VolumeCreateRunningStage::EnsuringVolume,
            },
            VolumeCreateOperationState::Completed,
        )
        | (
            VolumeCreateOperationState::Accepted
            | VolumeCreateOperationState::Planning
            | VolumeCreateOperationState::Running { .. },
            VolumeCreateOperationState::Failed { .. },
        ) => true,
        (_, VolumeCreateOperationState::Interrupted { .. })
        | (
            VolumeCreateOperationState::Accepted
            | VolumeCreateOperationState::Planning
            | VolumeCreateOperationState::Running { .. }
            | VolumeCreateOperationState::Completed
            | VolumeCreateOperationState::Failed { .. }
            | VolumeCreateOperationState::Interrupted { .. },
            VolumeCreateOperationState::Accepted
            | VolumeCreateOperationState::Planning
            | VolumeCreateOperationState::Running { .. }
            | VolumeCreateOperationState::Completed,
        )
        | (
            VolumeCreateOperationState::Completed
            | VolumeCreateOperationState::Failed { .. }
            | VolumeCreateOperationState::Interrupted { .. },
            VolumeCreateOperationState::Failed { .. },
        ) => false,
    }
}

pub fn project_volume_create_transition(
    current: &OperationStatus,
    transition: VolumeCreateTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let OperationStatus::VolumeCreate { request, state, .. } = current else {
        return Err(kind_mismatch(current, OperationKind::VolumeCreate));
    };
    project_event(
        request,
        state,
        VolumeCreateEvent::Transition(transition),
        event_sequence,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_commit_must_precede_ensuring() {
        assert!(!transition_allowed(
            &VolumeCreateOperationState::Planning,
            &VolumeCreateOperationState::Running {
                stage: VolumeCreateRunningStage::EnsuringVolume,
            },
        ));
        assert!(transition_allowed(
            &VolumeCreateOperationState::Running {
                stage: VolumeCreateRunningStage::CommittingPin,
            },
            &VolumeCreateOperationState::Running {
                stage: VolumeCreateRunningStage::EnsuringVolume,
            },
        ));
    }
}
