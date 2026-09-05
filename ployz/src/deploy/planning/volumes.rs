use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    DockerVolumeId, DockerVolumeName, DockerVolumeStorageObservation, MachineId,
    MachineObservation, MachineTarget, PreservedVolume, ProjectName, RequestedServiceSpec,
    ServiceMode, ServiceName, ServiceObservation, ServicePlacementEligibility, ServiceVolume,
    ServiceVolumeGraph, VolumeSource, machine_matches_target, owned_volume_project,
};

use crate::deploy::{
    DeployOperation, DeploySnapshot, EliminatingConstraint, PlanError, PlanOptions,
};

use super::capacity::CapacityBudget;
use super::placement::{
    HostSockets, ReplicatedCapacityReservation, reserve_replicated_service_demand,
};

/// Planner-internal assignment of Docker Volumes to Machines.
///
/// Anchors override Deploy Snapshot observations for a shared replicated
/// Docker Volume (the chosen Machine is the only legal location). `creates`
/// records missing managed Volumes for the informational preview and lets
/// later specs plan against the chosen location; it is not an executable list.
pub(super) struct VolumePins {
    anchors: BTreeMap<DockerVolumeName, MachineId>,
    creates: Vec<(MachineId, ServiceVolume)>,
    provisioned_plans: BTreeMap<DockerVolumeId, VolumeSource>,
}

impl VolumePins {
    pub(super) fn new() -> Self {
        Self {
            anchors: BTreeMap::new(),
            creates: Vec::new(),
            provisioned_plans: BTreeMap::new(),
        }
    }

    fn constrain(&mut self, name: DockerVolumeName, machine_id: MachineId) {
        self.anchors.insert(name, machine_id);
    }

    fn record_create(&mut self, machine_id: MachineId, volume: &ServiceVolume) {
        // Docker Volume identity on a Machine is the name; driver mismatch is a
        // conflict, not a second create.
        if self.creates.iter().any(|(id, existing)| {
            *id == machine_id && managed_volume_name(existing) == managed_volume_name(volume)
        }) {
            return;
        }
        self.creates.push((machine_id, volume.clone()));
    }

    fn anchor_for(&self, volume: &ServiceVolume) -> Option<MachineId> {
        managed_volume_name(volume).and_then(|name| self.anchors.get(name).copied())
    }

    fn observations<'pins>(
        &'pins self,
        snapshot: &'pins DeploySnapshot,
    ) -> impl Iterator<Item = VolumePresence<'pins>> + 'pins {
        snapshot
            .volume_snapshot
            .observations()
            .iter()
            .map(|observed| VolumePresence {
                machine_id: observed.id.machine_id,
                name: &observed.id.name,
                shape: VolumePresenceShape::Observed(observed),
            })
            .chain(
                self.creates
                    .iter()
                    .filter_map(|(machine_id, volume)| planned_presence(*machine_id, volume)),
            )
    }

    pub(super) fn into_creates_for(
        self,
        operations: &[DeployOperation],
    ) -> Vec<(MachineId, ServiceVolume)> {
        self.creates
            .into_iter()
            .filter(|(machine_id, volume)| {
                operations.iter().any(|operation| {
                    operation.machine_id() == *machine_id
                        && operation.spec().is_some_and(|spec| {
                            spec.volume_graph().mounts().iter().any(|mount| {
                                managed_volume_name(spec.volume_graph().volume_for(mount))
                                    == managed_volume_name(volume)
                            })
                        })
                })
            })
            .collect()
    }
}

/// Bind non-external named volumes to `project`: physical Docker name and ownership labels.
///
/// # Errors
///
/// Returns [`PlanError::ConflictingDockerVolumeDefinitions`] when Project scoping makes two
/// Service Volume aliases describe incompatible sources for one Docker Volume.
pub(super) fn scope_requested(
    mut spec: RequestedServiceSpec,
    project: &ProjectName,
) -> Result<RequestedServiceSpec, PlanError> {
    spec.mount_graph = match spec.mount_graph.scope_to_project(project) {
        Ok(graph) => graph,
        Err(ployz_core::ServiceVolumeGraphError::IncompatibleVolumeAliases { name }) => {
            return Err(PlanError::ConflictingDockerVolumeDefinitions { name });
        }
        Err(error) => unreachable!("scoping preserves Service Volume graph references: {error}"),
    };
    Ok(spec)
}

