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
    let CollectedVolumeTargets { unmounted, mounted } =
        collect_services(target, phases, &durable_pins)?;
    let constrained = connected_components(mounted)
        .into_iter()
        .map(constrain_component)
        .collect::<Result<Vec<_>, _>>()?;
    let placed = place_components(target, &durable_pins, context, constrained)?;
    let MaterializedPlacements {
        placements,
        mounted_by_machine,
    } = materialize_placements(&unmounted, &placed);

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
            service_id: service_for_volume(&placed, &mounted, target.status_service_id()),
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
    order: usize,
    service_id: ServiceId,
    mounted: BTreeSet<VolumeName>,
    eligible_machines: Vec<MachineId>,
    durable_machine: Option<MachineId>,
}

struct CollectedVolumeTargets {
    unmounted: Vec<ServiceId>,
    mounted: Vec<ServiceVolumeTarget>,
}

#[derive(Debug)]
struct VolumeComponent {
    order: usize,
    first: ServiceVolumeTarget,
    remaining: Vec<ServiceVolumeTarget>,
    mounted: BTreeSet<VolumeName>,
}

impl VolumeComponent {
    fn members(&self) -> impl Iterator<Item = &ServiceVolumeTarget> {
        std::iter::once(&self.first).chain(&self.remaining)
    }
}

enum ConstrainedComponent {
    Fixed {
        component: VolumeComponent,
        machine_id: MachineId,
    },
    Unpinned {
        component: VolumeComponent,
        first_candidate: MachineId,
        remaining_candidates: Vec<MachineId>,
    },
}

struct PlacedComponent {
    component: VolumeComponent,
    machine_id: MachineId,
}

struct UnpinnedComponent {
    component: VolumeComponent,
    first_candidate: MachineId,
    remaining_candidates: Vec<MachineId>,
}

struct MaterializedPlacements {
    placements: BTreeMap<ServiceId, VolumePlacement>,
    mounted_by_machine: BTreeMap<MachineId, BTreeSet<VolumeName>>,
}

