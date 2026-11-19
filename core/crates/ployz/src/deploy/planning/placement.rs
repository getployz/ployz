//! Machine placement and replacement planning.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::{
    ContainerId, ContainerObservation, CopyRotation, HookContainer, HostBind, MachineId, Namespace,
    PortPublication, Region, RegionWait, RequestedServiceSpec, ResolvedServiceSpec,
    ResolvedUpdateConfig, ServiceContainer, ServiceId, ServiceMode, ServiceName,
    ServiceObservation, SpecChange, UPDATE_CONTAINER_LIMITS_CAPABILITY,
    UPDATE_CONTAINER_PLACEMENT_CAPABILITY, UpdateOrder, compare_specs,
};

use super::capacity::{CapacityBudget, EndpointDemand, EndpointOperation};
use super::volumes::VolumePlacement;
use super::{DeployOperation, PlanError, PlanOptions, ReplacementOperation};
use ployz_core::{ContainerUpdateOperation, InPlaceChange, PlacementAdmission, admit_placement};

#[derive(Clone)]
pub(super) struct PlacementState {
    unobserved: BTreeMap<MachineId, ployz_core::MachineObservation>,
    occupancy: BTreeMap<MachineId, usize>,
    capacity: CapacityBudget,
    sockets: HostSockets,
    reservations: BTreeMap<ServiceName, ReplicatedReservation>,
    updaters: InPlaceUpdaters,
}

/// Shared Docker Volume trials own their budget and commit only accepted anchors.
pub(super) struct PlacementReservations {
    capacity: CapacityBudget,
    sockets: HostSockets,
    reservations: BTreeMap<ServiceName, ReplicatedReservation>,
    updaters: InPlaceUpdaters,
}

impl PlacementReservations {
    pub(super) fn new(snapshot: &super::DeploySnapshot) -> Self {
        Self {
            capacity: CapacityBudget::from_snapshot(snapshot),
            sockets: HostSockets::from_snapshot(snapshot),
            reservations: BTreeMap::new(),
            updaters: InPlaceUpdaters::from_snapshot(snapshot),
        }
    }

    pub(super) fn reserve_on<'spec>(
        &mut self,
        machine_id: MachineId,
        requested: impl Iterator<Item = &'spec RequestedServiceSpec>,
        observed: &[ServiceObservation],
        options: &PlanOptions,
    ) -> Result<(), PlanError> {
        let mut budget = (self.capacity.clone(), self.sockets.clone());
        let reservations = requested
            .map(|spec| {
                let observed = observed
                    .iter()
                    .find(|service| service.identity.name == spec.name);
                reserve_replicated_service_demand(
                    &mut budget,
                    spec,
                    observed,
                    machine_id,
                    options,
                    &self.updaters,
                )
                .map(|reservation| (spec.name.clone(), reservation))
            })
            .collect::<Result<Vec<_>, _>>()?;
        (self.capacity, self.sockets) = budget;
        self.reservations.extend(reservations);
        Ok(())
    }

    pub(super) fn into_placement(self, snapshot: &super::DeploySnapshot) -> PlacementState {
        PlacementState {
            unobserved: snapshot
                .machines
                .iter()
                .filter(|machine| {
                    matches!(
                        machine.membership_evidence,
                        Some(ployz_core::MembershipEvidence::Unrecognized { .. })
                    ) || matches!(
                        machine.membership,
                        ployz_core::MembershipObservation::Unknown
                            | ployz_core::MembershipObservation::Unrecognized(_)
                    )
                })
                .map(|machine| (machine.machine.id, machine.clone()))
                .collect(),
            occupancy: BTreeMap::new(),
            capacity: self.capacity,
            // Socket effects are applied in Deploy order, including unreserved Services.
            sockets: HostSockets::from_snapshot(snapshot),
            reservations: self.reservations,
            updaters: self.updaters,
        }
    }
}

impl PlacementState {
    pub(super) fn can_update_in_place(
        &self,
        service_id: ServiceId,
        current: &[ServiceContainer],
        requested: &RequestedServiceSpec,
        options: &PlanOptions,
    ) -> bool {
        self.updaters
            .can_update(service_id, current, requested, options)
    }

    /// Whether this Deploy keeps `container` running: as it is, or changed in place.
    pub(super) fn keeps_container(
        container: &ServiceContainer,
        requested: &RequestedServiceSpec,
        options: &PlanOptions,
        landing: Landing,
    ) -> bool {
        container_fate(container, requested, options, landing) != ContainerFate::Replace
    }

    pub(super) fn capacity_fits(&self, machine_id: &MachineId, peak: u64) -> bool {
        self.capacity.fits(machine_id, peak)
    }

    pub(super) fn take_reservation(
        &mut self,
        service: &ServiceName,
    ) -> Option<ReplicatedReservation> {
        self.reservations.remove(service)
    }