impl VolumePins {
    pub(super) fn validate_provisioned_volume_definitions(
        &mut self,
        target: &[RequestedServiceSpec],
        snapshot: &DeploySnapshot,
    ) -> Result<(), PlanError> {
        let name_errors_with_service = target.len() > 1;
        for spec in target {
            if !spec.volume_graph().has_mounted_provisioned_volume() {
                continue;
            }
            let result = (|| {
                let candidates = super::placement_candidates(spec, snapshot)?;
                self.record_provisioned(spec, &candidates)?;
                let mut machines = candidates
                    .into_iter()
                    .filter(|machine| {
                        spec.placement_eligibility(&machine.machine, machine.storage.as_ref())
                            == ServicePlacementEligibility::Eligible
                    })
                    .collect::<Vec<_>>();
                if machines.is_empty() {
                    return Ok(());
                }
                self.validate_provisioned_volumes(spec, &machines, snapshot)?;
                volume_constraints(spec, snapshot, self, &mut machines)?;
                Ok(())
            })();
            result.map_err(|source| {
                super::service_error(name_errors_with_service, spec.name.as_str(), source)
            })?;
        }
        Ok(())
    }

    fn record_provisioned(
        &mut self,
        spec: &RequestedServiceSpec,
        machines: &[&MachineObservation],
    ) -> Result<(), PlanError> {
        for volume in spec.volume_graph().mounted_provisioned_volumes() {
            let ployz_core::RawVolumeSource::Provisioned { name, .. } = volume.source.kind() else {
                unreachable!("mounted_provisioned_volumes filters source kinds")
            };
            for machine in machines {
                let id = DockerVolumeId {
                    machine_id: machine.machine.id,
                    name: name.clone(),
                };
                if let Some(existing) = self.provisioned_plans.get(&id)
                    && existing != &volume.source
                {
                    return Err(PlanError::ConflictingDockerVolumeDefinitions {
                        name: name.clone(),
                    });
                }
                self.provisioned_plans.insert(id, volume.source.clone());
            }
        }
        Ok(())
    }

    fn validate_provisioned_volumes(
        &self,
        spec: &RequestedServiceSpec,
        machines: &[&MachineObservation],
        snapshot: &DeploySnapshot,
    ) -> Result<(), PlanError> {
        for volume in spec.volume_graph().mounted_provisioned_volumes() {
            let ployz_core::RawVolumeSource::Provisioned {
                name,
                maximum_bytes,
                ..
            } = volume.source.kind()
            else {
                unreachable!("mounted_provisioned_volumes filters source kinds")
            };
            for machine in machines {
                let Some(existing) =
                    snapshot
                        .volume_snapshot
                        .observations()
                        .iter()
                        .find(|existing| {
                            existing.id.machine_id == machine.machine.id
                                && existing.id.name == *name
                        })
                else {
                    continue;
                };
                if !matches!(
                    existing.storage,
                    DockerVolumeStorageObservation::Provisioned { .. }
                ) {
                    return Err(PlanError::ExistingPlainVolume {
                        name: name.clone(),
                        machine: machine.machine.name.clone(),
                    });
                }
                if !volume.source.matches_managed_volume(existing) {
                    return Err(PlanError::ExistingProvisionedVolumeMismatch {
                        name: name.clone(),
                        machine: machine.machine.name.clone(),
                        maximum_bytes: *maximum_bytes,
                    });
                }
            }
        }
        Ok(())
    }
}

/// Owned Compose-declared Docker Volumes omitted from this Deploy's target.
pub(super) fn preserved_owned_volumes(
    project_name: &ProjectName,
    target: &[RequestedServiceSpec],
    snapshot: &DeploySnapshot,
) -> Vec<PreservedVolume> {
    let declared = declared_physical_names(target);
    let mut preserved = Vec::new();
    for volume in snapshot.volume_snapshot.observations() {
        if owned_volume_project(&volume.labels).as_ref() != Some(project_name) {
            continue;
        }
        if declared.contains(&volume.id.name) {
            continue;
        }
        let machine_name = snapshot
            .machines
            .iter()
            .find(|machine| machine.machine.id == volume.id.machine_id)
            .map(|machine| machine.machine.name.clone());
        preserved.push(PreservedVolume {
            id: volume.id.clone(),
            machine_name,
        });
    }
    preserved.sort_by(|left, right| {
        left.id
            .name
            .cmp(&right.id.name)
            .then_with(|| left.id.machine_id.cmp(&right.id.machine_id))
    });
    preserved
}

