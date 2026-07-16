//! Namespace-scoped volume placement and admission planning.

use std::collections::{BTreeMap, BTreeSet};

use super::{
    DeployPlanError, DeployPlanningContext, DeployPlanningInput, DeployServiceSpec, ServiceId,
    VolumeAdmissionDecision, VolumeAdmissionFailure, VolumeAdmissionInput,
    VolumeDeclaredDeployRequest, VolumeName, admit_mounted_volumes,
    validate_mounted_volume_structure,
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
    request: &VolumeDeclaredDeployRequest,
    phases: &[Vec<DeployPlanningInput>],
    context: DeployPlanningContext<'_>,
) -> Result<VolumePlan, DeployPlanError> {
    let durable_pins = canonical_volume_pins(phases);
    let mut assignments = durable_pins
        .iter()
        .filter(|pin| pin.namespace_id() == request.namespace_id())
        .map(|pin| (pin.volume_name().clone(), pin.machine_id().clone()))
        .collect::<BTreeMap<_, _>>();
    let mut services = Vec::new();

    // Structural intent is resolved first. Assignments made for an unpinned
    // volume remain visible to every later phase and service in this plan.
    for input in phases.iter().flatten() {
        let Some(service) = request.service(&input.service_id) else {
            return Err(DeployPlanError::UnknownService {
                service_id: input.service_id.clone(),
            });
        };
        let mounted = mounted_volume_names(service);
        if mounted.is_empty() {
            services.push(ServiceVolumeTarget {
                service_id: service.service_id.clone(),
                machine_id: None,
                mounted,
            });
            continue;
        }
        let machine_id = resolve_service_machine(
            request,
            service,
            &mounted,
            &input.eligible_machines,
            &durable_pins,
            &assignments,
        )?;
        validate_mounted_volume_structure(VolumeAdmissionInput {
            namespace_id: request.namespace_id(),
            mounted_volume_names: &mounted,
            declarations: &request.request().volumes,
            volume_pins: &durable_pins,
            selected_machine_id: &machine_id,
            storage_testimony: StorageTestimony::NoAnswer,
        })
        .map_err(|failure| DeployPlanError::VolumeAdmission {
            service_id: service.service_id.clone(),
            failure,
        })?;
        for volume_name in &mounted {
            assignments
                .entry(volume_name.clone())
                .or_insert_with(|| machine_id.clone());
        }
        services.push(ServiceVolumeTarget {
            service_id: service.service_id.clone(),
            machine_id: Some(machine_id),
            mounted,
        });
    }

    // Eligibility follows durable validation and precedes all live testimony.
    for target in &services {
        let Some(machine_id) = &target.machine_id else {
            continue;
        };
        let Some(input) = phases
            .iter()
            .flatten()
            .find(|input| input.service_id == target.service_id)
        else {
            return Err(DeployPlanError::UnknownService {
                service_id: target.service_id.clone(),
            });
        };
        if !input.eligible_machines.contains(machine_id) {
            return Err(DeployPlanError::NoEligibleMachines);
        }
    }

    // One aggregate admission per machine gives every service in this
    // operation the same capacity snapshot and reserves shared volumes once.
    let mut mounted_by_machine = BTreeMap::<MachineId, BTreeSet<VolumeName>>::new();
    for target in &services {
        if let Some(machine_id) = &target.machine_id {
            mounted_by_machine
                .entry(machine_id.clone())
                .or_default()
                .extend(target.mounted.iter().cloned());
        }
    }
    let mut commits = Vec::new();
    let mut ensures = Vec::new();
    for (machine_id, mounted) in mounted_by_machine {
        let mounted = mounted.into_iter().collect::<Vec<_>>();
        let decisions = admit_mounted_volumes(VolumeAdmissionInput {
            namespace_id: request.namespace_id(),
            mounted_volume_names: &mounted,
            declarations: &request.request().volumes,
            volume_pins: &durable_pins,
            selected_machine_id: &machine_id,
            storage_testimony: operation_storage_testimony(context.storage_testimony, &machine_id),
        })
        .map_err(|failure| DeployPlanError::VolumeAdmission {
            service_id: service_for_volume(&services, &mounted, request.status_service_id()),
            failure,
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
}

fn mounted_volume_names(service: &DeployServiceSpec) -> Vec<VolumeName> {
    service
        .runtime
        .volume_mounts
        .iter()
        .map(|mount| mount.volume_name.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn resolve_service_machine(
    request: &VolumeDeclaredDeployRequest,
    service: &DeployServiceSpec,
    mounted: &[VolumeName],
    eligible_machines: &[MachineId],
    durable_pins: &[VolumePinState],
    assignments: &BTreeMap<VolumeName, MachineId>,
) -> Result<MachineId, DeployPlanError> {
    let mut machines = BTreeSet::new();
    for volume_name in mounted {
        let matching = durable_pins
            .iter()
            .filter(|pin| {
                pin.namespace_id() == request.namespace_id() && pin.volume_name() == volume_name
            })
            .collect::<Vec<_>>();
        match matching.as_slice() {
            [] => {
                if let Some(machine_id) = assignments.get(volume_name) {
                    machines.insert(machine_id.clone());
                }
            }
            [pin] => {
                machines.insert(pin.machine_id().clone());
            }
            pins => {
                return Err(DeployPlanError::VolumeAdmission {
                    service_id: service.service_id.clone(),
                    failure: VolumeAdmissionFailure::AmbiguousPins {
                        volume_name: volume_name.clone(),
                        pin_count: pins.len(),
                    },
                });
            }
        }
    }
    match machines.into_iter().collect::<Vec<_>>().as_slice() {
        [] => eligible_machines
            .first()
            .cloned()
            .ok_or(DeployPlanError::NoEligibleMachines),
        [machine_id] => Ok(machine_id.clone()),
        several => Err(DeployPlanError::ConflictingVolumePins {
            service_id: service.service_id.clone(),
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
