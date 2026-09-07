//! Global catch-up: place observed eligible Globals onto this Machine only.

use std::collections::BTreeMap;

use ployz_core::{
    BridgeEndpointCapacity, ContainerObservation, EnsureGlobalSlotRequest, InspectRequest,
    ListContainersRequest, LiveServices, Machine, MachineId, MachineStorageObservation,
    MachineTarget, ObservedGlobalSlotSpec, QualifiedService, ResolvedServiceSpec, RpcError,
    ServiceObservation, ServicePlacementEligibility, op, service_containers,
};

use crate::{connect::Client, deploy::endpoint_capacity_error, failure::Failure};

/// Catch-up failed after membership committed.
#[derive(Debug)]
pub(crate) struct CatchUpError {
    cause: Failure,
    unresolved: Vec<QualifiedService>,
}

impl CatchUpError {
    /// Record the failure and Globals whose eligibility or running slot is unresolved.
    pub(crate) fn new(cause: Failure, unresolved: Vec<QualifiedService>) -> Self {
        Self { cause, unresolved }
    }
}

pub(crate) trait CatchUpClient {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, Failure>;
    async fn target_storage(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<MachineStorageObservation>, Failure>;
    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure>;
    async fn ensure_global_slot(
        &mut self,
        machine_id: &MachineId,
        request: EnsureGlobalSlotRequest,
    ) -> Result<(), RpcError>;
    /// List Containers directly from the joined Machine for final verification.
    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure>;
}

impl CatchUpClient for Client {
    async fn live_services(&mut self) -> Result<LiveServices<RpcError>, Failure> {
        Client::live_services(self).await.map_err(Into::into)
    }