fn declared_physical_names(target: &[RequestedServiceSpec]) -> BTreeSet<DockerVolumeName> {
    target
        .iter()
        .flat_map(|spec| spec.volume_graph().mounted_volumes())
        .filter_map(|volume| match volume.source.kind() {
            ployz_core::RawVolumeSource::Ordinary { name, .. }
            | ployz_core::RawVolumeSource::Provisioned { name, .. } => Some(name.clone()),
            ployz_core::RawVolumeSource::External { .. }
            | ployz_core::RawVolumeSource::Bind { .. }
            | ployz_core::RawVolumeSource::Tmpfs { .. } => None,
        })
        .collect()
}

#[derive(Clone, Copy)]
struct VolumePresence<'volume> {
    machine_id: MachineId,
    name: &'volume DockerVolumeName,
    shape: VolumePresenceShape<'volume>,
}

#[derive(Clone, Copy)]
enum VolumePresenceShape<'volume> {
    Observed(&'volume ployz_core::DockerVolume),
    Planned(&'volume VolumeSource),
}

fn planned_presence(machine_id: MachineId, volume: &ServiceVolume) -> Option<VolumePresence<'_>> {
    let name = managed_volume_name(volume)?;
    Some(VolumePresence {
        machine_id,
        name,
        shape: VolumePresenceShape::Planned(&volume.source),
    })
}

impl VolumePresence<'_> {
    fn matches(self, volume: &ServiceVolume) -> bool {
        if managed_volume_name(volume) != Some(self.name) {
            return false;
        }
        match self.shape {
            VolumePresenceShape::Observed(observed) => {
                volume.source.matches_managed_volume(observed)
            }
            VolumePresenceShape::Planned(source) => {
                volume.source.to_create_volume_request() == source.to_create_volume_request()
            }
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct ManagedVolumeUse<'service> {
    service_name: &'service str,
    service: &'service RequestedServiceSpec,
    volume: &'service ServiceVolume,
    global: bool,
}

pub(super) fn managed_volume_uses(
    requested: &[RequestedServiceSpec],
) -> BTreeMap<DockerVolumeName, Vec<ManagedVolumeUse<'_>>> {
    let mut uses = BTreeMap::<DockerVolumeName, Vec<ManagedVolumeUse<'_>>>::new();
    for spec in requested {
        let service_name = spec.name.as_str();
        for mount in spec.volume_graph().mounts() {
            let volume = spec.volume_graph().volume_for(mount);
            let Some(name) = managed_volume_name(volume) else {
                continue;
            };
            let uses = uses.entry(name.clone()).or_default();
            if !uses
                .iter()
                .any(|volume_use| volume_use.service_name == service_name)
            {
                uses.push(ManagedVolumeUse {
                    service_name,
                    service: spec,
                    volume,
                    global: matches!(spec.mode, ServiceMode::Global),
                });
            }
        }
    }
    uses
}

pub(super) fn reject_mixed_volume_modes(
    volume_uses: &BTreeMap<DockerVolumeName, Vec<ManagedVolumeUse<'_>>>,
) -> Result<(), PlanError> {
    for (name, uses) in volume_uses {
        if let (Some(global), Some(replicated)) = (
            uses.iter().find(|volume_use| volume_use.global),
            uses.iter().find(|volume_use| !volume_use.global),
        ) {
            return Err(PlanError::MixedVolumeModes {
                name: name.clone(),
                global: global.service_name.into(),
                replicated: replicated.service_name.into(),
            });
        }
    }
    Ok(())
}

struct SharedVolumeComponent<'volume_use> {
    volumes: Vec<(
        &'volume_use DockerVolumeName,
        &'volume_use Vec<ManagedVolumeUse<'volume_use>>,
    )>,
}

fn shared_volume_components<'volume_use>(
    volume_uses: &'volume_use BTreeMap<DockerVolumeName, Vec<ManagedVolumeUse<'volume_use>>>,
) -> Vec<SharedVolumeComponent<'volume_use>> {
    let mut remaining = volume_uses
        .iter()
        .filter(|(_, uses)| uses.len() > 1 && uses.iter().all(|volume_use| !volume_use.global))
        .collect::<Vec<_>>();
    let mut components = Vec::new();
    while !remaining.is_empty() {
        let mut component = SharedVolumeComponent {
            volumes: vec![remaining.remove(0)],
        };
        while let Some(index) = remaining
            .iter()
            .position(|(_, uses)| shares_a_service(&component, uses))
        {
            component.volumes.push(remaining.remove(index));
        }
        components.push(component);
    }
    components
}

