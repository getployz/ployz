use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    ContainerAction, DataLoss, DependencyCondition, HookContainer, IngressHost, MachineId,
    MachineObservation, MembershipObservation, ObservedDataLoss, PreservedVolume, ProjectName,
    PruneRefusal, QualifiedService, RequestedServiceSpec, ServiceId, ServiceMode, ServiceName,
    ServiceObservation, ServicePlacementEligibility, ServicePlacementIneligibleReason,
    VolumeToCreate, explicit_ingress_hosts, hostname_owners, machine_matches_target,
    same_service_mode_kind,
};

use super::{
    DeployIntent, DeployOperation, DeployPreview, DeploySnapshot, DeployWarning,
    EliminatingConstraint, PlanError, PlanOptions, ReplacementOperation,
};

pub(super) mod capacity;
mod placement;
mod volumes;

use capacity::CapacityBudget;
use placement::{
    CapacityAdmission, GlobalPlacement, PlacementState, ReplicatedPlacement, is_up_to_date,
    plan_global, plan_replicated,
};
use volumes::{
    VolumePins, constrain_volume_candidates, managed_volume_uses, plan_volume_operations,
    prepare_shared_replicated_volumes, preserved_owned_volumes, reject_mixed_volume_modes,
    scope_requested,
};

/// Whether Project removal keeps or destroys observer-visible managed volumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumeFate {
    /// Leave owned Docker Volumes in place. They keep listing under the Project.
    Preserve,
    /// Destroy each preserved owned Docker Volume after Services are removed.
    Destroy,
}

/// Reserved Cluster Domain used to expand applied Ingress Hostname intents.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IngressContext<'domain> {
    /// Hosted Cluster Domain, when reserved. Absence fails Cluster Domain expansion.
    pub cluster_domain: Option<&'domain str>,
}

/// Volume-bound target plus the expanded apply-set. Private planning phase.
struct BoundIntent {
    target: Vec<RequestedServiceSpec>,
    requested: Vec<RequestedServiceSpec>,
}

/// Operations and prune results before pending rows are attached.
struct Planned {
    operations: Vec<DeployOperation>,
    volumes_to_create: Vec<(MachineId, ployz_core::ServiceVolume)>,
    would_remove: Vec<QualifiedService>,
    preserved_volumes: Vec<PreservedVolume>,
    prune_refusal: Option<PruneRefusal>,
    warnings: Vec<DeployWarning>,
}

/// Plan removal of observer-visible compute for `project`.
///
/// Empty target is full reconciliation of nothing: owned Services are obsolete.
/// Managed volumes stay in `preserved_volumes` unless `volumes` is
/// [`VolumeFate::Destroy`] and pruning is not refused. Incomplete snapshots
/// reuse [`DeployIntent::prune_refusal`]; reserved names are refused at the
/// command boundary by [`crate::project::refuse_reserved`].
///
/// # Errors
///
/// Returns when [`preview_deploy`] cannot produce a preview.
pub fn plan_project_removal(
    project: &ProjectName,
    snapshot: &DeploySnapshot,
    volumes: VolumeFate,
) -> Result<DeployPreview, PlanError> {
    let intent = DeployIntent::apply_all(project.clone(), [], PlanOptions::default());
    let mut planned = plan_operations(&intent, snapshot, IngressContext::default())?;
    if volumes == VolumeFate::Destroy && planned.prune_refusal.is_none() {
        planned.operations.extend(
            planned
                .preserved_volumes
                .drain(..)
                .map(|volume| DeployOperation::RemoveVolume { id: volume.id }),
        );
    }
    Ok(preview_from(planned, snapshot, project))
}

/// Data Loss implied by a Project-removal preview.
///
/// Preserve previews are empty. Destroy names each `RemoveVolume`. Completeness
/// is the caller's check; this listing is not a Cluster view.
#[must_use]
pub fn data_loss_from_plan(preview: &DeployPreview) -> ObservedDataLoss {
    ObservedDataLoss {
        data_loss: preview
            .operations
            .iter()
            .filter_map(|row| remove_volume_loss(&row.operation))
            .collect(),
    }
}

fn remove_volume_loss(operation: &DeployOperation) -> Option<DataLoss> {
    let DeployOperation::RemoveVolume { id } = operation else {
        return None;
    };
    Some(DataLoss::DockerVolume { id: id.clone() })
}

