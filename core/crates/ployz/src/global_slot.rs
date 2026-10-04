//! A Global's slot on one Machine: its fresh eligibility there, and the removal of its
//! Containers there. Global catch-up and Drain's retirement both act on one slot this way.

use ployz_core::{
    ContainerKind, EnvironmentValues, InspectRequest, ListContainersRequest, MachineId,
    MachineTarget, Namespace, ResolvedServiceSpec, RpcError, RpcErrorCode, ServiceName,
    ServicePlacementEligibility, ServicePlacementUnknownReason, op,
};

use crate::connect::Client;

/// Fresh eligibility of `spec` on `machine_id`, from that Machine's own observation.
///
/// # Errors
///
/// Fails when the Machine does not answer, or does not observe itself participating.
pub(crate) async fn slot_eligibility(
    client: &mut Client,
    machine_id: &MachineId,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
) -> Result<ServicePlacementEligibility, RpcError> {
    let details = client
        .read::<op::Inspect>(
            InspectRequest {
                include_storage: true,
                ..Default::default()
            },
            &MachineTarget::from(machine_id),
        )
        .await?;
    let machine = details
        .machine
        .filter(|machine| {
            machine.id == *machine_id
                && details.phase == ployz_core::LocalMachinePhase::Participating
        })
        .ok_or_else(|| RpcError {
            code: RpcErrorCode::Conflict,
            message: format!("Server {machine_id} does not observe itself participating"),
            details: serde_json::Value::Null,
        })?;
    Ok(spec.placement_eligibility_in_namespace(namespace, &machine, details.storage.as_ref()))
}

/// Stop and remove every Service Container of `namespace`/`name` on `machine_id`.
///
/// # Errors
///
/// Fails at the first listing, stop or removal the Machine does not acknowledge.
pub(crate) async fn remove_slot(
    client: &mut Client,
    machine_id: &MachineId,
    namespace: &Namespace,
    name: &ServiceName,
) -> Result<(), RpcError> {
    let target = MachineTarget::from(machine_id);
    let containers = client
        .read::<op::ListContainers>(
            ListContainersRequest {
                environment: EnvironmentValues::Redacted,
            },
            &target,
        )
        .await?;
    for container in containers.containers.into_iter().filter(|container| {
        container.machine_id == *machine_id
            && container.kind == ContainerKind::ServiceContainer
            && container.namespace == *namespace
            && container.resolved_spec.name == *name
    }) {
        crate::ingress::stop_container(
            client,
            machine_id,
            ployz_core::StopContainerRequest {
                container_id: container.container_id,
                signal: None,
                grace_period_seconds: None,
            },
            None,
        )
        .await?;
        client
            .call::<op::RemoveContainer>(
                ployz_core::RemoveContainerRequest {
                    container_id: container.container_id,
                    remove_volumes: false,
                    force: false,
                },
                Some(&target),
            )
            .await
            .map_err(RpcError::from)?;
    }
    Ok(())
}

/// The refusal for a slot whose eligibility on the Server can't be told.
pub(crate) fn unknown_eligibility(reason: ServicePlacementUnknownReason) -> RpcError {
    let why = match reason {
        ServicePlacementUnknownReason::MissingStorageEvidence => {
            "its storage support was not observed"
        }
    };
    RpcError {
        code: RpcErrorCode::Conflict,
        message: format!("eligibility on the Server is unknown: {why}"),
        details: serde_json::Value::Null,
    }
}
