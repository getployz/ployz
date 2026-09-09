//! Volume planning owns observer-relative evidence and assigned commitments.
//!
//! Choosing a location records its complete Volume definition. Locality and
//! capacity read those assignments; preview rows are derived only at finish.
//! Failed inventory reads remain errors when absence would authorize creation.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    DockerVolumeId, DockerVolumeName, DockerVolumeStorageObservation, MachineId,
    MachineObservation, PlacementConstraint, PreservedVolume, ProjectName, RequestedServiceSpec,
    ServiceMode, ServiceName, ServiceObservation, ServicePlacementEligibility, ServiceStorageSpec,
    ServiceVolume, ServiceVolumeGraph, VolumeSource, owned_volume_project,
};

use crate::deploy::{
    DeployOperation, DeploySnapshot, EliminatingConstraint, PlanError, PlanOptions,
};

use super::placement::PlacementReservations;

/// Owns volume evidence and assignments for one observer-relative Deploy.
///
/// Every selected location carries its full Volume definition. Locality, budget
/// checks, and creation previews derive from those assignments; there is no
/// independently mutable list of pins or missing Volume commitments.
pub(super) struct VolumePlan<'snapshot> {
    snapshot: &'snapshot DeploySnapshot,
    project_name: ProjectName,
    assignments: BTreeMap<MachineId, BTreeMap<ServiceName, ServiceStorageSpec>>,
}

/// Informational projections derived from one completed assignment ledger.
pub(super) struct PlannedVolumes {
    /// Managed Volumes absent from the observed inventory and used by pending operations.
    pub(super) creates: Vec<(MachineId, ServiceVolume)>,
    /// Complete commitments on Machines preparing Provisioned Volumes.
    pub(super) budgets: Vec<ployz_core::MachineStorageBudget>,
}

/// A Service specification coupled to locations whose Volume commitments are recorded.
/// Only `VolumePlan::place` constructs this; container planning consumes it.
pub(super) struct VolumePlacement<'placement> {
    requested: &'placement RequestedServiceSpec,
    machines: Vec<&'placement MachineObservation>,
}

impl<'placement> VolumePlacement<'placement> {
    /// Consume the assigned specification and its eligible locations together.
    pub(super) fn into_parts(
        self,
    ) -> (
        &'placement RequestedServiceSpec,
        Vec<&'placement MachineObservation>,
    ) {
        (self.requested, self.machines)
    }
}

impl<'snapshot> VolumePlan<'snapshot> {
    /// Validate all declared definitions before choosing any locations.
    ///
    /// # Errors
    /// Returns conflicting definitions, incompatible modes, or unresolved locality.
    pub(super) fn new(
        snapshot: &'snapshot DeploySnapshot,
        project_name: &ProjectName,
        target: &[RequestedServiceSpec],
        requested: &[RequestedServiceSpec],
    ) -> Result<Self, PlanError> {
        reject_mixed_volume_modes(&managed_volume_uses(requested))?;
        let plan = Self {
            snapshot,
            project_name: project_name.clone(),
            assignments: BTreeMap::new(),
        };
        plan.validate_provisioned_volume_definitions(target)?;
        Ok(plan)
    }

