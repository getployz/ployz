use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    ContainerAction, ContainerId, ContainerRuntimeObservation, DataLoss, HookContainer, HostBind,
    IngressHost, MachineId, MachineObservation, MembershipObservation, ObservedDataLoss,
    PortPublication, ProjectName, QualifiedService, RequestedServiceSpec, ResolvedServiceSpec,
    ResolvedUpdateConfig, ServiceContainer, ServiceId, ServiceMode, ServiceName,
    ServiceObservation, ServiceVolumeGraph, SpecChange, UpdateOrder, VolumeSource, compare_specs,
    explicit_ingress_hosts, hostname_owners, machine_matches_target, same_service_mode_kind,
};

use super::{
    DeployIntent, DeployOperation, DeployPlan, DeploySnapshot, DeployWarning,
    EliminatingConstraint, PlanError, PlanOptions, ReplacementOperation,
};

mod volumes;

use volumes::{
    VolumePins, named_volume_uses, plan_volume_operations, prepare_shared_replicated_volumes,
    preserved_owned_volumes, reject_mixed_volume_modes, scope_requested,
};

/// Whether Project removal keeps or destroys observer-visible managed volumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumeFate {
    /// Leave owned Docker Volumes in place. They keep listing under the Project.
    Preserve,
    /// Destroy each preserved owned Docker Volume after Services are removed.
    Destroy,
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
/// Returns when [`plan_deploy`] cannot produce a plan.
pub fn plan_project_removal(
    project: &ProjectName,
    snapshot: &DeploySnapshot,
    volumes: VolumeFate,
) -> Result<DeployPlan, PlanError> {
    let intent = DeployIntent::apply_all(project.clone(), [], PlanOptions::default());
    let mut plan = plan_deploy(&intent, snapshot)?;
    if volumes == VolumeFate::Destroy && plan.prune_refusal.is_none() {
        plan.operations.extend(
            plan.preserved_volumes
                .drain(..)
                .map(|volume| DeployOperation::RemoveVolume { id: volume.id }),
        );
    }
    Ok(plan)
}

/// Data Loss implied by a Project-removal plan.
///
/// Preserve plans are empty. Destroy names each `RemoveVolume`. Completeness is
/// the caller's check; this listing is not a Cluster view.
#[must_use]
pub fn data_loss_from_plan(plan: &DeployPlan) -> ObservedDataLoss {
    ObservedDataLoss {
        data_loss: plan
            .operations
            .iter()
            .filter_map(remove_volume_loss)
            .collect(),
    }
}

fn remove_volume_loss(operation: &DeployOperation) -> Option<DataLoss> {
    let DeployOperation::RemoveVolume { id } = operation else {
        return None;
    };
    Some(DataLoss::DockerVolume(id.clone()))
}

/// Plan operations for the Services this Deploy applies from the target.
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
/// Returns when placement, volumes, service identity, hostname ownership, or
/// the apply-set dependency graph cannot produce a plan.
pub fn plan_deploy(
    intent: &DeployIntent,
    snapshot: &DeploySnapshot,
) -> Result<DeployPlan, PlanError> {
    let requested = requested_specs(intent)?;
    let warnings = hostname_policy_for(&intent.project_name, &requested, snapshot)?;
    assemble_plan(intent, snapshot, requested, warnings)
}

