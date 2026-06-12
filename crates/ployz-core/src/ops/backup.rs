use serde::{Deserialize, Serialize};

use crate::backup::{BackupArtifact, BackupManifest};
use crate::ids::OperationId;
use crate::ops::{CancellationReason, EventSequence, FailureMessage, OperationStatus};

use super::projection::{OperationProjection, ProjectionOperationState, StatusProjectionError};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "stage", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackupRunningStage {
    SnapshottingControlPlane,
    WritingManifest { artifact: BackupArtifact },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackupOperationState {
    Accepted,
    Running { stage: BackupRunningStage },
    Completed { manifest: BackupManifest },
    Failed { failure: BackupOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl BackupOperationState {
    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed { .. } | Self::Failed { .. } | Self::Cancelled { .. } => true,
            Self::Accepted | Self::Running { .. } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BackupOperationFailure {
    SnapshotFailed { message: FailureMessage },
    ManifestWriteFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupTransition {
    Running { stage: BackupRunningStage },
    Completed { manifest: BackupManifest },
    Failed { failure: BackupOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl BackupTransition {
    #[must_use]
    pub fn state(&self) -> BackupOperationState {
        match self {
            Self::Running { stage } => BackupOperationState::Running {
                stage: stage.clone(),
            },
            Self::Completed { manifest } => BackupOperationState::Completed {
                manifest: manifest.clone(),
            },
            Self::Failed { failure } => BackupOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => BackupOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

#[must_use]
pub const fn transition_allowed(
    current: &BackupOperationState,
    attempted: &BackupOperationState,
) -> bool {
    match (current, attempted) {
        (
            BackupOperationState::Accepted,
            BackupOperationState::Running {
                stage: BackupRunningStage::SnapshottingControlPlane,
            }
            | BackupOperationState::Failed { .. }
            | BackupOperationState::Cancelled { .. },
        )
        | (
            BackupOperationState::Running {
                stage: BackupRunningStage::SnapshottingControlPlane,
            },
            BackupOperationState::Running {
                stage: BackupRunningStage::WritingManifest { .. },
            }
            | BackupOperationState::Failed { .. }
            | BackupOperationState::Cancelled { .. },
        ) => true,
        (
            BackupOperationState::Running {
                stage: BackupRunningStage::WritingManifest { .. },
            },
            BackupOperationState::Completed { .. }
            | BackupOperationState::Failed { .. }
            | BackupOperationState::Cancelled { .. },
        ) => true,
        (
            BackupOperationState::Accepted,
            BackupOperationState::Accepted
            | BackupOperationState::Completed { .. }
            | BackupOperationState::Running {
                stage: BackupRunningStage::WritingManifest { .. },
            },
        )
        | (
            BackupOperationState::Running {
                stage: BackupRunningStage::SnapshottingControlPlane,
            },
            BackupOperationState::Accepted
            | BackupOperationState::Completed { .. }
            | BackupOperationState::Running {
                stage: BackupRunningStage::SnapshottingControlPlane,
            },
        )
        | (
            BackupOperationState::Running {
                stage: BackupRunningStage::WritingManifest { .. },
            },
            BackupOperationState::Accepted
            | BackupOperationState::Running {
                stage:
                    BackupRunningStage::SnapshottingControlPlane
                    | BackupRunningStage::WritingManifest { .. },
            },
        )
        | (
            BackupOperationState::Completed { .. }
            | BackupOperationState::Failed { .. }
            | BackupOperationState::Cancelled { .. },
            _,
        ) => false,
    }
}

pub(super) enum BackupEvent {
    Submitted,
    Transition(BackupTransition),
}

pub(super) fn project_event(
    id: &OperationId,
    state: &BackupOperationState,
    event: BackupEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        BackupEvent::Submitted => Ok(OperationProjection::StatusChanged {
            status: Box::new(status_cursor(id, state, event_sequence)),
        }),
        BackupEvent::Transition(transition) => {
            let attempted = transition.state();
            if state == &attempted {
                return Ok(OperationProjection::AlreadySatisfied);
            }
            transition_projection(id, state, transition, event_sequence)
        }
    }
}

pub(super) fn transition_projection(
    id: &OperationId,
    state: &BackupOperationState,
    transition: BackupTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let attempted = transition.state();

    if state.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: id.clone(),
            current: Box::new(ProjectionOperationState::Backup(state.clone())),
            attempted: Box::new(ProjectionOperationState::Backup(attempted)),
        });
    }
    if !transition_allowed(state, &attempted) {
        return Err(StatusProjectionError::InvalidTransition {
            operation_id: id.clone(),
            current: Box::new(ProjectionOperationState::Backup(state.clone())),
            attempted: Box::new(ProjectionOperationState::Backup(attempted)),
        });
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(OperationStatus::Backup {
            id: id.clone(),
            state: attempted,
            last_event_sequence: event_sequence,
        }),
    })
}

fn status_cursor(
    id: &OperationId,
    state: &BackupOperationState,
    event_sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::Backup {
        id: id.clone(),
        state: state.clone(),
        last_event_sequence: event_sequence,
    }
}