fn shares_a_service(
    component: &SharedVolumeComponent<'_>,
    candidate: &[ManagedVolumeUse<'_>],
) -> bool {
    candidate.iter().any(|candidate| {
        component.volumes.iter().any(|(_, uses)| {
            uses.iter()
                .any(|volume_use| volume_use.service_name == candidate.service_name)
        })
    })
}

pub(super) fn prepare_shared_replicated_volumes(
    volume_uses: &BTreeMap<DockerVolumeName, Vec<ManagedVolumeUse<'_>>>,
    snapshot: &DeploySnapshot,
    requested: &[RequestedServiceSpec],
    observed_services: &[ServiceObservation],
    pins: &mut VolumePins,
    capacity: &mut CapacityBudget,
    options: &PlanOptions,
) -> Result<Vec<(ServiceName, ReplicatedCapacityReservation)>, PlanError> {
    let mut reservations = Vec::new();
    let mut sockets = HostSockets::from_snapshot(snapshot);
    for component in shared_volume_components(volume_uses) {
        let anchor = shared_component_anchor(
            &component,
            snapshot,
            requested,
            observed_services,
            pins,
            (capacity, &mut sockets),
            options,
        )?;
        reservations.extend(anchor.capacity_reservations);
        pin_shared_component(&component, anchor.machine_id, snapshot, pins);
    }
    Ok(reservations)
}

struct SharedAnchor {
    machine_id: MachineId,
    capacity_reservations: Vec<(ServiceName, ReplicatedCapacityReservation)>,
}

fn shared_component_anchor(
    component: &SharedVolumeComponent<'_>,
    snapshot: &DeploySnapshot,
    requested: &[RequestedServiceSpec],
    observed_services: &[ServiceObservation],
    pins: &VolumePins,
    budget: (&mut CapacityBudget, &mut HostSockets),
    options: &PlanOptions,
) -> Result<SharedAnchor, PlanError> {
    let (capacity, sockets) = budget;
    let services = component
        .volumes
        .iter()
        .flat_map(|(_, uses)| uses.iter())
        .map(|volume_use| (volume_use.service_name, volume_use.service))
        .collect::<BTreeMap<_, _>>();
    let mut service_iter = services.iter();
    let (&first_service_name, &first_service) = service_iter
        .next()
        .expect("shared Volume component has at least two services");
    let mut eligible = volume_eligible_machine_ids(first_service, snapshot, pins, options)
        .map_err(|source| super::service_error(true, first_service_name, source))?;
    for (&service_name, &service) in service_iter {
        let other_eligible = volume_eligible_machine_ids(service, snapshot, pins, options)
            .map_err(|source| super::service_error(true, service_name, source))?;
        eligible.retain(|machine_id| other_eligible.contains(machine_id));
    }
    if eligible.is_empty() {
        return Err(super::service_error(
            true,
            first_service_name,
            no_eligible_shared(component, snapshot, pins),
        ));
    }
    let mut admission_error = capacity.error_for(&eligible);
    for machine_id in eligible {
        let mut projected = capacity.clone();
        let mut projected_sockets = sockets.clone();
        let projected_services = requested
            .iter()
            .filter(|spec| services.contains_key(spec.name.as_str()))
            .map(|spec| {
                let observed = observed_services
                    .iter()
                    .find(|service| service.identity.name == spec.name);
                reserve_replicated_service_demand(
                    &mut projected,
                    &mut projected_sockets,
                    spec,
                    observed,
                    machine_id,
                    options,
                )
                .map(|reservation| (spec.name.clone(), reservation))
            })
            .collect::<Result<Vec<_>, _>>();
        let capacity_reservations = match projected_services {
            Ok(reservations) => reservations,
            Err(error) => {
                admission_error = error;
                continue;
            }
        };
        *capacity = projected;
        *sockets = projected_sockets;
        return Ok(SharedAnchor {
            machine_id,
            capacity_reservations,
        });
    }
    Err(super::service_error(
        true,
        first_service_name,
        admission_error,
    ))
}

fn pin_shared_component(
    component: &SharedVolumeComponent<'_>,
    machine_id: MachineId,
    snapshot: &DeploySnapshot,
    pins: &mut VolumePins,
) {
    for (name, uses) in &component.volumes {
        pins.constrain((*name).clone(), machine_id);
        if pins
            .observations(snapshot)
            .any(|located| located.machine_id == machine_id && located.name == *name)
        {
            continue;
        }
        let first_use = uses.first().expect("shared Volume has at least two uses");
        pins.record_create(machine_id, first_use.volume);
    }
}

fn volume_eligible_machine_ids(
    spec: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    options: &PlanOptions,
) -> Result<Vec<MachineId>, PlanError> {
    let mut machines = super::eligible_machines(spec, snapshot, options)?;
    planned_volume_constraints(spec, snapshot, pins, &mut machines)?;
    Ok(machines
        .into_iter()
        .map(|machine| machine.machine.id)
        .collect())
}

pub(super) fn plan_volume_operations(
    spec: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &mut VolumePins,
    machines: &mut Vec<&MachineObservation>,
) -> Result<(), PlanError> {
    // TODO: preserve the placement ceiling: do not filter by memory, image platform, or local image presence.
    let (mounted_volumes, missing_volumes) =
        planned_volume_constraints(spec, snapshot, pins, machines)?;
    match spec.mode {
        ServiceMode::Replicated { .. } if !missing_volumes.is_empty() => {
            // TODO: named-volume driver and label options remain part of planned creation;
            // revisit only if Ployz changes to externally managed volumes exclusively.
            let machine_id = machines
                .first()
                .map(|machine| machine.machine.id)
                .expect("volume_constraints returns a Machine when it succeeds");
            machines.retain(|machine| machine.machine.id == machine_id);
            for volume in missing_volumes {
                if let Some(name) = managed_volume_name(volume) {
                    pins.constrain(name.clone(), machine_id);
                }
                pins.record_create(machine_id, volume);
            }
        }
        ServiceMode::Global => {
            for machine in machines.iter() {
                for volume in mounted_volumes.iter().copied() {
                    if pins.observations(snapshot).any(|located| {
                        located.machine_id == machine.machine.id && located.matches(volume)
                    }) {
                        continue;
                    }
                    pins.record_create(machine.machine.id, volume);
                }
            }
        }
        ServiceMode::Replicated { .. } => {}
    }
    pins.record_provisioned(spec, machines)?;
    Ok(())
}

pub(super) fn constrain_volume_candidates(
    spec: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    machines: &mut Vec<&MachineObservation>,
) -> Result<(), PlanError> {
    planned_volume_constraints(spec, snapshot, pins, machines).map(|_| ())
}

fn planned_volume_constraints<'spec>(
    spec: &'spec RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    machines: &mut Vec<&MachineObservation>,
) -> Result<(Vec<&'spec ServiceVolume>, Vec<&'spec ServiceVolume>), PlanError> {
    pins.validate_provisioned_volumes(spec, machines, snapshot)?;
    volume_constraints(spec, snapshot, pins, machines)
}

