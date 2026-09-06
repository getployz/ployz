//! Machine placement and replacement planning.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    ContainerId, ContainerRuntimeObservation, HookContainer, HostBind, MachineId,
    MachineObservation, PortPublication, RequestedServiceSpec, ResolvedServiceSpec,
    ResolvedUpdateConfig, ServiceContainer, ServiceId, ServiceMode, ServiceName,
    ServiceObservation, SpecChange, UpdateOrder, compare_specs,
};

use super::capacity::{CapacityBudget, EndpointDemand, EndpointOperation};
use super::{DeployOperation, PlanError, PlanOptions, ReplacementOperation};

pub(super) struct PlacementState {
    occupancy: BTreeMap<MachineId, usize>,
    capacity: CapacityBudget,
    sockets: HostSockets,
    reservations: BTreeMap<ServiceName, ReplicatedReservation>,
}

/// Shared Docker Volume trials own their budget and commit only accepted anchors.
pub(super) struct PlacementReservations {
    capacity: CapacityBudget,
    sockets: HostSockets,
    reservations: BTreeMap<ServiceName, ReplicatedReservation>,
}

impl PlacementReservations {
    pub(super) fn new(snapshot: &super::DeploySnapshot) -> Self {
        Self {
            capacity: CapacityBudget::from_snapshot(snapshot),
            sockets: HostSockets::from_snapshot(snapshot),
            reservations: BTreeMap::new(),
        }
    }

    pub(super) fn reserve_on<'spec>(
        &mut self,
        machine_id: MachineId,
        requested: impl Iterator<Item = &'spec RequestedServiceSpec>,
        observed: &[ServiceObservation],
        options: &PlanOptions,
    ) -> Result<(), PlanError> {
        let mut capacity = self.capacity.clone();
        let mut sockets = self.sockets.clone();
        let reservations = requested
            .map(|spec| {
                let observed = observed
                    .iter()
                    .find(|service| service.identity.name == spec.name);
                reserve_replicated_service_demand(
                    &mut capacity,
                    &mut sockets,
                    spec,
                    observed,
                    machine_id,
                    options,
                )
                .map(|reservation| (spec.name.clone(), reservation))
            })
            .collect::<Result<Vec<_>, _>>()?;
        self.capacity = capacity;
        self.sockets = sockets;
        self.reservations.extend(reservations);
        Ok(())
    }

    pub(super) fn into_placement(self, snapshot: &super::DeploySnapshot) -> PlacementState {
        PlacementState {
            occupancy: BTreeMap::new(),
            capacity: self.capacity,
            // Socket effects are applied in Deploy order, including unreserved Services.
            sockets: HostSockets::from_snapshot(snapshot),
            reservations: self.reservations,
        }
    }
}

impl PlacementState {
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

/// Snapshot-local claims, retained per owner so releasing one Container never
/// releases another Container's overlapping publication. Hooks bind no ports.
#[derive(Clone)]
struct HostSockets {
    claims: Vec<(MachineId, Option<ContainerId>, Vec<PortPublication>)>,
}

impl HostSockets {
    pub(super) fn from_snapshot(snapshot: &super::DeploySnapshot) -> Self {
        Self {
            claims: snapshot
                .containers
                .iter()
                .filter(|container| {
                    container.kind == ployz_core::ContainerKind::ServiceContainer
                        && super::super::is_active_runtime(&container.runtime)
                })
                .map(|container| {
                    (
                        container.machine_id,
                        Some(container.container_id),
                        container.resolved_spec.ports.clone(),
                    )
                })
                .collect(),
        }
    }

    fn release(&mut self, machine: MachineId, container: ContainerId) {
        self.claims.retain(|(owner_machine, owner, _)| {
            *owner_machine != machine || *owner != Some(container)
        });
    }

    fn fits(
        &self,
        machine: MachineId,
        requested: &RequestedServiceSpec,
        existing: Option<&ServiceContainer>,
        operation: EndpointOperation,
        pre_stops: &[ContainerId],
    ) -> bool {
        if matches!(operation, EndpointOperation::Unchanged) {
            return true;
        }
        let released = existing
            .filter(|container| {
                determine_update_order(Some(container), requested) == UpdateOrder::StopFirst
            })
            .map(|container| container.as_observation().container_id);
        // ponytail: linear claim scan; index by Machine/socket if large Deploys make this costly.
        !self.claims.iter().any(|(owner_machine, owner, ports)| {
            *owner_machine == machine
                && (released.is_none() || *owner != released)
                && !owner.is_some_and(|owner| pre_stops.contains(&owner))
                && ports.iter().any(|old| {
                    requested
                        .ports
                        .iter()
                        .any(|new| host_ports_conflict(old, new))
                })
        })
    }