    /// Cancel `service`'s shared reservation and restore the endpoints it committed. The old
    /// hooks it counted as free stay in use: a Service this Deploy does not attempt keeps them.
    pub(super) fn release(&mut self, service: &ServiceName) {
        let Some(reservation) = self.reservations.remove(service) else {
            return;
        };
        for choice in &reservation.choices {
            if matches!(choice.action, ReplicaAction::Create) {
                self.capacity.release(&choice.machine_id);
            }
        }
        if let Some(machine_id) = reservation.hook_machine {
            self.capacity.release(&machine_id);
        }
        for machine_id in &reservation.credited_hooks {
            self.capacity.take(machine_id);
        }
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
        if operation.keeps_container() {
            return true;
        }
        let released = existing
            .filter(|container| determine_update_order(Some(container), requested).stops_first())
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
        if !operation.keeps_container() {
            if let Some(container) = existing {
                self.release(machine, container.as_observation().container_id);
            }
            self.claims.push((machine, None, requested.ports.clone()));
        }
        Ok(())
    }
}

/// Whether `machine` has room for one new Container of `requested`: a free bridge
/// endpoint and none of its host ports taken.
pub(super) fn fits_one_more(
    snapshot: &super::DeploySnapshot,
    machine: MachineId,
    requested: &RequestedServiceSpec,
) -> bool {
    CapacityBudget::from_snapshot(snapshot).fits(&machine, 1)
        && HostSockets::from_snapshot(snapshot).fits(
            machine,
            requested,
            None,
            EndpointOperation::Create,
            &[],
        )
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
    pub(super) volumes: VolumePlacement<'placement>,
    pub(super) landing: Landing,
}