fn volume_constraints<'spec>(
    spec: &'spec RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    machines: &mut Vec<&MachineObservation>,
) -> Result<(Vec<&'spec ServiceVolume>, Vec<&'spec ServiceVolume>), PlanError> {
    let mounted_volumes = mounted_managed_volumes(spec.volume_graph());
    let incomplete = machines.iter().find_map(|machine| {
        snapshot
            .volume_snapshot
            .machine_gap(machine.machine.id)
            .map(|message| (machine.machine.id, machine.machine.name.clone(), message))
    });
    if !mounted_volumes.is_empty() {
        machines.retain(|machine| {
            snapshot
                .volume_snapshot
                .machine_gap(machine.machine.id)
                .is_none()
        });
    }
    if machines.is_empty()
        && let Some((machine_id, machine, message)) = incomplete
        && let Some(name) = mounted_volumes
            .first()
            .and_then(|volume| managed_volume_name(volume))
    {
        return Err(PlanError::DockerVolumeUnavailable {
            id: DockerVolumeId {
                machine_id,
                name: name.clone(),
            },
            message: format!("Machine '{machine}' {message}"),
        });
    }
    if let Some((id, message)) = snapshot.volume_snapshot.named_gap(|id| {
        machines
            .iter()
            .any(|machine| machine.machine.id == id.machine_id)
            && mounted_volumes
                .iter()
                .filter_map(|volume| managed_volume_name(volume))
                .any(|name| name == &id.name)
    }) {
        return Err(PlanError::DockerVolumeUnavailable { id, message });
    }
    let mut missing_volumes = Vec::new();
    for volume in mounted_volumes.iter().copied() {
        machines.retain(|machine| {
            !pins.observations(snapshot).any(|located| {
                located.machine_id == machine.machine.id
                    && managed_volume_name(volume) == Some(located.name)
                    && !located.matches(volume)
            })
        });
        if matches!(spec.mode, ServiceMode::Replicated { .. }) {
            if let Some(anchor) = pins.anchor_for(volume) {
                machines.retain(|machine| machine.machine.id == anchor);
                continue;
            }
            let locations = pins
                .observations(snapshot)
                .filter(|located| located.matches(volume))
                .map(|located| located.machine_id)
                .collect::<BTreeSet<_>>();
            if !locations.is_empty() {
                machines.retain(|machine| locations.contains(&machine.machine.id));
            } else {
                missing_volumes.push(volume);
            }
        }
    }
    if machines.is_empty() {
        // ponytail: name the filter that emptied the set; no per-Machine matrix.
        let requested = &spec.placement.machines;
        return Err(PlanError::no_eligible_machines(
            mounted_volumes
                .iter()
                .filter_map(|volume| managed_volume_name(volume))
                .filter_map(|name| volume_anchor(snapshot, pins, name, requested))
                .collect(),
        ));
    }
    Ok((mounted_volumes, missing_volumes))
}

