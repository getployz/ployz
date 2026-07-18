//! Namespace-scoped volume placement and admission planning.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    DeployPlanError, DeployPlanningContext, DeployPlanningInput, DeployPlanningService,
    DeployPlanningTarget, ServiceId, VolumeAdmissionDecision, VolumeAdmissionFailure,
    VolumeAdmissionInput, VolumeName, admit_mounted_volumes, validate_mounted_volume_structure,
};
use crate::ids::MachineId;
use crate::intent::VolumePinState;
use crate::machine::{StorageCapability, StorageTestimony};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VolumePlacement {
    pub(super) machine_id: Option<MachineId>,
}

/// One immutable interpretation of every mounted volume in a namespace
/// deploy. Per-service replica planning consumes these placements and commits
/// without re-reading pins, declarations, eligibility, or testimony.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VolumePlan {
    placements: BTreeMap<ServiceId, VolumePlacement>,
    commits: Vec<VolumePinState>,
    ensures: Vec<VolumePinState>,
}

impl VolumePlan {
    pub(super) fn placement(
        &self,
        service_id: &ServiceId,
    ) -> Result<VolumePlacement, DeployPlanError> {
        self.placements
            .get(service_id)
            .cloned()
            .ok_or_else(|| DeployPlanError::UnknownService {
                service_id: service_id.clone(),
            })
    }

    pub(super) fn commits(&self) -> &[VolumePinState] {
        &self.commits
    }

    pub(super) fn ensures(&self) -> &[VolumePinState] {
        &self.ensures
    }
}

