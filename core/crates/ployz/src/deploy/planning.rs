use std::collections::{BTreeMap, BTreeSet, VecDeque};

use ployz_core::{
    BlockedService, ContainerAction, ContainerId, CopyRotation, DataLoss, DependencyCondition,
    HookContainer, IngressHost, MachineId, MachineObservation, MembershipObservation, Namespace,
    ObservedDataLoss, PreservedVolume, PruneRefusal, QualifiedService, RequestedServiceSpec,
    RpcError, RpcErrorCode, ServiceContainer, ServiceId, ServiceMode, ServiceName,
    ServiceObservation, ServicePlacementEligibility, ServicePlacementIneligibleReason, UpdateOrder,
    VolumeToCreate, explicit_ingress_hosts, hostname_owners, machine_matches_placement,
    same_service_mode_kind,
};

use super::{
    DeployIntent, DeployOperation, DeployPreview, DeploySnapshot, DeployWarning,
    EliminatingConstraint, HeldIngress, HostnameRefusal, PlanError, PlanOptions,
    ReplacementOperation,
    schedule::{ServiceGroup, Step, TaskKind, starts_container},
};

pub(super) mod capacity;
#[cfg(test)]
pub(super) mod ingress_repairs;
mod observation;
mod placement;
mod storage;
mod volumes;

use placement::{
    CapacityAdmission, GlobalPlacement, LimitsChange, PlacementReservations, PlacementState,
    ReplicatedPlacement, ReplicatedReservation, plan_global, plan_replicated,
};
use volumes::{PlannedVolumes, VolumePlan, preserved_owned_volumes, scope_requested};

/// Whether Namespace removal keeps or destroys observer-visible managed volumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VolumeFate {
    /// Leave owned Docker Volumes in place. They keep listing under the Namespace.
    Preserve,
    /// Destroy each preserved owned Docker Volume after Services are removed.
    Destroy,
}

/// Volume-bound target plus the expanded apply-set. Private planning phase.
struct BoundIntent {
    target: Vec<RequestedServiceSpec>,
    requested: Vec<RequestedServiceSpec>,
    handovers: Vec<HostnameHandover>,
    retiring: Vec<QualifiedService>,
}

/// A hostname this Deploy moves between two Services it applies: `from` publishes it
/// today and its new spec drops it, `to` claims it. `from` waits for `to` to be published
/// and routed, so the hostname moves once its new owner can answer.
struct HostnameHandover {
    hostname: IngressHost,
    from: ServiceName,
    to: ServiceName,
}

/// Operations and prune results before pending rows are attached.
struct Planned {
    operations: Vec<DeployOperation>,
    groups: Vec<ServiceGroup>,
    volumes: PlannedVolumes,
    retiring: Vec<QualifiedService>,
    would_remove: Vec<QualifiedService>,
    preserved_volumes: Vec<PreservedVolume>,
    prune_refusal: Option<PruneRefusal>,
    warnings: Vec<DeployWarning>,
    blocked: Vec<BlockedService>,
}

/// An admitted, immutable sequence of Deploy operations and its informational preview.
///
/// Created by planning; serialized preview rows cannot be confirmed.
///
/// ```compile_fail
/// use ployz::{connect::Client, deploy::DeployPreview};
/// use tokio_util::sync::CancellationToken;
/// async fn forged(client: &Client, preview: DeployPreview) {
///     client.confirm(&preview, &CancellationToken::new(), None).await;
/// }
/// ```
///
/// ```compile_fail
/// let plan: ployz::deploy::DeployPlan = serde_json::from_str("{}").unwrap();
/// ```
#[derive(Clone, Debug)]
pub struct DeployPlan {
    operations: Vec<DeployOperation>,
    groups: Vec<ServiceGroup>,
    preview: DeployPreview,
    /// The intent's pull credentials, for execution only; never in the preview.
    registry_auth: std::collections::BTreeMap<ployz_core::ServiceName, ployz_core::RegistryAuth>,
    /// Every snapshot Machine's name, so a report names a Machine the plan places nothing on.
    machine_names: BTreeMap<MachineId, ployz_core::MachineName>,
    /// The Services this Deploy takes out wherever they run: each named removal the
    /// snapshot observed, and each Service it prunes.
    retiring: BTreeSet<QualifiedService>,
    /// Machines whose Container listing went unanswered while the entry's path probes
    /// found large packets to them dropped, so a notice can give that cause.
    dropped_large_packets: BTreeSet<MachineId>,
}

impl DeployPlan {
    /// Informational output, never an input to execution.
    #[must_use]
    pub fn preview(&self) -> &DeployPreview {
        &self.preview
    }

    pub(super) fn operations(&self) -> &[DeployOperation] {
        &self.operations
    }

    /// Service graph the executor walks, in execution order.
    #[must_use]
    pub fn groups(&self) -> &[ServiceGroup] {
        &self.groups
    }

    pub(super) const fn registry_auth(
        &self,
    ) -> &std::collections::BTreeMap<ployz_core::ServiceName, ployz_core::RegistryAuth> {
        &self.registry_auth
    }

    pub(crate) const fn machine_names(&self) -> &BTreeMap<MachineId, ployz_core::MachineName> {
        &self.machine_names
    }

    pub(super) const fn dropped_large_packets(&self) -> &BTreeSet<MachineId> {
        &self.dropped_large_packets
    }

    pub(super) const fn retiring(&self) -> &BTreeSet<QualifiedService> {
        &self.retiring
    }

    pub(super) fn pending_rows(&self) -> Vec<super::OperationRow> {
        self.operations
            .iter()
            .zip(&self.preview.operations)
            .enumerate()
            .map(|(index, (operation, row))| {
                super::OperationRow::pending(
                    u32::try_from(index).unwrap_or(u32::MAX),
                    operation.clone(),
                    row.machine_name.clone(),
                    row.display_name.clone(),
                    row.service_name.clone(),
                )
            })
            .collect()
    }

    pub(super) fn prepend_warnings(&mut self, warnings: Vec<DeployWarning>) {
        self.preview.warnings.splice(0..0, warnings);
    }

    #[cfg(test)]
    pub(super) fn for_execution_test(
        operations: Vec<DeployOperation>,
        namespace: Namespace,
    ) -> Self {
        let groups = test_groups(&operations);
        Self::for_execution_test_with_groups(operations, namespace, groups)
    }

    #[cfg(test)]
    pub(super) fn for_execution_test_with_groups(
        operations: Vec<DeployOperation>,
        namespace: Namespace,
        groups: Vec<ServiceGroup>,
    ) -> Self {
        let rows = super::pending_rows(&operations, &DeploySnapshot::default());
        Self {
            operations,
            groups,
            preview: DeployPreview::new(rows, Vec::new(), namespace),
            registry_auth: std::collections::BTreeMap::new(),
            machine_names: BTreeMap::new(),
            retiring: BTreeSet::new(),
            dropped_large_packets: BTreeSet::new(),
        }
    }

    /// This Deploy removes `services`, by name or by pruning.
    #[cfg(test)]
    pub(super) fn with_retiring(mut self, services: &[QualifiedService]) -> Self {
        self.retiring = services.iter().cloned().collect();
        self
    }

