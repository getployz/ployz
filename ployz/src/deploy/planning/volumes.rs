use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    DockerVolumeId, DockerVolumeName, MachineId, MachineObservation, RequestedServiceSpec,
    ServiceMode, ServiceVolume, VolumeSource,
};

use crate::deploy::{
    DeployOperation, DeploySnapshot, ObservedDockerVolume, PlanError, PlanOptions,
};

/// Planner-internal assignment of Docker Volumes to Machines.
///
/// `anchors` override Deploy Snapshot observations for a shared replicated
/// Docker Volume (the chosen Machine is the only legal location). `planned`
/// records CreateVolume ops so later specs see them without mutating the snapshot.
#[derive(Default)]
pub(super) struct VolumePins {
    anchors: BTreeMap<DockerVolumeName, MachineId>,
    planned: Vec<ObservedDockerVolume>,
}

impl VolumePins {
    fn constrain(&mut self, name: DockerVolumeName, machine_id: MachineId) {
        self.anchors.insert(name, machine_id);
    }

    fn record_create(&mut self, machine_id: MachineId, volume: &ServiceVolume) {
        let VolumeSource::Named { name, driver, .. } = &volume.source else {
            return;
        };
        self.planned.push(ObservedDockerVolume {
            id: DockerVolumeId {
                machine_id,
                name: name.clone(),
            },
            driver: driver
                .as_ref()
                .map_or_else(|| "local".into(), |driver| driver.name.clone()),
            options: driver
                .as_ref()
                .map_or_else(Default::default, |driver| driver.options.clone()),
        });
    }

    fn observations<'a>(
        &'a self,
        snapshot: &'a DeploySnapshot,
    ) -> impl Iterator<Item = &'a ObservedDockerVolume> {
        snapshot.volumes.iter().chain(self.planned.iter())
    }

    fn anchor_for(&self, volume: &ServiceVolume) -> Option<MachineId> {
        let VolumeSource::Named { name, .. } = &volume.source else {
            return None;
        };
        self.anchors.get(name).copied()
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
    for service in requested {
        let service_name = service.name.as_str();
        for mount in &service.mounts {
            let Some(volume) = service
                .volumes
                .iter()
                .find(|volume| volume.reference == mount.volume)
            else {
                continue;
            };
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
                    service,
                    volume,
                    global: matches!(service.mode, ServiceMode::Global),
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
    options: PlanOptions,
) -> Result<Vec<DeployOperation>, PlanError> {
    let mut operations = Vec::new();
    for component in shared_volume_components(volume_uses) {
        let machine_id = shared_component_anchor(&component, snapshot, pins, options)?;
        operations.extend(pin_shared_component(&component, machine_id, snapshot, pins));
    }
    Ok(operations)
}

fn shared_component_anchor(
    component: &SharedVolumeComponent<'_>,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    options: PlanOptions,
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
            PlanError::NoEligibleMachines,
        ));
    }
    Ok(eligible.remove(0))
}

fn pin_shared_component(
    component: &SharedVolumeComponent<'_>,
    machine_id: MachineId,
    snapshot: &DeploySnapshot,
    pins: &mut VolumePins,
) -> Vec<DeployOperation> {
    let mut operations = Vec::new();
    for (name, uses) in &component.volumes {
        pins.constrain((*name).clone(), machine_id);
        if snapshot
            .volumes
            .iter()
            .any(|volume| volume.id.machine_id == machine_id && volume.id.name == **name)
        {
            continue;
        }
        let first_use = uses.first().expect("shared Volume has at least two uses");
        pins.record_create(machine_id, first_use.volume);
        operations.push(DeployOperation::CreateVolume {
            machine_id,
            volume: first_use.volume.clone(),
        });
    }
    operations
}

fn volume_eligible_machine_ids(
    requested: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    options: PlanOptions,
) -> Result<Vec<MachineId>, PlanError> {
    let mut machines = super::eligible_machines(requested, snapshot, options);
    volume_constraints(requested, snapshot, pins, &mut machines)?;
    Ok(machines
        .into_iter()
        .map(|machine| machine.machine.id)
        .collect())
}

