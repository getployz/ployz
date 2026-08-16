use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    ContainerKind, ContainerObservation, ContainerRuntimeObservation, HostBind, MachineId,
    MachineObservation, MembershipObservation, PortPublication, RequestedServiceSpec,
    ResolvedServiceSpec, ResolvedUpdateConfig, ServiceId, ServiceMode, SpecChange, UpdateOrder,
    VolumeSource, compare_specs, machine_matches_selector, same_service_mode_kind,
};

use super::{
    DeployOperation, DeployPlan, DeploySnapshot, PlanError, PlanOptions, ReplacementOperation,
    ServicePlan,
};

mod volumes;

use volumes::{
    VolumePins, named_volume_uses, plan_volume_operations, prepare_shared_replicated_volumes,
    reject_mixed_volume_modes,
};

pub fn plan_deploy<'a>(
    requested: impl IntoIterator<Item = &'a RequestedServiceSpec>,
    snapshot: &DeploySnapshot,
    options: PlanOptions,
) -> Result<DeployPlan, PlanError> {
    // TODO(UT-009): preserve the missing within-spec port-conflict validation.
    let requested = requested
        .into_iter()
        .map(normalize_and_validate)
        .collect::<Result<Vec<_>, _>>()?;
    let volume_uses = named_volume_uses(&requested);
    reject_mixed_volume_modes(&volume_uses)?;
    let mut pins = VolumePins::default();
    let mut volume_operations =
        prepare_shared_replicated_volumes(&volume_uses, snapshot, &mut pins, options)?;
    let name_errors_with_service = requested.len() > 1;
    let mut service_plans = Vec::with_capacity(requested.len());
    for spec in &requested {
        service_plans.push(
            plan_one_service(spec, snapshot, &mut pins, &mut volume_operations, options).map_err(
                |source| service_error(name_errors_with_service, spec.name.as_str(), source),
            )?,
        );
    }
    Ok(DeployPlan::new(volume_operations, service_plans))
}

fn plan_one_service(
    requested: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    pins: &mut VolumePins,
    volume_operations: &mut Vec<DeployOperation>,
    options: PlanOptions,
) -> Result<ServicePlan, PlanError> {
    let mut machines = eligible_machines(requested, snapshot, options);
    volume_operations.extend(plan_volume_operations(
        requested,
        snapshot,
        pins,
        &mut machines,
    )?);
    let matching_service_ids = snapshot
        .containers
        .iter()
        .filter(|container| container.service_name == requested.name)
        .map(|container| container.service_id)
        .collect::<BTreeSet<_>>();
    let (service_id, is_new_service) = match matching_service_ids.len() {
        0 => (ServiceId::random(), true),
        1 => (
            matching_service_ids
                .into_iter()
                .next()
                .ok_or(PlanError::NoEligibleMachines)?,
            false,
        ),
        _ => {
            return Err(PlanError::AmbiguousService {
                matches: matching_service_ids.into_iter().collect(),
            });
        }
    };
    if !is_new_service
        && snapshot.containers.iter().any(|container| {
            container.service_id == service_id
                && !same_service_mode_kind(&container.resolved_spec.mode, &requested.mode)
        })
    {
        return Err(PlanError::ServiceModeCannotChange);
    }

    let service_operations = match requested.mode {
        ServiceMode::Replicated { replicas } => plan_replicated(
            requested,
            snapshot,
            &service_id,
            machines,
            replicas.get() as usize,
            options,
        ),
        ServiceMode::Global => plan_global(requested, snapshot, &service_id, machines, options),
    };
    let mut operations =
        pre_deploy_operations(requested, snapshot, &service_id, &service_operations);
    operations.extend(service_operations);
    Ok(ServicePlan {
        service_id,
        is_new_service,
        operations,
    })
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
    options: PlanOptions,
) -> Vec<&'a MachineObservation> {
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
                    .any(|selector| machine_matches_selector(&machine.machine, selector))
        })
        .collect::<Vec<_>>();
    shuffle(&mut machines, options.placement_seed);
    machines
}