pub(super) fn plan_global(
    target: GlobalPlacement<'_>,
    placement: &mut PlacementState,
    options: &PlanOptions,
) -> Result<(Vec<DeployOperation>, Option<MachineId>), PlanError> {
    let GlobalPlacement {
        service_id,
        current,
        hooks,
        volumes,
        landing,
    } = target;
    let (requested, machines) = volumes.into_parts();
    let endpoint_demand = EndpointDemand::for_operation;
    let endpoint_operation =
        |machine_id| global_endpoint_operation(current, machine_id, requested, options, landing);
    let has_changes = machines
        .iter()
        .any(|machine| !endpoint_operation(machine.machine.id).keeps_container());
    if requested.pre_deploy.is_some() && has_changes {
        release_hooks(&mut placement.capacity, hooks);
    }
    let capacity_error = placement
        .capacity
        .error_for(machines.iter().filter_map(|machine| {
            (!endpoint_operation(machine.machine.id).keeps_container())
                .then_some(&machine.machine.id)
        }));
    let emitter = ReplicaEmitter {
        service_id,
        requested,
        options,
    };
    let mut used = BTreeSet::new();
    let mut operations = Vec::new();
    let hook_machine = requested.pre_deploy.as_ref().and_then(|_| {
        machines.iter().find_map(|machine| {
            let operation = endpoint_operation(machine.machine.id);
            (!operation.keeps_container()
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

        if let Some((container, change)) = on_machine(current, machine_id).find_map(|container| {
            match container_fate(container, requested, options, landing) {
                ContainerFate::Update(change) => Some((container, change)),
                ContainerFate::Replace | ContainerFate::Keep => None,
            }
        }) {
            used.insert(container.as_observation().container_id);
            emitter.emit(
                &mut placement.sockets,
                &mut operations,
                machine_id,
                Replica::Update(container, change),
            )?;
            continue;
        }

        if let Some(container) = on_machine(current, machine_id)
            .find(|container| reusable_global(container, requested, options))
        {
            used.insert(container.as_observation().container_id);
            emitter.emit(
                &mut placement.sockets,
                &mut operations,
                machine_id,
                Replica::Reuse(container),
            )?;
            continue;
        }

        if let Some(container) = on_machine(current, machine_id)
            .filter(|container| {
                super::super::is_active_runtime(&container.as_observation().runtime)
            })
            .min_by_key(|container| !container.as_observation().runtime.may_serve())
        {
            let observation = container.as_observation();
            let demand =
                endpoint_demand(EndpointOperation::Replace, hook_machine == Some(machine_id));
            if !placement.capacity.reserve(&machine_id, demand) {
                return Err(capacity_error);
            }
            used.insert(observation.container_id);
            let siblings = on_machine(current, machine_id).collect::<Vec<_>>();
            let pre_stops = conflicting_siblings(
                requested,
                Some(container),
                EndpointOperation::Replace,
                &siblings,
            );
            emitter.emit(
                &mut placement.sockets,
                &mut operations,
                machine_id,
                Replica::Replace {
                    old: container,
                    pre_stops,
                },
            )?;
        } else {
            let demand =
                endpoint_demand(EndpointOperation::Create, hook_machine == Some(machine_id));
            if !placement.capacity.reserve(&machine_id, demand) {
                return Err(capacity_error);
            }
            emitter.emit(
                &mut placement.sockets,
                &mut operations,
                machine_id,
                Replica::Create,
            )?;
        }
    }

    remove_unused(&mut operations, requested, current, &used, placement);
    Ok((operations, hook_machine))
}

fn reusable_global(
    container: &ServiceContainer,
    requested: &RequestedServiceSpec,
    options: &PlanOptions,
) -> bool {
    let observation = container.as_observation();
    !options.force_recreate
        && !super::super::is_active_runtime(&observation.runtime)
        && observation.creation_key().is_some()
        && observation.resolved_spec
            == resolve(
                requested,
                observation.service_id(),
                observation.resolved_spec.update.order,
            )
}

fn global_endpoint_operation(
    current: &[ServiceContainer],
    machine_id: MachineId,
    requested: &RequestedServiceSpec,
    options: &PlanOptions,
    landing: Landing,
) -> EndpointOperation {
    if on_machine(current, machine_id).any(|container| {
        is_up_to_date(container, requested, options)
            || reusable_global(container, requested, options)
    }) {
        EndpointOperation::Unchanged
    } else if on_machine(current, machine_id).any(|container| {
        matches!(
            container_fate(container, requested, options, landing),
            ContainerFate::Update(_)
        )
    }) {
        EndpointOperation::Update
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

pub(super) struct Spread<'a> {
    requested: &'a RequestedServiceSpec,
    regions: Vec<RegionShare>,
    waiting: Vec<RegionShare>,
}

/// How near a Container in a waiting region is to the replica this Deploy asks for,
/// from the furthest.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Standing {
    NotServing,
    Outdated,
    Settled,
}

pub(super) struct RegionShare {
    pub(super) region: Region,
    pub(super) spec: RequestedServiceSpec,
    count: usize,
    labelled: BTreeSet<MachineId>,
}

impl<'a> Spread<'a> {
    pub(super) fn of(
        requested: &'a RequestedServiceSpec,
        snapshot: &super::DeploySnapshot,
    ) -> Self {
        let regions = requested
            .regions
            .iter()
            .filter(|_| matches!(requested.mode, ServiceMode::Replicated { .. }))
            .map(|(region, count)| RegionShare {
                region: region.clone(),
                spec: requested.held_to(region),
                count: count.get() as usize,
                labelled: snapshot
                    .machines
                    .iter()
                    .filter(|machine| Region::of(&machine.machine).as_ref() == Some(region))
                    .map(|machine| machine.machine.id)
                    .collect(),
            })
            .collect();
        Self {
            requested,
            regions,
            waiting: Vec::new(),
        }
    }

    fn anywhere(requested: &'a RequestedServiceSpec) -> Self {
        Self {
            requested,
            regions: Vec::new(),
            waiting: Vec::new(),
        }
    }

    pub(super) fn wait_for(&mut self, away: &[super::RegionAway]) {
        let (waiting, starting) = std::mem::take(&mut self.regions)
            .into_iter()
            .partition(|share| away.iter().any(|away| away.region == share.region));
        self.regions = starting;
        self.waiting = waiting;
    }

    /// What each waiting region holds back of its count, given the Service's `current`
    /// Containers: a replica there that already runs this Deploy's version waits for
    /// nothing.
    pub(super) fn held_back(&self, current: &[ServiceContainer]) -> BTreeMap<Region, RegionWait> {
        self.waiting
            .iter()
            .filter_map(|share| {
                let standing = |standing| {
                    current
                        .iter()
                        .map(ServiceContainer::as_observation)
                        .filter(|observation| share.labelled.contains(&observation.machine_id))
                        .filter(|observation| self.standing(observation) == standing)
                        .count()
                };
                let short = share.count.saturating_sub(standing(Standing::Settled));
                let outdated = standing(Standing::Outdated).min(short);
                let count = |count: usize| u32::try_from(count).unwrap_or(u32::MAX);
                (short > 0).then(|| {
                    let wait = RegionWait {
                        waiting: count(short - outdated),
                        outdated: count(outdated),
                    };
                    (share.region.clone(), wait)
                })
            })
            .collect()
    }

    /// The Containers the waiting regions keep as they are, `placed` being those the
    /// regions that start already keep or stop: in each, as many as its count, those
    /// nearest this Deploy's version first. A Container its own region doesn't need, past
    /// that region's count or on a Server in none of the counted regions, counts toward a
    /// waiting region still short of its count, so a Server whose label changes gives up
    /// no replica while a region waits. A Container no count needs is removed like any
    /// other.
    fn left_running(
        &self,
        current: &[ServiceContainer],
        placed: &BTreeSet<ContainerId>,
    ) -> Vec<ContainerId> {
        let nearest = |mut held: Vec<&ContainerObservation>, count: usize| {
            held.sort_by_key(|observation| std::cmp::Reverse(self.standing(observation)));
            held.truncate(count);
            held.into_iter()
                .map(|observation| observation.container_id)
                .collect::<Vec<_>>()
        };
        let mut kept = Vec::new();
        let mut short = 0;
        for share in &self.waiting {
            let own = current
                .iter()
                .map(ServiceContainer::as_observation)
                .filter(|observation| share.labelled.contains(&observation.machine_id))
                .filter(|observation| self.standing(observation) > Standing::NotServing)
                .collect();
            let own = nearest(own, share.count);
            short += share.count - own.len();
            kept.extend(own);
        }
        let spare = current
            .iter()
            .map(ServiceContainer::as_observation)
            .filter(|observation| !placed.contains(&observation.container_id))
            .filter(|observation| !kept.contains(&observation.container_id))
            .collect();
        kept.extend(nearest(spare, short));
        kept
    }

    fn standing(&self, observation: &ContainerObservation) -> Standing {
        if !observation.runtime.may_serve() || !serving_rotation(observation) {
            return Standing::NotServing;
        }
        match compare_specs(&observation.resolved_spec, self.requested) {
            SpecChange::UpToDate | SpecChange::NeedsPlacementUpdate => Standing::Settled,
            SpecChange::NeedsUpdate | SpecChange::NeedsRecreate => Standing::Outdated,
        }
    }

    pub(super) fn requested(&self) -> &'a RequestedServiceSpec {
        self.requested
    }

    pub(super) fn regions(&self) -> &[RegionShare] {
        &self.regions
    }

    pub(super) fn replicas(&self) -> Option<usize> {
        let ServiceMode::Replicated { replicas } = self.requested.mode else {
            return None;
        };
        Some(if self.regions.is_empty() {
            replicas.get() as usize
        } else {
            self.regions.iter().map(|share| share.count).sum()
        })
    }

    pub(super) fn supplied<'c>(
        &self,
        containers: impl IntoIterator<Item = &'c ServiceContainer>,
    ) -> usize {
        let mut quota = Quota::of(self);
        containers
            .into_iter()
            .filter(|container| quota.take(&container.as_observation().machine_id))
            .count()
    }
}

struct Quota<'a> {
    spread: &'a Spread<'a>,
    left: Vec<usize>,
}

impl<'a> Quota<'a> {
    fn of(spread: &'a Spread<'a>) -> Self {
        Self {
            spread,
            left: spread.regions.iter().map(|share| share.count).collect(),
        }
    }

    fn share(&self, machine: &MachineId) -> Option<usize> {
        self.spread
            .regions
            .iter()
            .position(|share| share.labelled.contains(machine))
    }

    fn open(&self, machine: &MachineId) -> bool {
        self.spread.regions.is_empty()
            || self
                .share(machine)
                .and_then(|share| self.left.get(share))
                .is_some_and(|left| *left > 0)
    }

    fn take(&mut self, machine: &MachineId) -> bool {
        if !self.open(machine) {
            return false;
        }
        if let Some(left) = self
            .share(machine)
            .and_then(|share| self.left.get_mut(share))
        {
            *left -= 1;
        }
        true
    }
}

pub(super) enum CapacityAdmission {
    Pending { error: PlanError, landing: Landing },
    Reserved { reservation: ReplicatedReservation },
}

#[derive(Clone)]
pub(super) struct ReplicatedReservation {
    choices: Vec<ReplicaChoice>,
    hook_machine: Option<MachineId>,
    /// The Machine of each old hook the reservation counted as free before it was made.
    credited_hooks: Vec<MachineId>,
}

impl ReplicatedReservation {
    fn trial_steps(&self) -> impl Iterator<Item = TrialStep> {
        self.choices
            .iter()
            .filter_map(|choice| match choice.action {
                ReplicaAction::Unchanged(_) => None,
                ReplicaAction::Update(_, change) => Some(TrialStep::Updates(change)),
                ReplicaAction::Replace { .. } => Some(TrialStep::Replaces),
                ReplicaAction::Create => Some(TrialStep::Starts),
            })
    }
}

#[derive(Clone)]
struct ReplicaChoice {
    machine_id: MachineId,
    action: ReplicaAction,
}

#[derive(Clone)]
enum ReplicaAction {
    Unchanged(ContainerId),
    /// The Container stays and takes the requested change in place.
    Update(ContainerId, InPlaceChange),
    Replace {
        container_id: ContainerId,
        pre_stops: Vec<ContainerId>,
    },
    Create,
}

pub(super) struct ReplicatedPlacement<'a> {
    pub(super) spread: &'a Spread<'a>,
    pub(super) volumes: VolumePlacement<'a>,
    pub(super) admission: CapacityAdmission,
}