    fn admit(
        &mut self,
        machine: MachineId,
        requested: &RequestedServiceSpec,
        existing: Option<&ServiceContainer>,
        operation: EndpointOperation,
    ) -> Result<(), PlanError> {
        if !self.fits(machine, requested, existing, operation, &[]) {
            return Err(socket_error(requested));
        }
        if !matches!(operation, EndpointOperation::Unchanged) {
            if let Some(container) = existing {
                self.release(machine, container.as_observation().container_id);
            }
            self.claims.push((machine, None, requested.ports.clone()));
        }
        Ok(())
    }
}

fn socket_error(requested: &RequestedServiceSpec) -> PlanError {
    PlanError::HostPortConflict {
        service: requested.name.clone(),
    }
}

pub(super) fn validate_host_ports(requested: &RequestedServiceSpec) -> Result<(), PlanError> {
    for (index, port) in requested.ports.iter().enumerate() {
        if requested
            .ports
            .iter()
            .skip(index + 1)
            .any(|other| host_ports_conflict(port, other))
        {
            return Err(PlanError::ConflictingHostPublications {
                service: requested.name.clone(),
            });
        }
    }
    Ok(())
}

pub(super) struct GlobalPlacement<'placement> {
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
        service_id,
        current,
        hooks,
        machines,
    } = target;
    let endpoint_demand = EndpointDemand::for_operation;
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
        return Err(capacity_error);
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
                return Err(capacity_error);
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
                    placement
                        .sockets
                        .release(machine_id, other_observation.container_id);
                    operations.push(DeployOperation::StopContainer {
                        machine_id,
                        container_id: other_observation.container_id,
                        purpose: ployz_core::StopContainerPurpose::FreeHostPorts,
                    });
                }
            }
            placement.sockets.admit(
                machine_id,
                requested,
                Some(container),
                EndpointOperation::Replace,
            )?;
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
                return Err(capacity_error);
            }
            placement
                .sockets
                .admit(machine_id, requested, None, EndpointOperation::Create)?;
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

    remove_unused(&mut operations, current, &used, placement);
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
    Pending { error: PlanError },
    Reserved { reservation: ReplicatedReservation },
}

pub(super) struct ReplicatedReservation {
    choices: Vec<ReplicaChoice>,
    hook_machine: Option<MachineId>,
}

struct ReplicaChoice {
    machine_id: MachineId,
    action: ReplicaAction,
}

enum ReplicaAction {
    Unchanged(ContainerId),
    Replace {
        container_id: ContainerId,
        pre_stops: Vec<ContainerId>,
    },
    Create,
}