/// Calculate a Deploy Preview for this Intent against this Snapshot.
///
/// Expands applied Cluster Domain hostnames, binds Project volumes once, then
/// constructs pending operation rows. Does not mutate `intent`.
///
/// Matching and replacement use only Containers owned by
/// [`DeployIntent::project_name`]. Empty `options.selected` is a full
/// reconciliation of `target` (profile-enabled Services start). Non-empty
/// `selected` is partial: those names expand through dependencies that are also
/// in `target`. Other target Services are unchanged. Visible obsolete Services
/// owned by that user Project are removed after desired work when pruning is
/// not refused; otherwise they are listed as `would_remove`.
///
/// # Errors
///
/// Returns when domain assignment, generated labels, hostname conflicts,
/// placement, volumes, service identity, or the apply-set dependency graph
/// cannot produce a preview.
pub fn preview_deploy(
    intent: &DeployIntent,
    snapshot: &DeploySnapshot,
    ingress: IngressContext<'_>,
) -> Result<DeployPreview, PlanError> {
    let planned = plan_operations(intent, snapshot, ingress)?;
    Ok(preview_from(planned, snapshot, &intent.project_name))
}

fn preview_from(
    planned: Planned,
    snapshot: &DeploySnapshot,
    project_name: &ProjectName,
) -> DeployPreview {
    DeployPreview {
        project_name: project_name.clone(),
        operations: super::pending_rows(&planned.operations, snapshot),
        warnings: planned.warnings,
        volumes_to_create: planned
            .volumes_to_create
            .into_iter()
            .filter_map(|(machine_id, volume)| {
                let (name, maximum_bytes) = match volume.source.kind().clone() {
                    ployz_core::RawVolumeSource::Ordinary { name, .. } => (name, None),
                    ployz_core::RawVolumeSource::Provisioned {
                        name,
                        maximum_bytes,
                        ..
                    } => (name, Some(maximum_bytes)),
                    ployz_core::RawVolumeSource::External { .. }
                    | ployz_core::RawVolumeSource::Bind { .. }
                    | ployz_core::RawVolumeSource::Tmpfs { .. } => return None,
                };
                Some(VolumeToCreate {
                    machine_id,
                    machine_name: snapshot
                        .machines
                        .iter()
                        .find(|machine| machine.machine.id == machine_id)
                        .map(|machine| machine.machine.name.clone()),
                    name,
                    maximum_bytes,
                })
            })
            .collect(),
        would_remove: planned.would_remove,
        preserved_volumes: planned.preserved_volumes,
        prune_refusal: planned.prune_refusal,
    }
}

fn plan_operations(
    intent: &DeployIntent,
    snapshot: &DeploySnapshot,
    ingress: IngressContext<'_>,
) -> Result<Planned, PlanError> {
    let bound = bind(intent, ingress)?;
    let warnings = hostname_policy_for(&intent.project_name, &bound.requested, snapshot)?;
    assemble_plan(intent, bound, snapshot, warnings)
}

fn bind(intent: &DeployIntent, ingress: IngressContext<'_>) -> Result<BoundIntent, PlanError> {
    let specs = specs_to_plan(intent)?;
    let target: Vec<_> = intent
        .target
        .iter()
        .cloned()
        .map(|spec| scope_requested(spec, &intent.project_name))
        .collect::<Result<_, _>>()?;
    let mut requested = Vec::new();
    for spec in specs {
        let scoped = target
            .iter()
            .find(|candidate| candidate.name == spec.name)
            .expect("apply-set names are drawn from the Intent target");
        let mut planned = scoped.clone();
        crate::dns::expand_ingress_ports(
            &mut planned,
            &intent.project_name,
            ingress.cluster_domain,
        )?;
        requested.push(planned);
    }
    Ok(BoundIntent { target, requested })
}

fn hostname_policy_for(
    project_name: &ProjectName,
    requested: &[RequestedServiceSpec],
    snapshot: &DeploySnapshot,
) -> Result<Vec<DeployWarning>, PlanError> {
    reject_hostname_conflicts(project_name, requested, snapshot)?;
    let mut warnings = Vec::new();
    if !snapshot.is_observer_complete()
        && requested
            .iter()
            .any(|spec| explicit_ingress_hosts(&spec.ports).next().is_some())
    {
        warnings.push(DeployWarning::ObserverRelativeHostnameConflict);
    }
    Ok(warnings)
}

