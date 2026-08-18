use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    DockerVolumeName, MachineId, MachineObservation, MachineTarget, PROJECT_NAME_LABEL,
    PreservedVolume, ProjectName, RequestedServiceSpec, ServiceMode, ServiceVolume,
    ServiceVolumeGraph, VolumeSource, machine_matches_target,
};

use crate::deploy::{
    DeployIntent, DeployOperation, DeploySnapshot, EliminatingConstraint, PlanError, PlanOptions,
};

/// Planner-internal assignment of Docker Volumes to Machines.
///
/// Anchors override Deploy Snapshot observations for a shared replicated
/// Docker Volume (the chosen Machine is the only legal location). `creates` is
/// the one CreateVolume list later specs consult; it is not a reconstructed
/// observation list.
#[derive(Default)]
pub(super) struct VolumePins {
    anchors: BTreeMap<DockerVolumeName, MachineId>,
    creates: Vec<(MachineId, ServiceVolume)>,
}

impl VolumePins {
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
            .volumes
            .iter()
            .map(|observed| VolumePresence {
                machine_id: observed.id.machine_id,
                name: &observed.id.name,
                driver: &observed.driver,
                options: &observed.options,
            })
            .chain(
                self.creates
                    .iter()
                    .filter_map(|(machine_id, volume)| planned_presence(*machine_id, volume)),
            )
    }

    pub(super) fn into_creates(self) -> Vec<DeployOperation> {
        self.creates
            .into_iter()
            .map(|(machine_id, volume)| DeployOperation::CreateVolume { machine_id, volume })
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

/// Owned Compose-declared Docker Volumes omitted from this Deploy's target.
pub(super) fn preserved_owned_volumes(
    intent: &DeployIntent,
    snapshot: &DeploySnapshot,
) -> Vec<PreservedVolume> {
    let declared = declared_physical_names(intent);
    let mut preserved = Vec::new();
    for volume in &snapshot.volumes {
        if volume.labels.get(PROJECT_NAME_LABEL).map(String::as_str)
            != Some(intent.project_name.as_str())
        {
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

fn declared_physical_names(intent: &DeployIntent) -> BTreeSet<DockerVolumeName> {
    intent
        .target
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
    pins: &mut VolumePins,
    options: &PlanOptions,
) -> Result<(), PlanError> {
    for component in shared_volume_components(volume_uses) {
        let machine_id = shared_component_anchor(&component, snapshot, pins, options)?;
        pin_shared_component(&component, machine_id, snapshot, pins);
    }
    Ok(())
}

fn shared_component_anchor(
    component: &SharedVolumeComponent<'_>,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    options: &PlanOptions,
) -> Result<MachineId, PlanError> {
    let services = component
        .volumes
        .iter()
        .flat_map(|(_, uses)| uses.iter())
        .map(|volume_use| (volume_use.service_name, volume_use.service))
        .collect::<BTreeMap<_, _>>();
    let mut services = services.into_iter();
    let (first_service_name, first_service) = services
        .next()
        .expect("shared Volume component has at least two services");
    let mut eligible = volume_eligible_machine_ids(first_service, snapshot, pins, options)
        .map_err(|source| super::service_error(true, first_service_name, source))?;
    for (service_name, service) in services {
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
    Ok(eligible.remove(0))
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
    volume_constraints(spec, snapshot, pins, &mut machines)?;
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
    let (mounted_volumes, missing_volumes) = volume_constraints(spec, snapshot, pins, machines)?;
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
    Ok(())
}

fn volume_constraints<'spec>(
    spec: &'spec RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    machines: &mut Vec<&MachineObservation>,
) -> Result<(Vec<&'spec ServiceVolume>, Vec<&'spec ServiceVolume>), PlanError> {
    let mounted_volumes = mounted_named_volumes(&spec.volume_graph)?;
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