pub(super) fn plan_replicated(
    service_id: &ServiceId,
    current: &[ServiceContainer],
    target: ReplicatedPlacement<'_>,
    placement: &mut PlacementState,
    options: &PlanOptions,
) -> Result<(Vec<DeployOperation>, Option<MachineId>), PlanError> {
    let (requested, machines) = target.volumes.into_parts();
    let reservation = match target.admission {
        CapacityAdmission::Reserved { reservation } => reservation,
        CapacityAdmission::Pending { error, landing } => {
            // Selection uses trial socket claims; the accepted choices are applied below.
            let mut sockets = placement.sockets.clone();
            select_replicated(
                target.spread,
                current,
                machines.iter().map(|machine| machine.machine.id).collect(),
                &placement.occupancy,
                (&mut placement.capacity, &mut sockets),
                error,
                (options, landing),
            )?
        }
    };
    let by_id = current
        .iter()
        .map(|container| (container.as_observation().container_id, container))
        .collect::<BTreeMap<_, _>>();
    let mut used = BTreeSet::new();
    let mut pre_stopped = BTreeSet::new();
    let mut operations = Vec::new();
    let emitter = ReplicaEmitter {
        service_id,
        requested,
        options,
    };
    let observed = |container_id| {
        *by_id
            .get(&container_id)
            .expect("placement choices retain observed Containers")
    };
    for ReplicaChoice { machine_id, action } in reservation.choices {
        *placement.occupancy.entry(machine_id).or_default() += 1;
        let replica = match action {
            ReplicaAction::Unchanged(container_id) => {
                used.insert(container_id);
                continue;
            }
            ReplicaAction::Update(container_id, change) => {
                used.insert(container_id);
                Replica::Update(observed(container_id), change)
            }
            ReplicaAction::Replace {
                container_id,
                pre_stops,
            } => {
                used.insert(container_id);
                pre_stopped.extend(pre_stops.iter().copied());
                Replica::Replace {
                    old: observed(container_id),
                    pre_stops,
                }
            }
            ReplicaAction::Create => Replica::Create,
        };
        emitter.emit(&mut placement.sockets, &mut operations, machine_id, replica)?;
    }
    let placed = used.union(&pre_stopped).copied().collect();
    used.extend(target.spread.left_running(current, &placed));
    remove_unused(&mut operations, requested, current, &used, placement);
    Ok((operations, reservation.hook_machine))
}

