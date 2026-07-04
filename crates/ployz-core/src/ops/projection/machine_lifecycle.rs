use super::{
    OperationProjection, ProjectionOperationState, StatusProjectionError,
    machine_lifecycle_mismatch,
};
use crate::ids::{MachineId, OperationId};
use crate::ops::classification::{MachineLifecycleEvent, OperationSubjectRef};
use crate::ops::{EventSequence, MachineLifecycleOperationState, OperationStatus};
use crate::state::MachineLifecycle;

#[derive(Clone, Copy)]
pub(super) struct MachineLifecycleFields<'status> {
    pub(super) id: &'status OperationId,
    pub(super) machine_id: &'status MachineId,
    pub(super) target: MachineLifecycle,
    pub(super) state: &'status MachineLifecycleOperationState,
}

impl MachineLifecycleFields<'_> {
    fn status_with(
        &self,
        state: MachineLifecycleOperationState,
        event_sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::MachineLifecycle {
            id: self.id.clone(),
            machine_id: self.machine_id.clone(),
            target: self.target,
            state,
            last_event_sequence: event_sequence,
        }
    }
}

pub(super) fn project_event(
    fields: MachineLifecycleFields<'_>,
    subject: OperationSubjectRef,
    event: MachineLifecycleEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if subject != OperationSubjectRef::MachineLifecycle(fields.machine_id.clone()) {
        return Err(machine_lifecycle_mismatch(
            fields.id,
            fields.machine_id,
            subject,
        ));
    }
    match event {
        MachineLifecycleEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
        MachineLifecycleEvent::Transition(transition) => {
            project_state(fields, transition.state(), event_sequence)
        }
    }
}

pub(super) fn project_state(
    fields: MachineLifecycleFields<'_>,
    attempted: MachineLifecycleOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if fields.state == &attempted {
        return Ok(OperationProjection::AlreadySatisfied);
    }
    if fields.state.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineLifecycle(
                fields.state.clone(),
            )),
            attempted: Box::new(ProjectionOperationState::MachineLifecycle(attempted)),
        });
    }
    if !transition_allowed(fields.state, &attempted) {
        return Err(StatusProjectionError::InvalidTransition {
            operation_id: fields.id.clone(),
            current: Box::new(ProjectionOperationState::MachineLifecycle(
                fields.state.clone(),
            )),
            attempted: Box::new(ProjectionOperationState::MachineLifecycle(attempted)),
        });
    }

    Ok(OperationProjection::StatusChanged {
        status: Box::new(fields.status_with(attempted, event_sequence)),
    })
}

fn transition_allowed(
    current: &MachineLifecycleOperationState,
    attempted: &MachineLifecycleOperationState,
) -> bool {
    match (current, attempted) {
        (
            MachineLifecycleOperationState::Accepted,
            MachineLifecycleOperationState::Completed
            | MachineLifecycleOperationState::Failed { .. }
            | MachineLifecycleOperationState::Cancelled { .. },
        ) => true,
        (MachineLifecycleOperationState::Accepted, MachineLifecycleOperationState::Accepted)
        | (
            MachineLifecycleOperationState::Completed
            | MachineLifecycleOperationState::Failed { .. }
            | MachineLifecycleOperationState::Cancelled { .. },
            _,
        ) => false,
    }
}
