//! Machine placement and replacement planning.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    ContainerId, ContainerRuntimeObservation, HookContainer, HostBind, IngressProxyNetworkMode,
    MachineId, MachineObservation, PortPublication, QualifiedService, RequestedServiceSpec,
    ResolvedServiceSpec, ResolvedUpdateConfig, ServiceContainer, ServiceId, ServiceMode,
    ServiceName, ServiceObservation, SpecChange, UpdateOrder, VolumeSource, compare_specs,
    requested_ingress_proxy_backend,
};

use super::capacity::{CapacityBudget, EndpointDemand, EndpointOperation};
use super::{DeployOperation, PlanError, PlanOptions, ReplacementOperation};

pub(super) struct PlacementState {
    occupancy: BTreeMap<MachineId, usize>,
    capacity: CapacityBudget,
    reservations: BTreeMap<ServiceName, ReplicatedCapacityReservation>,
}

impl PlacementState {
    pub(super) fn new(
        capacity: CapacityBudget,
        reservations: impl IntoIterator<Item = (ServiceName, ReplicatedCapacityReservation)>,
    ) -> Self {
        Self {
            occupancy: BTreeMap::new(),
            capacity,
            reservations: reservations.into_iter().collect(),
        }
    }

    pub(super) fn capacity_fits(&self, machine_id: &MachineId, peak: u64) -> bool {
        self.capacity.fits(machine_id, peak)
    }

    pub(super) fn take_reservation(&mut self, service: &ServiceName) -> Option<CapacityAdmission> {
        self.reservations
            .remove(service)
            .map(|reservation| CapacityAdmission::Reserved { reservation })
    }

    pub(super) fn capacity_error_for<'a>(
        &self,
        machine_ids: impl IntoIterator<Item = &'a MachineId>,
    ) -> PlanError {
        self.capacity.error_for(machine_ids)
    }

    pub(super) fn release_hooks(&mut self, hooks: &[HookContainer]) {
        release_hooks(&mut self.capacity, hooks);
    }
}

pub(super) struct GlobalPlacement<'placement> {
    pub(super) identity: &'placement QualifiedService,
    pub(super) service_id: &'placement ServiceId,
    pub(super) current: &'placement [ServiceContainer],
    pub(super) hooks: &'placement [HookContainer],
    pub(super) machines: Vec<&'placement MachineObservation>,
}