fn assemble_plan(
    intent: &DeployIntent,
    bound: BoundIntent,
    snapshot: &DeploySnapshot,
    mut warnings: Vec<DeployWarning>,
) -> Result<Planned, PlanError> {
    let BoundIntent { target, requested } = bound;
    warnings.extend(storage_eligibility_warnings(&requested, snapshot));
    // TODO: preserve the missing within-spec port-conflict validation.
    let volume_uses = managed_volume_uses(&requested);
    reject_mixed_volume_modes(&volume_uses)?;
    let mut pins = VolumePins::new();
    pins.validate_provisioned_volume_definitions(&target, snapshot)?;
    let name_errors_with_service = requested.len() > 1;
    let services = snapshot.services_in(&intent.project_name);
    let mut capacity = CapacityBudget::from_snapshot(snapshot);
    let reservations = prepare_shared_replicated_volumes(
        &volume_uses,
        snapshot,
        &requested,
        &services,
        &mut pins,
        &mut capacity,
        &intent.options,
    )?;
    let mut placement = PlacementState::new(capacity, reservations);
    let mut service_operations = Vec::new();
    for spec in &requested {
        let operations = plan_one_service(
            spec,
            &intent.project_name,
            snapshot,
            &services,
            &mut pins,
            &mut placement,
            &intent.options,
        )
        .map_err(|source| service_error(name_errors_with_service, spec.name.as_str(), source))?;
        if let Some(machine_id) = operations.iter().find_map(|operation| match operation {
            DeployOperation::RunContainer { machine_id, .. }
            | DeployOperation::ReplaceContainer(ReplacementOperation { machine_id, .. }) => {
                Some(*machine_id)
            }
            DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        }) {
            for dependency in intent.dependencies().get(&spec.name).into_iter().flatten() {
                if dependency.condition != DependencyCondition::ServiceHealthy {
                    continue;
                }
                let dependent =
                    QualifiedService::new(intent.project_name.clone(), spec.name.clone());
                let dependency =
                    QualifiedService::new(intent.project_name.clone(), dependency.service.clone());
                if intent.options.skip_health_monitor {
                    warnings.push(DeployWarning::SkippedDependencyHealth {
                        dependent,
                        dependency,
                    });
                } else {
                    service_operations.push(DeployOperation::WaitHealthy {
                        machine_id,
                        dependent,
                        dependency,
                    });
                }
            }
        }
        service_operations.extend(operations);
    }
    let volumes_to_create = pins.into_creates_for(&service_operations);
    let mut operations = service_operations;
    let would_remove = obsolete_services(intent, &services);
    let prune_refusal = intent.prune_refusal(snapshot.is_observer_complete());
    let preserved_volumes = preserved_owned_volumes(&intent.project_name, &target, snapshot);
    if prune_refusal.is_none() {
        operations.extend(removal_operations(&services, &would_remove));
    }
    Ok(Planned {
        operations,
        volumes_to_create,
        would_remove,
        preserved_volumes,
        prune_refusal,
        warnings,
    })
}

fn reject_hostname_conflicts(
    project_name: &ProjectName,
    requested: &[RequestedServiceSpec],
    snapshot: &DeploySnapshot,
) -> Result<(), PlanError> {
    let owners = hostname_owners(snapshot.containers.iter());
    let mut claimed = BTreeMap::<&IngressHost, QualifiedService>::new();
    for spec in requested {
        let identity = QualifiedService::new(project_name.clone(), spec.name.clone());
        for hostname in explicit_ingress_hosts(&spec.ports) {
            if let Some(owner) = claimed.get(hostname).or_else(|| owners.get(hostname))
                && *owner != identity
            {
                return Err(PlanError::HostnameConflict {
                    hostname: hostname.clone(),
                    owner: owner.clone(),
                });
            }
            claimed.entry(hostname).or_insert_with(|| identity.clone());
        }
    }
    Ok(())
}

fn obsolete_services(
    intent: &DeployIntent,
    services: &[ServiceObservation],
) -> Vec<QualifiedService> {
    if intent.project_name.is_reserved() {
        return Vec::new();
    }
    let declared = intent
        .target
        .iter()
        .map(|spec| &spec.name)
        .collect::<BTreeSet<_>>();
    services
        .iter()
        .filter(|service| !declared.contains(&service.identity.name))
        .map(|service| service.identity.clone())
        .collect()
}

fn removal_operations(
    services: &[ServiceObservation],
    obsolete: &[QualifiedService],
) -> Vec<DeployOperation> {
    let obsolete = obsolete.iter().collect::<BTreeSet<_>>();
    services
        .iter()
        .filter(|service| obsolete.contains(&service.identity))
        .flat_map(|service| service.containers_for(ContainerAction::Remove))
        .map(|container| {
            let observation = container.as_observation();
            DeployOperation::RemoveContainer {
                machine_id: observation.machine_id,
                container_id: observation.container_id,
            }
        })
        .collect()
}