pub(super) struct ReplicatedPlacement<'a> {
    pub(super) machines: Vec<&'a MachineObservation>,
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
    let reservation = match target.admission {
        CapacityAdmission::Reserved { reservation } => reservation,
        CapacityAdmission::Pending { error } => {
            // Selection uses trial socket claims; the accepted choices are applied below.
            let mut sockets = placement.sockets.clone();
            select_replicated(
                requested,
                current,
                target
                    .machines
                    .iter()
                    .map(|machine| machine.machine.id)
                    .collect(),
                &placement.occupancy,
                (&mut placement.capacity, &mut sockets),
                error,
                options,
            )?
        }
    };
    let by_id = current
        .iter()
        .map(|container| (container.as_observation().container_id, container))
        .collect::<BTreeMap<_, _>>();
    let mut used = BTreeSet::new();
    let mut operations = Vec::new();
    for ReplicaChoice { machine_id, action } in reservation.choices {
        *placement.occupancy.entry(machine_id).or_default() += 1;
        match action {
            ReplicaAction::Unchanged(container_id) => {
                used.insert(container_id);
            }
            ReplicaAction::Replace {
                container_id,
                pre_stops,
            } => {
                used.insert(container_id);
                let container = by_id
                    .get(&container_id)
                    .expect("placement choices retain observed Containers");
                for container_id in pre_stops {
                    placement.sockets.release(machine_id, container_id);
                    operations.push(DeployOperation::StopContainer {
                        machine_id,
                        container_id,
                        purpose: ployz_core::StopContainerPurpose::FreeHostPorts,
                    });
                }
                placement.sockets.admit(
                    machine_id,
                    requested,
                    Some(container),
                    EndpointOperation::Replace,
                )?;
                operations.push(DeployOperation::ReplaceContainer(ReplacementOperation {
                    machine_id,
                    old_container_id: container_id,
                    spec: resolve(
                        requested,
                        *service_id,
                        determine_update_order(Some(container), requested),
                    ),
                    skip_health_monitor: options.skip_health_monitor,
                }));
            }
            ReplicaAction::Create => {
                placement
                    .sockets
                    .admit(machine_id, requested, None, EndpointOperation::Create)?;
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
    }
    remove_unused(&mut operations, current, &used, placement);
    Ok((operations, reservation.hook_machine))
}

/// The same Container selection and admission for ordinary placement and shared Volume trials.
fn select_replicated(
    requested: &RequestedServiceSpec,
    current: &[ServiceContainer],
    mut machines: Vec<MachineId>,
    occupancy: &BTreeMap<MachineId, usize>,
    budget: (&mut CapacityBudget, &mut HostSockets),
    capacity_error: PlanError,
    options: &PlanOptions,
) -> Result<ReplicatedReservation, PlanError> {
    let (capacity, sockets) = budget;
    let ServiceMode::Replicated { replicas } = requested.mode else {
        return Err(capacity_error);
    };
    let replicas = replicas.get() as usize;
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
    let existing = machines
        .iter()
        .map(|id| by_machine.get(id).map_or(0, Vec::len))
        .sum::<usize>();
    let up_to_date = machines
        .iter()
        .flat_map(|id| by_machine.get(id).into_iter().flatten())
        .filter(|container| is_up_to_date(container, requested, options))
        .count();
    let required = replicas
        .saturating_sub(existing)
        .saturating_add(usize::from(
            requested.pre_deploy.is_some() && up_to_date < replicas,
        ));
    if !capacity.can_supply_persistent(&machines, required) {
        return Err(capacity_error);
    }
    machines.sort_by_key(|id| {
        let containers = by_machine.get(id);
        let up_to_date = containers
            .into_iter()
            .flatten()
            .filter(|container| is_up_to_date(container, requested, options))
            .count();
        (
            std::cmp::Reverse(up_to_date),
            std::cmp::Reverse(containers.map_or(0, Vec::len)),
            occupancy.get(id).copied().unwrap_or(0),
        )
    });
    let mut choices = Vec::new();
    let mut cursor = 0;
    let mut hook_pending = requested.pre_deploy.is_some();
    let mut hook_machine = None;
    for _ in 0..replicas {
        let mut socket_blocked = false;
        let mut selected = None;
        for _ in 0..machines.len() {
            let machine_id = *machines
                .get(cursor % machines.len())
                .expect("eligible Machines are non-empty");
            cursor += 1;
            let remaining = by_machine.get(&machine_id).map_or(&[][..], Vec::as_slice);
            let existing = remaining.last().copied();
            let operation = replicated_operation(existing, requested, options);
            let pre_stops = conflicting_siblings(requested, existing, operation, remaining);
            if !sockets.fits(machine_id, requested, existing, operation, &pre_stops) {
                socket_blocked = true;
                continue;
            }
            let demand = EndpointDemand::for_operation(operation, hook_pending);
            if capacity.reserve(&machine_id, demand) {
                selected = Some((machine_id, operation, pre_stops, demand));
                break;
            }
        }
        let Some((machine_id, operation, pre_stops, demand)) = selected else {
            return Err(if socket_blocked {
                socket_error(requested)
            } else {
                capacity_error
            });
        };
        if demand.uses_hook() {
            hook_pending = false;
            hook_machine = Some(machine_id);
        }
        let remaining = by_machine.entry(machine_id).or_default();
        let existing = remaining.pop();
        for container_id in &pre_stops {
            sockets.release(machine_id, *container_id);
        }
        remaining.retain(|container| !pre_stops.contains(&container.as_observation().container_id));
        sockets.admit(machine_id, requested, existing, operation)?;
        let action = match existing {
            Some(container) if matches!(operation, EndpointOperation::Unchanged) => {
                ReplicaAction::Unchanged(container.as_observation().container_id)
            }
            Some(container) => ReplicaAction::Replace {
                container_id: container.as_observation().container_id,
                pre_stops,
            },
            None => ReplicaAction::Create,
        };
        choices.push(ReplicaChoice { machine_id, action });
    }
    Ok(ReplicatedReservation {
        choices,
        hook_machine,
    })
}

// Only unselected same-Service siblings can be retired to free a replacement's ports.
// Stopping does not release endpoint capacity; removal still happens at the service tail.
fn conflicting_siblings(
    requested: &RequestedServiceSpec,
    existing: Option<&ServiceContainer>,
    operation: EndpointOperation,
    remaining: &[&ServiceContainer],
) -> Vec<ContainerId> {
    let Some(existing) = existing.filter(|_| matches!(operation, EndpointOperation::Replace))
    else {
        return Vec::new();
    };
    remaining
        .iter()
        .filter_map(|container| {
            let observation = container.as_observation();
            (observation.container_id != existing.as_observation().container_id
                && super::super::is_active_runtime(&observation.runtime)
                && observation.resolved_spec.ports.iter().any(|old| {
                    requested
                        .ports
                        .iter()
                        .any(|new| host_ports_conflict(old, new))
                }))
            .then_some(observation.container_id)
        })
        .collect()
}

fn remove_unused(
    operations: &mut Vec<DeployOperation>,
    current: &[ServiceContainer],
    used: &BTreeSet<ContainerId>,
    placement: &mut PlacementState,
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
            placement.capacity.release(&observation.machine_id);
            placement
                .sockets
                .release(observation.machine_id, observation.container_id);
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

fn reserve_replicated_service_demand(
    capacity: &mut CapacityBudget,
    sockets: &mut HostSockets,
    requested: &RequestedServiceSpec,
    observed: Option<&ServiceObservation>,
    machine_id: MachineId,
    options: &PlanOptions,
) -> Result<ReplicatedReservation, PlanError> {
    let current = observed.map_or(&[][..], |service| service.containers.as_slice());
    if let ServiceMode::Replicated { replicas } = requested.mode
        && requested.pre_deploy.is_some()
        && current
            .iter()
            .filter(|container| {
                container.as_observation().machine_id == machine_id
                    && is_up_to_date(container, requested, options)
            })
            .count()
            < replicas.get() as usize
    {
        release_hooks(
            capacity,
            observed
                .into_iter()
                .flat_map(|service| &service.hook_containers),
        );
    }
    let error = capacity.error_for([&machine_id]);
    let reservation = select_replicated(
        requested,
        current,
        vec![machine_id],
        &BTreeMap::new(),
        (capacity, sockets),
        error,
        options,
    )?;
    for container in current {
        let observation = container.as_observation();
        if !reservation
            .choices
            .iter()
            .any(|choice| match &choice.action {
                ReplicaAction::Unchanged(id)
                | ReplicaAction::Replace {
                    container_id: id, ..
                } => *id == observation.container_id,
                ReplicaAction::Create => false,
            })
        {
            sockets.release(observation.machine_id, observation.container_id);
        }
    }
    Ok(reservation)
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
    if requested.volume_graph().mounted_volumes().any(|volume| {
        matches!(
            volume.source.kind(),
            ployz_core::RawVolumeSource::External { .. }
                | ployz_core::RawVolumeSource::Ordinary { .. }
                | ployz_core::RawVolumeSource::Provisioned { .. }
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
                || (left.is_ipv4() == right.is_ipv4()
                    && (left.is_unspecified() || right.is_unspecified()))
        }
        (HostBind::Address { address }, HostBind::Prefix { prefix })
        | (HostBind::Prefix { prefix }, HostBind::Address { address }) => {
            prefix.contains(address)
                || (address.is_unspecified() && address.is_ipv4() == prefix.network().is_ipv4())
        }
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
    requested
        .to_resolved(
            service_id,
            ResolvedUpdateConfig {
                order,
                monitor_millis: requested.update.monitor_millis,
            },
        )
        .expect("volume graph is scoped")
}