fn collect_services(
    target: &DeployPlanningTarget,
    phases: &[Vec<DeployPlanningInput>],
    durable_pins: &[VolumePinState],
) -> Result<CollectedVolumeTargets, DeployPlanError> {
    let mut unmounted = Vec::new();
    let mut mounted_targets = Vec::new();
    for (order, input) in phases.iter().flatten().enumerate() {
        let Some(service) = target.service(&input.service_id) else {
            return Err(DeployPlanError::UnknownService {
                service_id: input.service_id.clone(),
            });
        };
        let mounted = mounted_volume_names(service);
        if mounted.is_empty() {
            unmounted.push(service.service_id().clone());
            continue;
        }
        let durable_machine =
            resolve_durable_machine(target, service.service_id(), &mounted, durable_pins)?;
        let structural_machine = durable_machine
            .clone()
            .or_else(|| input.eligible_machines.first().cloned())
            .ok_or_else(|| DeployPlanError::NoEligibleMachines {
                service_id: service.service_id().clone(),
            })?;
        let mounted_names = mounted.iter().cloned().collect::<Vec<_>>();
        validate_mounted_volume_structure(VolumeAdmissionInput {
            namespace_id: target.namespace_id(),
            mounted_volume_names: &mounted_names,
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
        mounted_targets.push(ServiceVolumeTarget {
            order,
            service_id: service.service_id().clone(),
            mounted,
            eligible_machines: input.eligible_machines.clone(),
            durable_machine,
        });
    }
    Ok(CollectedVolumeTargets {
        unmounted,
        mounted: mounted_targets,
    })
}

fn connected_components(mut services: Vec<ServiceVolumeTarget>) -> Vec<VolumeComponent> {
    let mut components = Vec::new();
    while !services.is_empty() {
        let first = services.remove(0);
        let order = first.order;
        let mut mounted = first.mounted.clone();
        let mut members = Vec::new();

        loop {
            let mut absorbed = Vec::new();
            let mut unconnected = Vec::new();
            for service in services {
                if service.mounted.iter().any(|name| mounted.contains(name)) {
                    mounted.extend(service.mounted.iter().cloned());
                    absorbed.push(service);
                } else {
                    unconnected.push(service);
                }
            }
            services = unconnected;
            if absorbed.is_empty() {
                break;
            }
            members.extend(absorbed);
        }

        members.sort_by_key(|service| service.order);
        components.push(VolumeComponent {
            order,
            first,
            remaining: members,
            mounted,
        });
    }
    components
}

fn constrain_component(
    component: VolumeComponent,
) -> Result<ConstrainedComponent, DeployPlanError> {
    let mut fixed_machine = None;
    let mut machines = BTreeSet::new();
    for service in component.members() {
        let Some(machine_id) = &service.durable_machine else {
            continue;
        };
        machines.insert(machine_id.clone());
        if fixed_machine
            .as_ref()
            .is_some_and(|fixed| fixed != machine_id)
        {
            return Err(DeployPlanError::ConflictingVolumePins {
                service_id: service.service_id.clone(),
                machines: machines.into_iter().collect(),
            });
        }
        fixed_machine = Some(machine_id.clone());
    }

    if let Some(machine_id) = fixed_machine {
        for service in component.members() {
            if !service.eligible_machines.contains(&machine_id) {
                return Err(DeployPlanError::NoEligibleMachines {
                    service_id: service.service_id.clone(),
                });
            }
        }
        return Ok(ConstrainedComponent::Fixed {
            component,
            machine_id,
        });
    }

    let mut candidates = component.first.eligible_machines.clone();
    for service in &component.remaining {
        candidates.retain(|machine_id| service.eligible_machines.contains(machine_id));
    }
    let [first_candidate, remaining_candidates @ ..] = candidates.as_slice() else {
        return Err(DeployPlanError::NoEligibleMachines {
            service_id: component.first.service_id.clone(),
        });
    };
    Ok(ConstrainedComponent::Unpinned {
        component,
        first_candidate: first_candidate.clone(),
        remaining_candidates: remaining_candidates.to_vec(),
    })
}

fn place_components(
    target: &DeployPlanningTarget,
    durable_pins: &[VolumePinState],
    context: DeployPlanningContext<'_>,
    constrained: Vec<ConstrainedComponent>,
) -> Result<Vec<PlacedComponent>, DeployPlanError> {
    let mut placed = Vec::new();
    let mut unpinned = Vec::new();
    for component in constrained {
        match component {
            ConstrainedComponent::Fixed {
                component,
                machine_id,
            } => placed.push(PlacedComponent {
                component,
                machine_id,
            }),
            ConstrainedComponent::Unpinned {
                component,
                first_candidate,
                remaining_candidates,
            } => unpinned.push(UnpinnedComponent {
                component,
                first_candidate,
                remaining_candidates,
            }),
        }
    }

    for unpinned in unpinned {
        let UnpinnedComponent {
            component,
            first_candidate,
            remaining_candidates,
        } = unpinned;
        let mut selected = None;
        let mut first_failure = None;
        for machine_id in std::iter::once(first_candidate).chain(remaining_candidates) {
            let mut mounted = component.mounted.clone();
            for reservation in &placed {
                if reservation.machine_id == machine_id {
                    mounted.extend(reservation.component.mounted.iter().cloned());
                }
            }
            let mounted_names = mounted.into_iter().collect::<Vec<_>>();
            match admit_mounted_volumes(VolumeAdmissionInput {
                namespace_id: target.namespace_id(),
                mounted_volume_names: &mounted_names,
                declarations: target.volumes(),
                volume_pins: durable_pins,
                selected_machine_id: &machine_id,
                storage_testimony: operation_storage_testimony(
                    context.storage_testimony,
                    &machine_id,
                ),
            }) {
                Ok(_) => {
                    selected = Some(machine_id);
                    break;
                }
                Err(failure) if first_failure.is_none() => {
                    first_failure = Some((machine_id, failure));
                }
                Err(_) => {}
            }
        }
        let Some(machine_id) = selected else {
            let Some((machine_id, failure)) = first_failure else {
                return Err(DeployPlanError::NoEligibleMachines {
                    service_id: component.first.service_id,
                });
            };
            return Err(DeployPlanError::VolumeAdmissionOnMachine {
                service_id: component.first.service_id,
                machine_id,
                failure: Box::new(failure),
            });
        };
        placed.push(PlacedComponent {
            component,
            machine_id,
        });
    }

    placed.sort_by_key(|placed| placed.component.order);
    Ok(placed)
}

fn materialize_placements(
    unmounted: &[ServiceId],
    components: &[PlacedComponent],
) -> MaterializedPlacements {
    let mut placements = unmounted
        .iter()
        .cloned()
        .map(|service_id| (service_id, VolumePlacement { machine_id: None }))
        .collect::<BTreeMap<_, _>>();
    let mut mounted_by_machine = BTreeMap::<MachineId, BTreeSet<VolumeName>>::new();
    for placed in components {
        for service in placed.component.members() {
            placements.insert(
                service.service_id.clone(),
                VolumePlacement {
                    machine_id: Some(placed.machine_id.clone()),
                },
            );
        }
        mounted_by_machine
            .entry(placed.machine_id.clone())
            .or_default()
            .extend(placed.component.mounted.iter().cloned());
    }
    MaterializedPlacements {
        placements,
        mounted_by_machine,
    }
}

fn mounted_volume_names(service: &DeployPlanningService) -> BTreeSet<VolumeName> {
    service
        .runtime()
        .volume_mounts
        .iter()
        .map(|mount| mount.volume_name.clone())
        .collect()
}

fn resolve_durable_machine(
    target: &DeployPlanningTarget,
    service_id: &ServiceId,
    mounted: &BTreeSet<VolumeName>,
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
    components: &[PlacedComponent],
    volumes: &[VolumeName],
    fallback: ServiceId,
) -> ServiceId {
    components
        .iter()
        .flat_map(|placed| placed.component.members())
        .find(|service| service.mounted.iter().any(|name| volumes.contains(name)))
        .map_or(fallback, |service| service.service_id.clone())
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