pub(super) fn plan_global(
    requested: &RequestedServiceSpec,
    target: GlobalPlacement<'_>,
    placement: &mut PlacementState,
    options: &PlanOptions,
) -> Result<(Vec<DeployOperation>, Option<MachineId>), PlanError> {
    let GlobalPlacement {
        identity,
        service_id,
        current,
        hooks,
        machines,
    } = target;
    let uses_bridge_endpoint = identity != &QualifiedService::system_ingress()
        || requested_ingress_proxy_backend(requested).map_or(true, |backend| {
            matches!(backend.network_mode(), IngressProxyNetworkMode::Bridge)
        });
    let endpoint_demand = |operation, hook_pending| {
        if uses_bridge_endpoint {
            EndpointDemand::for_operation(operation, hook_pending)
        } else {
            EndpointDemand::for_host_network(operation, hook_pending)
        }
    };
    let has_changes = machines.iter().any(|machine| {
        !matches!(
            global_endpoint_operation(current, machine.machine.id, requested, options),
            EndpointOperation::Unchanged
        )
    });
    if requested.pre_deploy.is_some() && has_changes {
        release_hooks(&mut placement.capacity, hooks);
    }
    let capacity_error = placement
        .capacity
        .error_for(machines.iter().filter_map(|machine| {
            (!matches!(
                global_endpoint_operation(current, machine.machine.id, requested, options),
                EndpointOperation::Unchanged
            ))
            .then_some(&machine.machine.id)
        }));
    let mut used = BTreeSet::new();
    let mut operations = Vec::new();
    let hook_machine = requested.pre_deploy.as_ref().and_then(|_| {
        machines.iter().find_map(|machine| {
            let operation =
                global_endpoint_operation(current, machine.machine.id, requested, options);
            (!matches!(operation, EndpointOperation::Unchanged)
                && placement
                    .capacity
                    .fits_demand(&machine.machine.id, endpoint_demand(operation, true)))
            .then_some(machine.machine.id)
        })
    });
    if requested.pre_deploy.is_some() && hook_machine.is_none() && has_changes {
        return Err(capacity_error.clone());
    }

    for machine in machines {
        let machine_id = machine.machine.id;
        *placement.occupancy.entry(machine_id).or_default() += 1;
        if let Some(kept) = on_machine(current, machine_id)
            .find(|container| is_up_to_date(container, requested, options))
        {
            used.insert(kept.as_observation().container_id);
            continue;
        }

        if let Some(container) = on_machine(current, machine_id)
            .find(|container| super::super::is_active_runtime(&container.as_observation().runtime))
        {
            let observation = container.as_observation();
            let demand =
                endpoint_demand(EndpointOperation::Replace, hook_machine == Some(machine_id));
            if !placement.capacity.reserve(&machine_id, demand) {
                return Err(capacity_error.clone());
            }
            used.insert(observation.container_id);
            for other in on_machine(current, machine_id) {
                let other_observation = other.as_observation();
                if other_observation.container_id != observation.container_id
                    && super::super::is_active_runtime(&other_observation.runtime)
                    && other_observation.resolved_spec.ports.iter().any(|old| {
                        requested
                            .ports
                            .iter()
                            .any(|new| host_ports_conflict(old, new))
                    })
                {
                    operations.push(DeployOperation::StopContainer {
                        machine_id,
                        container_id: other_observation.container_id,
                        purpose: ployz_core::StopContainerPurpose::FreeHostPorts,
                    });
                }
            }
            let order = determine_update_order(Some(container), requested);
            operations.push(DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id,
                old_container_id: observation.container_id,
                spec: resolve(requested, *service_id, order),
                skip_health_monitor: options.skip_health_monitor,
            }));
        } else {
            let demand =
                endpoint_demand(EndpointOperation::Create, hook_machine == Some(machine_id));
            if !placement.capacity.reserve(&machine_id, demand) {
                return Err(capacity_error.clone());
            }
            operations.push(DeployOperation::RunContainer {
                machine_id,
                spec: resolve(
                    requested,
                    *service_id,
                    determine_update_order(None, requested),
                ),
                skip_health_monitor: options.skip_health_monitor,
            });
        }
    }

    remove_unused(&mut operations, current, &used, &mut placement.capacity);
    Ok((operations, hook_machine))
}

fn global_endpoint_operation(
    current: &[ServiceContainer],
    machine_id: MachineId,
    requested: &RequestedServiceSpec,
    options: &PlanOptions,
) -> EndpointOperation {
    if on_machine(current, machine_id).any(|container| is_up_to_date(container, requested, options))
    {
        EndpointOperation::Unchanged
    } else if on_machine(current, machine_id)
        .any(|container| super::super::is_active_runtime(&container.as_observation().runtime))
    {
        EndpointOperation::Replace
    } else {
        EndpointOperation::Create
    }
}

fn on_machine(
    current: &[ServiceContainer],
    machine_id: MachineId,
) -> impl Iterator<Item = &ServiceContainer> {
    current
        .iter()
        .filter(move |container| container.as_observation().machine_id == machine_id)
}

pub(super) enum CapacityAdmission {
    Pending {
        error: PlanError,
    },
    Reserved {
        reservation: ReplicatedCapacityReservation,
    },
}

pub(super) struct ReplicatedCapacityReservation {
    machine_id: MachineId,
    operations: Vec<EndpointOperation>,
    hook_machine: Option<MachineId>,
}

pub(super) struct ReplicatedPlacement<'a> {
    pub(super) machines: Vec<&'a MachineObservation>,
    pub(super) replicas: usize,
    pub(super) admission: CapacityAdmission,
}