    fn storage_budget<'volume>(
        &'volume self,
        machine: &MachineObservation,
        volumes: impl Iterator<Item = &'volume ServiceVolume>,
    ) -> Result<Option<ployz_core::StorageBudget>, PlanError> {
        let requested = super::storage::bounds(
            self.assigned_volumes()
                .filter(|(id, _)| *id == machine.machine.id)
                .map(|(_, volume)| volume)
                .chain(volumes),
        );
        if requested.is_empty() {
            Ok(None)
        } else {
            super::storage::budget(self.snapshot, machine, &requested).map(Some)
        }
    }

    fn assigned_volumes(&self) -> impl Iterator<Item = (MachineId, &ServiceVolume)> {
        self.assignments.iter().flat_map(|(id, specs)| {
            specs.values().flat_map(move |spec| {
                spec.volume_graph()
                    .mounted_volumes()
                    .map(move |volume| (*id, volume))
            })
        })
    }

    fn assign(&mut self, machine_id: MachineId, spec: &RequestedServiceSpec) {
        self.assignments
            .entry(machine_id)
            .or_default()
            .entry(spec.name.clone())
            .or_insert_with(|| {
                ServiceStorageSpec::try_from(spec).expect("assigned Volume graph is Project-scoped")
            });
    }

    fn incompatible_on(&self, machine_id: MachineId, volume: &ServiceVolume) -> bool {
        self.assigned_volumes().any(|(id, assigned)| {
            id == machine_id
                && managed_volume_name(volume) == managed_volume_name(assigned)
                && assigned.source.to_create_volume_request()
                    != volume.source.to_create_volume_request()
        }) || self.observed_locations().any(|located| {
            located.machine_id == machine_id
                && managed_volume_name(volume) == Some(located.name)
                && !located.matches(volume)
        })
    }

    fn locality(&self, volume: &ServiceVolume) -> Result<VolumeLocality, PlanError> {
        let assigned = self
            .assigned_volumes()
            .filter(|(_, assigned)| managed_volume_name(volume) == managed_volume_name(assigned))
            .map(|(id, _)| id)
            .collect::<BTreeSet<_>>();
        let mut locations = if assigned.is_empty() {
            self.observed_locations()
                .filter(|located| located.matches(volume))
                .map(|located| located.machine_id)
                .collect::<BTreeSet<_>>()
        } else {
            assigned
        };
        if let Some(first) = locations.pop_first() {
            return Ok(VolumeLocality::Located {
                first,
                others: locations,
            });
        }
        if matches!(
            volume.source.kind(),
            ployz_core::RawVolumeSource::Provisioned { .. }
        ) {
            for machine in &self.snapshot.machines {
                super::storage::capacity(self.snapshot, machine)?;
            }
        }
        Ok(VolumeLocality::Absent)
    }

    fn observed_locations(&self) -> impl Iterator<Item = VolumePresence<'_>> {
        self.snapshot
            .volume_snapshot
            .observations()
            .iter()
            .map(|observed| VolumePresence {
                machine_id: observed.id.machine_id,
                name: &observed.id.name,
                shape: VolumePresenceShape::DockerVolume(observed),
            })
            .chain(
                self.snapshot
                    .storage_capacity
                    .iter()
                    .filter_map(|(machine_id, capacity)| {
                        Some((machine_id, capacity.as_ref().ok()?))
                    })
                    .flat_map(|(machine_id, capacity)| {
                        capacity.volumes.keys().map(move |name| VolumePresence {
                            machine_id: *machine_id,
                            name,
                            shape: VolumePresenceShape::ProvisionedDataset,
                        })
                    }),
            )
    }

    /// Derive creation rows and complete budgets from the same assigned Volumes.
    ///
    /// # Errors
    /// Returns a typed storage admission error for any target Machine.
    pub(super) fn finish(
        mut self,
        operations: &mut Vec<DeployOperation>,
    ) -> Result<PlannedVolumes, PlanError> {
        let active_machines = operations
            .iter()
            .filter(|operation| {
                operation
                    .spec()
                    .is_some_and(|spec| spec.volume_graph().has_mounted_provisioned_volume())
            })
            .map(DeployOperation::machine_id)
            .collect::<BTreeSet<_>>();
        // Preparation can recreate Docker metadata for unchanged assigned Services too.
        let used = self
            .assigned_volumes()
            .filter_map(|(machine_id, volume)| {
                let name = managed_volume_name(volume)?;
                (active_machines.contains(&machine_id)
                    && matches!(
                        volume.source.kind(),
                        ployz_core::RawVolumeSource::Provisioned { .. }
                    )
                    || operations.iter().any(|operation| {
                        operation.machine_id() == machine_id
                            && operation.spec().is_some_and(|spec| {
                                spec.volume_graph()
                                    .mounted_volumes()
                                    .any(|mounted| managed_volume_name(mounted) == Some(name))
                            })
                    }))
                .then(|| {
                    (
                        DockerVolumeId {
                            machine_id,
                            name: name.clone(),
                        },
                        volume,
                    )
                })
            })
            .collect::<BTreeMap<_, _>>();
        let creates = used
            .into_iter()
            .filter(|(id, volume)| {
                !self
                    .snapshot
                    .volume_snapshot
                    .observations()
                    .iter()
                    .any(|observed| {
                        observed.id == *id && volume.source.matches_managed_volume(observed)
                    })
            })
            .map(|(id, volume)| (id.machine_id, volume.clone()))
            .collect();
        let mut budgets = Vec::new();
        let mut preparations = Vec::new();
        for id in active_machines {
            let specs = self
                .assignments
                .remove(&id)
                .expect("container planning requires recorded VolumePlacement")
                .into_values()
                .filter(|spec| spec.volume_graph().has_mounted_provisioned_volume())
                .collect::<Vec<_>>();
            let requested = super::storage::bounds(
                specs
                    .iter()
                    .flat_map(|spec| spec.volume_graph().mounted_volumes()),
            );
            let machine = self
                .snapshot
                .machines
                .iter()
                .find(|machine| machine.machine.id == id)
                .expect("assigned Machine belongs to the snapshot");
            budgets.push(ployz_core::MachineStorageBudget {
                machine_id: id,
                machine_name: machine.machine.name.clone(),
                budget: super::storage::budget(self.snapshot, machine, &requested)?,
            });
            preparations.push(DeployOperation::PrepareVolumes {
                machine_id: id,
                specs,
            });
        }
        operations.splice(..0, preparations);
        Ok(PlannedVolumes { creates, budgets })
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

impl VolumePlan<'_> {
    fn validate_provisioned_volume_definitions(
        &self,
        target: &[RequestedServiceSpec],
    ) -> Result<(), PlanError> {
        let snapshot = self.snapshot;
        let mut definitions = BTreeMap::new();
        let name_errors_with_service = target.len() > 1;
        for spec in target {
            if !spec.volume_graph().has_mounted_provisioned_volume() {
                continue;
            }
            let result = (|| {
                let candidates = super::placement_candidates(spec, &self.project_name, snapshot)?;
                Self::record_provisioned(&mut definitions, spec, &candidates)?;
                let mut machines = candidates
                    .into_iter()
                    .filter(|machine| {
                        spec.placement_eligibility_in_project(
                            &self.project_name,
                            &machine.machine,
                            machine.storage.as_ref(),
                        ) == ServicePlacementEligibility::Eligible
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
        definitions: &mut BTreeMap<DockerVolumeId, VolumeSource>,
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
                if let Some(existing) = definitions.get(&id)
                    && existing != &volume.source
                {
                    return Err(PlanError::ConflictingDockerVolumeDefinitions {
                        name: name.clone(),
                    });
                }
                definitions.insert(id, volume.source.clone());
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

enum VolumeLocality {
    Absent,
    Located {
        first: MachineId,
        others: BTreeSet<MachineId>,
    },
}

#[derive(Clone, Copy)]
struct VolumePresence<'volume> {
    machine_id: MachineId,
    name: &'volume DockerVolumeName,
    shape: VolumePresenceShape<'volume>,
}

#[derive(Clone, Copy)]
enum VolumePresenceShape<'volume> {
    DockerVolume(&'volume ployz_core::DockerVolume),
    ProvisionedDataset,
}

impl VolumePresence<'_> {
    fn matches(self, volume: &ServiceVolume) -> bool {
        if managed_volume_name(volume) != Some(self.name) {
            return false;
        }
        match self.shape {
            VolumePresenceShape::DockerVolume(observed) => {
                volume.source.matches_managed_volume(observed)
            }
            // A bound mismatch must fail admission on the data's owner, not erase locality.
            VolumePresenceShape::ProvisionedDataset => matches!(
                volume.source.kind(),
                ployz_core::RawVolumeSource::Provisioned { .. }
            ),
        }
    }
}

#[derive(Clone, Copy)]
struct ManagedVolumeUse<'service> {
    service_name: &'service str,
    service: &'service RequestedServiceSpec,
    global: bool,
}

fn managed_volume_uses(
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
                    global: matches!(spec.mode, ServiceMode::Global),
                });
            }
        }
    }
    uses
}