fn normalize_and_validate(
    requested: &RequestedServiceSpec,
) -> Result<RequestedServiceSpec, PlanError> {
    let mut normalized = requested.clone();
    normalized.caddy_config = normalized
        .caddy_config
        .take()
        .map(|config| config.trim().to_owned())
        .filter(|config| !config.is_empty());

    let mut volumes = BTreeSet::new();
    for volume in &normalized.volumes {
        if !volumes.insert(volume.reference.clone()) {
            return Err(PlanError::DuplicateVolumeReference {
                reference: volume.reference.clone(),
            });
        }
    }
    for mount in &normalized.mounts {
        if !volumes.contains(&mount.volume) {
            return Err(PlanError::UnknownVolumeReference {
                reference: mount.volume.clone(),
            });
        }
    }

    let mut configs = BTreeSet::new();
    for config in &normalized.configs {
        if !configs.insert(config.name.as_str()) {
            return Err(PlanError::DuplicateConfigName {
                name: config.name.clone(),
            });
        }
    }
    for mount in &normalized.container.config_mounts {
        if !configs.contains(mount.config_name.as_str()) {
            return Err(PlanError::UnknownConfigName {
                name: mount.config_name.clone(),
            });
        }
    }
    Ok(normalized)
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
    snapshot: &DeploySnapshot,
    service_id: &ServiceId,
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
            | DeployOperation::RunHook { .. } => None,
        });
    let Some((machine_id, spec)) = target else {
        return Vec::new();
    };

    let hooks = snapshot
        .containers
        .iter()
        .filter(|container| {
            container.service_id == *service_id && container.kind == ContainerKind::PreDeployHook
        })
        .collect::<Vec<_>>();
    let mut operations = hooks
        .iter()
        .filter(|container| super::is_active_runtime(&container.runtime))
        .map(|container| DeployOperation::StopHook {
            machine_id: container.machine_id,
            container_id: container.container_id,
        })
        .collect::<Vec<_>>();
    operations.push(DeployOperation::RunHook {
        machine_id: *machine_id,
        spec: spec.clone(),
        old_hook_containers: hooks
            .into_iter()
            .map(|container| (container.machine_id, container.container_id))
            .collect(),
    });
    operations
}

fn has_mounted_named_volume(requested: &RequestedServiceSpec) -> bool {
    requested.mounts.iter().any(|mount| {
        requested.volumes.iter().any(|volume| {
            volume.reference == mount.volume && matches!(volume.source, VolumeSource::Named { .. })
        })
    })
}