    async fn target_storage(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<MachineStorageObservation>, Failure> {
        self.read::<op::Inspect>(
            InspectRequest {
                include_storage: true,
                ..Default::default()
            },
            &MachineTarget::from(machine_id),
        )
        .await
        .map(|details| details.storage)
        .map_err(Into::into)
    }

    async fn bridge_capacity(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Option<BridgeEndpointCapacity>, Failure> {
        let details = self
            .read::<op::Inspect>(
                InspectRequest {
                    telemetry: ployz_core::InspectTelemetry::BridgeCapacity,
                    ..Default::default()
                },
                &MachineTarget::from(machine_id),
            )
            .await
            .map_err(Failure::from)?;
        Ok(details.telemetry.map(|telemetry| telemetry.into_bridge()))
    }

    async fn ensure_global_slot(
        &mut self,
        machine_id: &MachineId,
        request: EnsureGlobalSlotRequest,
    ) -> Result<(), RpcError> {
        self.call::<op::EnsureGlobalSlot>(request, Some(&MachineTarget::from(machine_id)))
            .await
            .map(|_| ())
            .map_err(Into::into)
    }

    async fn target_containers(
        &mut self,
        machine_id: &MachineId,
    ) -> Result<Vec<ContainerObservation>, Failure> {
        self.read::<op::ListContainers>(ListContainersRequest {}, &MachineTarget::from(machine_id))
            .await
            .map(|list| list.containers)
            .map_err(Failure::from)
    }
}

pub(crate) fn joined_catch_up_error(error: CatchUpError) -> String {
    let mut message = format!(
        "Machine joined, but Global catch-up is incomplete; it remains a Cluster member. {}",
        error.cause
    );
    if !error.unresolved.is_empty() {
        message.push_str("\nGlobals requiring attention:");
        for identity in error.unresolved {
            if identity == QualifiedService::system_ingress() {
                message.push_str("\n- ployz-system/ingress: run `ployz ingress deploy`.");
            } else {
                message.push_str(&format!(
                    "\n- {identity}: redeploy Project Service `{identity}`."
                ));
            }
        }
    }
    message
}

/// Globals this Machine is eligible for and does not already run.
#[must_use]
pub fn plan_global_catch_up(
    services: &[ServiceObservation],
    this_machine: &Machine,
    storage: Option<&MachineStorageObservation>,
    skip_ingress: bool,
) -> Vec<ObservedGlobalSlotSpec> {
    services
        .iter()
        .filter_map(|service| {
            let slot = eligible_catch_up_slot(service, this_machine, storage)?;
            (!slot.is_running_on(&service.containers, this_machine)).then_some(slot)
        })
        .filter(|slot| !skip_ingress || slot.identity() != &QualifiedService::system_ingress())
        .collect()
}

fn eligible_catch_up_slot(
    service: &ServiceObservation,
    machine: &Machine,
    storage: Option<&MachineStorageObservation>,
) -> Option<ObservedGlobalSlotSpec> {
    let slot = service.observed_global_slot()?;
    (slot.resolved_spec().placement_eligibility(machine, storage)
        == ServicePlacementEligibility::Eligible)
        .then_some(slot)
}

/// Copy every observed eligible Global onto `this_machine` only.
///
/// # Errors
///
/// Fails when listing Services fails, the target Machine does not answer, or
/// any eligible Global cannot be placed, or required storage evidence is unknown.
pub(crate) async fn catch_up_globals<C: CatchUpClient>(
    client: &mut C,
    this_machine: &Machine,
    skip_ingress: bool,
) -> Result<(), CatchUpError> {
    let live = client
        .live_services()
        .await
        .map_err(|error| CatchUpError::new(error, Vec::new()))?;
    let services = live.services();
    let needs_storage = services
        .iter()
        .filter(|service| !skip_ingress || service.identity != QualifiedService::system_ingress())
        .filter_map(ServiceObservation::observed_global_slot_spec)
        .any(|spec| {
            matches!(
                spec.placement_eligibility(this_machine, None),
                ServicePlacementEligibility::Unknown(_)
            )
        });
    let storage_result = if needs_storage {
        client.target_storage(&this_machine.id).await
    } else {
        Ok(None)
    };
    let storage = storage_result.as_ref().ok().and_then(Option::as_ref);
    let unknown = services
        .iter()
        .filter_map(ServiceObservation::observed_global_slot)
        .filter(|slot| !skip_ingress || slot.identity() != &QualifiedService::system_ingress())
        .filter(|slot| {
            matches!(
                slot.resolved_spec()
                    .placement_eligibility(this_machine, storage),
                ServicePlacementEligibility::Unknown(_)
            )
        })
        .map(|slot| slot.identity().clone())
        .collect::<Vec<_>>();
    let initially_eligible = services
        .iter()
        .filter_map(|service| eligible_catch_up_slot(service, this_machine, storage))
        .filter(|slot| !skip_ingress || slot.identity() != &QualifiedService::system_ingress())
        .map(|slot| (slot.identity().clone(), slot))
        .collect::<BTreeMap<_, _>>();
    let slots = plan_global_catch_up(&services, this_machine, storage, skip_ingress);
    let initially_missing: Vec<_> = slots
        .iter()
        .map(|slot| slot.identity().clone())
        .chain(unknown.iter().cloned())
        .collect();
    let endpoint_creates = slots
        .iter()
        .filter(|slot| {
            !service_has_slot(
                &services,
                this_machine,
                slot.identity(),
                slot.resolved_spec(),
            )
        })
        .count();
    if endpoint_creates > 0 {
        let capacity = client
            .bridge_capacity(&this_machine.id)
            .await
            .map_err(|error| CatchUpError::new(error, initially_missing.clone()))?;
        if let Some(error) = endpoint_capacity_error(endpoint_creates, capacity.as_ref()) {
            return Err(CatchUpError::new(
                Failure::usage(error.to_string()),
                initially_missing,
            ));
        }
    }
    if !slots.is_empty() {
        eprintln!("Placing Global Services on this Machine.");
    }
    let mut failures = unknown
        .iter()
        .map(|identity| {
            (
                identity.clone(),
                match &storage_result {
                    Err(error) => format!("storage eligibility is unknown: {error}"),
                    Ok(_) => {
                        "storage eligibility is unknown; restore storage evidence and redeploy"
                            .to_owned()
                    }
                },
            )
        })
        .collect::<Vec<_>>();
    for slot in slots {
        let (identity, resolved_spec) = slot.into_parts();
        let failure_identity = identity.clone();
        if let Err(error) = client
            .ensure_global_slot(
                &this_machine.id,
                EnsureGlobalSlotRequest {
                    project_name: identity.project,
                    resolved_spec,
                },
            )
            .await
        {
            failures.push((failure_identity, error.to_string()));
        }
    }
    let missing_if_unverified = initially_eligible
        .keys()
        .cloned()
        .chain(unknown.iter().cloned())
        .collect();
    let target_containers = client
        .target_containers(&this_machine.id)
        .await
        .map_err(|error| CatchUpError::new(error, missing_if_unverified))?;
    let target_services = service_containers(target_containers);
    let missing = initially_eligible
        .into_iter()
        .filter_map(|(identity, slot)| {
            (!slot.is_running_on(&target_services, this_machine)).then_some(identity)
        })
        .chain(unknown)
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        let details = failures
            .iter()
            .map(|(identity, error)| format!("{identity}: {error}"))
            .collect::<Vec<_>>()
            .join("; ");
        let cause = if details.is_empty() {
            Failure::usage("eligible Globals are not running after catch-up")
        } else {
            Failure::usage(format!("Global catch-up incomplete: {details}"))
        };
        return Err(CatchUpError::new(cause, missing));
    }
    Ok(())
}

fn service_has_slot(
    services: &[ServiceObservation],
    machine: &Machine,
    identity: &QualifiedService,
    spec: &ResolvedServiceSpec,
) -> bool {
    let wanted = spec.serving_shape();
    services
        .iter()
        .flat_map(|service| &service.containers)
        .any(|container| {
            let observation = container.as_observation();
            &observation.identity() == identity
                && observation.machine_id == machine.id
                && observation.resolved_spec.serving_shape() == wanted
        })
}

#[cfg(test)]
#[path = "global_catch_up_tests.rs"]
mod tests;