fn reject_mixed_volume_modes(
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

impl VolumePlan<'_> {
    /// Reserve shared placement and its full mounted commitments together.
    ///
    /// # Errors
    /// Returns no common legal Machine, conflicting definitions, or endpoint admission failure.
    pub(super) fn reserve_shared(
        &mut self,
        requested: &[RequestedServiceSpec],
        observed_services: &[ServiceObservation],
        placement: &mut PlacementReservations,
        options: &PlanOptions,
    ) -> Result<(), PlanError> {
        let snapshot = self.snapshot;
        let volume_uses = managed_volume_uses(requested);
        for component in shared_volume_components(&volume_uses) {
            let anchor = shared_component_anchor(
                &component,
                snapshot,
                requested,
                observed_services,
                self,
                placement,
                options,
            )?;
            assign_shared_component(&component, anchor, self);
        }
        Ok(())
    }
}

fn shared_component_anchor(
    component: &SharedVolumeComponent<'_>,
    snapshot: &DeploySnapshot,
    requested: &[RequestedServiceSpec],
    observed_services: &[ServiceObservation],
    plan: &VolumePlan<'_>,
    placement: &mut PlacementReservations,
    options: &PlanOptions,
) -> Result<MachineId, PlanError> {
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
    let mut eligible = volume_eligible_machine_ids(first_service, snapshot, plan, options)
        .map_err(|source| super::service_error(true, first_service_name, source))?;
    for (&service_name, &service) in service_iter {
        let other_eligible = volume_eligible_machine_ids(service, snapshot, plan, options)
            .map_err(|source| super::service_error(true, service_name, source))?;
        eligible.retain(|machine_id| other_eligible.contains(machine_id));
    }
    if eligible.is_empty() {
        return Err(super::service_error(
            true,
            first_service_name,
            no_eligible_shared(component, snapshot, plan),
        ));
    }
    let requested = requested
        .iter()
        .filter(|spec| services.contains_key(spec.name.as_str()));
    let mut admission_error = None;
    let fitting = eligible
        .iter()
        .copied()
        .filter(|id| {
            let machine = snapshot
                .machines
                .iter()
                .find(|machine| machine.machine.id == *id)
                .expect("eligible Machine is observed");
            plan.storage_budget(
                machine,
                requested
                    .clone()
                    .flat_map(|spec| spec.volume_graph().mounted_volumes()),
            )
            .is_ok()
        })
        .collect::<Vec<_>>();
    let eligible = if fitting.is_empty() {
        eligible
    } else {
        fitting
    };
    for machine_id in eligible {
        match placement.reserve_on(machine_id, requested.clone(), observed_services, options) {
            Ok(()) => return Ok(machine_id),
            Err(error) => admission_error = Some(error),
        }
    }
    Err(super::service_error(
        true,
        first_service_name,
        admission_error.expect("non-empty eligible Machines were tried"),
    ))
}