fn specs_to_plan(intent: &DeployIntent) -> Result<Vec<&RequestedServiceSpec>, PlanError> {
    order_included(intent, &names_to_plan(intent))
}

fn names_to_plan(intent: &DeployIntent) -> BTreeSet<&ServiceName> {
    if intent.options.selected.is_empty() {
        intent
            .target
            .iter()
            .filter(|spec| intent.service_starts(&spec.name))
            .map(|spec| &spec.name)
            .collect()
    } else {
        expand_selected(intent)
    }
}

fn expand_selected(intent: &DeployIntent) -> BTreeSet<&ServiceName> {
    let present = intent
        .target
        .iter()
        .map(|spec| &spec.name)
        .collect::<BTreeSet<_>>();
    let mut included = BTreeSet::new();
    let mut pending = intent
        .options
        .selected
        .iter()
        .map(|attempt| &attempt.name)
        .filter(|name| present.contains(name))
        .collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        if included.insert(name) {
            pending.extend(
                intent
                    .dependencies()
                    .get(name)
                    .into_iter()
                    .flatten()
                    .map(|dependency| &dependency.service)
                    .filter(|dependency| present.contains(dependency)),
            );
        }
    }
    included
}

fn order_included<'intent>(
    intent: &'intent DeployIntent,
    included: &BTreeSet<&'intent ServiceName>,
) -> Result<Vec<&'intent RequestedServiceSpec>, PlanError> {
    fn visit<'intent>(
        name: &'intent ServiceName,
        intent: &'intent DeployIntent,
        included: &BTreeSet<&'intent ServiceName>,
        by_name: &BTreeMap<&ServiceName, &'intent RequestedServiceSpec>,
        visiting: &mut BTreeSet<&'intent ServiceName>,
        visited: &mut BTreeSet<&'intent ServiceName>,
        ordered: &mut Vec<&'intent RequestedServiceSpec>,
    ) -> Result<(), PlanError> {
        if visited.contains(name) {
            return Ok(());
        }
        if !visiting.insert(name) {
            return Err(PlanError::DependencyCycle {
                service: name.as_str().to_owned(),
            });
        }
        if let Some(dependencies) = intent.dependencies().get(name) {
            for dependency in dependencies {
                if included.contains(&&dependency.service) {
                    visit(
                        &dependency.service,
                        intent,
                        included,
                        by_name,
                        visiting,
                        visited,
                        ordered,
                    )?;
                }
            }
        }
        visiting.remove(name);
        visited.insert(name);
        if let Some(spec) = by_name.get(name) {
            ordered.push(*spec);
        }
        Ok(())
    }

    let mut by_name = BTreeMap::new();
    for spec in &intent.target {
        if by_name.insert(&spec.name, spec).is_some() {
            return Err(PlanError::DuplicateTargetService {
                service: spec.name.clone(),
            });
        }
    }
    let mut ordered = Vec::new();
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for spec in &intent.target {
        if included.contains(&&spec.name) {
            visit(
                &spec.name,
                intent,
                included,
                &by_name,
                &mut visiting,
                &mut visited,
                &mut ordered,
            )?;
        }
    }
    Ok(ordered)
}