pub(super) fn plan_replicated(
    requested: &RequestedServiceSpec,
    service_id: &ServiceId,
    current: &[ServiceContainer],
    target: ReplicatedPlacement<'_>,
    placement: &mut PlacementState,
    options: &PlanOptions,
) -> Result<(Vec<DeployOperation>, Option<MachineId>), PlanError> {
    let ReplicatedPlacement {
        mut machines,
        replicas,
        admission,
    } = target;
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
    if let CapacityAdmission::Pending { error } = &admission {
        let existing = machines
            .iter()
            .map(|machine| by_machine.get(&machine.machine.id).map_or(0, Vec::len))
            .sum::<usize>();
        let up_to_date = machines
            .iter()
            .flat_map(|machine| by_machine.get(&machine.machine.id).into_iter().flatten())
            .filter(|container| is_up_to_date(container, requested, options))
            .count();
        let required = replicas
            .saturating_sub(existing)
            .saturating_add(usize::from(
                requested.pre_deploy.is_some() && up_to_date < replicas,
            ));
        if !placement
            .capacity
            .can_supply_persistent(machines.iter().map(|machine| &machine.machine.id), required)
        {
            return Err(error.clone());
        }
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
            placement
                .occupancy
                .get(&machine.machine.id)
                .copied()
                .unwrap_or(0),
        )
    });

    let mut used = BTreeSet::new();
    let mut operations = Vec::new();
    let mut cursor = 0;
    let (pending_error, reserved_machine, mut reserved_operations, mut hook_machine) =
        match admission {
            CapacityAdmission::Pending { error } => (Some(error), None, None, None),
            CapacityAdmission::Reserved { reservation } => (
                None,
                Some(reservation.machine_id),
                Some(reservation.operations.into_iter()),
                reservation.hook_machine,
            ),
        };
    let mut hook_pending = requested.pre_deploy.is_some() && reserved_operations.is_none();
    for _ in 0..replicas {
        let selected = if let Some(operations) = reserved_operations.as_mut() {
            let operation = operations
                .next()
                .expect("capacity reservation has one slot per replica");
            let machine = machines
                .iter()
                .find(|machine| Some(machine.machine.id) == reserved_machine)
                .copied()
                .expect("reserved Machine remains volume-eligible");
            Some((machine, operation, None))
        } else {
            let mut selected = None;
            for _ in 0..machines.len() {
                let machine = machines
                    .get(cursor % machines.len())
                    .copied()
                    .expect("eligible Machines are non-empty");
                cursor += 1;
                let existing = by_machine
                    .get(&machine.machine.id)
                    .and_then(|containers| containers.last())
                    .copied();
                let operation = replicated_operation(existing, requested, options);
                let demand = EndpointDemand::for_operation(operation, hook_pending);
                if placement.capacity.fits_demand(&machine.machine.id, demand) {
                    selected = Some((machine, operation, Some(demand)));
                    break;
                }
            }
            selected
        };
        let Some((machine, operation, demand)) = selected else {
            return Err(pending_error.expect("only unreserved placement can exhaust capacity"));
        };
        if let Some(demand) = demand {
            placement.capacity.reserve(&machine.machine.id, demand);
            if demand.uses_hook() {
                hook_pending = false;
                hook_machine = Some(machine.machine.id);
            }
        }
        *placement.occupancy.entry(machine.machine.id).or_default() += 1;
        let existing = by_machine
            .get_mut(&machine.machine.id)
            .and_then(Vec::pop)
            .inspect(|container| {
                used.insert(container.as_observation().container_id);
            });
        match (operation, existing) {
            (EndpointOperation::Unchanged, Some(_)) => {}
            (EndpointOperation::Replace, Some(container)) => {
                let order = determine_update_order(Some(container), requested);
                operations.push(DeployOperation::ReplaceContainer(ReplacementOperation {
                    machine_id: machine.machine.id,
                    old_container_id: container.as_observation().container_id,
                    spec: resolve(requested, *service_id, order),
                    skip_health_monitor: options.skip_health_monitor,
                }));
            }
            (EndpointOperation::Create, None) => operations.push(DeployOperation::RunContainer {
                machine_id: machine.machine.id,
                spec: resolve(
                    requested,
                    *service_id,
                    determine_update_order(None, requested),
                ),
                skip_health_monitor: options.skip_health_monitor,
            }),
            (EndpointOperation::Unchanged, None)
            | (EndpointOperation::Replace, None)
            | (EndpointOperation::Create, Some(_)) => {
                unreachable!("replica projection agrees with existing containers")
            }
        }
    }
    remove_unused(&mut operations, current, &used, &mut placement.capacity);
    Ok((operations, hook_machine))
}