fn plan_global(
    requested: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
    service_id: &ServiceId,
    machines: Vec<&MachineObservation>,
    options: PlanOptions,
) -> Vec<DeployOperation> {
    let current = service_containers(snapshot, service_id);
    let mut used = BTreeSet::new();
    let mut operations = Vec::new();

    for machine in machines {
        let on_machine = current
            .iter()
            .copied()
            .filter(|(_, container)| container.machine_id == machine.machine.id)
            .collect::<Vec<_>>();
        if let Some((kept_index, _)) = on_machine
            .iter()
            .copied()
            .find(|(_, container)| is_up_to_date(container, requested, options))
        {
            used.insert(kept_index);
            continue;
        }

        if let Some((replaced_index, container)) = on_machine
            .iter()
            .copied()
            .find(|(_, container)| super::is_active_runtime(&container.runtime))
        {
            used.insert(replaced_index);
            for (index, other) in on_machine.iter().copied() {
                if index != replaced_index
                    && super::is_active_runtime(&other.runtime)
                    && other.resolved_spec.ports.iter().any(|old| {
                        requested
                            .ports
                            .iter()
                            .any(|new| host_ports_conflict(old, new))
                    })
                {
                    operations.push(DeployOperation::StopContainer {
                        machine_id: machine.machine.id,
                        container_id: other.container_id,
                    });
                }
            }
            let order = determine_update_order(container, requested);
            operations.push(DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id: machine.machine.id,
                old_container_id: container.container_id,
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
    snapshot: &DeploySnapshot,
    service_id: &ServiceId,
    mut machines: Vec<&MachineObservation>,
    replicas: usize,
    options: PlanOptions,
) -> Vec<DeployOperation> {
    let current = service_containers(snapshot, service_id);
    let mut by_machine = BTreeMap::<MachineId, Vec<(usize, &ContainerObservation)>>::new();
    for (index, container) in &current {
        by_machine
            .entry(container.machine_id)
            .or_default()
            .push((*index, container));
    }
    for containers in by_machine.values_mut() {
        containers.sort_by_key(|(_, container)| is_up_to_date(container, requested, options));
    }
    machines.sort_by_key(|machine| {
        let containers = by_machine.get(&machine.machine.id);
        let up_to_date = containers
            .into_iter()
            .flatten()
            .filter(|(_, container)| is_up_to_date(container, requested, options))
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
            .map(|(index, container)| {
                used.insert(index);
                container
            });
        match existing {
            Some(container) if is_up_to_date(container, requested, options) => {}
            Some(container) => {
                let order = determine_update_order(container, requested);
                operations.push(DeployOperation::ReplaceContainer(ReplacementOperation {
                    machine_id: machine.machine.id,
                    old_container_id: container.container_id,
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

fn service_containers<'a>(
    snapshot: &'a DeploySnapshot,
    service_id: &ServiceId,
) -> Vec<(usize, &'a ContainerObservation)> {
    snapshot
        .containers
        .iter()
        .enumerate()
        .filter(|(_, container)| {
            container.service_id == *service_id && container.kind == ContainerKind::ServiceContainer
        })
        .collect()
}

fn remove_unused(
    operations: &mut Vec<DeployOperation>,
    current: Vec<(usize, &ContainerObservation)>,
    used: &BTreeSet<usize>,
) {
    for (index, container) in current {
        if !used.contains(&index) {
            // TODO(UT-075): placement changes remove now-ineligible containers; there is no
            // deploy-time Machine filter that leaves excluded containers running.
            operations.push(DeployOperation::RemoveContainer {
                machine_id: container.machine_id,
                container_id: container.container_id,
            });
        }
    }
}

fn is_up_to_date(
    container: &ContainerObservation,
    requested: &RequestedServiceSpec,
    options: PlanOptions,
) -> bool {
    !options.force_recreate
        && is_running(container)
        && compare_specs(&container.resolved_spec, requested) == SpecChange::UpToDate
}

fn is_running(container: &ContainerObservation) -> bool {
    matches!(
        container.runtime,
        ContainerRuntimeObservation::Running { .. }
    )
}

fn determine_update_order(
    current: &ContainerObservation,
    requested: &RequestedServiceSpec,
) -> UpdateOrder {
    if let Some(order) = requested.update.order {
        return order;
    }
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
        && has_mounted_named_volume(requested)
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

#[must_use]
fn resolve(
    requested: &RequestedServiceSpec,
    service_id: ServiceId,
    order: UpdateOrder,
) -> ResolvedServiceSpec {
    ResolvedServiceSpec {
        service_id,
        name: requested.name.clone(),
        mode: requested.mode.clone(),
        container: requested.container.clone(),
        placement: requested.placement.clone(),
        ports: requested.ports.clone(),
        volumes: requested.volumes.clone(),
        mounts: requested.mounts.clone(),
        configs: requested.configs.clone(),
        pre_deploy: requested.pre_deploy.clone(),
        caddy_config: requested.caddy_config.clone(),
        update: ResolvedUpdateConfig {
            order,
            monitor_millis: requested.update.monitor_millis,
        },
    }
}