fn plan_one_service(
    requested: &RequestedServiceSpec,
    project_name: &ProjectName,
    snapshot: &DeploySnapshot,
    services: &[ServiceObservation],
    pins: &mut VolumePins,
    placement: &mut PlacementState,
    options: &PlanOptions,
) -> Result<Vec<DeployOperation>, PlanError> {
    let mut machines = eligible_machines(requested, snapshot, options)?;
    let identity = QualifiedService::new(project_name.clone(), requested.name.clone());
    let existing = services.iter().find(|service| service.identity == identity);
    let (service_id, current, hooks) = match existing {
        None => (ServiceId::random(), &[][..], &[][..]),
        Some(service) => {
            if service.members().any(|container| {
                !same_service_mode_kind(
                    &container.as_observation().resolved_spec.mode,
                    &requested.mode,
                )
            }) {
                return Err(PlanError::ServiceModeCannotChange);
            }
            (
                service.service_id,
                service.containers.as_slice(),
                service.hook_containers.as_slice(),
            )
        }
    };
    let reservation = placement.take_reservation(&requested.name);
    let mut capacity_error = None;
    if matches!(requested.mode, ServiceMode::Replicated { .. }) && reservation.is_none() {
        constrain_volume_candidates(requested, snapshot, pins, &mut machines)?;
        let ServiceMode::Replicated { replicas } = requested.mode else {
            unreachable!("replicated branch has replicated mode")
        };
        let up_to_date = current
            .iter()
            .filter(|container| {
                machines
                    .iter()
                    .any(|machine| machine.machine.id == container.as_observation().machine_id)
                    && is_up_to_date(container, requested, options)
            })
            .count();
        if requested.pre_deploy.is_some() && up_to_date < replicas.get() as usize {
            placement.release_hooks(hooks);
        }
        let relevant = machines
            .iter()
            .map(|machine| machine.machine.id)
            .collect::<Vec<_>>();
        capacity_error = Some(placement.capacity_error_for(&relevant));
        machines.retain(|machine| {
            current.iter().any(|container| {
                container.as_observation().machine_id == machine.machine.id
                    && is_up_to_date(container, requested, options)
            }) || placement.capacity_fits(&machine.machine.id, 1)
        });
        if machines.is_empty() {
            return Err(placement.capacity_error_for(&relevant));
        }
    }
    plan_volume_operations(requested, snapshot, pins, &mut machines)?;
    let (service_operations, hook_machine) = match requested.mode {
        ServiceMode::Replicated { replicas } => plan_replicated(
            requested,
            &service_id,
            current,
            ReplicatedPlacement {
                machines,
                replicas: replicas.get() as usize,
                admission: reservation.unwrap_or_else(|| CapacityAdmission::Pending {
                    error: capacity_error.expect("unreserved capacity has relevant Machines"),
                }),
            },
            placement,
            options,
        )?,
        ServiceMode::Global => plan_global(
            requested,
            GlobalPlacement {
                identity: &identity,
                service_id: &service_id,
                current,
                hooks,
                machines,
            },
            placement,
            options,
        )?,
    };
    let mut operations =
        pre_deploy_operations(requested, hooks, &service_operations, hook_machine.as_ref());
    operations.extend(service_operations);
    Ok(operations)
}

fn service_error(name_errors_with_service: bool, service: &str, source: PlanError) -> PlanError {
    if name_errors_with_service {
        PlanError::Service {
            service: service.into(),
            source: Box::new(source),
        }
    } else {
        source
    }
}

fn eligible_machines<'snapshot>(
    requested: &RequestedServiceSpec,
    snapshot: &'snapshot DeploySnapshot,
    options: &PlanOptions,
) -> Result<Vec<&'snapshot MachineObservation>, PlanError> {
    let candidates = placement_candidates(requested, snapshot)?;
    let mut unknown = Vec::new();
    let mut machines = Vec::new();
    for machine in &candidates {
        match requested.placement_eligibility(&machine.machine, machine.storage.as_ref()) {
            ServicePlacementEligibility::Eligible => machines.push(*machine),
            ServicePlacementEligibility::Unknown(_) => {
                unknown.push(machine.machine.name.clone());
            }
            ServicePlacementEligibility::Ineligible(_) => {}
        }
    }
    if machines.is_empty() {
        if !unknown.is_empty() {
            return Err(PlanError::ProvisionedVolumeStorageUnknown { names: unknown });
        }
        if requested.placement.machines.len() == 1 && candidates.len() == 1 {
            return Err(PlanError::ProvisionedVolumeStorageRequired {
                machine: candidates
                    .first()
                    .expect("explicit placement resolved exactly one Machine")
                    .machine
                    .name
                    .clone(),
            });
        }
        return Err(PlanError::ProvisionedVolumeStorageUnavailable);
    }
    shuffle(
        &mut machines,
        placement_state(options.placement_seed, &requested.name),
    );
    Ok(machines)
}

fn placement_candidates<'snapshot>(
    requested: &RequestedServiceSpec,
    snapshot: &'snapshot DeploySnapshot,
) -> Result<Vec<&'snapshot MachineObservation>, PlanError> {
    let candidates = snapshot
        .machines
        .iter()
        .filter(|machine| machine.membership != MembershipObservation::Down)
        .filter(|machine| {
            !matches!(
                requested.placement_eligibility(&machine.machine, machine.storage.as_ref()),
                ServicePlacementEligibility::Ineligible(
                    ServicePlacementIneligibleReason::PlacementMismatch
                )
            )
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(placement_error(requested, snapshot));
    }
    Ok(candidates)
}

