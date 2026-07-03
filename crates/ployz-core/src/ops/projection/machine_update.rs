use super::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, machine_update_mismatch,
};
use crate::ids::{MachineId, OperationId};
use crate::install::InstallArtifactVersion;
use crate::ops::classification::{MachineUpdateEvent, OperationSubjectRef};
use crate::ops::{EventSequence, MachineUpdateOperationState, OperationStatus};

#[derive(Clone, Copy)]
pub(super) struct MachineUpdateFields<'status> {
    pub(super) id: &'status OperationId,
    pub(super) machine_id: &'status MachineId,
    pub(super) target_version: &'status InstallArtifactVersion,
    pub(super) state: &'status MachineUpdateOperationState,
}

impl MachineUpdateFields<'_> {
    fn status_with(
        &self,
        state: MachineUpdateOperationState,
        event_sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::MachineUpdate {
            id: self.id.clone(),
            machine_id: self.machine_id.clone(),
            target_version: self.target_version.clone(),
            state,
            last_event_sequence: event_sequence,
        }
    }
}

pub(super) fn project_event(
    fields: MachineUpdateFields<'_>,
    subject: OperationSubjectRef,
    event: MachineUpdateEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if subject != OperationSubjectRef::MachineUpdate(fields.machine_id.clone()) {
        return Err(machine_update_mismatch(
            fields.id,
            fields.machine_id,
            subject,
        ));
    }
    match event {
        MachineUpdateEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
        MachineUpdateEvent::Transition(transition) => {
            project_state(fields, transition.state(), event_sequence)
        }
    }
}

pub(super) fn project_state(
    fields: MachineUpdateFields<'_>,
    attempted: MachineUpdateOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if fields.state == &attempted {
        return Ok(OperationProjection::AlreadySatisfied);
    }
    if fields.state.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineUpdate(
                fields.state.clone(),
            )),
            attempted: Box::new(ProjectionOperationState::MachineUpdate(attempted)),
        });
    }
    if !transition_allowed(fields.state, &attempted) {
        return Err(StatusProjectionError::InvalidTransition {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineUpdate(
                fields.state.clone(),
            )),
            attempted: Box::new(ProjectionOperationState::MachineUpdate(attempted)),
        });
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(fields.status_with(attempted, event_sequence)),
    })
}

fn transition_allowed(
    current: &MachineUpdateOperationState,
    attempted: &MachineUpdateOperationState,
) -> bool {
    match (current, attempted) {
        (
            MachineUpdateOperationState::Accepted,
            MachineUpdateOperationState::Running | MachineUpdateOperationState::Cancelled { .. },
        )
        | (
            MachineUpdateOperationState::Running,
            MachineUpdateOperationState::Completed { .. }
            | MachineUpdateOperationState::Failed { .. }
            | MachineUpdateOperationState::Cancelled { .. },
        ) => true,
        (
            MachineUpdateOperationState::Accepted
            | MachineUpdateOperationState::Running
            | MachineUpdateOperationState::Completed { .. }
            | MachineUpdateOperationState::Failed { .. }
            | MachineUpdateOperationState::Cancelled { .. },
            MachineUpdateOperationState::Accepted
            | MachineUpdateOperationState::Running
            | MachineUpdateOperationState::Completed { .. }
            | MachineUpdateOperationState::Failed { .. }
            | MachineUpdateOperationState::Cancelled { .. },
        ) => false,
    }
}
