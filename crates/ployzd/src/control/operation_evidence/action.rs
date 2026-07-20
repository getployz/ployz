use super::{
    BuildOperationPayload, BuildOperationSubmission, CertOperationPayload, CertOperationSubmission,
    CoreReplaceOperationSubmission, CoreReplacePayload, CredentialGrantOperationSubmission,
    DeployOperationPayload, DeployOperationSubmission, IngressConfigureOperationSubmission,
    MachineBuildCachePruneOperationSubmission, MachineBuildCachePrunePayload,
    MachineLifecycleOperationSubmission, MachineLifecyclePayload,
    MachineStoragePrepareOperationSubmission, MachineStoragePreparePayload,
    MachineUpdateOperationSubmission, MachineUpdatePayload, ManagedDnsReconcileOperationSubmission,
    ManagedDnsReconcilePayload, NamespaceRemoveOperationSubmission, NamespaceRemovePayload,
    NetworkRepairOperationSubmission, NetworkRepairPayload, ServiceRestartOperationSubmission,
    ServiceRestartPayload, VolumeCreateOperationSubmission, VolumeCreatePayload,
    VolumeRemoveOperationSubmission, VolumeRemovePayload,
};
use ployz_core::ids::OperationId;
use ployz_core::operation::{
    CredentialGrantAction, EventSequence, OperationEvent, OperationKind, OperationStatus,
};

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

impl OperationAction for BuildOperationSubmission {
    type Payload = BuildOperationPayload;
    const KIND: OperationKind = OperationKind::Build;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::BuildSubmitted {
            operation_id,
            target: payload.target,
            source: payload.source,
            adapter: payload.adapter,
            platforms: payload.platforms,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::BuildSubmitted {
            operation_id,
            target,
            source,
            adapter,
            platforms,
        } = event
        else {
            return None;
        };
        Some((
            operation_id,
            BuildOperationPayload {
                target,
                source,
                adapter,
                platforms,
            },
        ))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::build_accepted(
            operation_id,
            payload.target.clone(),
            payload.source.clone(),
            payload.adapter.clone(),
            payload.platforms.clone(),
            sequence,
        )
    }
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
            payload.target.request().namespace_id.clone(),
            payload.target.status_service_id(),
            payload.target.request().origin.clone(),
            sequence,
        )
    }
}

impl OperationAction for CredentialGrantOperationSubmission {
    type Payload = CredentialGrantAction;
    const KIND: OperationKind = OperationKind::CredentialGrant;

    fn submitted_event(operation_id: OperationId, action: Self::Payload) -> OperationEvent {
        OperationEvent::CredentialGrantSubmitted {
            operation_id,
            action,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::CredentialGrantSubmitted {
            operation_id,
            action,
        } = event
        else {
            return None;
        };
        Some((operation_id, action))
    }

    fn accepted_status(
        operation_id: OperationId,
        action: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::credential_grant_accepted(operation_id, action.clone(), sequence)
    }
}

impl OperationAction for IngressConfigureOperationSubmission {
    type Payload = ployz_core::ingress::IngressConfiguration;
    const KIND: OperationKind = OperationKind::IngressConfigure;

    fn submitted_event(operation_id: OperationId, configuration: Self::Payload) -> OperationEvent {
        OperationEvent::IngressConfigureSubmitted {
            operation_id,
            configuration,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::IngressConfigureSubmitted {
            operation_id,
            configuration,
        } = event
        else {
            return None;
        };
        Some((operation_id, configuration))
    }

    fn accepted_status(
        operation_id: OperationId,
        configuration: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::ingress_configure_accepted(operation_id, configuration.clone(), sequence)
    }
}

impl OperationAction for CertOperationSubmission {
    type Payload = CertOperationPayload;
    const KIND: OperationKind = OperationKind::Cert;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::CertProvisionSubmitted {
            operation_id,
            cert_id: payload.cert_id,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::CertProvisionSubmitted {
            operation_id,
            cert_id,
        } = event
        else {
            return None;
        };
        Some((operation_id, CertOperationPayload { cert_id }))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::cert_accepted(operation_id, payload.cert_id.clone(), sequence)
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

impl OperationAction for MachineStoragePrepareOperationSubmission {
    type Payload = MachineStoragePreparePayload;
    const KIND: OperationKind = OperationKind::MachineStoragePrepare;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::MachineStoragePrepareSubmitted {
            operation_id,
            machine_id: payload.machine_id,
            requested_pool: payload.requested_pool,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::MachineStoragePrepareSubmitted {
            operation_id,
            machine_id,
            requested_pool,
        } = event
        else {
            return None;
        };
        Some((
            operation_id,
            MachineStoragePreparePayload {
                machine_id,
                requested_pool,
            },
        ))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::machine_storage_prepare_accepted(
            operation_id,
            payload.machine_id.clone(),
            payload.requested_pool.clone(),
            sequence,
        )
    }
}

impl OperationAction for MachineBuildCachePruneOperationSubmission {
    type Payload = MachineBuildCachePrunePayload;
    const KIND: OperationKind = OperationKind::MachineBuildCachePrune;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::MachineBuildCachePruneSubmitted {
            operation_id,
            machine_id: payload.machine_id,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::MachineBuildCachePruneSubmitted {
            operation_id,
            machine_id,
        } = event
        else {
            return None;
        };
        Some((operation_id, MachineBuildCachePrunePayload { machine_id }))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::machine_build_cache_prune_accepted(
            operation_id,
            payload.machine_id.clone(),
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

impl OperationAction for NetworkRepairOperationSubmission {
    type Payload = NetworkRepairPayload;
    const KIND: OperationKind = OperationKind::NetworkRepair;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        let NetworkRepairPayload { target_machine_id } = payload;
        OperationEvent::NetworkRepairSubmitted {
            operation_id,
            target_machine_id,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::NetworkRepairSubmitted {
            operation_id,
            target_machine_id,
        } = event
        else {
            return None;
        };
        Some((operation_id, NetworkRepairPayload { target_machine_id }))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::network_repair_accepted(
            operation_id,
            payload.target_machine_id.clone(),
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

impl OperationAction for VolumeCreateOperationSubmission {
    type Payload = VolumeCreatePayload;
    const KIND: OperationKind = OperationKind::VolumeCreate;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        debug_assert_eq!(operation_id, payload.request.operation_id);
        OperationEvent::VolumeCreateSubmitted {
            request: payload.request,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::VolumeCreateSubmitted { request } = event else {
            return None;
        };
        Some((
            request.operation_id.clone(),
            VolumeCreatePayload { request },
        ))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        debug_assert_eq!(operation_id, payload.request.operation_id);
        OperationStatus::volume_create_accepted(payload.request.clone(), sequence)
    }
}

impl OperationAction for ManagedDnsReconcileOperationSubmission {
    type Payload = ManagedDnsReconcilePayload;
    const KIND: OperationKind = OperationKind::ManagedDnsReconcile;

    fn submitted_event(operation_id: OperationId, payload: Self::Payload) -> OperationEvent {
        OperationEvent::ManagedDnsReconcileSubmitted {
            operation_id,
            subject: payload.subject,
        }
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::ManagedDnsReconcileSubmitted {
            operation_id,
            subject,
        } = event
        else {
            return None;
        };
        Some((operation_id, ManagedDnsReconcilePayload { subject }))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::managed_dns_reconcile_accepted(
            operation_id,
            payload.subject.clone(),
            sequence,
        )
    }
}
