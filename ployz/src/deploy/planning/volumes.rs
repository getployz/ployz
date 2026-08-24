use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    DockerVolumeId, DockerVolumeName, DockerVolumeStorageObservation, MachineId,
    MachineObservation, MachineStorageObservation, MachineTarget, PreservedVolume, ProjectName,
    ProvisionedVolume, ProvisionedVolumeMaximumBytes, RequestedServiceSpec, ServiceMode,
    ServiceName, ServiceObservation, ServiceVolume, ServiceVolumeGraph, VolumeSource,
    machine_matches_target, owned_volume_project,
};

use crate::deploy::{
    DeployOperation, DeploySnapshot, EliminatingConstraint, PlanError, PlanOptions,
};

use super::capacity::CapacityBudget;
use super::placement::{ReplicatedCapacityReservation, reserve_replicated_service_demand};

/// Planner-internal assignment of Docker Volumes to Machines.
///
/// Anchors override Deploy Snapshot observations for a shared replicated
/// Docker Volume (the chosen Machine is the only legal location). `creates` is
/// the one CreateVolume list later specs consult; it is not a reconstructed
/// observation list.
pub(super) struct VolumePins {
    anchors: BTreeMap<DockerVolumeName, MachineId>,
    creates: Vec<(MachineId, ServiceVolume)>,
    provisioned: ProvisionedVolumeBindings,
    provisioned_plans: BTreeMap<DockerVolumeId, ProvisionedVolumeMaximumBytes>,
}

impl VolumePins {
    pub(super) fn new(provisioned: ProvisionedVolumeBindings) -> Self {
        Self {
            anchors: BTreeMap::new(),
            creates: Vec::new(),
            provisioned,
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
            *id == machine_id && named_volume_name(existing) == named_volume_name(volume)
        }) {
            return;
        }
        self.creates.push((machine_id, volume.clone()));
    }

    fn anchor_for(&self, volume: &ServiceVolume) -> Option<MachineId> {
        let VolumeSource::Named { name, .. } = &volume.source else {
            return None;
        };
        self.anchors.get(name).copied()
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
                driver: observed.driver(),
                options: &observed.options,
            })
            .chain(
                self.creates
                    .iter()
                    .filter_map(|(machine_id, volume)| planned_presence(*machine_id, volume)),
            )
    }

    pub(super) fn into_creates(self) -> Vec<DeployOperation> {
        let provisioned_plans = self.provisioned_plans;
        self.creates
            .into_iter()
            .map(|(machine_id, volume)| {
                let provisioned = named_volume_name(&volume).and_then(|name| {
                    provisioned_plans
                        .get(&DockerVolumeId {
                            machine_id,
                            name: name.clone(),
                        })
                        .copied()
                });
                match provisioned {
                    Some(provisioned) => DeployOperation::CreateProvisionedVolume {
                        machine_id,
                        volume,
                        maximum_bytes: provisioned,
                    },
                    None => DeployOperation::CreateVolume { machine_id, volume },
                }
            })
            .collect()
    }
}

/// Bind non-external named volumes to `project`: physical Docker name and ownership labels.
pub(super) fn scope_requested(
    mut spec: RequestedServiceSpec,
    project: &ProjectName,
) -> RequestedServiceSpec {
    let mut volumes = spec.volume_graph.volumes().to_vec();
    let mounts = spec.volume_graph.mounts().to_vec();
    for volume in &mut volumes {
        volume.source.scope_to_project(project);
    }
    spec.volume_graph = ServiceVolumeGraph::parse(volumes, mounts)
        .expect("scoping Docker Volume names does not change Service Volume References");
    spec
}

pub(super) struct ProvisionedVolumeBindings {
    bounds: BTreeMap<ServiceName, BTreeMap<DockerVolumeName, ProvisionedVolumeMaximumBytes>>,
}

