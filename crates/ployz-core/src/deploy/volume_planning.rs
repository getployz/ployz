//! Namespace-scoped volume placement and admission preflight.

use std::collections::{BTreeMap, BTreeSet};

use super::volume_admission::classify_mounted_volumes;
use super::{
    DeployPlanError, DeployPlanningContext, DeployPlanningInput, DeployServiceSpec,
    VolumeAdmissionDecision, VolumeAdmissionFailure, VolumeAdmissionInput,
    VolumeDeclaredDeployRequest, VolumeName, admit_mounted_volumes,
};
use crate::ids::MachineId;
use crate::intent::VolumePinState;
use crate::machine::{StorageCapability, StorageTestimony};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VolumePlacement {
    pub(super) machine_id: Option<MachineId>,
    pub(super) commits: Vec<VolumePinState>,
}

pub(super) fn canonical_volume_pins(phases: &[Vec<DeployPlanningInput>]) -> Vec<VolumePinState> {
    let mut canonical = Vec::new();
    for pin in phases.iter().flatten().flat_map(|input| &input.volume_pins) {
        if !canonical.contains(pin) {
            canonical.push(pin.clone());
        }
    }
    canonical
}

pub(super) fn preflight_namespace_volume_admission(
    request: &VolumeDeclaredDeployRequest,
    phases: &[Vec<DeployPlanningInput>],
    durable_volume_pins: &[VolumePinState],
    context: DeployPlanningContext<'_>,
) -> Result<(), DeployPlanError> {
    let mut working_volume_pins = durable_volume_pins.to_vec();
    let mut mounted_by_machine = BTreeMap::<MachineId, BTreeSet<VolumeName>>::new();
    for input in phases.iter().flatten() {
        let Some(service) = request.service(&input.service_id) else {
            return Err(DeployPlanError::UnknownService {
                service_id: input.service_id.clone(),
            });
        };
        let Some((mounted_volume_names, selected_machine_id)) = volume_target(
            request,
            service,
            &input.eligible_machines,
            &working_volume_pins,
        )?
        else {
            continue;
        };
        let testimony =
            operation_storage_testimony(context.storage_testimony, &selected_machine_id);
        let classified = classify_mounted_volumes(VolumeAdmissionInput {
            namespace_id: request.namespace_id(),
            mounted_volume_names: &mounted_volume_names,
            declarations: &request.request().volumes,
            volume_pins: &working_volume_pins,
            selected_machine_id: &selected_machine_id,
            storage_testimony: testimony,
        })
        .map_err(|failure| DeployPlanError::VolumeAdmission {
            service_id: service.service_id.clone(),
            failure,
        })?;
        if !input.eligible_machines.contains(&selected_machine_id) {
            return Err(DeployPlanError::NoEligibleMachines);
        }
        let decisions =
            classified
                .complete(testimony)
                .map_err(|failure| DeployPlanError::VolumeAdmission {
                    service_id: service.service_id.clone(),
                    failure,
                })?;
        for decision in decisions {
            if let VolumeAdmissionDecision::NeedsCreation { pin } = decision
                && !working_volume_pins.contains(&pin)
            {
                working_volume_pins.push(pin);
            }
        }
        mounted_by_machine
            .entry(selected_machine_id)
            .or_default()
            .extend(mounted_volume_names);
    }

    let mut provisioning_barrier = None;
    for (machine_id, mounted_volume_names) in mounted_by_machine {
        let mounted_volume_names = mounted_volume_names.into_iter().collect::<Vec<_>>();
        let decisions = admit_mounted_volumes(VolumeAdmissionInput {
            namespace_id: request.namespace_id(),
            mounted_volume_names: &mounted_volume_names,
            declarations: &request.request().volumes,
            volume_pins: durable_volume_pins,
            selected_machine_id: &machine_id,
            storage_testimony: operation_storage_testimony(context.storage_testimony, &machine_id),
        })
        .map_err(|failure| DeployPlanError::VolumeAdmission {
            service_id: request.status_service_id(),
            failure,
        })?;
        // Provisioned admission is not an execution effect. Until a deploy
        // stage can realize these decisions atomically, planning must stop
        // instead of emitting a pin commit or a container step.
        if provisioning_barrier.is_none() {
            provisioning_barrier = decisions.into_iter().find_map(|decision| match decision {
                VolumeAdmissionDecision::Existing { .. } => None,
                VolumeAdmissionDecision::NeedsCreation { pin }
                    if matches!(pin.kind(), crate::intent::VolumeKind::Plain) =>
                {
                    None
                }
                VolumeAdmissionDecision::NeedsCreation { pin }
                | VolumeAdmissionDecision::NeedsQuotaGrowth {
                    replacement: pin, ..
                } => Some(DeployPlanError::ProvisionedVolumeRequiresProvisioning {
                    service_id: request.status_service_id(),
                    volume_name: pin.volume_name().clone(),
                }),
            });
        }
    }
    match provisioning_barrier {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

pub(super) fn volume_placement(
    request: &VolumeDeclaredDeployRequest,
    service: &DeployServiceSpec,
    eligible_machines: &[MachineId],
    volume_pins: &[VolumePinState],
    storage_testimony: &BTreeMap<MachineId, Option<StorageCapability>>,
) -> Result<VolumePlacement, DeployPlanError> {
    let Some((mounted_volume_names, selected_machine_id)) =
        volume_target(request, service, eligible_machines, volume_pins)?
    else {
        return Ok(VolumePlacement {
            machine_id: None,
            commits: Vec::new(),
        });
    };
    let testimony = operation_storage_testimony(storage_testimony, &selected_machine_id);
    let classified = classify_mounted_volumes(VolumeAdmissionInput {
        namespace_id: request.namespace_id(),
        mounted_volume_names: &mounted_volume_names,
        declarations: &request.request().volumes,
        volume_pins,
        selected_machine_id: &selected_machine_id,
        storage_testimony: testimony,
    })
    .map_err(|failure| DeployPlanError::VolumeAdmission {
        service_id: service.service_id.clone(),
        failure,
    })?;
    if !eligible_machines.contains(&selected_machine_id) {
        return Err(DeployPlanError::NoEligibleMachines);
    }
    let decisions =
        classified
            .complete(testimony)
            .map_err(|failure| DeployPlanError::VolumeAdmission {
                service_id: service.service_id.clone(),
                failure,
            })?;
    let mut commits = Vec::new();
    for decision in decisions {
        match decision {
            VolumeAdmissionDecision::Existing { .. } => {}
            VolumeAdmissionDecision::NeedsCreation { pin }
                if matches!(pin.kind(), crate::intent::VolumeKind::Plain) =>
            {
                commits.push(pin);
            }
            VolumeAdmissionDecision::NeedsCreation { pin }
            | VolumeAdmissionDecision::NeedsQuotaGrowth {
                replacement: pin, ..
            } => {
                return Err(DeployPlanError::ProvisionedVolumeRequiresProvisioning {
                    service_id: service.service_id.clone(),
                    volume_name: pin.volume_name().clone(),
                });
            }
        }
    }
    Ok(VolumePlacement {
        machine_id: Some(selected_machine_id),
        commits,
    })
}

fn volume_target(
    request: &VolumeDeclaredDeployRequest,
    service: &DeployServiceSpec,
    eligible_machines: &[MachineId],
    volume_pins: &[VolumePinState],
) -> Result<Option<(Vec<VolumeName>, MachineId)>, DeployPlanError> {
    if service.runtime.volume_mounts.is_empty() {
        return Ok(None);
    }
    let mut mounted_volume_names = service
        .runtime
        .volume_mounts
        .iter()
        .map(|mount| mount.volume_name.clone())
        .collect::<Vec<_>>();
    mounted_volume_names.sort();
    mounted_volume_names.dedup();

    let mut pinned_machines = Vec::new();
    for volume_name in &mounted_volume_names {
        let matching_pins = volume_pins
            .iter()
            .filter(|pin| {
                pin.namespace_id() == request.namespace_id() && pin.volume_name() == volume_name
            })
            .collect::<Vec<_>>();
        if matching_pins.len() > 1 {
            return Err(DeployPlanError::VolumeAdmission {
                service_id: service.service_id.clone(),
                failure: VolumeAdmissionFailure::AmbiguousPins {
                    volume_name: volume_name.clone(),
                    pin_count: matching_pins.len(),
                },
            });
        }
        if let [pin] = matching_pins.as_slice() {
            pinned_machines.push(pin.machine_id().clone());
        }
    }
    pinned_machines.sort();
    pinned_machines.dedup();
    let selected_machine_id = match pinned_machines.as_slice() {
        [] => eligible_machines
            .first()
            .cloned()
            .ok_or(DeployPlanError::NoEligibleMachines)?,
        [machine_id] => machine_id.clone(),
        [_, _, ..] => {
            return Err(DeployPlanError::ConflictingVolumePins {
                service_id: service.service_id.clone(),
                machines: pinned_machines,
            });
        }
    };
    Ok(Some((mounted_volume_names, selected_machine_id)))
}

fn operation_storage_testimony<'a>(
    testimony: &'a BTreeMap<MachineId, Option<StorageCapability>>,
    machine_id: &MachineId,
) -> StorageTestimony<'a> {
    testimony
        .get(machine_id)
        .map_or(StorageTestimony::NoAnswer, |storage| {
            StorageTestimony::Answered(storage.as_ref())
        })
}