    /// The entry's path probes found large packets to `machines` dropped.
    #[cfg(test)]
    pub(super) fn with_dropped_large_packets(mut self, machines: &[MachineId]) -> Self {
        self.dropped_large_packets = machines.iter().copied().collect();
        self
    }

    #[cfg(test)]
    pub(super) fn with_machine_names(mut self, machines: &[MachineId]) -> Self {
        self.machine_names = machines
            .iter()
            .map(|id| {
                let hex = id.to_string().chars().next().expect("a machine id");
                let name = ployz_core::MachineName::parse(format!("machine-{hex}")).unwrap();
                (*id, name)
            })
            .collect();
        self
    }

    /// The plan's snapshot listed no Containers on `machines`, as for a Down Machine.
    #[cfg(test)]
    pub(super) fn with_unobserved(mut self, machines: &[MachineId]) -> Self {
        self.preview
            .warnings
            .extend(
                machines
                    .iter()
                    .map(|machine_id| DeployWarning::ObservationOmitted {
                        gap: None,
                        kind: ployz_core::ObservationKind::Container,
                        machine_id: *machine_id,
                    }),
            );
        self
    }
}

/// One group per Service in operation order, each after the one before it, so
/// a flat test plan runs as the sequential executor ran it. Operations naming
/// no Service join the Service before them; leading Volume work is the prelude
/// and anything after the last Service is the tail.
#[cfg(test)]
pub(super) fn test_groups(operations: &[DeployOperation]) -> Vec<ServiceGroup> {
    let mut groups: Vec<ServiceGroup> = Vec::new();
    let mut ranges: Vec<(Option<ServiceName>, bool, std::ops::Range<usize>)> = Vec::new();
    for (index, operation) in operations.iter().enumerate() {
        let prelude = matches!(operation, DeployOperation::PrepareVolumes { .. });
        let service = operation.service_name().cloned();
        match ranges.last_mut() {
            Some((current, false, range))
                if !prelude && (service.is_none() || *current == service) && current.is_some() =>
            {
                range.end = index + 1;
            }
            Some((None, was_prelude, range)) if service.is_none() && prelude == *was_prelude => {
                range.end = index + 1;
            }
            _ => ranges.push((service, prelude, index..index + 1)),
        }
    }
    let mut previous = None;
    for (service, prelude, range) in ranges {
        let dependencies = match (&service, prelude) {
            (None, true) => Vec::new(),
            (None, false) => groups
                .iter()
                .filter_map(|group| group.service.clone())
                .collect(),
            (Some(_), _) => previous.iter().cloned().collect(),
        };
        groups.extend(service_group(
            operations,
            service.clone(),
            dependencies,
            range,
        ));
        if service.is_some() {
            previous = service;
        }
    }
    groups
}

fn service_group(
    operations: &[DeployOperation],
    service: Option<ServiceName>,
    dependencies: Vec<ServiceName>,
    range: std::ops::Range<usize>,
) -> Vec<ServiceGroup> {
    let indexed = || {
        operations
            .iter()
            .enumerate()
            .take(range.end)
            .skip(range.start)
    };
    let rollout_removal =
        |operation: &DeployOperation| TaskKind::of(operation) == TaskKind::Unroute;
    let publish = service.is_some()
        && indexed()
            .any(|(_, operation)| starts_container(operation) || rollout_removal(operation));
    let rolling = indexed().any(|(_, operation)| {
        operation
            .spec()
            .is_some_and(|spec| spec.update.order == UpdateOrder::Rolling)
    });
    let mut steps: Vec<Step> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    let flush = |steps: &mut Vec<Step>, starts: &mut Vec<usize>| {
        if !rolling {
            if !starts.is_empty() {
                steps.push(Step::Parallel(std::mem::take(starts)));
            }
            return;
        }
        let mut machines: Vec<(MachineId, Vec<usize>)> = Vec::new();
        for index in starts.drain(..) {
            let machine = operations
                .get(index)
                .expect("group ranges index this plan's operations")
                .machine_id();
            match machines.iter_mut().find(|(owner, _)| *owner == machine) {
                Some((_, indices)) => indices.push(index),
                None => machines.push((machine, vec![index])),
            }
        }
        steps.extend(
            machines
                .into_iter()
                .map(|(_, indices)| Step::Parallel(indices)),
        );
    };
    let mut removals = Vec::new();
    for (index, operation) in indexed() {
        if publish && rollout_removal(operation) {
            removals.push(Step::Serial(index));
        } else if starts_container(operation) {
            starts.push(index);
        } else {
            flush(&mut steps, &mut starts);
            steps.push(Step::Serial(index));
        }
    }
    flush(&mut steps, &mut starts);
    let mut groups = vec![ServiceGroup {
        service: service.clone(),
        dependencies,
        steps,
        publish,
    }];
    if !removals.is_empty() {
        groups.push(ServiceGroup {
            service: None,
            dependencies: service.into_iter().collect(),
            steps: removals,
            publish: false,
        });
    }
    groups
}

impl std::ops::Deref for DeployPlan {
    type Target = DeployPreview;

    fn deref(&self) -> &Self::Target {
        self.preview()
    }
}

/// Plan removal of observer-visible compute for `namespace`.
///
/// Empty target is full reconciliation of nothing: owned Services are obsolete.
/// Managed volumes stay in `preserved_volumes` unless `volumes` is
/// [`VolumeFate::Destroy`] and pruning is not refused. Incomplete snapshots
/// reuse [`DeployIntent::prune_refusal`]; reserved names are refused at the
/// command boundary by [`crate::namespace::refuse_reserved`].
///
/// # Errors
///
/// Returns when [`preview_deploy`] cannot produce a preview.
pub fn plan_namespace_removal(
    namespace: &Namespace,
    snapshot: &DeploySnapshot,
    volumes: VolumeFate,
) -> Result<DeployPreview, PlanError> {
    Ok(prepare_namespace_removal(namespace, snapshot, volumes)?.preview)
}

pub(super) fn prepare_namespace_removal(
    namespace: &Namespace,
    snapshot: &DeploySnapshot,
    volumes: VolumeFate,
) -> Result<DeployPlan, PlanError> {
    let intent = DeployIntent::apply_all(namespace.clone(), [], PlanOptions::default());
    let mut planned = plan_operations(&intent, snapshot)?;
    if volumes == VolumeFate::Destroy && planned.prune_refusal.is_none() {
        let first = planned.operations.len();
        planned.operations.extend(
            planned
                .preserved_volumes
                .drain(..)
                .map(|volume| DeployOperation::RemoveVolume { id: volume.id }),
        );
        // Volume removal joins the tail so it runs after every stop, one at a time.
        let range = first..planned.operations.len();
        match planned.groups.last_mut() {
            Some(tail) if tail.service.is_none() && !range.is_empty() => {
                tail.steps.extend(range.map(Step::Serial));
            }
            _ if !range.is_empty() => {
                let every_service = planned
                    .groups
                    .iter()
                    .filter_map(|group| group.service.clone())
                    .collect();
                planned.groups.extend(service_group(
                    &planned.operations,
                    None,
                    every_service,
                    range,
                ));
            }
            _ => {}
        }
    }
    Ok(seal_plan(planned, snapshot, namespace))
}