fn no_eligible_shared(
    component: &SharedVolumeComponent<'_>,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
) -> PlanError {
    PlanError::no_eligible_machines(
        component
            .volumes
            .iter()
            .filter_map(|(name, uses)| {
                let mut requested = Vec::new();
                for volume_use in uses.iter() {
                    for target in &volume_use.service.placement.machines {
                        if !requested.contains(target) {
                            requested.push(target.clone());
                        }
                    }
                }
                if requested.is_empty() {
                    volume_anchor(snapshot, pins, name, &requested)
                } else {
                    Some(EliminatingConstraint::SharedVolumeNoCommonMachine {
                        volume: (*name).clone(),
                        requested,
                    })
                }
            })
            .collect(),
    )
}

fn volume_anchor(
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    name: &DockerVolumeName,
    requested: &[MachineTarget],
) -> Option<EliminatingConstraint> {
    let mut located_on = Vec::new();
    for located in pins
        .observations(snapshot)
        .filter(|located| located.name == name)
    {
        let Some(machine_name) = snapshot
            .machines
            .iter()
            .find(|machine| machine.machine.id == located.machine_id)
            .map(|machine| machine.machine.name.clone())
        else {
            continue;
        };
        if !located_on.contains(&machine_name) {
            located_on.push(machine_name);
        }
    }
    let hits_located = requested.iter().any(|target| {
        snapshot.machines.iter().any(|machine| {
            located_on.contains(&machine.machine.name)
                && machine_matches_target(&machine.machine, target)
        })
    });
    if located_on.is_empty() {
        if requested.is_empty() {
            None
        } else {
            Some(EliminatingConstraint::SharedVolumeNoCommonMachine {
                volume: name.clone(),
                requested: requested.to_vec(),
            })
        }
    } else if requested.is_empty() || hits_located {
        Some(EliminatingConstraint::VolumeAlreadyOn {
            volume: name.clone(),
            located_on,
        })
    } else {
        Some(EliminatingConstraint::VolumeConflictsWithPlacement {
            volume: name.clone(),
            located_on,
            requested: requested.to_vec(),
        })
    }
}

fn managed_volume_name(volume: &ServiceVolume) -> Option<&DockerVolumeName> {
    match volume.source.kind() {
        ployz_core::RawVolumeSource::Ordinary { name, .. }
        | ployz_core::RawVolumeSource::Provisioned { name, .. } => Some(name),
        ployz_core::RawVolumeSource::External { .. }
        | ployz_core::RawVolumeSource::Bind { .. }
        | ployz_core::RawVolumeSource::Tmpfs { .. } => None,
    }
}

fn mounted_managed_volumes(graph: &ServiceVolumeGraph) -> Vec<&ServiceVolume> {
    let mut by_docker_name = BTreeMap::<&DockerVolumeName, &ServiceVolume>::new();
    for volume in graph.mounted_volumes() {
        let Some(name) = managed_volume_name(volume) else {
            continue;
        };
        by_docker_name.entry(name).or_insert(volume);
    }
    by_docker_name.into_values().collect()
}