/// The removals of a replicated Service none of whose regions can start a replica: every
/// Container past the count of the region it counts toward.
pub(super) fn withdraw(
    spread: &Spread<'_>,
    current: &[ServiceContainer],
    requested: &RequestedServiceSpec,
    placement: &mut PlacementState,
) -> Vec<DeployOperation> {
    let used = spread
        .left_running(current, &BTreeSet::new())
        .into_iter()
        .collect();
    let mut operations = Vec::new();
    remove_unused(&mut operations, requested, current, &used, placement);
    operations
}

/// What one Machine's copy of a Service becomes, as [`ReplicaEmitter::emit`] writes it.
enum Replica<'a> {
    /// The Container stays and takes the requested change in place.
    Update(&'a ServiceContainer, InPlaceChange),
    /// A stopped Container of the requested spec is started again.
    Reuse(&'a ServiceContainer),
    /// A new Container replaces `old`, after `pre_stops` free its host ports.
    Replace {
        old: &'a ServiceContainer,
        pre_stops: Vec<ContainerId>,
    },
    Create,
}

/// Writes the operations of one Machine's copy for both placements of a Service, so the
/// order of its socket claims and its operations is written once.
struct ReplicaEmitter<'a> {
    service_id: &'a ServiceId,
    requested: &'a RequestedServiceSpec,
    options: &'a PlanOptions,
}

impl ReplicaEmitter<'_> {
    fn emit(
        &self,
        sockets: &mut HostSockets,
        operations: &mut Vec<DeployOperation>,
        machine_id: MachineId,
        replica: Replica<'_>,
    ) -> Result<(), PlanError> {
        let Self {
            service_id,
            requested,
            options,
        } = self;
        match replica {
            Replica::Update(container, change) => {
                let observation = container.as_observation();
                let spec = match change {
                    InPlaceChange::Limits => resolve(
                        requested,
                        **service_id,
                        determine_update_order(Some(container), requested),
                    ),
                    InPlaceChange::Placement => placed_spec(container, **service_id, requested),
                };
                operations.push(DeployOperation::UpdateContainer(ContainerUpdateOperation {
                    machine_id,
                    container_id: observation.container_id,
                    spec,
                    change,
                }));
            }
            Replica::Reuse(container) => {
                sockets.admit(machine_id, requested, None, EndpointOperation::Create)?;
                operations.push(DeployOperation::RunContainer {
                    machine_id,
                    spec: container.as_observation().resolved_spec.clone(),
                    skip_health_monitor: options.skip_health_monitor,
                });
            }
            Replica::Replace { old, pre_stops } => {
                for container_id in pre_stops {
                    sockets.release(machine_id, container_id);
                    operations.push(DeployOperation::StopContainer {
                        machine_id,
                        container_id,
                        purpose: ployz_core::StopContainerPurpose::FreeHostPorts,
                    });
                }
                sockets.admit(machine_id, requested, Some(old), EndpointOperation::Replace)?;
                operations.push(DeployOperation::ReplaceContainer(ReplacementOperation {
                    machine_id,
                    old_container_id: old.as_observation().container_id,
                    spec: resolve(
                        requested,
                        **service_id,
                        determine_update_order(Some(old), requested),
                    ),
                    skip_health_monitor: options.skip_health_monitor,
                }));
            }
            Replica::Create => {
                sockets.admit(machine_id, requested, None, EndpointOperation::Create)?;
                operations.push(DeployOperation::RunContainer {
                    machine_id,
                    spec: resolve(
                        requested,
                        **service_id,
                        determine_update_order(None, requested),
                    ),
                    skip_health_monitor: options.skip_health_monitor,
                });
            }
        }
        Ok(())
    }
}