/// Data Loss implied by a Namespace-removal preview.
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
/// Expands applied Cluster Domain hostnames, binds Namespace volumes once, then
/// constructs pending operation rows. Does not mutate `intent`.
///
/// Matching and replacement use only Containers owned by
/// [`DeployIntent::namespace`]. Empty `options.selected` is a full
/// reconciliation of `target`. Non-empty
/// `selected` is partial: those names expand through dependencies that are also
/// in `target`. Other target Services are unchanged. Visible obsolete Services
/// owned by that user Namespace are removed after desired work when pruning is
/// not refused; otherwise they are listed as `would_remove`.
///
/// # Errors
///
/// Returns when hostname conflicts,
/// placement, volumes, service identity, or the apply-set dependency graph
/// cannot produce a preview.
pub fn preview_deploy(
    intent: &DeployIntent,
    snapshot: &DeploySnapshot,
) -> Result<DeployPreview, PlanError> {
    Ok(plan_deploy(intent, snapshot)?.preview)
}

/// Admit a Deploy for confirmation against this observer-relative snapshot.
///
/// # Errors
/// Returns the same admission failures as [`preview_deploy`].
pub fn plan_deploy(
    intent: &DeployIntent,
    snapshot: &DeploySnapshot,
) -> Result<DeployPlan, PlanError> {
    Ok(admit(intent, snapshot, plan_operations(intent, snapshot)?))
}

fn admit(intent: &DeployIntent, snapshot: &DeploySnapshot, mut planned: Planned) -> DeployPlan {
    let budgets = std::mem::take(&mut planned.volumes.budgets);
    let mut plan = seal_plan(planned, snapshot, &intent.namespace);
    plan.registry_auth.clone_from(&intent.registry_auth);
    if !budgets.is_empty() {
        plan.preview
            .warnings
            .push(DeployWarning::UnbudgetedDiskUsage);
    }
    for storage in &budgets {
        let budget = &storage.budget;
        let remaining_bytes = budget
            .available_bytes
            .saturating_sub(budget.reserve_bytes)
            .saturating_sub(budget.required_growth_bytes);
        if budget.required_growth_bytes > 0 && remaining_bytes < ployz_core::STORAGE_GIB {
            plan.preview.warnings.push(DeployWarning::StorageHeadroom {
                machine_id: storage.machine_id,
                remaining_bytes,
            });
        }
    }
    plan.preview.storage = budgets;
    plan
}

/// What brings back one ingress Server's Ingress Proxy.
#[derive(Clone, Debug)]
pub enum SlotAction {
    /// No Ingress Proxy is observed there, so one is created. Admission trouble here
    /// fails only this Server's repair.
    Create(Result<Box<DeployPlan>, PlanError>),
    /// The newest Ingress Proxy at rest there starts again as it is.
    Start(ContainerId),
    /// Only dead Ingress Proxy containers are there, the newest given. Docker starts
    /// none, so the Server's repair fails until they are removed.
    Dead(ContainerId),
    /// Its Ingress Proxy keeps restarting, the newest given. Starting or recreating it
    /// would crash the same way, so the Server's repair fails with no work.
    Restarting(ContainerId),
    /// The Server's containers went unobserved, so it was not checked.
    Unchecked(String),
}

/// One ingress Server's repair.
#[derive(Clone, Debug)]
pub struct SlotRepair {
    pub machine_id: MachineId,
    pub action: SlotAction,
}

/// Plan each ingress Server of `snapshot` on its own: start the newest Ingress Proxy at
/// rest there, create one from `intent` where none is observed, and list the Servers
/// that went unobserved.
#[must_use]
pub fn plan_ingress_repairs(intent: &DeployIntent, snapshot: &DeploySnapshot) -> Vec<SlotRepair> {
    let service_id = snapshot
        .system_ingress()
        .map_or_else(ServiceId::random, |service| service.service_id);
    let unchecked = snapshot
        .unobserved_ingress()
        .into_iter()
        .map(|(machine, cause)| SlotRepair {
            machine_id: machine.machine.id,
            action: SlotAction::Unchecked(cause),
        });
    let repairs = snapshot
        .empty_ingress()
        .into_iter()
        .map(|empty| SlotRepair {
            machine_id: empty.machine.machine.id,
            action: match empty.held {
                HeldIngress::Stopped(stopped) => SlotAction::Start(stopped.container_id),
                HeldIngress::Dead(dead) => SlotAction::Dead(dead.container_id),
                HeldIngress::Restarting(restarting) => {
                    SlotAction::Restarting(restarting.container_id)
                }
                HeldIngress::None => SlotAction::Create(
                    plan_create(intent, &snapshot.only(empty.machine), service_id).map(Box::new),
                ),
            },
        });
    let mut all: Vec<_> = repairs.chain(unchecked).collect();
    all.sort_by_key(|repair| repair.machine_id);
    all
}

/// `repairs` without the unchecked Servers that left since the plan read membership,
/// by `reread`, a later read of it: one now marked leaving, or no longer listed,
/// went with its ingress role. A plan can read membership just before a Leave marks
/// the Server and then time out on it as it goes (#3288). Without a re-read, the
/// repairs stand.
#[must_use]
pub fn without_departed(
    repairs: Vec<SlotRepair>,
    reread: Option<&[MachineObservation]>,
) -> Vec<SlotRepair> {
    let Some(reread) = reread else {
        return repairs;
    };
    repairs
        .into_iter()
        .filter(|repair| {
            !matches!(repair.action, SlotAction::Unchecked(_))
                || reread.iter().any(|machine| {
                    machine.machine.id == repair.machine_id && !machine.machine.runtime.leaving
                })
        })
        .collect()
}

/// Plan the Ingress Proxy on the one Machine of `only` as the Service `service_id`, which
/// its siblings run or are created as in the same check and `only` cannot see: they share
/// its creation key.
fn plan_create(
    intent: &DeployIntent,
    only: &DeploySnapshot,
    service_id: ServiceId,
) -> Result<DeployPlan, PlanError> {
    let mut planned = plan_operations(intent, only)?;
    for operation in &mut planned.operations {
        if let DeployOperation::RunContainer { spec, .. } = operation {
            spec.service_id = service_id;
        }
    }
    Ok(admit(intent, only, planned))
}

/// The Servers each deployed Service has a copy on once every operation completes.
/// An operation that names an observed copy replaces, updates, stops or removes it,
/// so every copy no operation names stays where it is.
fn placed_on(
    planned: &Planned,
    snapshot: &DeploySnapshot,
    namespace: &Namespace,
) -> BTreeMap<ServiceName, BTreeSet<MachineId>> {
    let named: BTreeSet<ContainerId> = planned
        .operations
        .iter()
        .filter_map(DeployOperation::container_id)
        .collect();
    let observed = snapshot.services_in(namespace);
    planned
        .groups
        .iter()
        .filter_map(|group| group.service.as_ref())
        .map(|service| {
            let kept = observed
                .iter()
                .filter(|observed| observed.identity.name == *service)
                .flat_map(|observed| &observed.containers)
                .map(ServiceContainer::as_observation)
                .filter(|copy| !named.contains(&copy.container_id))
                .map(|copy| copy.machine_id);
            let started = planned
                .operations
                .iter()
                .filter(|operation| {
                    matches!(
                        operation,
                        DeployOperation::RunContainer { .. }
                            | DeployOperation::ReplaceContainer(_)
                            | DeployOperation::UpdateContainer(_)
                    ) && operation.service_name() == Some(service)
                })
                .map(DeployOperation::machine_id);
            (service.clone(), kept.chain(started).collect())
        })
        .collect()
}