fn storage_eligibility_warnings(
    requested: &[RequestedServiceSpec],
    snapshot: &DeploySnapshot,
) -> Vec<DeployWarning> {
    let mut warned = BTreeSet::new();
    requested
        .iter()
        .flat_map(|spec| {
            snapshot
                .machines
                .iter()
                .filter(|machine| machine.membership != MembershipObservation::Down)
                .filter(move |machine| {
                    matches!(
                        spec.placement_eligibility(&machine.machine, machine.storage.as_ref()),
                        ServicePlacementEligibility::Unknown(_)
                    )
                })
        })
        .filter(|machine| warned.insert(machine.machine.id))
        .map(|machine| DeployWarning::StorageObservationUnknown {
            machine_id: machine.machine.id,
        })
        .collect()
}

fn placement_error(spec: &RequestedServiceSpec, snapshot: &DeploySnapshot) -> PlanError {
    if snapshot.machines.is_empty() {
        return PlanError::no_eligible_machines(vec![EliminatingConstraint::NoMachines]);
    }
    let targets = &spec.placement.machines;
    if targets.is_empty() {
        let names = snapshot
            .machines
            .iter()
            .filter(|machine| machine.membership == MembershipObservation::Down)
            .map(|machine| machine.machine.name.clone())
            .collect::<Vec<_>>();
        return PlanError::no_eligible_machines(vec![EliminatingConstraint::MachineDown { names }]);
    }
    let mut unknown = Vec::new();
    let mut down = Vec::new();
    for target in targets {
        let matched = snapshot
            .machines
            .iter()
            .filter(|machine| machine_matches_target(&machine.machine, target))
            .collect::<Vec<_>>();
        if matched.is_empty() {
            unknown.push(target.clone());
        } else if matched
            .iter()
            .all(|machine| machine.membership == MembershipObservation::Down)
        {
            for machine in matched {
                if !down.contains(&machine.machine.name) {
                    down.push(machine.machine.name.clone());
                }
            }
        }
    }
    let mut constraints = Vec::new();
    if !unknown.is_empty() {
        constraints.push(EliminatingConstraint::UnknownPlacement { targets: unknown });
    }
    if !down.is_empty() {
        constraints.push(EliminatingConstraint::MachineDown { names: down });
    }
    PlanError::no_eligible_machines(constraints)
}

fn placement_state(seed: u64, name: &ServiceName) -> u64 {
    name.as_str().bytes().fold(seed, |state, byte| {
        scramble(
            state
                .wrapping_add(u64::from(byte))
                .wrapping_add(0x9e37_79b9_7f4a_7c15),
        )
    })
}

fn scramble(mut state: u64) -> u64 {
    state = (state ^ (state >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    state = (state ^ (state >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    state ^ (state >> 31)
}

fn shuffle<T>(values: &mut [T], mut state: u64) {
    for upper in (1..values.len()).rev() {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        values.swap(upper, scramble(state) as usize % (upper + 1));
    }
}

fn pre_deploy_operations(
    requested: &RequestedServiceSpec,
    hooks: &[HookContainer],
    service_operations: &[DeployOperation],
    hook_machine: Option<&MachineId>,
) -> Vec<DeployOperation> {
    if requested.pre_deploy.is_none() {
        return Vec::new();
    }
    let target = service_operations
        .iter()
        .find_map(|operation| match operation {
            DeployOperation::RunContainer {
                machine_id, spec, ..
            }
            | DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id, spec, ..
            }) if hook_machine == Some(machine_id) => Some((machine_id, spec)),
            DeployOperation::RunContainer { .. } | DeployOperation::ReplaceContainer(_) => None,
            DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        });
    let Some((machine_id, spec)) = target else {
        return Vec::new();
    };

    let mut operations = hooks
        .iter()
        .filter(|container| super::is_active_runtime(&container.as_observation().runtime))
        .map(|container| {
            let observation = container.as_observation();
            DeployOperation::StopHook {
                machine_id: observation.machine_id,
                container_id: observation.container_id,
            }
        })
        .collect::<Vec<_>>();
    operations.push(DeployOperation::RunHook {
        machine_id: *machine_id,
        spec: spec.clone(),
        old_hook_containers: hooks
            .iter()
            .map(|container| {
                let observation = container.as_observation();
                (observation.machine_id, observation.container_id)
            })
            .collect(),
    });
    operations
}