fn assign_shared_component(
    component: &SharedVolumeComponent<'_>,
    machine_id: MachineId,
    plan: &mut VolumePlan<'_>,
) {
    for (_, uses) in &component.volumes {
        for volume_use in uses.iter() {
            plan.assign(machine_id, volume_use.service);
        }
    }
}

fn volume_eligible_machine_ids(
    spec: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    plan: &VolumePlan<'_>,
    options: &PlanOptions,
) -> Result<Vec<MachineId>, PlanError> {
    let mut machines = super::eligible_machines(spec, &plan.project_name, snapshot, options)?;
    planned_volume_constraints(spec, snapshot, plan, &mut machines)?;
    Ok(machines
        .into_iter()
        .map(|machine| machine.machine.id)
        .collect())
}

impl<'snapshot> VolumePlan<'snapshot> {
    /// Select legal Volume locations and reserve every mounted commitment together.
    ///
    /// # Errors
    /// Returns incompatible definitions, unresolved locality, or no eligible Machine.
    pub(super) fn place<'placement>(
        &mut self,
        spec: &'placement RequestedServiceSpec,
        mut machines: Vec<&'placement MachineObservation>,
    ) -> Result<VolumePlacement<'placement>, PlanError> {
        let snapshot = self.snapshot;
        // TODO: preserve the placement ceiling: do not filter by memory, image platform, or local image presence.
        let (mounted_volumes, missing_volumes) =
            planned_volume_constraints(spec, snapshot, self, &mut machines)?;
        if matches!(spec.mode, ServiceMode::Replicated { .. })
            && machines.iter().any(|machine| {
                self.storage_budget(machine, mounted_volumes.iter().copied())
                    .is_ok()
            })
        {
            machines.retain(|machine| {
                self.storage_budget(machine, mounted_volumes.iter().copied())
                    .is_ok()
            });
        }
        if matches!(spec.mode, ServiceMode::Replicated { .. }) && !missing_volumes.is_empty() {
            let machine_id = machines
                .first()
                .expect("volume_constraints returns a Machine when it succeeds")
                .machine
                .id;
            machines.retain(|machine| machine.machine.id == machine_id);
        }
        for machine in &machines {
            self.assign(machine.machine.id, spec);
        }
        Ok(VolumePlacement {
            requested: spec,
            machines,
        })
    }
}

