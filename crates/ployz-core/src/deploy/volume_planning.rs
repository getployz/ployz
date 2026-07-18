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
    let services = collect_services(target, phases, &durable_pins)?;
    let mut components = connected_components(&services);
    resolve_component_constraints(target, &services, &durable_pins, &mut components)?;
    place_unpinned_components(target, &services, &durable_pins, context, &mut components)?;
    let MaterializedPlacements {
        placements,
        mounted_by_machine,
    } = materialize_placements(&services, &components)?;

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
    Ok(VolumePlan {
        placements,
        commits,
        ensures,
    })
}

#[derive(Debug)]
struct ServiceVolumeTarget {
    service_id: ServiceId,
    mounted: Vec<VolumeName>,
    eligible_machines: Vec<MachineId>,
}

#[derive(Debug)]
struct VolumeComponent {
    members: Vec<usize>,
    mounted: BTreeSet<VolumeName>,
    candidates: Vec<MachineId>,
    machine_id: Option<MachineId>,
}

struct MaterializedPlacements {
    placements: BTreeMap<ServiceId, VolumePlacement>,
    mounted_by_machine: BTreeMap<MachineId, BTreeSet<VolumeName>>,
}

fn collect_services(
    target: &DeployPlanningTarget,
    phases: &[Vec<DeployPlanningInput>],
    durable_pins: &[VolumePinState],
) -> Result<Vec<ServiceVolumeTarget>, DeployPlanError> {
    phases
        .iter()
        .flatten()
        .map(|input| {
            let Some(service) = target.service(&input.service_id) else {
                return Err(DeployPlanError::UnknownService {
                    service_id: input.service_id.clone(),
                });
            };
            let mounted = mounted_volume_names(service);
            if !mounted.is_empty() {
                let structural_machine =
                    resolve_durable_service_machine(target, service, &mounted, durable_pins)?
                        .or_else(|| input.eligible_machines.first().cloned())
                        .ok_or_else(|| DeployPlanError::NoEligibleMachines {
                            service_id: service.service_id().clone(),
                        })?;
                validate_mounted_volume_structure(VolumeAdmissionInput {
                    namespace_id: target.namespace_id(),
                    mounted_volume_names: &mounted,
                    declarations: target.volumes(),
                    volume_pins: durable_pins,
                    selected_machine_id: &structural_machine,
                    storage_testimony: StorageTestimony::NoAnswer,
                })
                .map_err(|failure| DeployPlanError::VolumeAdmissionOnMachine {
                    service_id: service.service_id().clone(),
                    machine_id: structural_machine,
                    failure: Box::new(failure),
                })?;
            }
            Ok(ServiceVolumeTarget {
                service_id: service.service_id().clone(),
                mounted,
                eligible_machines: input.eligible_machines.clone(),
            })
        })
        .collect()
}

fn connected_components(services: &[ServiceVolumeTarget]) -> Vec<VolumeComponent> {
    let mut services_by_volume = BTreeMap::<VolumeName, Vec<usize>>::new();
    for (index, service) in services.iter().enumerate() {
        for volume_name in &service.mounted {
            services_by_volume
                .entry(volume_name.clone())
                .or_default()
                .push(index);
        }
    }

    let mut visited = vec![false; services.len()];
    let mut components = Vec::new();
    for (start, service) in services.iter().enumerate() {
        let Some(is_visited) = visited.get(start) else {
            unreachable!("visited entries match collected services");
        };
        if *is_visited || service.mounted.is_empty() {
            continue;
        }
        let mut pending = BTreeSet::from([start]);
        let mut members = Vec::new();
        let mut mounted = BTreeSet::new();
        while let Some(index) = pending.pop_first() {
            let Some(was_visited) = visited.get_mut(index) else {
                unreachable!("volume adjacency contains only collected services");
            };
            if std::mem::replace(was_visited, true) {
                continue;
            }
            members.push(index);
            let Some(service) = services.get(index) else {
                unreachable!("volume adjacency contains only collected services");
            };
            for volume_name in &service.mounted {
                mounted.insert(volume_name.clone());
                if let Some(linked) = services_by_volume.get(volume_name) {
                    pending.extend(linked.iter().copied().filter(|linked| {
                        let Some(visited) = visited.get(*linked) else {
                            unreachable!("volume adjacency contains only collected services");
                        };
                        !visited
                    }));
                }
            }
        }
        components.push(VolumeComponent {
            members,
            mounted,
            candidates: Vec::new(),
            machine_id: None,
        });
    }
    components
}