pub(super) fn build_namespace_volume_plan(
    target: &DeployPlanningTarget,
    phases: &[Vec<DeployPlanningInput>],
    context: DeployPlanningContext<'_>,
) -> Result<VolumePlan, DeployPlanError> {
    let durable_pins = canonical_volume_pins(phases);
    let mut assignments = durable_pins
        .iter()
        .filter(|pin| pin.namespace_id() == target.namespace_id())
        .map(|pin| (pin.volume_name().clone(), pin.machine_id().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut services = Vec::new();
    let mut mounted_by_machine = BTreeMap::<MachineId, BTreeSet<VolumeName>>::new();

    // Resolve durable constraints for the whole namespace before live
    // admission. This makes pinned quota and colocation visible even when the
    // pinned service belongs to a later deploy phase.
    for input in phases.iter().flatten() {
        let Some(service) = target.service(&input.service_id) else {
            return Err(DeployPlanError::UnknownService {
                service_id: input.service_id.clone(),
            });
        };
        let mounted = mounted_volume_names(service);
        if mounted.is_empty() {
            services.push(ServiceVolumeTarget {
                service_id: service.service_id().clone(),
                machine_id: None,
                mounted,
                eligible_machines: input.eligible_machines.clone(),
            });
            continue;
        }
        let machine_id = resolve_durable_service_machine(target, service, &mounted, &durable_pins)?;
        let structural_machine = match &machine_id {
            Some(machine_id) => machine_id,
            None => input.eligible_machines.first().ok_or_else(|| {
                DeployPlanError::NoEligibleMachines {
                    service_id: service.service_id().clone(),
                }
            })?,
        };
        validate_mounted_volume_structure(VolumeAdmissionInput {
            namespace_id: target.namespace_id(),
            mounted_volume_names: &mounted,
            declarations: target.volumes(),
            volume_pins: &durable_pins,
            selected_machine_id: structural_machine,
            storage_testimony: StorageTestimony::NoAnswer,
        })
        .map_err(|failure| DeployPlanError::VolumeAdmissionOnMachine {
            service_id: service.service_id().clone(),
            machine_id: structural_machine.clone(),
            failure: Box::new(failure),
        })?;
        if let Some(machine_id) = &machine_id {
            if !input.eligible_machines.contains(machine_id) {
                return Err(DeployPlanError::NoEligibleMachines {
                    service_id: service.service_id().clone(),
                });
            }
            assigned_machine_for_service(service.service_id(), &mounted, &assignments)?;
            for volume_name in &mounted {
                assignments
                    .entry(volume_name.clone())
                    .or_insert_with(|| machine_id.clone());
            }
            mounted_by_machine
                .entry(machine_id.clone())
                .or_default()
                .extend(mounted.iter().cloned());
        }
        services.push(ServiceVolumeTarget {
            service_id: service.service_id().clone(),
            machine_id,
            mounted,
            eligible_machines: input.eligible_machines.clone(),
        });
    }

    // Place unpinned services in stable phase/service order. Each candidate is
    // admitted with the full union already assigned to that machine.
    for service in &mut services {
        if service.mounted.is_empty() {
            continue;
        }
        if service.machine_id.is_some() {
            continue;
        }
        let fixed_machine =
            assigned_machine_for_service(&service.service_id, &service.mounted, &assignments)?;
        let candidates = match fixed_machine {
            Some(machine_id) => {
                if !service.eligible_machines.contains(&machine_id) {
                    return Err(DeployPlanError::NoEligibleMachines {
                        service_id: service.service_id.clone(),
                    });
                }
                vec![machine_id]
            }
            None => service.eligible_machines.clone(),
        };
        let mut first_failure = None;
        let selected = candidates.iter().find_map(|machine_id| {
            let mut mounted = mounted_by_machine
                .get(machine_id)
                .cloned()
                .unwrap_or_default();
            mounted.extend(service.mounted.iter().cloned());
            let mounted = mounted.into_iter().collect::<Vec<_>>();
            match admit_mounted_volumes(VolumeAdmissionInput {
                namespace_id: target.namespace_id(),
                mounted_volume_names: &mounted,
                declarations: target.volumes(),
                volume_pins: &durable_pins,
                selected_machine_id: machine_id,
                storage_testimony: operation_storage_testimony(
                    context.storage_testimony,
                    machine_id,
                ),
            }) {
                Ok(_) => Some(machine_id.clone()),
                Err(failure) => {
                    if first_failure.is_none() {
                        first_failure = Some((machine_id.clone(), failure));
                    }
                    None
                }
            }
        });
        let Some(machine_id) = selected else {
            let Some((machine_id, failure)) = first_failure else {
                return Err(DeployPlanError::NoEligibleMachines {
                    service_id: service.service_id.clone(),
                });
            };
            return Err(DeployPlanError::VolumeAdmissionOnMachine {
                service_id: service.service_id.clone(),
                machine_id,
                failure: Box::new(failure),
            });
        };
        for volume_name in &service.mounted {
            assignments
                .entry(volume_name.clone())
                .or_insert_with(|| machine_id.clone());
        }
        mounted_by_machine
            .entry(machine_id.clone())
            .or_default()
            .extend(service.mounted.iter().cloned());
        service.machine_id = Some(machine_id);
    }

    // The aggregate admission remains the sole producer of durable commits
    // and machine-local ensures.
    let mut commits = Vec::new();
    let mut ensures = Vec::new();
    for (machine_id, mounted) in mounted_by_machine {
        let mounted = mounted.into_iter().collect::<Vec<_>>();
        let decisions = admit_mounted_volumes(VolumeAdmissionInput {
            namespace_id: target.namespace_id(),
            mounted_volume_names: &mounted,
            declarations: target.volumes(),
            volume_pins: &durable_pins,
            selected_machine_id: &machine_id,
            storage_testimony: operation_storage_testimony(context.storage_testimony, &machine_id),
        })
        .map_err(|failure| DeployPlanError::VolumeAdmissionOnMachine {
            service_id: service_for_volume(&services, &mounted, target.status_service_id()),
            machine_id: machine_id.clone(),
            failure: Box::new(failure),
        })?;
        for decision in decisions {
            ensures.push(decision.desired_pin().clone());
            match decision {
                VolumeAdmissionDecision::Existing { .. } => {}
                VolumeAdmissionDecision::NeedsCreation { pin }
                | VolumeAdmissionDecision::NeedsQuotaGrowth {
                    replacement: pin, ..
                } => commits.push(pin),
            }
        }
    }
    commits.sort_by(|left, right| left.volume_name().cmp(right.volume_name()));
    commits.dedup();
    ensures.sort_by(|left, right| {
        left.machine_id()
            .cmp(right.machine_id())
            .then_with(|| left.volume_name().cmp(right.volume_name()))
    });
    ensures.dedup();
    let placements = services
        .into_iter()
        .map(|target| {
            (
                target.service_id,
                VolumePlacement {
                    machine_id: target.machine_id,
                },
            )
        })
        .collect();
    Ok(VolumePlan {
        placements,
        commits,
        ensures,
    })
}

#[derive(Debug)]
struct ServiceVolumeTarget {
    service_id: ServiceId,
    machine_id: Option<MachineId>,
    mounted: Vec<VolumeName>,
    eligible_machines: Vec<MachineId>,
}

fn mounted_volume_names(service: &DeployPlanningService) -> Vec<VolumeName> {
    service
        .runtime()
        .volume_mounts
        .iter()
        .map(|mount| mount.volume_name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn resolve_durable_service_machine(
    target: &DeployPlanningTarget,
    service: &DeployPlanningService,
    mounted: &[VolumeName],
    durable_pins: &[VolumePinState],
) -> Result<Option<MachineId>, DeployPlanError> {
    let mut machines = BTreeSet::new();
    for volume_name in mounted {
        let matching = durable_pins
            .iter()
            .filter(|pin| {
                pin.namespace_id() == target.namespace_id() && pin.volume_name() == volume_name
            })
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [] => {}
            [pin] => {
                machines.insert(pin.machine_id().clone());
            }
            pins => {
                return Err(DeployPlanError::VolumeAdmission {
                    service_id: service.service_id().clone(),
                    failure: VolumeAdmissionFailure::AmbiguousPins {
                        volume_name: volume_name.clone(),
                        pin_count: pins.len(),
                    },
                });
            }
        }
    }
    match machines.into_iter().collect::<Vec<_>>().as_slice() {
        [] => Ok(None),
        [machine_id] => Ok(Some(machine_id.clone())),
        several => Err(DeployPlanError::ConflictingVolumePins {
            service_id: service.service_id().clone(),
            machines: several.to_vec(),
        }),
    }
}

fn assigned_machine_for_service(
    service_id: &ServiceId,
    mounted: &[VolumeName],
    assignments: &BTreeMap<VolumeName, MachineId>,
) -> Result<Option<MachineId>, DeployPlanError> {
    let machines = mounted
        .iter()
        .filter_map(|volume_name| assignments.get(volume_name).cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    match machines.as_slice() {
        [] => Ok(None),
        [machine_id] => Ok(Some(machine_id.clone())),
        several => Err(DeployPlanError::ConflictingVolumePins {
            service_id: service_id.clone(),
            machines: several.to_vec(),
        }),
    }
}

fn service_for_volume(
    services: &[ServiceVolumeTarget],
    volumes: &[VolumeName],
    fallback: ServiceId,
) -> ServiceId {
    services
        .iter()
        .find(|target| target.mounted.iter().any(|name| volumes.contains(name)))
        .map_or(fallback, |target| target.service_id.clone())
}

fn canonical_volume_pins(phases: &[Vec<DeployPlanningInput>]) -> Vec<VolumePinState> {
    let mut pins = Vec::new();
    for pin in phases.iter().flatten().flat_map(|input| &input.volume_pins) {
        if !pins.contains(pin) {
            pins.push(pin.clone());
        }
    }
    pins
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