fn requested_specs(intent: &DeployIntent) -> Result<Vec<RequestedServiceSpec>, PlanError> {
    Ok(specs_to_plan(intent)?.into_iter().map(normalize).collect())
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
    snapshot: &DeploySnapshot,
    requested: Vec<RequestedServiceSpec>,
    warnings: Vec<DeployWarning>,
) -> Result<DeployPlan, PlanError> {
    // TODO(UT-009): preserve the missing within-spec port-conflict validation.
    let mut intent = intent.clone();
    intent.target = intent
        .target
        .iter()
        .map(|spec| scope_requested(spec.clone(), &intent.project_name))
        .collect();
    let requested: Vec<_> = requested
        .into_iter()
        .map(|spec| scope_requested(spec, &intent.project_name))
        .collect();
    let options = &intent.options;
    let volume_uses = named_volume_uses(&requested);
    reject_mixed_volume_modes(&volume_uses)?;
    let mut pins = VolumePins::default();
    prepare_shared_replicated_volumes(&volume_uses, snapshot, &mut pins, options)?;
    let name_errors_with_service = requested.len() > 1;
    let services = snapshot.services_in(&intent.project_name);
    let mut service_operations = Vec::new();
    for spec in &requested {
        service_operations.extend(
            plan_one_service(
                spec,
                &intent.project_name,
                snapshot,
                &services,
                &mut pins,
                options,
            )
            .map_err(|source| {
                service_error(name_errors_with_service, spec.name.as_str(), source)
            })?,
        );
    }
    let mut operations = pins.into_creates();
    operations.extend(service_operations);
    let would_remove = obsolete_services(&intent, &services);
    let prune_refusal = intent.prune_refusal(snapshot.is_observer_complete());
    let preserved_volumes = preserved_owned_volumes(&intent, snapshot);
    if prune_refusal.is_none() {
        operations.extend(removal_operations(&services, &would_remove));
    }
    Ok(DeployPlan {
        operations,
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
                if included.contains(&dependency) {
                    visit(
                        dependency, intent, included, by_name, visiting, visited, ordered,
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

    let by_name = intent
        .target
        .iter()
        .map(|spec| (&spec.name, spec))
        .collect::<BTreeMap<_, _>>();
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
    options: &PlanOptions,
) -> Result<Vec<DeployOperation>, PlanError> {
    let mut machines = eligible_machines(requested, snapshot, options)?;
    plan_volume_operations(requested, snapshot, pins, &mut machines)?;
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
    let service_operations = match requested.mode {
        ServiceMode::Replicated { replicas } => plan_replicated(
            requested,
            &service_id,
            current,
            machines,
            replicas.get() as usize,
            options,
        ),
        ServiceMode::Global => plan_global(requested, &service_id, current, machines, options),
    };
    let mut operations = pre_deploy_operations(requested, hooks, &service_operations);
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

fn eligible_machines<'a>(
    requested: &RequestedServiceSpec,
    snapshot: &'a DeploySnapshot,
    options: &PlanOptions,
) -> Result<Vec<&'a MachineObservation>, PlanError> {
    let mut machines = snapshot
        .machines
        .iter()
        .filter(|machine| machine.membership != MembershipObservation::Down)
        .filter(|machine| {
            requested.placement.machines.is_empty()
                || requested
                    .placement
                    .machines
                    .iter()
                    .any(|target| machine_matches_target(&machine.machine, target))
        })
        .collect::<Vec<_>>();
    if machines.is_empty() {
        return Err(placement_error(requested, snapshot));
    }
    shuffle(&mut machines, options.placement_seed);
    Ok(machines)
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

fn normalize(requested: &RequestedServiceSpec) -> RequestedServiceSpec {
    let mut requested = requested.clone();
    requested.caddy_config = requested
        .caddy_config
        .take()
        .map(|config| config.trim().to_owned())
        .filter(|config| !config.is_empty());
    requested
}

fn shuffle<T>(values: &mut [T], mut state: u64) {
    for upper in (1..values.len()).rev() {
        state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut random = state;
        random = (random ^ (random >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        random = (random ^ (random >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        random ^= random >> 31;
        values.swap(upper, random as usize % (upper + 1));
    }
}

fn pre_deploy_operations(
    requested: &RequestedServiceSpec,
    hooks: &[HookContainer],
    service_operations: &[DeployOperation],
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
            }) => Some((machine_id, spec)),
            DeployOperation::CreateVolume { .. }
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

fn has_mounted_named_volume(graph: &ServiceVolumeGraph) -> bool {
    graph
        .mounts()
        .iter()
        .any(|mount| matches!(graph.volume_for(mount).source, VolumeSource::Named { .. }))
}

fn plan_global(
    requested: &RequestedServiceSpec,
    service_id: &ServiceId,
    current: &[ServiceContainer],
    machines: Vec<&MachineObservation>,
    options: &PlanOptions,
) -> Vec<DeployOperation> {
    let mut used = BTreeSet::new();
    let mut operations = Vec::new();

    for machine in machines {
        let on_machine = current
            .iter()
            .filter(|container| container.as_observation().machine_id == machine.machine.id)
            .collect::<Vec<_>>();
        if let Some(kept) = on_machine
            .iter()
            .copied()
            .find(|container| is_up_to_date(container, requested, options))
        {
            used.insert(kept.as_observation().container_id);
            continue;
        }

        if let Some(container) = on_machine
            .iter()
            .copied()
            .find(|container| super::is_active_runtime(&container.as_observation().runtime))
        {
            let observation = container.as_observation();
            used.insert(observation.container_id);
            for other in &on_machine {
                let other_observation = other.as_observation();
                if other_observation.container_id != observation.container_id
                    && super::is_active_runtime(&other_observation.runtime)
                    && other_observation.resolved_spec.ports.iter().any(|old| {
                        requested
                            .ports
                            .iter()
                            .any(|new| host_ports_conflict(old, new))
                    })
                {
                    operations.push(DeployOperation::StopContainer {
                        machine_id: machine.machine.id,
                        container_id: other_observation.container_id,
                    });
                }
            }
            let order = determine_update_order(container, requested);
            operations.push(DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id: machine.machine.id,
                old_container_id: observation.container_id,
                spec: resolve(requested, *service_id, order),
                skip_health_monitor: options.skip_health_monitor,
            }));
        } else {
            operations.push(DeployOperation::RunContainer {
                machine_id: machine.machine.id,
                spec: resolve(
                    requested,
                    *service_id,
                    requested.update.order.unwrap_or(UpdateOrder::StartFirst),
                ),
                skip_health_monitor: options.skip_health_monitor,
            });
        }
    }

    remove_unused(&mut operations, current, &used);
    operations
}

fn plan_replicated(
    requested: &RequestedServiceSpec,
    service_id: &ServiceId,
    current: &[ServiceContainer],
    mut machines: Vec<&MachineObservation>,
    replicas: usize,
    options: &PlanOptions,
) -> Vec<DeployOperation> {
    let mut by_machine = BTreeMap::<MachineId, Vec<&ServiceContainer>>::new();
    for container in current {
        by_machine
            .entry(container.as_observation().machine_id)
            .or_default()
            .push(container);
    }
    for containers in by_machine.values_mut() {
        containers.sort_by_key(|container| is_up_to_date(container, requested, options));
    }
    machines.sort_by_key(|machine| {
        let containers = by_machine.get(&machine.machine.id);
        let up_to_date = containers
            .into_iter()
            .flatten()
            .filter(|container| is_up_to_date(container, requested, options))
            .count();
        (
            std::cmp::Reverse(up_to_date),
            std::cmp::Reverse(containers.map_or(0, Vec::len)),
        )
    });

    let mut used = BTreeSet::new();
    let mut operations = Vec::new();
    for machine in machines.iter().cycle().take(replicas) {
        let existing = by_machine
            .get_mut(&machine.machine.id)
            .and_then(Vec::pop)
            .inspect(|container| {
                used.insert(container.as_observation().container_id);
            });
        match existing {
            Some(container) if is_up_to_date(container, requested, options) => {}
            Some(container) => {
                let order = determine_update_order(container, requested);
                operations.push(DeployOperation::ReplaceContainer(ReplacementOperation {
                    machine_id: machine.machine.id,
                    old_container_id: container.as_observation().container_id,
                    spec: resolve(requested, *service_id, order),
                    skip_health_monitor: options.skip_health_monitor,
                }));
            }
            None => operations.push(DeployOperation::RunContainer {
                machine_id: machine.machine.id,
                spec: resolve(
                    requested,
                    *service_id,
                    requested.update.order.unwrap_or(UpdateOrder::StartFirst),
                ),
                skip_health_monitor: options.skip_health_monitor,
            }),
        }
    }
    remove_unused(&mut operations, current, &used);
    operations
}

fn remove_unused(
    operations: &mut Vec<DeployOperation>,
    current: &[ServiceContainer],
    used: &BTreeSet<ContainerId>,
) {
    for container in current {
        let observation = container.as_observation();
        if !used.contains(&observation.container_id) {
            // TODO(UT-075): placement changes remove now-ineligible containers; there is no
            // deploy-time Machine filter that leaves excluded containers running.
            operations.push(DeployOperation::RemoveContainer {
                machine_id: observation.machine_id,
                container_id: observation.container_id,
            });
        }
    }
}

fn is_up_to_date(
    container: &ServiceContainer,
    requested: &RequestedServiceSpec,
    options: &PlanOptions,
) -> bool {
    let observation = container.as_observation();
    !options.force_recreate
        && is_running(&observation.runtime)
        && compare_specs(&observation.resolved_spec, requested) == SpecChange::UpToDate
}

fn is_running(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(runtime, ContainerRuntimeObservation::Running { .. })
}

fn determine_update_order(
    current: &ServiceContainer,
    requested: &RequestedServiceSpec,
) -> UpdateOrder {
    if let Some(order) = requested.update.order {
        return order;
    }
    let current = current.as_observation();
    if current.resolved_spec.ports.iter().any(|old| {
        requested
            .ports
            .iter()
            .any(|new| host_ports_conflict(old, new))
    }) {
        return UpdateOrder::StopFirst;
    }
    if (matches!(requested.mode, ServiceMode::Global)
        || matches!(
            requested.mode,
            ServiceMode::Replicated { replicas } if replicas.get() == 1
        ))
        && has_mounted_named_volume(&requested.volume_graph)
    {
        return UpdateOrder::StopFirst;
    }
    UpdateOrder::StartFirst
}

fn host_ports_conflict(left: &PortPublication, right: &PortPublication) -> bool {
    let (
        PortPublication::Host {
            bind: left_bind,
            published_port: left_port,
            transport_protocol: left_protocol,
            ..
        },
        PortPublication::Host {
            bind: right_bind,
            published_port: right_port,
            transport_protocol: right_protocol,
            ..
        },
    ) = (left, right)
    else {
        return false;
    };
    left_port == right_port
        && left_protocol == right_protocol
        && binds_overlap(left_bind, right_bind)
}

fn binds_overlap(left: &HostBind, right: &HostBind) -> bool {
    match (left, right) {
        (HostBind::All, _) | (_, HostBind::All) => true,
        (HostBind::Address { address: left }, HostBind::Address { address: right }) => {
            left == right
        }
        (HostBind::Address { address }, HostBind::Prefix { prefix })
        | (HostBind::Prefix { prefix }, HostBind::Address { address }) => prefix.contains(address),
        (HostBind::Prefix { prefix: left }, HostBind::Prefix { prefix: right }) => {
            left.contains(&right.network()) || right.contains(&left.network())
        }
    }
}

fn resolve(
    requested: &RequestedServiceSpec,
    service_id: ServiceId,
    order: UpdateOrder,
) -> ResolvedServiceSpec {
    requested.to_resolved(
        service_id,
        ResolvedUpdateConfig {
            order,
            monitor_millis: requested.update.monitor_millis,
        },
    )
}