fn remove_unused(
    operations: &mut Vec<DeployOperation>,
    current: &[ServiceContainer],
    used: &BTreeSet<ContainerId>,
    capacity: &mut CapacityBudget,
) {
    for container in current {
        let observation = container.as_observation();
        if !used.contains(&observation.container_id) {
            // TODO: placement changes remove now-ineligible containers; there is no
            // deploy-time Machine filter that leaves excluded containers running.
            operations.push(DeployOperation::RemoveContainer {
                machine_id: observation.machine_id,
                container_id: observation.container_id,
            });
            capacity.release(&observation.machine_id);
        }
    }
}

pub(super) fn is_up_to_date(
    container: &ServiceContainer,
    requested: &RequestedServiceSpec,
    options: &PlanOptions,
) -> bool {
    let observation = container.as_observation();
    !options.force_recreate
        && is_running(&observation.runtime)
        && compare_specs(&observation.resolved_spec, requested) == SpecChange::UpToDate
}

pub(super) fn reserve_replicated_service_demand(
    capacity: &mut CapacityBudget,
    requested: &RequestedServiceSpec,
    observed: Option<&ServiceObservation>,
    machine_id: MachineId,
    options: &PlanOptions,
) -> Result<ReplicatedCapacityReservation, ()> {
    let ServiceMode::Replicated { replicas } = requested.mode else {
        return Err(());
    };
    let mut existing = observed
        .into_iter()
        .flat_map(|service| &service.containers)
        .filter(|container| container.as_observation().machine_id == machine_id)
        .collect::<Vec<_>>();
    existing.sort_by_key(|container| is_up_to_date(container, requested, options));
    let has_changes = existing
        .iter()
        .filter(|container| is_up_to_date(container, requested, options))
        .count()
        < replicas.get() as usize;
    if requested.pre_deploy.is_some() && has_changes {
        release_hooks(
            capacity,
            observed
                .into_iter()
                .flat_map(|service| service.hook_containers.iter()),
        );
    }
    let required = (replicas.get() as usize)
        .saturating_sub(existing.len())
        .saturating_add(usize::from(requested.pre_deploy.is_some() && has_changes));
    if !capacity.can_supply_persistent([&machine_id], required) {
        return Err(());
    }
    let mut hook_pending = requested.pre_deploy.is_some();
    let mut hook_machine = None;
    let mut operations = Vec::new();
    for _ in 0..replicas.get() {
        let operation = replicated_operation(existing.pop(), requested, options);
        let demand = EndpointDemand::for_operation(operation, hook_pending);
        if !capacity.reserve(&machine_id, demand) {
            return Err(());
        }
        if demand.uses_hook() {
            hook_machine = Some(machine_id);
        }
        hook_pending &= !demand.uses_hook();
        operations.push(operation);
    }
    Ok(ReplicatedCapacityReservation {
        machine_id,
        operations,
        hook_machine,
    })
}

fn release_hooks<'a>(
    capacity: &mut CapacityBudget,
    hooks: impl IntoIterator<Item = &'a HookContainer>,
) {
    for hook in hooks {
        capacity.release(&hook.as_observation().machine_id);
    }
}

fn replicated_operation(
    existing: Option<&ServiceContainer>,
    requested: &RequestedServiceSpec,
    options: &PlanOptions,
) -> EndpointOperation {
    match existing {
        Some(container) if is_up_to_date(container, requested, options) => {
            EndpointOperation::Unchanged
        }
        Some(_) => EndpointOperation::Replace,
        None => EndpointOperation::Create,
    }
}

fn is_running(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(runtime, ContainerRuntimeObservation::Running { .. })
}

fn determine_update_order(
    current: Option<&ServiceContainer>,
    requested: &RequestedServiceSpec,
) -> UpdateOrder {
    if let Some(order) = requested.update.order {
        return order;
    }
    if current.is_some_and(|current| {
        current
            .as_observation()
            .resolved_spec
            .ports
            .iter()
            .any(|old| {
                requested
                    .ports
                    .iter()
                    .any(|new| host_ports_conflict(old, new))
            })
    }) {
        return UpdateOrder::StopFirst;
    }
    if requested.volume_graph.mounted_volumes().any(|volume| {
        matches!(
            volume.source,
            VolumeSource::External { .. }
                | VolumeSource::Ordinary { .. }
                | VolumeSource::Provisioned { .. }
        )
    }) {
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