pub(super) fn plan_volume_operations(
    requested: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &mut VolumePins,
    machines: &mut Vec<&MachineObservation>,
) -> Result<Vec<DeployOperation>, PlanError> {
    // TODO(UT-001, UT-007, UT-008, UT-051, UT-052, UT-078): preserve the baseline
    // placement/pull ceiling: do not filter by memory, image platform, or local image presence,
    // and do not pull images from other Machines.
    let (mounted_volumes, missing_volumes) =
        volume_constraints(requested, snapshot, pins, machines)?;
    let mut operations = Vec::new();
    match requested.mode {
        ServiceMode::Replicated { .. } if !missing_volumes.is_empty() => {
            // TODO(UT-005): named-volume driver and label options remain part of planned creation;
            // revisit only if Ployz changes to externally managed volumes exclusively.
            let machine_id = machines
                .first()
                .map(|machine| machine.machine.id)
                .ok_or(PlanError::NoEligibleMachines)?;
            machines.retain(|machine| machine.machine.id == machine_id);
            for volume in missing_volumes {
                pins.record_create(machine_id, &volume);
                if let VolumeSource::Named { name, .. } = &volume.source {
                    pins.constrain(name.clone(), machine_id);
                }
                operations.push(DeployOperation::CreateVolume { machine_id, volume });
            }
        }
        ServiceMode::Global => {
            for machine in machines.iter() {
                for volume in &mounted_volumes {
                    if pins.observations(snapshot).any(|observed| {
                        observed.id.machine_id == machine.machine.id
                            && volume_matches(observed, volume)
                    }) {
                        continue;
                    }
                    pins.record_create(machine.machine.id, volume);
                    operations.push(DeployOperation::CreateVolume {
                        machine_id: machine.machine.id,
                        volume: volume.clone(),
                    });
                }
            }
        }
        ServiceMode::Replicated { .. } => {}
    }
    Ok(operations)
}

fn volume_constraints(
    requested: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &VolumePins,
    machines: &mut Vec<&MachineObservation>,
) -> Result<(Vec<ServiceVolume>, Vec<ServiceVolume>), PlanError> {
    let mounted_volumes = mounted_named_volumes(requested)?
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    let mut missing_volumes = Vec::new();
    for volume in &mounted_volumes {
        machines.retain(|machine| {
            !pins.observations(snapshot).any(|observed| {
                observed.id.machine_id == machine.machine.id
                    && volume_has_same_name(observed, volume)
                    && !volume_matches(observed, volume)
            })
        });
        if matches!(requested.mode, ServiceMode::Replicated { .. }) {
            if let Some(anchor) = pins.anchor_for(volume) {
                machines.retain(|machine| machine.machine.id == anchor);
                continue;
            }
            let locations = pins
                .observations(snapshot)
                .filter(|observed| volume_matches(observed, volume))
                .map(|observed| observed.id.machine_id)
                .collect::<BTreeSet<_>>();
            if !locations.is_empty() {
                machines.retain(|machine| locations.contains(&machine.machine.id));
            } else {
                missing_volumes.push(volume.clone());
            }
        }
    }
    if machines.is_empty() {
        // TODO(UT-079, UT-093): keep the baseline's coarse constraint failure until a
        // separately approved diagnostic contract defines a detailed report.
        return Err(PlanError::NoEligibleMachines);
    }
    Ok((mounted_volumes, missing_volumes))
}

fn volume_has_same_name(observed: &ObservedDockerVolume, volume: &ServiceVolume) -> bool {
    let VolumeSource::Named { name, .. } = &volume.source else {
        return false;
    };
    observed.id.name == *name
}

fn volume_matches(observed: &ObservedDockerVolume, volume: &ServiceVolume) -> bool {
    let VolumeSource::Named { name, driver, .. } = &volume.source else {
        return false;
    };
    observed.id.name == *name
        && driver.as_ref().is_none_or(|required| {
            required.name == observed.driver && required.options == observed.options
        })
}

fn mounted_named_volumes(
    requested: &RequestedServiceSpec,
) -> Result<Vec<&ServiceVolume>, PlanError> {
    let mut by_docker_name = BTreeMap::<DockerVolumeName, &ServiceVolume>::new();
    for mount in &requested.mounts {
        let Some(volume) = requested
            .volumes
            .iter()
            .find(|volume| volume.reference == mount.volume)
        else {
            continue;
        };
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