impl ProvisionedVolumeBindings {
    pub(super) fn parse(
        target: &[RequestedServiceSpec],
        declarations: &[ProvisionedVolume],
    ) -> Result<Self, PlanError> {
        let mut bounds = BTreeMap::new();
        for declaration in declarations {
            let volume = target
                .iter()
                .find(|spec| spec.name == declaration.service)
                .and_then(|spec| {
                    spec.volume_graph
                        .volumes()
                        .iter()
                        .find(|volume| volume.reference == declaration.reference)
                })
                .ok_or_else(|| PlanError::UnknownProvisionedVolumeReference {
                    service: declaration.service.clone(),
                    reference: declaration.reference.clone(),
                })?;
            let VolumeSource::Named { name, .. } = &volume.source else {
                return Err(PlanError::ProvisionedVolumeReferenceNotNamed {
                    service: declaration.service.clone(),
                    reference: declaration.reference.clone(),
                });
            };
            let service_bounds = bounds
                .entry(declaration.service.clone())
                .or_insert_with(BTreeMap::new);
            if let Some(existing_maximum_bytes) =
                service_bounds.insert(name.clone(), declaration.maximum_bytes)
                && existing_maximum_bytes != declaration.maximum_bytes
            {
                return Err(PlanError::ConflictingProvisionedVolumeBounds {
                    name: name.clone(),
                    existing_maximum_bytes,
                    conflicting_maximum_bytes: declaration.maximum_bytes,
                });
            }
        }
        Ok(Self { bounds })
    }

    fn bound_for(
        &self,
        service: &ServiceName,
        name: &DockerVolumeName,
    ) -> Option<ProvisionedVolumeMaximumBytes> {
        self.bounds
            .get(service)
            .and_then(|bounds| bounds.get(name))
            .copied()
    }

    fn volumes_for<'spec>(
        &'spec self,
        spec: &'spec RequestedServiceSpec,
    ) -> impl Iterator<Item = (&'spec DockerVolumeName, ProvisionedVolumeMaximumBytes)> + 'spec
    {
        spec.volume_graph.mounts().iter().filter_map(|mount| {
            let volume = spec.volume_graph.volume_for(mount);
            let VolumeSource::Named { name, .. } = &volume.source else {
                return None;
            };
            self.bound_for(&spec.name, name)
                .map(|maximum_bytes| (name, maximum_bytes))
        })
    }
}