impl VolumePlan<'_> {
    /// Apply existing Volume locality before reserving container endpoints.
    ///
    /// # Errors
    /// Returns conflicting definitions, unresolved locality, or no eligible Machine.
    pub(super) fn constrain_candidates(
        &self,
        spec: &RequestedServiceSpec,
        machines: &mut Vec<&MachineObservation>,
    ) -> Result<(), PlanError> {
        planned_volume_constraints(spec, self.snapshot, self, machines).map(|_| ())
    }
}

fn planned_volume_constraints<'spec>(
    spec: &'spec RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    plan: &VolumePlan<'_>,
    machines: &mut Vec<&MachineObservation>,
) -> Result<(Vec<&'spec ServiceVolume>, Vec<&'spec ServiceVolume>), PlanError> {
    plan.validate_provisioned_volumes(spec, machines, snapshot)?;
    volume_constraints(spec, snapshot, plan, machines)
}

fn volume_constraints<'spec>(
    spec: &'spec RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    plan: &VolumePlan<'_>,
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
        machines.retain(|machine| !plan.incompatible_on(machine.machine.id, volume));
        if matches!(spec.mode, ServiceMode::Replicated { .. }) {
            match plan.locality(volume)? {
                VolumeLocality::Located { first, others } => {
                    machines.retain(|machine| {
                        machine.machine.id == first || others.contains(&machine.machine.id)
                    });
                }
                VolumeLocality::Absent => missing_volumes.push(volume),
            }
        }
    }
    if machines.is_empty() {
        // ponytail: name the filter that emptied the set; no per-Machine matrix.
        let requested = &spec.placement.constraints;
        return Err(PlanError::no_eligible_machines(
            mounted_volumes
                .iter()
                .filter_map(|volume| managed_volume_name(volume))
                .filter_map(|name| volume_anchor(snapshot, plan, name, requested))
                .collect(),
        ));
    }
    Ok((mounted_volumes, missing_volumes))
}

fn no_eligible_shared(
    component: &SharedVolumeComponent<'_>,
    snapshot: &DeploySnapshot,
    plan: &VolumePlan<'_>,
) -> PlanError {
    PlanError::no_eligible_machines(
        component
            .volumes
            .iter()
            .filter_map(|(name, uses)| {
                let mut requested = std::collections::BTreeSet::new();
                for volume_use in uses.iter() {
                    requested.extend(volume_use.service.placement.constraints.iter().cloned());
                }
                if requested.is_empty() {
                    volume_anchor(snapshot, plan, name, &requested)
                } else {
                    Some(EliminatingConstraint::SharedVolumeNoCommonMachine {
                        volume: (*name).clone(),
                        requested: requested.into_iter().collect(),
                    })
                }
            })
            .collect(),
    )
}

fn volume_anchor(
    snapshot: &DeploySnapshot,
    plan: &VolumePlan<'_>,
    name: &DockerVolumeName,
    requested: &std::collections::BTreeSet<PlacementConstraint>,
) -> Option<EliminatingConstraint> {
    let mut located_on = Vec::new();
    for located in plan
        .observed_locations()
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
    let hits_located = snapshot.machines.iter().any(|machine| {
        located_on.contains(&machine.machine.name)
            && requested
                .iter()
                .all(|constraint| constraint.matches(&machine.machine))
    });
    if located_on.is_empty() {
        if requested.is_empty() {
            None
        } else {
            Some(EliminatingConstraint::SharedVolumeNoCommonMachine {
                volume: name.clone(),
                requested: requested.iter().cloned().collect(),
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
            requested: requested.iter().cloned().collect(),
        })
    }
}

fn managed_volume_name(volume: &ServiceVolume) -> Option<&DockerVolumeName> {
    volume.source.managed_docker_volume_name()
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
