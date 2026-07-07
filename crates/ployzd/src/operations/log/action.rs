use super::{
    DeployOperationSubmission, MachineLifecycleOperationSubmission, MachineLifecyclePayload,
    MachineUpdateOperationSubmission, MachineUpdatePayload,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{EventSequence, OperationEvent, OperationKind, OperationStatus};

pub(super) trait OperationAction: Sized {
    type Payload: Clone + Send + 'static;
    const KIND: OperationKind;
    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent;
    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)>;
    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus;
}

impl OperationAction for DeployOperationSubmission {
    type Payload = ployz_core::deploy::DeployRequest;
    const KIND: OperationKind = OperationKind::Deploy;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::DeploySubmitted {
            operation_id,
            target: payload,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::DeploySubmitted {
            operation_id,
            target,
        } = event
        else {
            return None;
        };
        Some((operation_id, target))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::deploy_accepted(
            operation_id,
            payload.namespace_id.clone(),
            payload.status_service_id(),
            sequence,
        )
    }
}

impl OperationAction for MachineUpdateOperationSubmission {
    type Payload = MachineUpdatePayload;
    const KIND: OperationKind = OperationKind::MachineUpdate;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::MachineUpdateSubmitted {
            operation_id,
            machine_id: payload.machine_id,
            target_version: payload.target_version,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::MachineUpdateSubmitted {
            operation_id,
            machine_id,
            target_version,
        } = event
        else {
            return None;
        };
        Some((
            operation_id,
            MachineUpdatePayload {
                machine_id,
                target_version,
            },
        ))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::machine_update_accepted(
            operation_id,
            payload.machine_id.clone(),
            payload.target_version.clone(),
            sequence,
        )
    }
}

impl OperationAction for MachineLifecycleOperationSubmission {
    type Payload = MachineLifecyclePayload;
    const KIND: OperationKind = OperationKind::MachineLifecycle;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::MachineLifecycleSubmitted {
            operation_id,
            machine_id: payload.machine_id,
            target: payload.target,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::MachineLifecycleSubmitted {
            operation_id,
            machine_id,
            target,
        } = event
        else {
            return None;
        };
        Some((operation_id, MachineLifecyclePayload { machine_id, target }))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::machine_lifecycle_accepted(
            operation_id,
            payload.machine_id.clone(),
            payload.target,
            sequence,
        )
    }
}