impl VolumePins {
    pub(super) fn validate_provisioned_volume_bounds(
        &mut self,
        target: &[RequestedServiceSpec],
        snapshot: &DeploySnapshot,
        options: &PlanOptions,
    ) -> Result<(), PlanError> {
        let name_errors_with_service = target.len() > 1;
        for spec in target {
            if !self.provisioned.bounds.contains_key(&spec.name) {
                continue;
            }
            let result = (|| {
                let mut machines = super::eligible_machines(spec, snapshot, options)?;
                volume_constraints(spec, snapshot, self, &mut machines)?;
                self.record_provisioned(spec, &machines)
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
        for (name, maximum_bytes) in self.provisioned.volumes_for(spec) {
            for machine in machines {
                let id = DockerVolumeId {
                    machine_id: machine.machine.id,
                    name: name.clone(),
                };
                let plan = self.provisioned_plans.entry(id).or_insert(maximum_bytes);
                if *plan != maximum_bytes {
                    return Err(PlanError::ConflictingProvisionedVolumeBounds {
                        name: name.clone(),
                        existing_maximum_bytes: *plan,
                        conflicting_maximum_bytes: maximum_bytes,
                    });
                }
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
        if !self.provisioned.bounds.contains_key(&spec.name) {
            return Ok(());
        }
        for (name, maximum_bytes) in self.provisioned.volumes_for(spec) {
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
                if !crate::volume::ProvisionedVolumeSize::from_maximum_bytes(maximum_bytes)
                    .matches(existing)
                {
                    return Err(PlanError::ExistingProvisionedVolumeMismatch {
                        name: name.clone(),
                        machine: machine.machine.name.clone(),
                        maximum_bytes,
                    });
                }
            }
        }
        Ok(())
    }

    fn constrain_provisioned_candidates(
        &self,
        spec: &RequestedServiceSpec,
        machines: &mut Vec<&MachineObservation>,
    ) -> Result<(), PlanError> {
        if !self.provisioned.bounds.contains_key(&spec.name) {
            return Ok(());
        }
        let explicit = spec.placement.machines.len() == 1 && machines.len() == 1;
        let explicit_machine = machines.first().map(|machine| machine.machine.name.clone());
        machines.retain(|machine| {
            matches!(
                machine.storage,
                Some(MachineStorageObservation::Ready | MachineStorageObservation::Pool { .. })
            )
        });
        if !machines.is_empty() {
            return Ok(());
        }
        if explicit {
            Err(PlanError::ProvisionedVolumeStorageRequired {
                machine: explicit_machine
                    .expect("explicit Provisioned Volume placement resolved one Machine"),
            })
        } else {
            Err(PlanError::ProvisionedVolumeStorageUnavailable)
        }
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
        .flat_map(|spec| spec.volume_graph.volumes())
        .filter_map(|volume| match &volume.source {
            VolumeSource::Named {
                name,
                external: false,
                ..
            } => Some(name.clone()),
            VolumeSource::Named { external: true, .. }
            | VolumeSource::Bind { .. }
            | VolumeSource::Tmpfs { .. } => None,
        })
        .collect()
}

static EMPTY_VOLUME_OPTIONS: BTreeMap<String, String> = BTreeMap::new();

#[derive(Clone, Copy)]
struct VolumePresence<'a> {
    machine_id: MachineId,
    name: &'a DockerVolumeName,
    driver: &'a str,
    options: &'a BTreeMap<String, String>,
}

fn planned_presence(machine_id: MachineId, volume: &ServiceVolume) -> Option<VolumePresence<'_>> {
    let VolumeSource::Named { name, driver, .. } = &volume.source else {
        return None;
    };
    Some(VolumePresence {
        machine_id,
        name,
        driver: driver
            .as_ref()
            .map_or("local", |driver| driver.name.as_str()),
        options: driver
            .as_ref()
            .map_or(&EMPTY_VOLUME_OPTIONS, |driver| &driver.options),
    })
}

impl VolumePresence<'_> {
    fn matches(self, volume: &ServiceVolume) -> bool {
        let VolumeSource::Named { name, driver, .. } = &volume.source else {
            return false;
        };
        self.name == name
            && driver.as_ref().is_none_or(|required| {
                required.name == self.driver && required.options == *self.options
            })
    }
}

#[derive(Clone, Copy)]
pub(super) struct NamedVolumeUse<'a> {
    service_name: &'a str,
    service: &'a RequestedServiceSpec,
    volume: &'a ServiceVolume,
    global: bool,
}

pub(super) fn named_volume_uses(
    requested: &[RequestedServiceSpec],
) -> BTreeMap<DockerVolumeName, Vec<NamedVolumeUse<'_>>> {
    let mut uses = BTreeMap::<DockerVolumeName, Vec<NamedVolumeUse<'_>>>::new();
    for spec in requested {
        let service_name = spec.name.as_str();
        for mount in spec.volume_graph.mounts() {
            let volume = spec.volume_graph.volume_for(mount);
            let VolumeSource::Named { name, .. } = &volume.source else {
                continue;
            };
            let uses = uses.entry(name.clone()).or_default();
            if !uses
                .iter()
                .any(|volume_use| volume_use.service_name == service_name)
            {
                uses.push(NamedVolumeUse {
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
    volume_uses: &BTreeMap<DockerVolumeName, Vec<NamedVolumeUse<'_>>>,
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

struct SharedVolumeComponent<'a> {
    volumes: Vec<(&'a DockerVolumeName, &'a Vec<NamedVolumeUse<'a>>)>,
}

fn shared_volume_components<'a>(
    volume_uses: &'a BTreeMap<DockerVolumeName, Vec<NamedVolumeUse<'a>>>,
) -> Vec<SharedVolumeComponent<'a>> {
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
    candidate: &[NamedVolumeUse<'_>],
) -> bool {
    candidate.iter().any(|candidate| {
        component.volumes.iter().any(|(_, uses)| {
            uses.iter()
                .any(|volume_use| volume_use.service_name == candidate.service_name)
        })
    })
}

pub(super) fn prepare_shared_replicated_volumes(
    volume_uses: &BTreeMap<DockerVolumeName, Vec<NamedVolumeUse<'_>>>,
    snapshot: &DeploySnapshot,
    requested: &[RequestedServiceSpec],
    observed_services: &[ServiceObservation],
    pins: &mut VolumePins,
    capacity: &mut CapacityBudget,
    options: &PlanOptions,
) -> Result<Vec<(ServiceName, ReplicatedCapacityReservation)>, PlanError> {
    let mut reservations = Vec::new();
    for component in shared_volume_components(volume_uses) {
        let anchor = shared_component_anchor(
            &component,
            snapshot,
            requested,
            observed_services,
            pins,
            capacity,
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
    capacity: &mut CapacityBudget,
    options: &PlanOptions,
) -> Result<SharedAnchor, PlanError> {
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
    let capacity_error = capacity.error_for(&eligible);
    for machine_id in eligible {
        let mut projected = capacity.clone();
        let projected_services = requested
            .iter()
            .filter(|spec| services.contains_key(spec.name.as_str()))
            .map(|spec| {
                let observed = observed_services
                    .iter()
                    .find(|service| service.identity.name == spec.name);
                reserve_replicated_service_demand(
                    &mut projected,
                    spec,
                    observed,
                    machine_id,
                    options,
                )
                .map(|reservation| (spec.name.clone(), reservation))
            })
            .collect::<Result<Vec<_>, _>>();
        if let Ok(capacity_reservations) = projected_services {
            *capacity = projected;
            return Ok(SharedAnchor {
                machine_id,
                capacity_reservations,
            });
        }
    }
    Err(super::service_error(
        true,
        first_service_name,
        capacity_error,
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
    // TODO(UT-001, UT-051, UT-052, UT-078): preserve the baseline
    // placement ceiling: do not filter by memory, image platform, or local image presence.
    let (mounted_volumes, missing_volumes) =
        planned_volume_constraints(spec, snapshot, pins, machines)?;
    match spec.mode {
        ServiceMode::Replicated { .. } if !missing_volumes.is_empty() => {
            // TODO(UT-005): named-volume driver and label options remain part of planned creation;
            // revisit only if Ployz changes to externally managed volumes exclusively.
            let machine_id = machines
                .first()
                .map(|machine| machine.machine.id)
                .expect("volume_constraints returns a Machine when it succeeds");
            machines.retain(|machine| machine.machine.id == machine_id);
            for volume in missing_volumes {
                if let VolumeSource::Named { name, .. } = &volume.source {
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
    let volumes = volume_constraints(spec, snapshot, pins, machines)?;
    pins.constrain_provisioned_candidates(spec, machines)?;
    Ok(volumes)
}

fn volume_constraints<'spec>(
    spec: &'spec RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    machines: &mut Vec<&MachineObservation>,
) -> Result<(Vec<&'spec ServiceVolume>, Vec<&'spec ServiceVolume>), PlanError> {
    let mounted_volumes = mounted_named_volumes(&spec.volume_graph)?;
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
            .and_then(|volume| named_volume_name(volume))
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
                .filter_map(|volume| named_volume_name(volume))
                .any(|name| name == &id.name)
    }) {
        return Err(PlanError::DockerVolumeUnavailable { id, message });
    }
    let mut missing_volumes = Vec::new();
    for volume in mounted_volumes.iter().copied() {
        machines.retain(|machine| {
            !pins.observations(snapshot).any(|located| {
                located.machine_id == machine.machine.id
                    && named_volume_name(volume) == Some(located.name)
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
                .filter_map(|volume| named_volume_name(volume))
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

fn named_volume_name(volume: &ServiceVolume) -> Option<&DockerVolumeName> {
    let VolumeSource::Named { name, .. } = &volume.source else {
        return None;
    };
    Some(name)
}

fn mounted_named_volumes(graph: &ServiceVolumeGraph) -> Result<Vec<&ServiceVolume>, PlanError> {
    let mut by_docker_name = BTreeMap::<DockerVolumeName, &ServiceVolume>::new();
    for mount in graph.mounts() {
        let volume = graph.volume_for(mount);
        let VolumeSource::Named { name, .. } = &volume.source else {
            continue;
        };
        if let Some(existing) = by_docker_name.get(name) {
            if existing.source != volume.source {
                return Err(PlanError::ConflictingDockerVolumeDefinitions { name: name.clone() });
            }
        } else {
            by_docker_name.insert(name.clone(), volume);
        }
    }
    Ok(by_docker_name.into_values().collect())
}
