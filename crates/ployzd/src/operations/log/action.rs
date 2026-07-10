use super::{
    CoreReplaceOperationSubmission, CoreReplacePayload, DeployOperationPayload,
    DeployOperationSubmission, MachineLifecycleOperationSubmission, MachineLifecyclePayload,
    MachineUpdateOperationSubmission, MachineUpdatePayload, ManagedLeaseOperationSubmission,
    ManagedLeasePayload, NamespaceRemoveOperationSubmission, NamespaceRemovePayload,
    ServiceRestartOperationSubmission, ServiceRestartPayload, VolumeRemoveOperationSubmission,
    VolumeRemovePayload,
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
    type Payload = DeployOperationPayload;
    const KIND: OperationKind = OperationKind::Deploy;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::DeploySubmitted {
            operation_id,
            reservation_id: payload.reservation_id,
            target: payload.target,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::DeploySubmitted {
            operation_id,
            reservation_id,
            target,
        } = event
        else {
            return None;
        };
        Some((
            operation_id,
            DeployOperationPayload {
                reservation_id,
                target,
            },
        ))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::deploy_accepted(
            operation_id,
            payload.target.namespace_id.clone(),
            payload.target.status_service_id(),
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

impl OperationAction for CoreReplaceOperationSubmission {
    type Payload = CoreReplacePayload;
    const KIND: OperationKind = OperationKind::CoreReplace;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::CoreReplaceSubmitted {
            operation_id,
            machine_id: payload.machine_id,
            successor_nats_url: payload.successor_nats_url,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::CoreReplaceSubmitted {
            operation_id,
            machine_id,
            successor_nats_url,
        } = event
        else {
            return None;
        };
        Some((
            operation_id,
            CoreReplacePayload {
                machine_id,
                successor_nats_url,
            },
        ))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::core_replace_accepted(
            operation_id,
            payload.machine_id.clone(),
            payload.successor_nats_url.clone(),
            sequence,
        )
    }
}

impl OperationAction for ServiceRestartOperationSubmission {
    type Payload = ServiceRestartPayload;
    const KIND: OperationKind = OperationKind::ServiceRestart;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::ServiceRestartSubmitted {
            operation_id,
            namespace_id: payload.namespace_id,
            service_id: payload.service_id,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::ServiceRestartSubmitted {
            operation_id,
            namespace_id,
            service_id,
        } = event
        else {
            return None;
        };
        Some((
            operation_id,
            ServiceRestartPayload {
                namespace_id,
                service_id,
            },
        ))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::service_restart_accepted(
            operation_id,
            payload.namespace_id.clone(),
            payload.service_id.clone(),
            sequence,
        )
    }
}

impl OperationAction for NamespaceRemoveOperationSubmission {
    type Payload = NamespaceRemovePayload;
    const KIND: OperationKind = OperationKind::NamespaceRemove;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::NamespaceRemoveSubmitted {
            operation_id,
            namespace_id: payload.namespace_id,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::NamespaceRemoveSubmitted {
            operation_id,
            namespace_id,
        } = event
        else {
            return None;
        };
        Some((operation_id, NamespaceRemovePayload { namespace_id }))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::namespace_remove_accepted(
            operation_id,
            payload.namespace_id.clone(),
            sequence,
        )
    }
}

impl OperationAction for VolumeRemoveOperationSubmission {
    type Payload = VolumeRemovePayload;
    const KIND: OperationKind = OperationKind::VolumeRemove;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::VolumeRemoveSubmitted {
            operation_id,
            namespace_id: payload.namespace_id,
            volume_name: payload.volume_name,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::VolumeRemoveSubmitted {
            operation_id,
            namespace_id,
            volume_name,
        } = event
        else {
            return None;
        };
        Some((
            operation_id,
            VolumeRemovePayload {
                namespace_id,
                volume_name,
            },
        ))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::volume_remove_accepted(
            operation_id,
            payload.namespace_id.clone(),
            payload.volume_name.clone(),
            sequence,
        )
    }
}

impl OperationAction for ManagedLeaseOperationSubmission {
    type Payload = ManagedLeasePayload;
    const KIND: OperationKind = OperationKind::ManagedLease;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::ManagedLeaseSubmitted {
            operation_id,
            subject: payload.subject,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::ManagedLeaseSubmitted {
            operation_id,
            subject,
        } = event
        else {
            return None;
        };
        Some((operation_id, ManagedLeasePayload { subject }))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::managed_lease_accepted(operation_id, payload.subject.clone(), sequence)
    }
}