fn resolve_component_constraints(
    target: &DeployPlanningTarget,
    services: &[ServiceVolumeTarget],
    durable_pins: &[VolumePinState],
    components: &mut [VolumeComponent],
) -> Result<(), DeployPlanError> {
    for component in components {
        let mut fixed_machine = None;
        let mut machines = BTreeSet::new();
        for &member in &component.members {
            let Some(service) = services.get(member) else {
                unreachable!("component members come from collected services");
            };
            let Some(machine_id) =
                resolve_durable_service_machine_for_target(target, service, durable_pins)?
            else {
                continue;
            };
            machines.insert(machine_id.clone());
            if fixed_machine
                .as_ref()
                .is_some_and(|fixed| fixed != &machine_id)
            {
                return Err(DeployPlanError::ConflictingVolumePins {
                    service_id: service.service_id.clone(),
                    machines: machines.into_iter().collect(),
                });
            }
            fixed_machine = Some(machine_id);
        }

        if let Some(machine_id) = fixed_machine {
            for &member in &component.members {
                let Some(service) = services.get(member) else {
                    unreachable!("component members come from collected services");
                };
                if !service.eligible_machines.contains(&machine_id) {
                    return Err(DeployPlanError::NoEligibleMachines {
                        service_id: service.service_id.clone(),
                    });
                }
            }
            component.candidates = vec![machine_id.clone()];
            component.machine_id = Some(machine_id);
            continue;
        }

        let [first_member, remaining @ ..] = component.members.as_slice() else {
            unreachable!("every volume component has at least one member");
        };
        let Some(first) = services.get(*first_member) else {
            unreachable!("component members come from collected services");
        };
        let mut candidates = first.eligible_machines.clone();
        for &member in remaining {
            let Some(service) = services.get(member) else {
                unreachable!("component members come from collected services");
            };
            candidates.retain(|machine_id| service.eligible_machines.contains(machine_id));
        }
        if candidates.is_empty() {
            return Err(DeployPlanError::NoEligibleMachines {
                service_id: first.service_id.clone(),
            });
        }
        component.candidates = candidates;
    }
    Ok(())
}

fn place_unpinned_components(
    target: &DeployPlanningTarget,
    services: &[ServiceVolumeTarget],
    durable_pins: &[VolumePinState],
    context: DeployPlanningContext<'_>,
    components: &mut [VolumeComponent],
) -> Result<(), DeployPlanError> {
    for index in 0..components.len() {
        let Some(component) = components.get(index) else {
            unreachable!("component indices come from the component slice");
        };
        if component.machine_id.is_some() {
            continue;
        }
        let Some(first_member) = component.members.first() else {
            unreachable!("every volume component has at least one member");
        };
        let Some(first_service) = services.get(*first_member) else {
            unreachable!("component members come from collected services");
        };
        let service_id = first_service.service_id.clone();
        let candidates = component.candidates.clone();
        let component_mounted = component.mounted.clone();
        let mut first_failure = None;
        for machine_id in candidates {
            let mut mounted = component_mounted.clone();
            for component in components.iter() {
                if component.machine_id.as_ref() == Some(&machine_id) {
                    mounted.extend(component.mounted.iter().cloned());
                }
            }
            let mounted = mounted.into_iter().collect::<Vec<_>>();
            match admit_mounted_volumes(VolumeAdmissionInput {
                namespace_id: target.namespace_id(),
                mounted_volume_names: &mounted,
                declarations: target.volumes(),
                volume_pins: durable_pins,
                selected_machine_id: &machine_id,
                storage_testimony: operation_storage_testimony(
                    context.storage_testimony,
                    &machine_id,
                ),
            }) {
                Ok(_) => {
                    let Some(component) = components.get_mut(index) else {
                        unreachable!("component indices come from the component slice");
                    };
                    component.machine_id = Some(machine_id);
                    break;
                }
                Err(failure) if first_failure.is_none() => {
                    first_failure = Some((machine_id, failure));
                }
                Err(_) => {}
            }
        }
        if components
            .get(index)
            .is_some_and(|component| component.machine_id.is_none())
        {
            let Some((machine_id, failure)) = first_failure else {
                return Err(DeployPlanError::NoEligibleMachines { service_id });
            };
            return Err(DeployPlanError::VolumeAdmissionOnMachine {
                service_id,
                machine_id,
                failure: Box::new(failure),
            });
        }
    }
    Ok(())
}

fn materialize_placements(
    services: &[ServiceVolumeTarget],
    components: &[VolumeComponent],
) -> Result<MaterializedPlacements, DeployPlanError> {
    let mut placements = services
        .iter()
        .map(|service| {
            (
                service.service_id.clone(),
                VolumePlacement { machine_id: None },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut mounted_by_machine = BTreeMap::<MachineId, BTreeSet<VolumeName>>::new();
    for component in components {
        let Some(machine_id) = &component.machine_id else {
            let Some(first_member) = component.members.first() else {
                unreachable!("every volume component has at least one member");
            };
            let Some(service) = services.get(*first_member) else {
                unreachable!("component members come from collected services");
            };
            return Err(DeployPlanError::NoEligibleMachines {
                service_id: service.service_id.clone(),
            });
        };
        for &member in &component.members {
            let Some(service) = services.get(member) else {
                unreachable!("component members come from collected services");
            };
            let Some(placement) = placements.get_mut(&service.service_id) else {
                return Err(DeployPlanError::UnknownService {
                    service_id: service.service_id.clone(),
                });
            };
            placement.machine_id = Some(machine_id.clone());
        }
        mounted_by_machine
            .entry(machine_id.clone())
            .or_default()
            .extend(component.mounted.iter().cloned());
    }
    Ok(MaterializedPlacements {
        placements,
        mounted_by_machine,
    })
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
    resolve_durable_machine(target, service.service_id(), mounted, durable_pins)
}

fn resolve_durable_service_machine_for_target(
    target: &DeployPlanningTarget,
    service: &ServiceVolumeTarget,
    durable_pins: &[VolumePinState],
) -> Result<Option<MachineId>, DeployPlanError> {
    resolve_durable_machine(target, &service.service_id, &service.mounted, durable_pins)
}

fn resolve_durable_machine(
    target: &DeployPlanningTarget,
    service_id: &ServiceId,
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
                    service_id: service_id.clone(),
                    failure: VolumeAdmissionFailure::AmbiguousPins {
                        volume_name: volume_name.clone(),
                        pin_count: pins.len(),
                    },
                });
            }
        }
    }
    let machines = machines.into_iter().collect::<Vec<_>>();
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