fn seal_plan(planned: Planned, snapshot: &DeploySnapshot, namespace: &Namespace) -> DeployPlan {
    let placed_on = placed_on(&planned, snapshot, namespace);
    let preview = DeployPreview {
        storage: Vec::new(),
        namespace: namespace.clone(),
        operations: super::pending_rows(&planned.operations, snapshot),
        warnings: planned.warnings,
        volumes_to_create: planned
            .volumes
            .creates
            .into_iter()
            .filter_map(|(machine_id, volume)| {
                let (name, maximum_bytes) = match volume.source.kind() {
                    ployz_core::RawVolumeSource::Ordinary { name, .. } => (name.clone(), None),
                    ployz_core::RawVolumeSource::Provisioned {
                        name,
                        maximum_bytes,
                        ..
                    } => (name.clone(), Some(*maximum_bytes)),
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
        depths: super::report::depths(&planned.groups),
        dependencies: super::report::dependencies(&planned.groups),
        placed_on,
        blocked: planned.blocked,
    };
    DeployPlan {
        operations: planned.operations,
        groups: planned.groups,
        preview,
        registry_auth: std::collections::BTreeMap::new(),
        machine_names: super::progress::machine_names(snapshot),
        retiring: planned.retiring.into_iter().collect(),
        dropped_large_packets: snapshot
            .container_failures
            .iter()
            .filter(|failure| {
                snapshot.machines.iter().any(|machine| {
                    machine.machine.id == failure.machine_id
                        && super::large_packets_note(machine, &failure.error).is_some()
                })
            })
            .map(|failure| failure.machine_id)
            .collect(),
    }
}

fn plan_operations(intent: &DeployIntent, snapshot: &DeploySnapshot) -> Result<Planned, PlanError> {
    let bound = bind(intent, snapshot)?;
    assemble_plan(intent, bound, snapshot)
}

fn bind(intent: &DeployIntent, snapshot: &DeploySnapshot) -> Result<BoundIntent, PlanError> {
    check_removals(intent)?;
    let retiring = retiring_services(intent, snapshot);
    let handovers = hostname_handovers(intent, snapshot, &retiring)?;
    let specs = order_included(intent, &intent.applied_names(), &handovers)?;
    let target: Vec<_> = intent
        .target
        .iter()
        .cloned()
        .map(|spec| scope_requested(spec, &intent.namespace))
        .collect::<Result<_, _>>()?;
    let requested = specs
        .into_iter()
        .map(|spec| {
            target
                .iter()
                .find(|candidate| candidate.name == spec.name)
                .expect("apply-set names are drawn from the Intent target")
                .clone()
        })
        .collect();
    Ok(BoundIntent {
        target,
        requested,
        handovers,
        retiring,
    })
}

/// The Services this Deploy retires: what a full Deploy prunes, and the removals it
/// names. Absence is evidence only in a complete snapshot, so a partial one prunes
/// nothing. A named removal needs no such evidence: it goes ahead on the Servers the
/// snapshot observed, and the next full Deploy prunes a copy on one it couldn't check.
fn retiring_services(intent: &DeployIntent, snapshot: &DeploySnapshot) -> Vec<QualifiedService> {
    let services = snapshot.services_in(&intent.namespace);
    let mut retiring = if intent
        .prune_refusal(snapshot.is_observer_complete())
        .is_none()
    {
        obsolete_services(intent, &services)
    } else {
        Vec::new()
    };
    for service in &services {
        if intent.options.removals.contains(&service.identity.name)
            && !retiring.contains(&service.identity)
        {
            retiring.push(service.identity.clone());
        }
    }
    retiring
}

/// The hostnames moving between two Services this Deploy applies. Each one orders the
/// Service gaining it before the one dropping it, so a hostname whose gainer already
/// waits for its dropper can't move in this Deploy, and neither can one whose dropper
/// this Deploy leaves out.
fn hostname_handovers(
    intent: &DeployIntent,
    snapshot: &DeploySnapshot,
    retiring: &[QualifiedService],
) -> Result<Vec<HostnameHandover>, PlanError> {
    let applied = intent.applied_names();
    let owners = hostname_owners(
        snapshot
            .containers
            .iter()
            .filter(|container| !retiring.contains(&container.identity())),
    );
    let mut handovers: Vec<HostnameHandover> = Vec::new();
    for spec in intent
        .target
        .iter()
        .filter(|spec| applied.contains(&spec.name))
    {
        for hostname in explicit_ingress_hosts(&spec.ports) {
            let Some(owner) = owners.get(hostname) else {
                continue;
            };
            if owner.namespace != intent.namespace || owner.name == spec.name {
                continue;
            }
            let Some(dropper) = intent.target.iter().find(|spec| spec.name == owner.name) else {
                continue;
            };
            if explicit_ingress_hosts(&dropper.ports).any(|kept| kept == hostname) {
                continue;
            }
            let refusal = if !applied.contains(&owner.name) {
                Some(HostnameRefusal::OwnerNotIncluded)
            } else {
                waits_for(intent, &applied, &handovers, &spec.name, &owner.name).map(|because| {
                    HostnameRefusal::GainerWaitsForOwner {
                        to: spec.name.clone(),
                        because,
                    }
                })
            };
            if let Some(refusal) = refusal {
                return Err(PlanError::HostnameConflict {
                    hostname: hostname.clone(),
                    owner: owner.clone(),
                    refusal,
                });
            }
            handovers.push(HostnameHandover {
                hostname: hostname.clone(),
                from: owner.name.clone(),
                to: spec.name.clone(),
            });
        }
    }
    Ok(handovers)
}

/// Why `service` waits for `other` before it starts, if it does: a declared dependency or
/// a handover already ordered, directly or through other applied Services.
fn waits_for(
    intent: &DeployIntent,
    applied: &BTreeSet<&ServiceName>,
    handovers: &[HostnameHandover],
    service: &ServiceName,
    other: &ServiceName,
) -> Option<String> {
    let mut pending = VecDeque::from([(service, None)]);
    let mut seen = BTreeSet::new();
    while let Some((name, because)) = pending.pop_front() {
        if name == other {
            return because;
        }
        if !seen.insert(name) {
            continue;
        }
        let because = |step: String| {
            if name == service {
                format!("{name} {step}")
            } else {
                format!("{service} waits for {name}, which {step}")
            }
        };
        for dependency in intent.dependencies().get(name).into_iter().flatten() {
            if applied.contains(&&dependency.service) {
                let step = because(format!("depends on {}", dependency.service));
                pending.push_back((&dependency.service, Some(step)));
            }
        }
        for handover in handovers.iter().filter(|handover| handover.from == *name) {
            let step = because(format!("hands {} to {}", handover.hostname, handover.to));
            pending.push_back((&handover.to, Some(step)));
        }
    }
    None
}

fn hostname_policy_for(
    namespace: &Namespace,
    requested: &[RequestedServiceSpec],
    snapshot: &DeploySnapshot,
    retiring: &[QualifiedService],
    handovers: &[HostnameHandover],
) -> Result<Vec<DeployWarning>, PlanError> {
    reject_hostname_conflicts(namespace, requested, snapshot, retiring, handovers)?;
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

/// Whether `error` says only that `spec`'s Volume sits on a Machine that can't take it
/// now: down, silent this Deploy, or, for a provisioned Volume, without its storage.
/// Configuration that matches no Machine, or a Volume on a Machine that answered,
/// still fails the Deploy.
fn volume_server_away(
    error: &PlanError,
    snapshot: &DeploySnapshot,
    spec: &RequestedServiceSpec,
) -> bool {
    let PlanError::NoEligibleMachines { constraints } = error else {
        return false;
    };
    let away = |volume: &ployz_core::DockerVolumeName, name: &ployz_core::MachineName| {
        let provisioned = spec
            .volume_graph()
            .mounted_provisioned_volumes()
            .any(|mounted| mounted.source.docker_volume_name() == Some(volume));
        snapshot
            .machines
            .iter()
            .find(|machine| &machine.machine.name == name)
            .is_some_and(|machine| {
                machine.membership == MembershipObservation::Down
                    || snapshot.silent(machine)
                    || (provisioned && storage::capacity(snapshot, machine).is_err())
            })
    };
    let constraints = constraints.as_slice();
    let located = constraints
        .iter()
        .filter_map(|constraint| {
            if let EliminatingConstraint::VolumeAlreadyOn { volume, located_on } = constraint {
                Some((volume, located_on))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    !located.is_empty()
        && located
            .iter()
            .all(|(volume, located_on)| located_on.iter().all(|machine| away(volume, machine)))
        && constraints.iter().all(|constraint| {
            matches!(
                constraint,
                EliminatingConstraint::VolumeAlreadyOn { .. }
                    | EliminatingConstraint::MachineDown { .. }
                    | EliminatingConstraint::Unanswered { .. }
            )
        })
}

/// The unknown storage of a Machine where `operations` start a Service that mounts a
/// provisioned Volume. The Deploy's storage budget needs it there.
fn silent_volume_server(
    operations: &[DeployOperation],
    snapshot: &DeploySnapshot,
) -> Option<PlanError> {
    operations
        .iter()
        .filter(|operation| {
            operation
                .spec()
                .is_some_and(|spec| spec.volume_graph().has_mounted_provisioned_volume())
        })
        .find_map(|operation| {
            let machine = snapshot
                .machines
                .iter()
                .find(|machine| machine.machine.id == operation.machine_id())?;
            storage::capacity(snapshot, machine).err()
        })
}

fn assemble_plan(
    intent: &DeployIntent,
    bound: BoundIntent,
    snapshot: &DeploySnapshot,
) -> Result<Planned, PlanError> {
    let BoundIntent {
        target,
        requested,
        handovers,
        retiring,
    } = bound;
    let services = snapshot.services_in(&intent.namespace);
    let would_remove = obsolete_services(intent, &services);
    let prune_refusal = intent.prune_refusal(snapshot.is_observer_complete());
    let mut warnings = observation::warnings(
        &intent.namespace,
        &requested,
        &retiring,
        &intent.options.removals,
        snapshot,
    );
    if prune_refusal == Some(PruneRefusal::RemovalsOnly) {
        warnings.extend(
            would_remove
                .iter()
                .filter(|service| !retiring.contains(service))
                .map(|service| DeployWarning::NotInStore {
                    service: service.clone(),
                }),
        );
    }
    warnings.extend(hostname_policy_for(
        &intent.namespace,
        &requested,
        snapshot,
        &retiring,
        &handovers,
    )?);
    warnings.extend(storage_eligibility_warnings(
        &requested,
        &intent.namespace,
        snapshot,
    ));
    for spec in &requested {
        placement::validate_host_ports(spec)?;
    }
    let mut volume_plan = VolumePlan::new(
        snapshot,
        &intent.namespace,
        &intent.volume_names,
        &intent.deployed_volumes,
        &target,
        &requested,
        intent.options.operator,
    )?;
    let name_errors_with_service = requested.len() > 1;
    let mut reservations = PlacementReservations::new(snapshot);
    let mut blocked =
        volume_plan.reserve_shared(&requested, &services, &mut reservations, &intent.options)?;
    let mut budget = ServiceBudget {
        volumes: volume_plan,
        placement: reservations.into_placement(snapshot),
    };
    let mut service_operations = Vec::new();
    let mut ranges = Vec::new();
    // `requested` places every dependency before its dependents and a hostname's gainer
    // before its dropper, so a Service whose dependency or gainer is blocked is seen after it.
    for spec in &requested {
        if blocked.iter().any(|entry| entry.service == spec.name) {
            continue;
        }
        if let Some(dependency) = intent
            .dependencies()
            .get(&spec.name)
            .into_iter()
            .flatten()
            .find(|dependency| {
                blocked
                    .iter()
                    .any(|entry| entry.service == dependency.service)
            })
        {
            blocked.push(BlockedService {
                service: spec.name.clone(),
                error: dependency_blocked(&dependency.service),
            });
            budget.release(&spec.name);
            continue;
        }
        if let Some(handover) = handovers.iter().find(|handover| {
            handover.from == spec.name && blocked.iter().any(|entry| entry.service == handover.to)
        }) {
            blocked.push(BlockedService {
                service: spec.name.clone(),
                error: handover_blocked(handover),
            });
            budget.release(&spec.name);
            continue;
        }
        let first = service_operations.len();
        // Placement spends capacity and host ports as it goes, so a Service blocked once
        // it is placed puts back the budget it found.
        let before = intent.options.block_unmountable.then(|| budget.clone());
        let operations = match plan_one_service(
            spec,
            &intent.namespace,
            snapshot,
            &services,
            &mut budget,
            &intent.options,
        ) {
            Ok(operations) => operations,
            // A Volume nobody can mount stops only the Services that mount it: the rest of
            // the Deploy goes ahead, and these are reported not attempted (bet 8).
            Err(
                error @ (PlanError::NoWriter { .. }
                | PlanError::CopiesUnknown { .. }
                | PlanError::VolumeHolderUnknown { .. }
                | PlanError::VolumeServerTakesNoServices { .. }),
            ) if intent.options.block_unmountable => {
                blocked.push(BlockedService {
                    service: spec.name.clone(),
                    error: error.into_rpc_error(),
                });
                budget.release(&spec.name);
                continue;
            }
            Err(error)
                if intent.options.block_unmountable
                    && volume_server_away(&error, snapshot, spec) =>
            {
                blocked.push(BlockedService {
                    service: spec.name.clone(),
                    error: error.into_rpc_error(),
                });
                budget.release(&spec.name);
                continue;
            }
            Err(source) => {
                return Err(service_error(
                    name_errors_with_service,
                    spec.name.as_str(),
                    source,
                ));
            }
        };
        // A Volume on a Server that didn't return its storage can't be budgeted there, so
        // only the Service mounting it waits; the rest of the Deploy goes ahead (bet 4).
        if intent.options.block_unmountable
            && let Some(error) = silent_volume_server(&operations, snapshot)
        {
            blocked.push(BlockedService {
                service: spec.name.clone(),
                error: error.into_rpc_error(),
            });
            if let Some(before) = before {
                budget = before;
            }
            budget.release(&spec.name);
            continue;
        }
        if let Some(machine_id) = operations.iter().find_map(|operation| match operation {
            DeployOperation::RunContainer { machine_id, .. }
            | DeployOperation::ReplaceContainer(ReplacementOperation { machine_id, .. }) => {
                Some(*machine_id)
            }
            DeployOperation::UpdateContainer(_)
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::PrepareVolumes { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        }) {
            for dependency in intent.dependencies().get(&spec.name).into_iter().flatten() {
                if dependency.condition != DependencyCondition::ServiceHealthy {
                    continue;
                }
                let dependent = QualifiedService::new(intent.namespace.clone(), spec.name.clone());
                let dependency =
                    QualifiedService::new(intent.namespace.clone(), dependency.service.clone());
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
        ranges.push((spec.name.clone(), first..service_operations.len()));
    }
    // Shared components were blocked before the loop; in `requested` order the first
    // entry is the earliest root cause, which is the one the Deployment reports.
    blocked.sort_by_key(|entry| requested.iter().position(|spec| spec.name == entry.service));
    let before_volumes = service_operations.len();
    let volumes = budget.volumes.finish(&mut service_operations)?;
    let mut operations = service_operations;
    let prelude = operations.len() - before_volumes;
    let preserved_volumes = preserved_owned_volumes(&intent.namespace, &target, snapshot);
    let (removals, tail) = removal_operations(&services, &retiring, operations.len());
    operations.extend(removals);
    let mut groups = Vec::new();
    if prelude > 0 {
        groups.extend(service_group(&operations, None, Vec::new(), 0..prelude));
    }
    let dependencies = intent.dependencies();
    for (name, range) in ranges {
        let range = range.start + prelude..range.end + prelude;
        let mut dependencies: Vec<ServiceName> = dependencies
            .get(&name)
            .into_iter()
            .flatten()
            .map(|dependency| dependency.service.clone())
            .collect();
        // A Service dropping a hostname replaces its copies only once the Service taking it
        // over is published and routed, so the hostname moves to a copy that answers.
        for handover in &handovers {
            if handover.from == name && !dependencies.contains(&handover.to) {
                dependencies.push(handover.to.clone());
            }
        }
        groups.extend(service_group(&operations, Some(name), dependencies, range));
    }
    if !tail.is_empty() {
        groups.push(ServiceGroup {
            service: None,
            dependencies: requested
                .iter()
                .map(|spec| spec.name.clone())
                .filter(|name| !blocked.iter().any(|entry| entry.service == *name))
                .collect(),
            steps: tail,
            publish: false,
        });
    }
    Ok(Planned {
        operations,
        groups,
        volumes,
        retiring,
        would_remove,
        preserved_volumes,
        prune_refusal,
        warnings,
        blocked,
    })
}

/// Why a Service that depends on a blocked one is blocked too.
fn dependency_blocked(dependency: &ServiceName) -> RpcError {
    RpcError {
        code: RpcErrorCode::Conflict,
        message: format!("depends on {dependency}, which was not attempted"),
        details: serde_json::json!({ "reason": "dependency_blocked", "dependency": dependency }),
        cause: Vec::new(),
    }
}

/// Why a Service handing a hostname to a blocked one is blocked too: replacing its copies
/// would take the hostname from the only copies that answer.
fn handover_blocked(handover: &HostnameHandover) -> RpcError {
    RpcError {
        code: RpcErrorCode::Conflict,
        message: format!(
            "hands hostname {} to {}, which was not attempted",
            handover.hostname, handover.to
        ),
        details: serde_json::json!({
            "reason": "dependency_blocked",
            "dependency": handover.to,
            "hostname": handover.hostname,
        }),
        cause: Vec::new(),
    }
}

fn reject_hostname_conflicts(
    namespace: &Namespace,
    requested: &[RequestedServiceSpec],
    snapshot: &DeploySnapshot,
    retiring: &[QualifiedService],
    handovers: &[HostnameHandover],
) -> Result<(), PlanError> {
    // A full Deploy may transfer a hostname from a Service it retires after desired work
    // succeeds, and a Service may hand one to another in the same Deploy. Recompute
    // ownership without the copies that give it up so other publishers still block it.
    let retiring = retiring.iter().collect::<BTreeSet<_>>();
    let owner_of = |hostname: &IngressHost, claimant: &ServiceName| {
        let handed_over = |identity: &QualifiedService| {
            identity.namespace == *namespace
                && handovers.iter().any(|handover| {
                    handover.hostname == *hostname
                        && handover.from == identity.name
                        && handover.to == *claimant
                })
        };
        hostname_owners(snapshot.containers.iter().filter(|container| {
            let identity = container.identity();
            !retiring.contains(&identity) && !handed_over(&identity)
        }))
        .remove(hostname)
    };
    let mut claimed = BTreeMap::<&IngressHost, QualifiedService>::new();
    for spec in requested {
        let identity = QualifiedService::new(namespace.clone(), spec.name.clone());
        for hostname in explicit_ingress_hosts(&spec.ports) {
            let refused = match claimed.get(hostname) {
                Some(owner) => Some((
                    owner.clone(),
                    HostnameRefusal::RequestedTwice {
                        to: spec.name.clone(),
                    },
                )),
                None => owner_of(hostname, &spec.name)
                    .map(|owner| (owner, HostnameRefusal::StillPublished)),
            };
            if let Some((owner, refusal)) = refused
                && owner != identity
            {
                return Err(PlanError::HostnameConflict {
                    hostname: hostname.clone(),
                    owner,
                    refusal,
                });
            }
            claimed.entry(hostname).or_insert_with(|| identity.clone());
        }
    }
    Ok(())
}

/// Find observed Services absent from the full target, excluding reserved namespaces.
/// The caller must apply the intent's prune refusal before deleting them.
fn obsolete_services(
    intent: &DeployIntent,
    services: &[ServiceObservation],
) -> Vec<QualifiedService> {
    if intent.namespace.is_reserved() {
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

/// Refuse retirement a Deploy can't carry out exactly: of a Service it also
/// deploys, of one a targeted Deploy doesn't select, or in a reserved Namespace.
fn check_removals(intent: &DeployIntent) -> Result<(), PlanError> {
    if intent.options.removals.is_empty() {
        return Ok(());
    }
    if intent.namespace.is_reserved() {
        return Err(PlanError::RemovalInReservedNamespace {
            namespace: intent.namespace.clone(),
        });
    }
    for service in intent.options.removals.iter() {
        if intent.target.iter().any(|spec| spec.name == *service) {
            return Err(PlanError::RemovalInTarget {
                service: service.clone(),
            });
        }
        if !intent.options.selected.is_empty()
            && !intent
                .options
                .selected
                .iter()
                .any(|attempt| attempt.name == *service)
        {
            return Err(PlanError::RemovalNotSelected {
                service: service.clone(),
            });
        }
    }
    Ok(())
}

/// Removals of every copy of each obsolete Service, and the steps that run
/// them from index `first` on. A Pending copy keeps every proxy on the last
/// routed copy, so Pending copies go first, one at a time. The rest go Out in
/// one step: a copy still In that cannot serve keeps every proxy on an Out
/// sibling, whose own wait then never ends.
fn removal_operations(
    services: &[ServiceObservation],
    obsolete: &[QualifiedService],
    first: usize,
) -> (Vec<DeployOperation>, Vec<Step>) {
    let obsolete = obsolete.iter().collect::<BTreeSet<_>>();
    let (pending, rest): (Vec<_>, Vec<_>) = services
        .iter()
        .filter(|service| obsolete.contains(&service.identity))
        .flat_map(|service| service.containers_for(ContainerAction::Remove))
        .partition(|container| *container.as_observation().rotation() == CopyRotation::Pending);
    let serial = first..first + pending.len();
    let at_once = serial.end..serial.end + rest.len();
    let mut steps: Vec<Step> = serial.map(Step::Serial).collect();
    if !at_once.is_empty() {
        steps.push(Step::Parallel(at_once.collect()));
    }
    let operations = pending
        .into_iter()
        .chain(rest)
        .map(|container| {
            let observation = container.as_observation();
            DeployOperation::RemoveContainer {
                machine_id: observation.machine_id,
                container_id: observation.container_id,
                purpose: ployz_core::RemoveContainerPurpose::Retire,
            }
        })
        .collect();
    (operations, steps)
}

fn order_included<'intent>(
    intent: &'intent DeployIntent,
    included: &BTreeSet<&'intent ServiceName>,
    handovers: &[HostnameHandover],
) -> Result<Vec<&'intent RequestedServiceSpec>, PlanError> {
    let mut by_name = BTreeMap::new();
    for spec in &intent.target {
        if by_name.insert(&spec.name, spec).is_some() {
            return Err(PlanError::DuplicateTargetService {
                service: spec.name.clone(),
            });
        }
    }
    let mut walk = DependencyWalk {
        intent,
        included,
        handovers,
        by_name,
        visiting: BTreeSet::new(),
        visited: BTreeSet::new(),
        ordered: Vec::new(),
    };
    for spec in &intent.target {
        if included.contains(&&spec.name) {
            walk.visit(&spec.name)?;
        }
    }
    Ok(walk.ordered)
}

/// Depth-first walk that places every Service after the ones it waits for.
struct DependencyWalk<'walk, 'intent> {
    intent: &'intent DeployIntent,
    included: &'walk BTreeSet<&'intent ServiceName>,
    handovers: &'walk [HostnameHandover],
    by_name: BTreeMap<&'intent ServiceName, &'intent RequestedServiceSpec>,
    visiting: BTreeSet<&'intent ServiceName>,
    visited: BTreeSet<&'intent ServiceName>,
    ordered: Vec<&'intent RequestedServiceSpec>,
}

impl<'intent> DependencyWalk<'_, 'intent> {
    fn visit(&mut self, name: &'intent ServiceName) -> Result<(), PlanError> {
        if self.visited.contains(name) {
            return Ok(());
        }
        if !self.visiting.insert(name) {
            return Err(PlanError::DependencyCycle {
                service: name.as_str().to_owned(),
            });
        }
        for before in self.before(name) {
            self.visit(before)?;
        }
        self.visiting.remove(name);
        self.visited.insert(name);
        if let Some(spec) = self.by_name.get(name) {
            self.ordered.push(*spec);
        }
        Ok(())
    }

    /// The applied Services `name` waits for: its declared dependencies, then the
    /// Service taking over a hostname it drops.
    fn before(&self, name: &ServiceName) -> Vec<&'intent ServiceName> {
        let declared = self
            .intent
            .dependencies()
            .get(name)
            .into_iter()
            .flatten()
            .map(|dependency| &dependency.service)
            .filter(|dependency| self.included.contains(dependency));
        let gainers = self
            .handovers
            .iter()
            .filter(|handover| handover.from == *name)
            .map(|handover| &handover.to);
        declared
            .chain(gainers)
            .filter_map(|service| self.by_name.get_key_value(service).map(|(name, _)| *name))
            .collect()
    }
}

/// What planning a Service spends: Volume assignments and Machine capacity.
/// A trial that is abandoned leaves its clone behind and the budget untouched.
#[derive(Clone)]
struct ServiceBudget<'snapshot> {
    volumes: VolumePlan<'snapshot>,
    placement: PlacementState,
}

impl ServiceBudget<'_> {
    /// Give back everything `service`, which this Deploy does not attempt, had committed.
    fn release(&mut self, service: &ServiceName) {
        self.volumes.release(service);
        self.placement.release(service);
    }
}

/// One Service to place, with what the Cluster already runs of it.
struct ServiceTarget<'a, 'snapshot> {
    requested: &'a RequestedServiceSpec,
    service_id: ServiceId,
    current: &'a [ServiceContainer],
    hooks: &'a [HookContainer],
    machines: Vec<&'snapshot MachineObservation>,
}

fn plan_one_service<'snapshot>(
    requested: &RequestedServiceSpec,
    namespace: &Namespace,
    snapshot: &'snapshot DeploySnapshot,
    services: &[ServiceObservation],
    budget: &mut ServiceBudget<'snapshot>,
    options: &PlanOptions,
) -> Result<Vec<DeployOperation>, PlanError> {
    let mut machines = eligible_machines(requested, namespace, snapshot, options)?;
    let identity = QualifiedService::new(namespace.clone(), requested.name.clone());
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
    let reservation = budget.placement.take_reservation(&requested.name);
    if matches!(requested.mode, ServiceMode::Replicated { .. }) && reservation.is_none() {
        budget
            .volumes
            .constrain_candidates(requested, &mut machines)?;
    }
    let target = ServiceTarget {
        requested,
        service_id,
        current,
        hooks,
        machines,
    };
    match reservation {
        // The reservation fixed each copy's action when it was made.
        Some(reservation) => place_service(
            &target,
            Some(reservation),
            budget,
            options,
            LimitsChange::Replace,
        ),
        None => LimitsChange::decide(
            budget
                .placement
                .can_update_in_place(current, requested, options),
            budget,
            |budget, limits| place_service(&target, None, budget, options, limits),
            |operations| operations.iter().any(starts_copy),
        ),
    }
}

fn starts_copy(operation: &DeployOperation) -> bool {
    matches!(
        operation,
        DeployOperation::RunContainer { .. } | DeployOperation::ReplaceContainer(_)
    )
}

/// Place one Service under one way of landing its limits change.
fn place_service<'snapshot>(
    target: &ServiceTarget<'_, 'snapshot>,
    reservation: Option<ReplicatedReservation>,
    budget: &mut ServiceBudget<'snapshot>,
    options: &PlanOptions,
    limits: LimitsChange,
) -> Result<Vec<DeployOperation>, PlanError> {
    let ServiceTarget {
        requested,
        service_id,
        current,
        hooks,
        machines,
    } = target;
    let ServiceBudget {
        volumes: volume_plan,
        placement,
    } = budget;
    let mut machines = machines.clone();
    let mut capacity_error = None;
    if let ServiceMode::Replicated { replicas } = requested.mode
        && reservation.is_none()
    {
        let up_to_date = current
            .iter()
            .filter(|container| {
                machines
                    .iter()
                    .any(|machine| machine.machine.id == container.as_observation().machine_id)
                    && PlacementState::keeps_container(container, requested, options, limits)
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
                    && PlacementState::keeps_container(container, requested, options, limits)
            }) || placement.capacity_fits(&machine.machine.id, 1)
        });
        if machines.is_empty() {
            return Err(placement.capacity_error_for(&relevant));
        }
    }
    let volumes = volume_plan.place(requested, machines)?;
    let (service_operations, hook_machine) = match requested.mode {
        ServiceMode::Replicated { .. } => plan_replicated(
            service_id,
            current,
            ReplicatedPlacement {
                volumes,
                admission: match reservation {
                    Some(reservation) => CapacityAdmission::Reserved { reservation },
                    None => CapacityAdmission::Pending {
                        error: capacity_error.expect("unreserved capacity has relevant Machines"),
                        limits,
                    },
                },
            },
            placement,
            options,
        )?,
        ServiceMode::Global => plan_global(
            GlobalPlacement {
                service_id,
                current,
                hooks,
                volumes,
                limits,
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
    namespace: &Namespace,
    snapshot: &'snapshot DeploySnapshot,
    options: &PlanOptions,
) -> Result<Vec<&'snapshot MachineObservation>, PlanError> {
    let candidates = placement_candidates(requested, namespace, snapshot)?;
    let mut unknown = Vec::new();
    let mut machines = Vec::new();
    for machine in &candidates {
        match requested.placement_eligibility_in_namespace(
            namespace,
            &machine.machine,
            machine.storage.as_ref(),
        ) {
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
        if !requested.placement.constraints.is_empty() && candidates.len() == 1 {
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

/// The Machine for one more Container of a running replicated Service: eligible, with a
/// free bridge endpoint and its host ports, holding the fewest of this Service's Containers.
pub(super) fn place_one(
    requested: &RequestedServiceSpec,
    namespace: &Namespace,
    snapshot: &DeploySnapshot,
) -> Result<MachineId, PlanError> {
    let machines = eligible_machines(requested, namespace, snapshot, &PlanOptions::default())?;
    let fitting = machines
        .iter()
        .filter(|machine| placement::fits_one_more(snapshot, machine.machine.id, requested))
        .collect::<Vec<_>>();
    let occupancy = |machine: &MachineObservation| {
        snapshot
            .containers
            .iter()
            .filter(|container| {
                container.kind == ployz_core::ContainerKind::ServiceContainer
                    && container.machine_id == machine.machine.id
                    && container.namespace == *namespace
                    && container.resolved_spec.name == requested.name
            })
            .count()
    };
    if let Some(machine) = fitting.into_iter().min_by_key(|machine| occupancy(machine)) {
        return Ok(machine.machine.id);
    }
    let budget = capacity::CapacityBudget::from_snapshot(snapshot);
    let ids = machines.iter().map(|machine| &machine.machine.id);
    if ids.clone().any(|id| budget.fits(id, 1)) {
        Err(PlanError::HostPortConflict {
            service: requested.name.clone(),
        })
    } else {
        Err(budget.error_for(ids))
    }
}

fn placement_candidates<'snapshot>(
    requested: &RequestedServiceSpec,
    namespace: &Namespace,
    snapshot: &'snapshot DeploySnapshot,
) -> Result<Vec<&'snapshot MachineObservation>, PlanError> {
    let candidates = snapshot
        .machines
        .iter()
        .filter(|machine| machine.invites_rpc())
        // A Machine whose silence is a listing gap would stall a start.
        .filter(|machine| !snapshot.silent(machine))
        .filter(|machine| {
            !matches!(
                requested.placement_eligibility_in_namespace(
                    namespace,
                    &machine.machine,
                    machine.storage.as_ref()
                ),
                ServicePlacementEligibility::Ineligible(
                    ServicePlacementIneligibleReason::PlacementMismatch
                        | ServicePlacementIneligibleReason::WorkNotAccepted
                )
            )
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Err(placement_error(requested, namespace, snapshot));
    }
    Ok(candidates)
}

fn storage_eligibility_warnings(
    requested: &[RequestedServiceSpec],
    namespace: &Namespace,
    snapshot: &DeploySnapshot,
) -> Vec<DeployWarning> {
    let mut warned = BTreeSet::new();
    requested
        .iter()
        .flat_map(|spec| {
            snapshot
                .machines
                .iter()
                .filter(|machine| machine.invites_rpc())
                .filter(move |machine| {
                    matches!(
                        spec.placement_eligibility_in_namespace(
                            namespace,
                            &machine.machine,
                            machine.storage.as_ref()
                        ),
                        ServicePlacementEligibility::Unknown(_)
                    )
                })
        })
        .filter(|machine| warned.insert(machine.machine.id))
        .map(|machine| DeployWarning::StorageObservationUnknown {
            machine_id: machine.machine.id,
            machine_name: Some(machine.machine.name.clone()),
        })
        .collect()
}

fn placement_error(
    spec: &RequestedServiceSpec,
    namespace: &Namespace,
    snapshot: &DeploySnapshot,
) -> PlanError {
    if snapshot.machines.is_empty() {
        return PlanError::no_eligible_machines(vec![EliminatingConstraint::NoMachines]);
    }
    let matched = snapshot
        .machines
        .iter()
        .filter(|machine| machine_matches_placement(&machine.machine, &spec.placement))
        .collect::<Vec<_>>();
    if matched.is_empty() {
        return PlanError::no_eligible_machines(vec![EliminatingConstraint::UnknownPlacement {
            targets: spec.placement.constraints.iter().cloned().collect(),
        }]);
    }
    let mut down = Vec::new();
    let mut silent = Vec::new();
    let mut refused = Vec::new();
    for machine in matched {
        if machine.membership == MembershipObservation::Down {
            down.push(machine.machine.name.clone());
        } else if snapshot.silent(machine) {
            silent.push(machine.machine.name.clone());
        } else if matches!(
            spec.placement_eligibility_in_namespace(
                namespace,
                &machine.machine,
                machine.storage.as_ref()
            ),
            ServicePlacementEligibility::Ineligible(
                ServicePlacementIneligibleReason::WorkNotAccepted
            )
        ) {
            refused.push(machine.machine.name.clone());
        }
    }
    let mut constraints = Vec::new();
    if !down.is_empty() {
        constraints.push(EliminatingConstraint::MachineDown { names: down });
    }
    if !silent.is_empty() {
        constraints.push(EliminatingConstraint::Unanswered { names: silent });
    }
    if !refused.is_empty() {
        constraints.push(EliminatingConstraint::WorkNotAccepted { names: refused });
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
        // Removing a running hook would drain it first, so it waits for a
        // later deploy, after it exits.
        return hooks
            .iter()
            .map(HookContainer::as_observation)
            .filter(|observation| !super::is_active_runtime(&observation.runtime))
            .map(|observation| DeployOperation::RemoveContainer {
                machine_id: observation.machine_id,
                container_id: observation.container_id,
                purpose: ployz_core::RemoveContainerPurpose::Retire,
            })
            .collect();
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
            DeployOperation::RunContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::UpdateContainer(_)
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::PrepareVolumes { .. }
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