/// The same Container selection and admission for ordinary placement and shared Volume trials.
fn select_replicated(
    spread: &Spread<'_>,
    current: &[ServiceContainer],
    mut machines: Vec<MachineId>,
    occupancy: &BTreeMap<MachineId, usize>,
    budget: (&mut CapacityBudget, &mut HostSockets),
    capacity_error: PlanError,
    fate: (&PlanOptions, Landing),
) -> Result<ReplicatedReservation, PlanError> {
    let (capacity, sockets) = budget;
    let (options, landing) = fate;
    let requested = spread.requested;
    let keeps = |container: &ServiceContainer| {
        container_fate(container, requested, options, landing) != ContainerFate::Replace
    };
    let Some(replicas) = spread.replicas() else {
        return Err(capacity_error);
    };
    let mut by_machine = BTreeMap::<MachineId, Vec<&ServiceContainer>>::new();
    for container in current {
        by_machine
            .entry(container.as_observation().machine_id)
            .or_default()
            .push(container);
    }
    for containers in by_machine.values_mut() {
        containers.sort_by_key(|container| {
            (
                container_fate(container, requested, options, landing),
                container.as_observation().runtime.may_serve(),
            )
        });
    }
    let on_machines = |counts: &dyn Fn(&ServiceContainer) -> bool| {
        spread.supplied(
            machines
                .iter()
                .flat_map(|id| by_machine.get(id).into_iter().flatten().copied())
                .filter(|container| counts(container)),
        )
    };
    let existing = on_machines(&|_| true);
    let up_to_date = on_machines(&|container| keeps(container));
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
            .filter(|container| keeps(container))
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
    let mut quota = Quota::of(spread);
    for _ in 0..replicas {
        let mut socket_blocked = false;
        let mut selected = None;
        for _ in 0..machines.len() {
            let machine_id = *machines
                .get(cursor % machines.len())
                .expect("eligible Machines are non-empty");
            cursor += 1;
            if !quota.open(&machine_id) {
                continue;
            }
            let remaining = by_machine.get(&machine_id).map_or(&[][..], Vec::as_slice);
            let existing = remaining.last().copied();
            let operation = replicated_operation(existing, requested, options, landing);
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
        quota.take(&machine_id);
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
        let action = match existing.map(|container| {
            (
                container.as_observation().container_id,
                container_fate(container, requested, options, landing),
            )
        }) {
            Some((container_id, ContainerFate::Keep)) => ReplicaAction::Unchanged(container_id),
            Some((container_id, ContainerFate::Update(change))) => {
                ReplicaAction::Update(container_id, change)
            }
            Some((container_id, ContainerFate::Replace)) => ReplicaAction::Replace {
                container_id,
                pre_stops,
            },
            None => ReplicaAction::Create,
        };
        choices.push(ReplicaChoice { machine_id, action });
    }
    Ok(ReplicatedReservation {
        choices,
        hook_machine,
        credited_hooks: Vec::new(),
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
    requested: &RequestedServiceSpec,
    current: &[ServiceContainer],
    used: &BTreeSet<ContainerId>,
    placement: &mut PlacementState,
) {
    for container in current {
        let observation = container.as_observation();
        let unknown_placement = placement
            .unobserved
            .get(&observation.machine_id)
            .is_some_and(|machine| {
                !matches!(
                    requested.placement_eligibility_in_namespace(
                        &observation.namespace,
                        &machine.machine,
                        machine.storage.as_ref()
                    ),
                    ployz_core::ServicePlacementEligibility::Ineligible(_)
                )
            });
        if !used.contains(&observation.container_id) && !unknown_placement {
            // TODO: placement changes remove now-ineligible containers; there is no
            // deploy-time Machine filter that leaves excluded containers running.
            operations.push(DeployOperation::RemoveContainer {
                machine_id: observation.machine_id,
                container_id: observation.container_id,
                purpose: ployz_core::RemoveContainerPurpose::Rollout,
            });
            placement.capacity.release(&observation.machine_id);
            placement
                .sockets
                .release(observation.machine_id, observation.container_id);
        }
    }
}

fn is_up_to_date(
    container: &ServiceContainer,
    requested: &RequestedServiceSpec,
    options: &PlanOptions,
) -> bool {
    container_fate(container, requested, options, Landing::Replace) == ContainerFate::Keep
}

/// Machines whose daemon changes a running Container in place, by what it changes.
#[derive(Clone, Default)]
struct InPlaceUpdaters {
    limits: BTreeSet<MachineId>,
    placement: BTreeSet<MachineId>,
}

impl InPlaceUpdaters {
    fn from_snapshot(snapshot: &super::DeploySnapshot) -> Self {
        let advertising = |capability| {
            snapshot
                .machines
                .iter()
                .filter(|machine| machine.machine.runtime.capabilities.contains(capability))
                .map(|machine| machine.machine.id)
                .collect()
        };
        Self {
            limits: advertising(UPDATE_CONTAINER_LIMITS_CAPABILITY),
            placement: advertising(UPDATE_CONTAINER_PLACEMENT_CAPABILITY),
        }
    }

    /// Whether every copy of the Service that would take the requested change
    /// in place runs on a Machine whose daemon makes that change, and at least
    /// one would. A new placement also has to pass [`admit_placement`] over the
    /// copy's spec.
    fn can_update(
        &self,
        service_id: ServiceId,
        current: &[ServiceContainer],
        requested: &RequestedServiceSpec,
        options: &PlanOptions,
    ) -> bool {
        let mut any = false;
        for container in current {
            let ContainerFate::Update(change) =
                container_fate(container, requested, options, Landing::InPlace)
            else {
                continue;
            };
            let observation = container.as_observation();
            let updates = match change {
                InPlaceChange::Limits => self.limits.contains(&observation.machine_id),
                InPlaceChange::Placement => {
                    self.placement.contains(&observation.machine_id)
                        && admit_placement(
                            &observation.resolved_spec,
                            &placed_spec(container, service_id, requested),
                        ) == Ok(PlacementAdmission::Record)
                }
            };
            if !updates {
                return false;
            }
            any = true;
        }
        any
    }
}

/// The counts by region go with the placement. Other changes the request makes
/// outside the serving shape, such as the replica count, stay off this spec and
/// wait for the copy's replacement, as they do for a copy left untouched.
fn placed_spec(
    container: &ServiceContainer,
    service_id: ServiceId,
    requested: &RequestedServiceSpec,
) -> ResolvedServiceSpec {
    ResolvedServiceSpec {
        service_id,
        placement: requested.placement.clone(),
        regions: requested.regions.clone(),
        ..container.as_observation().resolved_spec.clone()
    }
}

/// How one Deploy lands a change of one Service that a running copy can take
/// in place: new limits, or a new placement its Machine still satisfies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Landing {
    /// Every serving copy takes the change under its own id.
    InPlace,
    /// New Containers carry the change, as a Deploy without the capability does.
    Replace,
}

/// What one step of an in-place trial does to a copy of the Service.
pub(super) enum TrialStep {
    Replaces,
    Starts,
    Updates(InPlaceChange),
}

impl TrialStep {
    pub(super) fn of(operation: &DeployOperation) -> Option<Self> {
        match operation {
            DeployOperation::ReplaceContainer(_) => Some(Self::Replaces),
            DeployOperation::RunContainer { .. } => Some(Self::Starts),
            DeployOperation::UpdateContainer(update) => Some(Self::Updates(update.change)),
            DeployOperation::PrepareVolumes { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        }
    }
}

impl Landing {
    /// Plan one Service in place when that trial [stands](Self::stands), else with
    /// replacement, on a budget the abandoned trial never touched.
    pub(super) fn decide<Budget: Clone, Planned>(
        worth_trying: bool,
        budget: &mut Budget,
        mut plan: impl FnMut(&mut Budget, Self) -> Result<Planned, PlanError>,
        stands: impl FnOnce(&Planned) -> bool,
    ) -> Result<Planned, PlanError> {
        if worth_trying {
            let mut trial = budget.clone();
            let planned = plan(&mut trial, Self::InPlace)?;
            if stands(&planned) {
                *budget = trial;
                return Ok(planned);
            }
        }
        plan(budget, Self::Replace)
    }

    /// A new placement stands beside copies that start, so a Service that spreads
    /// or grows keeps the copies it has. New limits do not: a user is told that
    /// every copy takes them on a new Container when the Deploy also starts one.
    ///
    /// A copy updated in place keeps its gen and reads as the new spec, beside
    /// started copies of a newer gen. A Machine coming back sets Out a copy a Deploy
    /// would make anew, or one its Service has more of than it asks for, never a
    /// copy for its gen alone. So a copy whose in-place update missed it keeps
    /// serving on its old limits or placement until the next Deploy.
    pub(super) fn stands(steps: impl IntoIterator<Item = TrialStep>) -> bool {
        let (mut starts, mut new_limits) = (false, false);
        for step in steps {
            match step {
                TrialStep::Replaces => return false,
                TrialStep::Starts => starts = true,
                TrialStep::Updates(InPlaceChange::Limits) => new_limits = true,
                TrialStep::Updates(InPlaceChange::Placement) => {}
            }
        }
        !(starts && new_limits)
    }
}

/// What a Deploy does with one observed Container of the requested Service,
/// ordered from the most to the least work.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ContainerFate {
    Replace,
    Update(InPlaceChange),
    Keep,
}

fn container_fate(
    container: &ServiceContainer,
    requested: &RequestedServiceSpec,
    options: &PlanOptions,
    landing: Landing,
) -> ContainerFate {
    let observation = container.as_observation();
    if options.force_recreate || !observation.runtime.may_serve() || !serving_rotation(observation)
    {
        return ContainerFate::Replace;
    }
    match compare_specs(&observation.resolved_spec, requested) {
        SpecChange::UpToDate => ContainerFate::Keep,
        SpecChange::NeedsUpdate if landing == Landing::InPlace => {
            ContainerFate::Update(InPlaceChange::Limits)
        }
        SpecChange::NeedsPlacementUpdate if landing == Landing::InPlace => {
            ContainerFate::Update(InPlaceChange::Placement)
        }
        SpecChange::NeedsUpdate | SpecChange::NeedsPlacementUpdate | SpecChange::NeedsRecreate => {
            ContainerFate::Replace
        }
    }
}

/// A copy another run left Pending, Held or Out is not routed, so it is no
/// replica until replaced. A copy with no recorded rotation predates rotation.
fn serving_rotation(observation: &ContainerObservation) -> bool {
    match &observation.rotation {
        None | Some(CopyRotation::In) => true,
        Some(CopyRotation::Pending | CopyRotation::Held(_) | CopyRotation::Out) => {
            observation.namespace == Namespace::system()
        }
    }
}

fn reserve_replicated_service_demand(
    budget: &mut (CapacityBudget, HostSockets),
    requested: &RequestedServiceSpec,
    observed: Option<&ServiceObservation>,
    machine_id: MachineId,
    options: &PlanOptions,
    updaters: &InPlaceUpdaters,
) -> Result<ReplicatedReservation, PlanError> {
    let current = observed.map_or(&[][..], |service| service.containers.as_slice());
    let reserve = |budget: &mut (CapacityBudget, HostSockets), landing| {
        let (capacity, sockets) = budget;
        let mut credited_hooks = Vec::new();
        if let ServiceMode::Replicated { replicas } = requested.mode
            && requested.pre_deploy.is_some()
            && current
                .iter()
                .filter(|container| {
                    container.as_observation().machine_id == machine_id
                        && container_fate(container, requested, options, landing)
                            != ContainerFate::Replace
                })
                .count()
                < replicas.get() as usize
        {
            credited_hooks.extend(
                observed
                    .into_iter()
                    .flat_map(|service| &service.hook_containers)
                    .map(|hook| hook.as_observation().machine_id),
            );
            for machine_id in &credited_hooks {
                capacity.release(machine_id);
            }
        }
        let error = capacity.error_for([&machine_id]);
        let mut reservation = select_replicated(
            &Spread::anywhere(requested),
            current,
            vec![machine_id],
            &BTreeMap::new(),
            (capacity, sockets),
            error,
            (options, landing),
        )?;
        reservation.credited_hooks = credited_hooks;
        for container in current {
            let observation = container.as_observation();
            if !reservation
                .choices
                .iter()
                .any(|choice| match &choice.action {
                    ReplicaAction::Unchanged(id)
                    | ReplicaAction::Update(id, _)
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
    };
    Landing::decide(
        observed.is_some_and(|service| {
            updaters.can_update(service.service_id, current, requested, options)
        }),
        budget,
        reserve,
        |reservation| Landing::stands(reservation.trial_steps()),
    )
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
    landing: Landing,
) -> EndpointOperation {
    match existing.map(|container| container_fate(container, requested, options, landing)) {
        Some(ContainerFate::Keep) => EndpointOperation::Unchanged,
        Some(ContainerFate::Update(_)) => EndpointOperation::Update,
        Some(ContainerFate::Replace) => EndpointOperation::Replace,
        None => EndpointOperation::Create,
    }
}

/// A host port the current Container holds forces stop-first over any order but
/// `rolling`; a stateful Volume picks stop-first only when no order is given.
fn determine_update_order(
    current: Option<&ServiceContainer>,
    requested: &RequestedServiceSpec,
) -> UpdateOrder {
    let holds_host_port = current.is_some_and(|current| {
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
    });
    match requested.update.order {
        Some(order) if order.stops_first() => order,
        _ if holds_host_port => UpdateOrder::StopFirst,
        Some(order) => order,
        None if requested.volume_graph().mounted_volumes().any(|volume| {
            matches!(
                volume.source.kind(),
                ployz_core::RawVolumeSource::External { .. }
                    | ployz_core::RawVolumeSource::Ordinary { .. }
                    | ployz_core::RawVolumeSource::Provisioned { .. }
            )
        }) =>
        {
            UpdateOrder::StopFirst
        }
        None => UpdateOrder::StartFirst,
    }
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
