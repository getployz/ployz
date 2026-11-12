use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use futures_util::{
    StreamExt,
    future::{Either, join_all},
    stream::FuturesUnordered,
};
use ployz_core::{
    AnonymousVolumes, ContainerDetails, ContainerId, ContainerKind, ContainerLocator,
    ContainerObservation, ContainerRefusal, ContainerWant, CopyRotation, CreationKey,
    DependencyHealthFailure, DeployEvent, DeployWarning, DeploymentLogId, Drain, ExecutionError,
    ExposedPort, FailedOperation, Gen, HealthFailure, HealthVerdict, MachineAction, MachineId,
    MachineName, Namespace, ObservationKind, OperationPhase, PortPublication, PullPolicy,
    QualifiedService, RemoveContainerPurpose, ResolvedServiceSpec, RpcError, RpcErrorCode,
    ServiceId, ServiceName, ServiceOutcome, SlotKey, StopOptions, TransportProtocol,
};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    DeployOperation, DeployOutcome, ReplacementCompensation, ReplacementOperation, RestoreAttempt,
    StopAttempt,
    schedule::{Action, Event, Schedule, Stage, Task, TaskKind, creates_container},
};

use super::cleanup::{self, ACK_WAIT, RotationFailure};
pub(super) use super::progress::Progress;

mod health;
mod hooks;
mod machine_ops;
mod publish;
mod start;
mod stop_first;
mod transport;

use health::wait_healthy;
use hooks::run_hook;
pub(in crate::deploy) use machine_ops::MachineOperations;
use machine_ops::Unanswered;
pub(crate) use machine_ops::{CopyLifecycle, ProxyPlacement};
#[cfg(test)]
pub(crate) use machine_ops::{RedialFailure, Session};
use publish::{
    Done, GroupCopies, Published, ack_tick, gaps_still_routing, old_copies, publish_group,
    rolling_step, route_new,
};
use start::{Attempt, FailedStart, Row, SlotRun, answer_when, out_of_memory, remove_stray, start};
pub(in crate::deploy) use start::{MoveContainerError, move_container};
use stop_first::{
    leave_unstopped, remove_container, replace_stop_first, restore_group, stop_old_copy, withdraw,
};
pub(crate) use transport::RestartTolerant;

/// How long a Deploy waits on the entry Machine's store for its gen floor.
const STORED_GENS_WAIT: Duration = Duration::from_secs(5);

struct Stop {
    index: usize,
    error: ExecutionError,
    compensation: Option<ReplacementCompensation<ExecutionError>>,
}

fn operation_at(operations: &[DeployOperation], index: usize) -> &DeployOperation {
    operations
        .get(index)
        .expect("schedule indices come from this plan")
}

fn outcome_from(
    operations: &[DeployOperation],
    completed: &BTreeSet<usize>,
    stop: Option<Stop>,
    services: Vec<ServiceOutcome>,
) -> DeployOutcome<ExecutionError> {
    let done = |skip: Option<usize>, wanted: bool| {
        operations
            .iter()
            .enumerate()
            .filter(|(index, _)| Some(*index) != skip && completed.contains(index) == wanted)
            .map(|(_, operation)| operation.clone())
            .collect::<Vec<_>>()
    };
    match stop {
        None if completed.len() == operations.len() => DeployOutcome::Success {
            completed: done(None, true),
            services,
        },
        None => {
            let index = (0..operations.len())
                .find(|index| !completed.contains(index))
                .unwrap_or_default();
            DeployOutcome::Failed {
                completed: done(Some(index), true),
                failed: FailedOperation::Operation {
                    operation: operation_at(operations, index).clone(),
                    error: ExecutionError::Cancelled,
                },
                unexecuted: done(Some(index), false),
                services,
            }
        }
        Some(Stop {
            index,
            error,
            compensation,
        }) => {
            let operation = operation_at(operations, index);
            let failed = match (operation, compensation) {
                (DeployOperation::ReplaceContainer(replacement), Some(compensation)) => {
                    FailedOperation::Replacement {
                        operation: replacement.clone(),
                        error,
                        compensation,
                    }
                }
                (_, _) => FailedOperation::Operation {
                    operation: operation.clone(),
                    error,
                },
            };
            DeployOutcome::Failed {
                completed: done(Some(index), true),
                failed,
                unexecuted: done(Some(index), false),
                services,
            }
        }
    }
}

/// What a completed operation left behind.
#[derive(Default)]
struct Ran {
    /// The new Container it started.
    started: Option<ContainerId>,
    /// The old copy it withdrew and stopped, which a failed Service restores.
    held_old: Option<Box<(ResolvedServiceSpec, cleanup::Copy)>>,
}

impl Ran {
    fn started(id: ContainerId) -> Self {
        Self {
            started: Some(id),
            ..Self::default()
        }
    }
}

#[derive(Clone)]
enum OperationFailure {
    Ordinary(ExecutionError),
    Replacement {
        error: ExecutionError,
        compensation: Box<ReplacementCompensation<ExecutionError>>,
    },
}

impl OperationFailure {
    fn into_parts(
        self,
    ) -> (
        ExecutionError,
        Option<ReplacementCompensation<ExecutionError>>,
    ) {
        match self {
            Self::Ordinary(error) => (error, None),
            Self::Replacement {
                error,
                compensation,
            } => (error, Some(*compensation)),
        }
    }
}

impl From<ExecutionError> for OperationFailure {
    fn from(error: ExecutionError) -> Self {
        Self::Ordinary(error)
    }
}

/// Names the slot each new Container of one Deploy lands on. A resend of the same
/// operation of the same run names the same slot, so a lost reply never doubles a
/// Container; another run or another plan names another slot.
pub(super) struct Slots<'a> {
    plan: String,
    namespace: &'a Namespace,
}

impl<'a> Slots<'a> {
    /// `run` names the attempt: Cloud's deployment id, or a fresh id per CLI run.
    pub(super) fn new(run: &str, namespace: &'a Namespace, operations: &[DeployOperation]) -> Self {
        let digest = Sha256::digest(
            serde_json::to_vec(operations).expect("a Deploy Plan serializes to JSON"),
        );
        let digest = digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Self {
            plan: format!("{run}:{digest}"),
            namespace,
        }
    }

    fn slot(
        &self,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
        index: usize,
        replacing: Option<ContainerId>,
    ) -> SlotKey {
        SlotPolicy::of(kind, &spec.mode).slot(self.namespace, kind, spec, replacing, || {
            format!("{}:{index}", self.plan)
        })
    }
}

/// How a new Container's slot is keyed. A Global Service Container is keyed by
/// its spec, so a retry or another run lands on the Container already there;
/// every other Container is keyed by the run that creates it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotPolicy {
    BySpec,
    PerRun,
}

impl SlotPolicy {
    fn of(kind: ContainerKind, mode: &ployz_core::ServiceMode) -> Self {
        if kind == ContainerKind::ServiceContainer && *mode == ployz_core::ServiceMode::Global {
            Self::BySpec
        } else {
            Self::PerRun
        }
    }

    fn keyed_by_spec(self) -> bool {
        self == Self::BySpec
    }

    /// The slot of a new Container of `spec`. `per_run` names it when the spec does not.
    fn slot(
        self,
        namespace: &Namespace,
        kind: ContainerKind,
        spec: &ResolvedServiceSpec,
        replacing: Option<ContainerId>,
        per_run: impl FnOnce() -> String,
    ) -> SlotKey {
        let key = match self {
            Self::BySpec => {
                let key = crate::cluster::global_creation_key(spec);
                // Explicit replacement can overlap even an identical spec.
                match replacing {
                    Some(old) => format!("{key}:replace:{old}"),
                    None => key,
                }
            }
            Self::PerRun => per_run(),
        };
        SlotKey {
            namespace: namespace.clone(),
            service: spec.name.clone(),
            kind,
            creation_key: CreationKey::try_from(key).expect("creation keys stay within 512 bytes"),
        }
    }
}

pub(super) async fn execute_operation_sequence<C: MachineOperations>(
    plan: &super::DeployPlan,
    run: &DeploymentLogId,
    client: &C,
    cancellation: &CancellationToken,
    tx: Option<UnboundedSender<DeployEvent>>,
) -> DeployOutcome<ExecutionError> {
    // TODO: there is deliberately no persisted "already run" guard at this boundary.
    let operations = plan.operations();
    let slots = Slots::new(run.as_str(), &plan.namespace, operations);
    let progress = Progress::new(plan.pending_rows(), tx);
    progress.emit();
    let client = RestartTolerant::new(client, cancellation);
    let mut completed = BTreeSet::new();
    let tasks = operations
        .iter()
        .map(|operation| Task {
            machine: operation.machine_id(),
            kind: TaskKind::of(operation),
        })
        .collect();
    let mut schedule = Schedule::new(plan.groups().to_vec(), tasks);

    match pull_images(&client, operations).await {
        Ok(warnings) => warnings
            .into_iter()
            .for_each(|warning| progress.warn(warning)),
        Err((index, error)) => return untouched(&progress, &schedule, operations, index, error),
    }

    let rows = Rows::new(plan, run);
    let mut records: BTreeMap<usize, GroupRecord> = BTreeMap::new();
    let mut stop = None;
    let mut blocked_since: Option<Instant> = None;
    let mut blocked_before = Vec::new();
    let (progress, client, slots, rows) = (&progress, &client, &slots, &rows);
    let mut inflight = FuturesUnordered::new();

    loop {
        for action in schedule.next() {
            let next = match action {
                Action::Start(index) => Either::Left(Either::Left(async move {
                    let result = if cancellation.is_cancelled() {
                        Err(ExecutionError::Cancelled.into())
                    } else {
                        execute_operation(
                            operation_at(operations, index),
                            Row { progress, index },
                            client,
                            cancellation,
                            slots,
                            rows,
                        )
                        .await
                    };
                    Done::Task(index, result)
                })),
                Action::Publish(group) => {
                    let service = QualifiedService::new(
                        plan.namespace.clone(),
                        schedule
                            .group(group)
                            .service
                            .clone()
                            .expect("only Service groups publish"),
                    );
                    let new = records
                        .get(&group)
                        .map(|record| record.started.clone())
                        .unwrap_or_default();
                    let old = old_copies(schedule.group(group), operations);
                    Either::Right(async move {
                        let copies = GroupCopies {
                            service: &service,
                            new,
                            old,
                        };
                        let published =
                            publish_group(client, operations, progress, rows, copies, cancellation)
                                .await;
                        Done::Publish(group, published)
                    })
                }
            };
            inflight.push(next);
        }
        let blocked = schedule.waiting_on_routes();
        if blocked != blocked_before {
            blocked_since = None;
            blocked_before.clone_from(&blocked);
        }
        if blocked.is_empty() && inflight.is_empty() {
            break;
        }
        let done = tokio::select! {
            biased;
            done = inflight.next(), if !inflight.is_empty() => done,
            () = ack_tick(cancellation, blocked_since), if !blocked.is_empty() => None,
        };
        match done {
            Some(Done::Task(index, result)) => {
                let group = schedule.group_of(index);
                let operation = operation_at(operations, index);
                let new_container = match &result {
                    Ok(ran) => ran.started,
                    Err(OperationFailure::Ordinary(ExecutionError::Health {
                        container_id,
                        ..
                    })) if creates_container(operation) => Some(*container_id),
                    Err(_) => None,
                };
                let record = records.entry(group).or_default();
                if let Some(id) = new_container {
                    record.started.push((index, operation.machine_id(), id));
                }
                match result {
                    Ok(ran) => {
                        if let Some(old) = ran.held_old {
                            record.stopped.push(*old);
                        }
                        if let (true, Some(container_id)) =
                            (schedule.group(group).publish, ran.started)
                        {
                            progress.set_running(
                                index,
                                OperationPhase::WaitingForSwitch { container_id },
                            );
                            record.awaiting_flip.push(index);
                        } else {
                            completed.insert(index);
                            progress.set_completed(index);
                        }
                        let service = schedule.group(group).publishing_service().map(|service| {
                            QualifiedService::new(plan.namespace.clone(), service.clone())
                        });
                        if let Some((service, new)) = service
                            .filter(|service| *service != QualifiedService::system_ingress())
                            .zip(rolling_step(operation, ran.started))
                        {
                            inflight.push(Either::Left(Either::Right(async move {
                                let new = [(index, new.0, new.1)];
                                match route_new(
                                    client,
                                    operations,
                                    rows,
                                    &service,
                                    &new,
                                    cancellation,
                                )
                                .await
                                {
                                    Ok(_) => Done::Admitted(index),
                                    Err(published) => Done::Publish(group, published),
                                }
                            })));
                        } else {
                            schedule.apply(Event::Completed(index));
                        }
                    }
                    Err(failure) => {
                        let (error, compensation) = failure.into_parts();
                        Ledger::new(progress, &mut stop, &mut schedule).fail(
                            group,
                            index,
                            error,
                            compensation,
                            Event::Failed(index),
                        );
                    }
                }
            }
            Some(Done::Admitted(index)) => schedule.apply(Event::Completed(index)),
            Some(Done::Publish(group, published)) => {
                let service = schedule
                    .group(group)
                    .service
                    .clone()
                    .expect("only Service groups publish");
                let last = schedule.publish_failure_task(group);
                let record = records.entry(group).or_default();
                let mut ledger = Ledger::new(progress, &mut stop, &mut schedule);
                match published {
                    Published::Listed => {
                        for index in std::mem::take(&mut record.awaiting_flip) {
                            completed.insert(index);
                            progress.set_completed(index);
                        }
                        ledger.schedule.apply(Event::Routed(service));
                        ledger.schedule.apply(Event::Published(group));
                    }
                    // Its old copies were left In beside the new ones and named.
                    Published::Gapped => {
                        for index in std::mem::take(&mut record.awaiting_flip) {
                            completed.insert(index);
                            progress.set_completed(index);
                        }
                        ledger.schedule.apply(Event::Routed(service));
                        ledger.schedule.apply(Event::PublishGapped(group));
                    }
                    Published::Unconfirmed(machines) => {
                        record.unread = true;
                        let error = machine_error(
                            MachineAction::ReadRoutes,
                            RpcError::unavailable(rows.not_confirmed(&machines)),
                        );
                        ledger.fail(group, last, error, None, Event::PublishUnconfirmed(group));
                    }
                    // The new copies serve and the old ones went Out: none
                    // is removed or brought back.
                    Published::OutUnconfirmed(error) => {
                        record.unread = true;
                        record.forget_stopped();
                        ledger.fail(group, last, error, None, Event::PublishUnconfirmed(group));
                    }
                    Published::Cancelled => ledger.fail(
                        group,
                        last,
                        ExecutionError::Cancelled,
                        None,
                        Event::PublishFailed(group),
                    ),
                    Published::CopyFailed { task, error } => {
                        ledger.fail(group, task, error, None, Event::PublishFailed(group));
                    }
                    Published::Failed(error) => {
                        ledger.fail(group, last, error, None, Event::PublishFailed(group));
                    }
                }
            }
            None => {
                let expired = blocked_since.is_some_and(|since| since.elapsed() >= ACK_WAIT);
                if cancellation.is_cancelled() || expired {
                    let mut failed = BTreeSet::new();
                    for (task, dependency) in blocked {
                        if !failed.insert(task) {
                            continue;
                        }
                        let error = if cancellation.is_cancelled() {
                            ExecutionError::Cancelled
                        } else {
                            ExecutionError::DependencyHealth {
                                dependency: QualifiedService::new(
                                    plan.namespace.clone(),
                                    dependency,
                                ),
                                failure: DependencyHealthFailure::NotRouted {
                                    machine_id: operation_at(operations, task).machine_id(),
                                },
                            }
                        };
                        let group = schedule.group_of(task);
                        Ledger::new(progress, &mut stop, &mut schedule).fail(
                            group,
                            task,
                            error,
                            None,
                            Event::Failed(task),
                        );
                    }
                } else {
                    blocked_since.get_or_insert_with(Instant::now);
                }
            }
        }
        for group in schedule.failed_groups() {
            rows.restore_unclaimed(client, progress, |task| schedule.group_of(task) == group)
                .await;
            let service = schedule
                .group(group)
                .service
                .clone()
                .map(|name| QualifiedService::new(plan.namespace.clone(), name));
            let record = records.remove(&group).unwrap_or_default();
            restore_group(client, progress, service.as_ref(), record, rows).await;
        }
    }

    progress.settle_unexecuted();
    let outcome = outcome_from(operations, &completed, stop, schedule.services());
    progress.outcome(outcome.clone());
    outcome
}

/// Where the loop records that a task or a publish of a group failed: the row,
/// the Deploy's first failure, and the Schedule's phase for the group.
struct Ledger<'a> {
    progress: &'a Progress,
    stop: &'a mut Option<Stop>,
    schedule: &'a mut Schedule,
}

impl<'a> Ledger<'a> {
    fn new(progress: &'a Progress, stop: &'a mut Option<Stop>, schedule: &'a mut Schedule) -> Self {
        Self {
            progress,
            stop,
            schedule,
        }
    }

    /// `task` of `group` failed with `error`. A publish names the group itself,
    /// since the row it reports under can belong to the group after it.
    fn fail(
        &mut self,
        group: usize,
        task: usize,
        error: ExecutionError,
        compensation: Option<ReplacementCompensation<ExecutionError>>,
        event: Event,
    ) {
        if lost_to_newer(&error, compensation.as_ref()) {
            self.schedule.apply(Event::NewerServing(group));
        }
        self.progress.fail(task, error.clone());
        self.stop.get_or_insert(Stop {
            index: task,
            error,
            compensation,
        });
        self.schedule.apply(event);
    }
}

/// What one group's tasks left that its cleanup has to undo or finish.
#[derive(Default)]
struct GroupRecord {
    /// The new Containers its tasks started, with their tasks and Machines.
    started: Vec<(usize, MachineId, ContainerId)>,
    /// The old copies its tasks withdrew and stopped.
    stopped: Vec<(ResolvedServiceSpec, cleanup::Copy)>,
    /// The tasks that finished and wait for the group's flip to complete.
    awaiting_flip: Vec<usize>,
    /// The publish ended with its routes unread, so the new copies may serve.
    unread: bool,
}

impl GroupRecord {
    /// A restored copy is no longer stopped.
    fn forget_stopped(&mut self) {
        self.stopped.clear();
    }
}

fn lost_to_newer(
    error: &ExecutionError,
    compensation: Option<&ReplacementCompensation<ExecutionError>>,
) -> bool {
    matches!(error, ExecutionError::NewerServing { .. })
        || matches!(
            compensation,
            Some(ReplacementCompensation::OldStopped {
                restore_old_container: RestoreAttempt::NewerServing,
                ..
            })
        )
}

fn left_by(error: ExecutionError) -> Left {
    match error {
        ExecutionError::Machine { error, .. } => Left::Failed(error),
        error @ (ExecutionError::Health { .. }
        | ExecutionError::DependencyHealth { .. }
        | ExecutionError::Hook { .. }
        | ExecutionError::NewerServing { .. }
        | ExecutionError::Cancelled) => {
            Left::Failed(RpcError::caused(RpcErrorCode::Internal, &error))
        }
    }
}

/// The Deploy ends at `index` before any Container changed.
fn untouched(
    progress: &Progress,
    schedule: &Schedule,
    operations: &[DeployOperation],
    index: usize,
    error: ExecutionError,
) -> DeployOutcome<ExecutionError> {
    progress.fail(index, error.clone());
    progress.settle_unexecuted();
    let outcome = outcome_from(
        operations,
        &BTreeSet::new(),
        Some(Stop {
            index,
            error,
            compensation: None,
        }),
        schedule.services(),
    );
    progress.outcome(outcome.clone());
    outcome
}

/// Pull every image the plan runs, then warn about each Service whose image exposes
/// TCP ports none of its domains send to.
async fn pull_images<C: MachineOperations>(
    client: &C,
    operations: &[DeployOperation],
) -> Result<Vec<DeployWarning>, (usize, ExecutionError)> {
    // One pull per image on each Machine, under the strongest policy any of its
    // Services asks for: what `Always` pulls also satisfies `Missing` and `Never`.
    let mut pulls: BTreeMap<(MachineId, &str), (usize, &ResolvedServiceSpec)> = BTreeMap::new();
    for (index, operation) in operations.iter().enumerate() {
        let Some(spec) = operation.spec() else {
            continue;
        };
        let pull = pulls
            .entry((operation.machine_id(), spec.container.image.as_str()))
            .or_insert((index, spec));
        if strength(spec.container.pull_policy) > strength(pull.1.container.pull_policy) {
            *pull = (index, spec);
        }
    }
    let results = join_all(
        pulls
            .iter()
            .map(|((machine, _), (index, spec))| async move {
                client
                    .ensure_image(machine, spec)
                    .await
                    .map_err(|error| (*index, machine_error(MachineAction::PullImage, error)))
            }),
    )
    .await;
    let mut exposed = BTreeMap::new();
    let mut failure: Option<(usize, ExecutionError)> = None;
    for (key, result) in pulls.keys().zip(results) {
        match result {
            Ok(pulled) => {
                exposed.insert(*key, pulled.exposed_ports);
            }
            Err(error) => {
                if failure.as_ref().is_none_or(|(first, _)| error.0 < *first) {
                    failure = Some(error);
                }
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(port_mismatches(operations, &exposed)),
    }
}

/// One warning per Service that some Machine pulled an image for whose TCP ports
/// its domains send to none of. `exposed` joins the ports of those Machines only.
/// A Machine that reports no ports claims nothing.
fn port_mismatches(
    operations: &[DeployOperation],
    exposed: &BTreeMap<(MachineId, &str), Vec<ExposedPort>>,
) -> Vec<DeployWarning> {
    struct Mismatch<'a> {
        service_id: ServiceId,
        image: &'a str,
        machines: BTreeSet<MachineId>,
        exposed: BTreeSet<u16>,
        routed: BTreeSet<u16>,
    }
    let mut services: BTreeMap<&ServiceName, Mismatch<'_>> = BTreeMap::new();
    for operation in operations {
        let Some(spec) = operation.spec() else {
            continue;
        };
        let image = spec.container.image.as_str();
        let machine_id = operation.machine_id();
        let tcp = exposed
            .get(&(machine_id, image))
            .into_iter()
            .flatten()
            .filter(|port| port.protocol == TransportProtocol::Tcp)
            .map(|port| port.port.get())
            .collect::<BTreeSet<_>>();
        let routed = spec
            .ports
            .iter()
            .filter_map(|port| match port {
                PortPublication::Ingress { container_port, .. } => Some(container_port.get()),
                PortPublication::Host { .. } => None,
            })
            .collect::<BTreeSet<_>>();
        if tcp.is_empty() || routed.is_empty() || !tcp.is_disjoint(&routed) {
            continue;
        }
        let mismatch = services.entry(&spec.name).or_insert_with(|| Mismatch {
            service_id: spec.service_id,
            image,
            machines: BTreeSet::new(),
            exposed: BTreeSet::new(),
            routed: BTreeSet::new(),
        });
        mismatch.machines.insert(machine_id);
        mismatch.exposed.extend(tcp);
        mismatch.routed.extend(routed);
    }
    services
        .into_iter()
        .map(|(service, mismatch)| DeployWarning::ImagePortMismatch {
            service: service.clone(),
            service_id: mismatch.service_id,
            image: mismatch.image.to_owned(),
            machines: mismatch.machines.into_iter().collect(),
            exposed: mismatch.exposed.into_iter().collect(),
            routed: mismatch.routed.into_iter().collect(),
        })
        .collect()
}

fn strength(policy: PullPolicy) -> u8 {
    match policy {
        PullPolicy::Never => 0,
        PullPolicy::Missing => 1,
        PullPolicy::Always => 2,
    }
}

struct Rows<'a> {
    namespace: &'a Namespace,
    /// Each Service whose publish ended with only gap Servers' proxies unconfirmed.
    /// Those proxies may still route any of its old copies, so none is retired.
    gapped: std::sync::Mutex<BTreeMap<QualifiedService, cleanup::Unconfirmed>>,
    run: &'a DeploymentLogId,
    names: BTreeMap<MachineId, MachineName>,
    /// The Service each rollout removal retires a copy of, so one that cannot
    /// inspect its copy can still name it.
    services: BTreeMap<usize, &'a ServiceName>,
    /// The removals of copies ended runs left Pending, with each copy's Service.
    strays: &'a BTreeMap<usize, ServiceName>,
    gens: std::sync::Mutex<
        BTreeMap<QualifiedService, std::sync::Arc<tokio::sync::OnceCell<Numbered>>>,
    >,
    stored: tokio::sync::OnceCell<Result<Vec<ContainerObservation>, RpcError>>,
    /// Machines whose Containers the plan's snapshot did not list.
    unobserved: BTreeSet<MachineId>,
    /// The Services this Deploy takes out wherever they run.
    retiring: &'a BTreeSet<QualifiedService>,
    /// Machines the plan's listing found unanswered over a path dropping large packets.
    dropped_large_packets: &'a BTreeSet<MachineId>,
    /// Copies on those Machines a removal already left to the next full Deploy,
    /// so a Service losing several live copies names each one once.
    unreached: std::sync::Mutex<BTreeSet<ContainerId>>,
    stop_first: Vec<StopFirstStep<'a>>,
}

struct StopFirstStep<'a> {
    tasks: Vec<(usize, &'a ployz_core::ReplacementOperation)>,
    ready: tokio::sync::OnceCell<Inspected>,
}

struct Inspected {
    old: BTreeMap<usize, Result<Option<ContainerObservation>, ExecutionError>>,
    /// A withdrawal that failed, so no task of the step creates a copy.
    failed: Option<ExecutionError>,
    withdrawn: std::sync::Mutex<BTreeMap<usize, StepCopy>>,
}

/// An old copy the step withdrew before any of its tasks created a copy:
/// a new copy left Pending would keep every proxy routing the copies not
/// yet withdrawn.
enum StepCopy {
    /// The copy and the spec it ran.
    Unclaimed(cleanup::Copy, Box<ResolvedServiceSpec>),
    /// The Service failed before the task claimed it, so it was brought back.
    Restored,
}

enum Claim {
    Withdrawn(cleanup::Copy),
    Restored,
    /// Not withdrawn for a step: the task withdraws it itself.
    Own,
}

impl<'a> Rows<'a> {
    fn new(plan: &'a super::DeployPlan, run: &'a DeploymentLogId) -> Self {
        let operations = plan.operations();
        let stop_first = plan
            .groups()
            .iter()
            .flat_map(|group| &group.steps)
            .map(|step| {
                step.tasks()
                    .iter()
                    .copied()
                    .filter_map(|index| {
                        let DeployOperation::ReplaceContainer(replacement) =
                            operation_at(operations, index)
                        else {
                            return None;
                        };
                        replacement
                            .spec
                            .update
                            .order
                            .stops_first()
                            .then_some((index, replacement))
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|tasks| !tasks.is_empty())
            .map(|tasks| StopFirstStep {
                tasks,
                ready: tokio::sync::OnceCell::new(),
            })
            .collect();
        let names = plan.machine_names().clone();
        // A rollout's removals run in a group of their own that depends on just
        // the Service they roll out.
        let services = plan
            .groups()
            .iter()
            .filter_map(|group| match group.dependencies.as_slice() {
                [service] if group.service.is_none() => Some((service, &group.steps)),
                _ => None,
            })
            .flat_map(|(service, steps)| {
                steps
                    .iter()
                    .flat_map(Stage::tasks)
                    .map(move |index| (*index, service))
            })
            .filter(|(index, _)| {
                matches!(
                    operation_at(operations, *index),
                    DeployOperation::RemoveContainer {
                        purpose: RemoveContainerPurpose::Rollout,
                        ..
                    }
                )
            })
            .collect();
        Self {
            namespace: &plan.namespace,
            gapped: std::sync::Mutex::new(BTreeMap::new()),
            run,
            names,
            services,
            gens: std::sync::Mutex::new(BTreeMap::new()),
            stored: tokio::sync::OnceCell::new(),
            unobserved: plan
                .preview()
                .warnings
                .iter()
                .filter_map(|warning| {
                    let (DeployWarning::ObservationFailed {
                        kind: ObservationKind::Container,
                        machine_id,
                        ..
                    }
                    | DeployWarning::ObservationOmitted {
                        kind: ObservationKind::Container,
                        machine_id,
                        ..
                    }) = warning
                    else {
                        return None;
                    };
                    Some(*machine_id)
                })
                .collect(),
            retiring: plan.retiring(),
            strays: plan.strays(),
            dropped_large_packets: plan.dropped_large_packets(),
            unreached: std::sync::Mutex::new(BTreeSet::new()),
            stop_first,
        }
    }

    async fn inspect_step<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        index: usize,
        cancellation: &CancellationToken,
    ) -> Result<Option<ContainerObservation>, ExecutionError> {
        let step = self
            .stop_first
            .iter()
            .find(|step| step.tasks.iter().any(|(task, _)| *task == index))
            .expect("stop-first tasks belong to a step");
        let inspected = step
            .ready
            .get_or_init(|| async {
                let mut old = BTreeMap::new();
                for (task, operation) in &step.tasks {
                    let observed =
                        match inspect(client, &operation.machine_id, &operation.old_container_id)
                            .await
                        {
                            Ok(details) => Ok(Some(details.container)),
                            Err(ExecutionError::Machine { error, .. })
                                if error.code == RpcErrorCode::NotFound =>
                            {
                                Ok(None)
                            }
                            Err(error) => Err(error),
                        };
                    old.insert(*task, observed);
                }
                self.withdraw_step(client, progress, old, cancellation)
                    .await
            })
            .await;
        if let Some(error) = &inspected.failed {
            return Err(error.clone());
        }
        inspected
            .old
            .get(&index)
            .cloned()
            .expect("every task of the step was inspected")
    }

    /// Withdraw every old copy of a stop-first step together. If one is not
    /// withdrawn, bring back each one this run Held, before any copy starts.
    async fn withdraw_step<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        mut old: BTreeMap<usize, Result<Option<ContainerObservation>, ExecutionError>>,
        cancellation: &CancellationToken,
    ) -> Inspected {
        // A dead run's Pending copies were never routed, so when the step
        // withdraws a routed copy, they go first with no proxy wait.
        let withdraws = old
            .values()
            .filter_map(|observed| observed.as_ref().ok()?.as_ref())
            .any(|observed| {
                !crate::ingress::is_system_ingress(observed)
                    && matches!(
                        observed.rotation(),
                        CopyRotation::In | CopyRotation::Held(_)
                    )
            });
        let pending = join_all(
            old.iter()
                .filter(|_| withdraws)
                .filter_map(|(task, observed)| Some((*task, observed.as_ref().ok()?.as_ref()?)))
                .filter(|(_, observed)| {
                    *observed.rotation() == CopyRotation::Pending
                        && !crate::ingress::is_system_ingress(observed)
                })
                .map(|(task, observed)| async move {
                    let copy = cleanup::Copy::observed(observed);
                    let removed = cleanup::remove(client, &copy, AnonymousVolumes::Remove).await;
                    (task, removed)
                }),
        )
        .await;
        for (task, removed) in pending {
            let error = match removed {
                Ok(cleanup::Removal::Removed) => {
                    old.insert(task, Ok(None));
                    continue;
                }
                Ok(cleanup::Removal::Refused(refusal)) => {
                    machine_error(MachineAction::RemoveContainer, refusal.into_error())
                }
                Err(error) => machine_error(MachineAction::RemoveContainer, error),
            };
            return Inspected {
                old,
                failed: Some(error),
                withdrawn: std::sync::Mutex::new(BTreeMap::new()),
            };
        }
        let withdrawals = join_all(old.iter().filter_map(|(task, observed)| {
            let observed = observed.as_ref().ok()?.as_ref()?;
            Some(async move {
                let (copy, step) = withdraw(client, observed, self, cancellation).await?;
                Some((*task, observed, copy, step))
            })
        }))
        .await;
        let mut failed = None;
        let mut withdrawn = Vec::new();
        for (task, observed, mut copy, step) in withdrawals.into_iter().flatten() {
            let (left, error) = match step.map(cleanup::Rotated::failure) {
                Ok(Ok(())) => (None, None),
                Ok(Err(failure)) => {
                    // A refusal came before the Machine acted, so the copy is as observed.
                    if matches!(failure, RotationFailure::Refused(_)) {
                        copy.rotation = observed.rotation().clone();
                    }
                    let (left, error) = self.failed(failure, MachineAction::StopContainer);
                    (Some(left), Some(error))
                }
                Err(error) => (
                    Some(Left::Failed(error.clone())),
                    Some(machine_error(MachineAction::StopContainer, error)),
                ),
            };
            if let Some(error) = error {
                failed.get_or_insert(error);
            }
            withdrawn.push((task, observed, copy, left));
        }
        if failed.is_some() {
            join_all(withdrawn.iter().map(|(_, observed, copy, left)| {
                let left = left.clone().unwrap_or(Left::Interrupted);
                leave_unstopped(client, progress, copy, &observed.resolved_spec, self, left)
            }))
            .await;
            withdrawn.clear();
        }
        let withdrawn = withdrawn
            .into_iter()
            .map(|(task, observed, copy, _)| {
                (
                    task,
                    StepCopy::Unclaimed(copy, Box::new(observed.resolved_spec.clone())),
                )
            })
            .collect();
        Inspected {
            old,
            failed,
            withdrawn: std::sync::Mutex::new(withdrawn),
        }
    }

    fn claim(&self, index: usize) -> Claim {
        let Some(inspected) = self
            .stop_first
            .iter()
            .find(|step| step.tasks.iter().any(|(task, _)| *task == index))
            .and_then(|step| step.ready.get())
        else {
            return Claim::Own;
        };
        let mut withdrawn = inspected.withdrawn.lock().expect("withdrawn copies lock");
        match withdrawn.remove(&index) {
            Some(StepCopy::Unclaimed(copy, _)) => Claim::Withdrawn(copy),
            Some(StepCopy::Restored) => {
                withdrawn.insert(index, StepCopy::Restored);
                Claim::Restored
            }
            None => Claim::Own,
        }
    }

    /// Bring back each old copy a failed group's steps withdrew that no task claimed.
    async fn restore_unclaimed<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        in_group: impl Fn(usize) -> bool,
    ) {
        let mut unclaimed = Vec::new();
        for step in &self.stop_first {
            let Some(inspected) = step.ready.get() else {
                continue;
            };
            let mut withdrawn = inspected.withdrawn.lock().expect("withdrawn copies lock");
            for (task, entry) in withdrawn.iter_mut() {
                if !in_group(*task) {
                    continue;
                }
                if let StepCopy::Unclaimed(copy, spec) =
                    std::mem::replace(entry, StepCopy::Restored)
                {
                    unclaimed.push((copy, spec));
                }
            }
        }
        join_all(unclaimed.iter().map(|(copy, spec)| {
            leave_unstopped(client, progress, copy, spec, self, Left::Interrupted)
        }))
        .await;
    }

    fn service(&self, spec: &ResolvedServiceSpec) -> QualifiedService {
        QualifiedService::new(self.namespace.clone(), spec.name.clone())
    }

    /// The gen a new copy of `service` on `destination` takes. A Machine that left the
    /// listing unanswered is a gap the Deploy goes on without, never a destination.
    async fn gen_for<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        service: &QualifiedService,
        destination: &MachineId,
        cancellation: &CancellationToken,
    ) -> Result<Gen, ExecutionError> {
        let numbered = self
            .numbered(client, progress, service, cancellation)
            .await?;
        match numbered
            .silent
            .iter()
            .find(|silent| silent.failure.machine_id == *destination)
        {
            Some(silent) => Err(machine_error(
                MachineAction::ListContainers,
                naming_machine(
                    crate::cluster::unanswered_by(&silent.name, silent.failure.error.clone()),
                    destination,
                ),
            )),
            None => Ok(numbered.r#gen),
        }
    }

    async fn numbered<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        service: &QualifiedService,
        cancellation: &CancellationToken,
    ) -> Result<Numbered, ExecutionError> {
        // One cell per Service, so a silent Machine's deadline is paid by each
        // Service at once rather than one Service after another.
        let cell = std::sync::Arc::clone(
            self.gens
                .lock()
                .expect("gens lock")
                .entry(service.clone())
                .or_default(),
        );
        cell.get_or_try_init(|| async {
            let listed = client
                .listed_copies(service)
                .await
                .map_err(|error| machine_error(MachineAction::ListContainers, error))?;
            for silent in &listed.unanswered {
                silent.warn(progress);
            }
            // A silent Machine's copies count only through the store's rows, so a new
            // copy that could not be numbered above them would never replace them.
            let stored = match (
                self.stored(client, cancellation).await?,
                listed.unanswered.first(),
            ) {
                (Ok(stored), _) => stored.as_slice(),
                (Err(_), None) => &[],
                (Err(unread), Some(silent)) => return Err(unfloored(silent, unread)),
            };
            let highest = listed
                .copies
                .iter()
                .filter(|copy| copy.kind == ContainerKind::ServiceContainer)
                .chain(stored.iter().filter(|copy| copy.identity() == *service))
                .map(|copy| copy.copy_gen())
                .max();
            Ok(Numbered {
                r#gen: highest.map_or(Gen::FIRST, |highest| Gen::new(highest.get() + 1)),
                silent: std::sync::Arc::new(listed.unanswered),
            })
        })
        .await
        .cloned()
    }

    /// What a removal knows of `service`'s copies on the Machines this Deploy did
    /// not observe. Every proxy reads the entry Machine's store too, so while a row
    /// there is In or Pending, a copy sent Out beside it stays routed and its drain
    /// never confirms. Only a Service this Deploy removes must reach them. One it
    /// keeps owns its other copies, so they are [`Unreached::None`] to its cleanup.
    async fn unreached<C: MachineOperations>(
        &self,
        client: &C,
        service: &QualifiedService,
        cancellation: &CancellationToken,
    ) -> Result<Unreached, ExecutionError> {
        if self.unobserved.is_empty() || !self.retiring.contains(service) {
            return Ok(Unreached::None);
        }
        let Ok(stored) = self.stored(client, cancellation).await? else {
            return Ok(Unreached::Unknown);
        };
        let copies: Vec<_> = stored
            .iter()
            .filter(|copy| {
                copy.identity() == *service
                    && self.unobserved.contains(&copy.machine_id)
                    && copy.rotation().keeps_route()
            })
            .map(cleanup::Copy::observed)
            .collect();
        Ok(if copies.is_empty() {
            Unreached::None
        } else {
            Unreached::Copies(copies)
        })
    }

    /// Names each copy a removal leaves on a Machine it could not reach, once.
    fn leave_unreached(&self, progress: &Progress, copies: &[cleanup::Copy]) {
        let mut named = self.unreached.lock().expect("unreached copies lock");
        for copy in copies {
            if named.insert(copy.container_id) {
                self.leave_running(progress, copy, Left::Unreached);
            }
        }
    }

    /// The Service Containers in the entry Machine's store, read once per Deploy,
    /// or why the read failed or outlasted [`STORED_GENS_WAIT`]. The store keeps the
    /// rows of a Machine that did not answer, and a new copy at or below one of those
    /// gens is refused In or never replaces it. Without the read, gens and removals
    /// follow the live answers.
    async fn stored<C: MachineOperations>(
        &self,
        client: &C,
        cancellation: &CancellationToken,
    ) -> Result<&Result<Vec<ContainerObservation>, RpcError>, ExecutionError> {
        self.stored
            .get_or_try_init(|| async {
                let read = tokio::select! {
                    biased;
                    () = cancellation.cancelled() => return Err(ExecutionError::Cancelled),
                    read = tokio::time::timeout(STORED_GENS_WAIT, client.replicated_containers()) => read,
                };
                let containers = match read {
                    Ok(Ok(containers)) => containers,
                    Ok(Err(error)) => {
                        tracing::warn!(
                            code = error.code.as_str(),
                            error = error.message.as_str(),
                            "couldn't read the entry Machine's store; new gens follow the live answers only"
                        );
                        return Ok(Err(error));
                    }
                    Err(_) => {
                        tracing::warn!(
                            wait = ?STORED_GENS_WAIT,
                            "the entry Machine's store didn't answer in time; new gens follow the live answers only"
                        );
                        return Ok(Err(RpcError::unavailable(format!("the store didn't answer in {STORED_GENS_WAIT:?}"))));
                    }
                };
                Ok(Ok(containers
                    .into_iter()
                    .filter(|copy| copy.kind == ContainerKind::ServiceContainer)
                    .collect()))
            })
            .await
    }

    /// What a rotation that did not finish leaves its copy as, and the Deploy's error
    /// for `action`.
    fn failed(&self, failure: RotationFailure, action: MachineAction) -> (Left, ExecutionError) {
        let error = match &failure {
            RotationFailure::Refused(refusal) => {
                machine_error(action, refusal.clone().into_error())
            }
            RotationFailure::NotConfirmed(unconfirmed) => machine_error(
                action,
                RpcError::unavailable(self.not_confirmed(unconfirmed)),
            ),
            RotationFailure::Cancelled => ExecutionError::Cancelled,
        };
        (Left::from(failure), error)
    }

    /// The Machines `service`'s listing left unanswered whose proxy acks nothing, so
    /// no publish waits on them. A Suspect Machine still turns Down, and one behind a
    /// path dropping large packets still acks.
    fn gaps(&self, service: &QualifiedService) -> BTreeSet<MachineId> {
        let cell = self.gens.lock().expect("gens lock").get(service).cloned();
        cell.and_then(|cell| {
            cell.get().map(|numbered| {
                numbered
                    .silent
                    .iter()
                    .filter(|silent| silent.acks_unwritten)
                    .map(|silent| silent.failure.machine_id)
                    .collect()
            })
        })
        .unwrap_or_default()
    }

    /// The gap Servers, as in [`Self::gaps`], whose proxy may still route `copy`, as
    /// in [`gaps_still_routing`]. None when `copy`'s Service could not be listed, so the
    /// removal goes on as it would without a gap.
    async fn gaps_routing<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        copy: &cleanup::Copy,
        cancellation: &CancellationToken,
    ) -> Option<cleanup::Unconfirmed> {
        let numbered = self
            .numbered(client, progress, &copy.service, cancellation)
            .await
            .ok()?;
        let gaps = numbered
            .silent
            .iter()
            .filter(|silent| silent.acks_unwritten)
            .map(|silent| silent.failure.machine_id);
        gaps_still_routing(client, copy, gaps).await
    }

    /// The proxies that never confirmed `service`'s publish, when only gap Servers'
    /// did not.
    fn gapped(&self, service: &QualifiedService) -> Option<cleanup::Unconfirmed> {
        self.gapped
            .lock()
            .expect("gapped lock")
            .get(service)
            .cloned()
    }

    fn not_confirmed(&self, unconfirmed: &cleanup::Unconfirmed) -> String {
        // Every proxy in the read confirmed, but the read did not name every live Machine.
        if unconfirmed.machines.is_empty() {
            return "the route set did not name every live Machine".into();
        }
        let names: Vec<String> = unconfirmed
            .machines
            .iter()
            .map(|id| self.machine_name(id))
            .collect();
        let causes: String = unconfirmed
            .machines
            .iter()
            .filter(|id| self.dropped_large_packets.contains(id))
            .map(|id| crate::deploy::dropped_note(self.machine_name(id)))
            .collect();
        format!("proxy on {} did not confirm{causes}", names.join(", "))
    }

    fn machine_name(&self, id: &MachineId) -> String {
        self.names
            .get(id)
            .map_or_else(|| id.to_string(), |name| name.to_string())
    }

    /// A copy this run couldn't remove that never went In: its own new one, or one an
    /// ended run left. No proxy routes it once its Machine is back, and the next
    /// Deploy removes it.
    fn leave_unrouted(
        &self,
        progress: &Progress,
        service: &QualifiedService,
        machine_id: MachineId,
        container_id: ContainerId,
        error: RpcError,
    ) {
        let unanswered = error.code == RpcErrorCode::Unavailable;
        let (reason, _) = self.failed_on(machine_id, error, unanswered);
        self.name_left(progress, service, machine_id, container_id, reason);
    }

    /// A copy this run couldn't remove, for `reason`, which keeps running.
    fn name_left(
        &self,
        progress: &Progress,
        service: &QualifiedService,
        machine_id: MachineId,
        container_id: ContainerId,
        reason: String,
    ) {
        progress.warn(DeployWarning::LeftRunning {
            service: service.clone(),
            container_id,
            machine: self.machine_name(&machine_id),
            reason,
        });
    }

    /// A Machine that was cut off or powered off can route the copy again once
    /// it is back, so the report names each one.
    fn leave_running(&self, progress: &Progress, copy: &cleanup::Copy, left: Left) {
        let (reason, away) = self.explain(copy, left);
        progress.warn(DeployWarning::LeftRunning {
            service: copy.service.clone(),
            container_id: copy.container_id,
            machine: self.machine_name(&copy.machine_id),
            reason,
        });
        for machine in away {
            progress.warn(DeployWarning::MixedAfterReturn {
                machine: self.machine_name(&machine),
            });
        }
    }

    fn explain(&self, copy: &cleanup::Copy, left: Left) -> (String, Vec<MachineId>) {
        match left {
            Left::NotConfirmed(unconfirmed) => {
                (self.not_confirmed(&unconfirmed), unconfirmed.unanswered)
            }
            Left::Interrupted => ("interrupted".into(), Vec::new()),
            Left::Unread(error) => (
                format!(
                    "the routes could not be read through the Machine this Deploy connects to: {}",
                    error.message
                ),
                Vec::new(),
            ),
            Left::DeployFailed => ("the Deploy failed".into(), Vec::new()),
            Left::Unreached => (
                format!(
                    "{} didn't answer, the next full deploy removes its copy once it is back",
                    self.machine_name(&copy.machine_id)
                ),
                Vec::new(),
            ),
            Left::NotListed => (
                format!(
                    "{} did not see the new copies listed",
                    self.machine_name(&copy.machine_id)
                ),
                Vec::new(),
            ),
            Left::Refused(refusal) => (refusal.message, Vec::new()),
            Left::Failed(error) => {
                let unanswered = error.code == RpcErrorCode::Unavailable;
                self.failed_on(copy.machine_id, error, unanswered)
            }
            Left::NotRestored(left, why) => {
                let (reason, away) = self.explain(copy, *left);
                (
                    format!("{reason}, and it could not go back In: {why}"),
                    away,
                )
            }
            Left::OlderNotRemoved(left) => {
                let (reason, away) = self.explain(copy, *left);
                let newer = ExecutionError::NewerServing {
                    service: copy.service.clone(),
                };
                (format!("{newer}, and removing it failed: {reason}"), away)
            }
        }
    }

    /// A call the client saw go unanswered names its target as away. So does an
    /// unavailable call to the copy's own owner, which covers this client's own
    /// deadline. Any other error says nothing about which Machine is cut off.
    fn failed_on(
        &self,
        machine_id: MachineId,
        error: RpcError,
        unanswered: bool,
    ) -> (String, Vec<MachineId>) {
        if unanswered || error.target_unanswered() {
            (
                format!("{} did not answer", self.machine_name(&machine_id)),
                vec![machine_id],
            )
        } else {
            (error.message, Vec::new())
        }
    }
}

/// A removal's copies of its Service on the Machines this Deploy did not observe.
enum Unreached {
    /// None keeps the route: every Machine was observed, the store lists no In or
    /// Pending copy there, or the Service stays and its other copies are its own.
    None,
    /// The entry Machine's store lists these there, each In or Pending.
    Copies(Vec<cleanup::Copy>),
    /// The store could not be read, so a copy there may keep the route.
    Unknown,
}

impl Unreached {
    /// Whether a proxy may keep routing the Service once its live copy is Out, so
    /// that the drain could never confirm.
    fn may_keep_route(&self) -> bool {
        !matches!(self, Self::None)
    }

    fn copies(&self) -> &[cleanup::Copy] {
        match self {
            Self::Copies(copies) => copies,
            Self::None | Self::Unknown => &[],
        }
    }
}

#[derive(Clone)]
enum Left {
    NotConfirmed(cleanup::Unconfirmed),
    Interrupted,
    /// The entry Machine could not read the routes, so no copy was seen unlisted.
    Unread(RpcError),
    /// The owner's view never listed a new copy, so it would refuse the Out.
    NotListed,
    Refused(ContainerRefusal),
    /// A call to the copy's owner failed.
    Failed(RpcError),
    /// This run Held the copy and could not bring it back In, for this reason.
    NotRestored(Box<Left>, String),
    OlderNotRemoved(Box<Left>),
    DeployFailed,
    /// A removal could not reach the copy's Machine. It stays until the next full
    /// Deploy prunes it, and its store row keeps the live copies' drain unconfirmed.
    Unreached,
}

impl From<RotationFailure> for Left {
    fn from(failure: RotationFailure) -> Self {
        match failure {
            RotationFailure::Refused(refusal) => Left::Refused(refusal),
            RotationFailure::NotConfirmed(unconfirmed) => Left::NotConfirmed(unconfirmed),
            RotationFailure::Cancelled => Left::Interrupted,
        }
    }
}

impl From<cleanup::Rotated> for Option<Left> {
    fn from(rotated: cleanup::Rotated) -> Self {
        rotated.failure().err().map(Left::from)
    }
}

async fn execute_operation<C: MachineOperations>(
    operation: &DeployOperation,
    row: Row<'_>,
    client: &C,
    cancellation: &CancellationToken,
    slots: &Slots<'_>,
    rows: &Rows<'_>,
) -> Result<Ran, OperationFailure> {
    let Row { progress, index } = row;
    row.running(match operation {
        DeployOperation::StopContainer { .. } | DeployOperation::StopHook { .. } => {
            OperationPhase::StoppingContainer
        }
        DeployOperation::RemoveContainer { .. } => OperationPhase::RemovingContainer,
        DeployOperation::RemoveVolume { .. } => OperationPhase::RemovingVolume,
        DeployOperation::UpdateContainer(_) => OperationPhase::UpdatingContainer,
        DeployOperation::PrepareVolumes { .. }
        | DeployOperation::WaitHealthy { .. }
        | DeployOperation::RunContainer { .. }
        | DeployOperation::ReplaceContainer(_)
        | DeployOperation::RunHook { .. } => OperationPhase::Starting,
    });
    let attempt = Attempt {
        client,
        row: Some(row),
        cancellation,
    };
    match operation {
        DeployOperation::PrepareVolumes { machine_id, specs } => client
            .prepare_volumes(machine_id, specs)
            .await
            .map(|_| Ran::default())
            .map_err(|error| {
                machine_error(
                    MachineAction::PrepareVolumes,
                    naming_machine(error, machine_id),
                )
                .into()
            }),
        DeployOperation::WaitHealthy { dependency, .. } => {
            wait_healthy(client, index, progress, dependency, rows.run, cancellation)
                .await
                .map(|()| Ran::default())
                .map_err(Into::into)
        }
        DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor,
        } => {
            let slot = slots.slot(ContainerKind::ServiceContainer, spec, index, None);
            let r#gen = rows
                .gen_for(
                    client,
                    progress,
                    &rows.service(spec),
                    machine_id,
                    cancellation,
                )
                .await?;
            let run = SlotRun {
                machine_id,
                slot,
                spec,
                deploy: Some(rows.run),
                r#gen,
                answer: answer_when(*skip_health_monitor),
            };
            start(&attempt, run, FailedStart::Discard)
                .await
                .map(Ran::started)
                .map_err(Into::into)
        }
        DeployOperation::StopContainer {
            machine_id,
            container_id,
            ..
        } => match inspect(client, machine_id, container_id).await {
            Ok(details) => stop_old_copy(
                client,
                index,
                progress,
                &details.container,
                rows,
                cancellation,
            )
            .await
            .map(|copy| Ran {
                held_old: (copy.rotation == CopyRotation::Held(rows.run.clone()))
                    .then(|| Box::new((details.container.resolved_spec.clone(), copy))),
                ..Ran::default()
            })
            .map_err(Into::into),
            Err(ExecutionError::Machine { error, .. }) if error.code == RpcErrorCode::NotFound => {
                Ok(Ran::default())
            }
            Err(error) => Err(error.into()),
        },
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
            ..
        } if let Some(service) = rows.strays.get(&index) => {
            let service = QualifiedService::new(rows.namespace.clone(), service.clone());
            remove_stray(
                client,
                progress,
                rows,
                (&service, machine_id, *container_id),
                cancellation,
            )
            .await;
            Ok(Ran::default())
        }
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
            purpose,
        } => {
            remove_container(
                client,
                index,
                progress,
                (machine_id, container_id, *purpose),
                rows,
                cancellation,
            )
            .await
        }
        DeployOperation::ReplaceContainer(replacement) => {
            let slot = slots.slot(
                ContainerKind::ServiceContainer,
                &replacement.spec,
                index,
                Some(replacement.old_container_id),
            );
            let r#gen = rows
                .gen_for(
                    client,
                    progress,
                    &rows.service(&replacement.spec),
                    &replacement.machine_id,
                    cancellation,
                )
                .await?;
            if replacement.spec.update.order.stops_first() {
                let old = rows
                    .inspect_step(client, progress, index, cancellation)
                    .await?;
                let run = SlotRun {
                    machine_id: &replacement.machine_id,
                    slot,
                    spec: &replacement.spec,
                    deploy: Some(rows.run),
                    r#gen,
                    answer: answer_when(replacement.skip_health_monitor),
                };
                return replace_stop_first(client, row, replacement, old, run, rows, cancellation)
                    .await;
            }
            // The old Container keeps serving until every new copy of the
            // Service is In and listed; it retires in the group's cleanup.
            let run = SlotRun {
                machine_id: &replacement.machine_id,
                slot,
                spec: &replacement.spec,
                deploy: Some(rows.run),
                r#gen,
                answer: answer_when(replacement.skip_health_monitor),
            };
            start(&attempt, run, FailedStart::Discard)
                .await
                .map(Ran::started)
                .map_err(Into::into)
        }
        // The copy stays In under its own id whether the update succeeds or fails.
        DeployOperation::UpdateContainer(update) => client
            .update_container_limits(&update.machine_id, &update.container_id, &update.spec)
            .await
            .map(|_| Ran::default())
            .map_err(|error| {
                machine_error(
                    MachineAction::UpdateContainerLimits,
                    naming_machine(error, &update.machine_id),
                )
                .into()
            }),
        DeployOperation::StopHook {
            machine_id,
            container_id,
        } => stop(client, machine_id, container_id, Drain::NONE, None)
            .await
            .map(|()| Ran::default())
            .map_err(Into::into),
        DeployOperation::RunHook {
            machine_id,
            spec,
            old_hook_containers,
        } => {
            let slot = slots.slot(ContainerKind::PreDeployHook, spec, index, None);
            run_hook(
                client,
                row,
                (machine_id, slot),
                (spec, rows.run),
                old_hook_containers,
                cancellation,
            )
            .await
            .map(|()| Ran::default())
            .map_err(Into::into)
        }
        DeployOperation::RemoveVolume { id } => ignore_not_found(client.remove_volume(id).await)
            .map(|()| Ran::default())
            .map_err(|error| machine_error(MachineAction::RemoveVolume, error).into()),
    }
}

/// Stop a Container; one already gone counts as stopped.
async fn stop<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    drain: Drain,
    grace_period_seconds: Option<u16>,
) -> Result<(), ExecutionError> {
    ignore_not_found(
        client
            .ensure_container(
                machine_id,
                ContainerWant::Stopped {
                    container: ContainerLocator::Id(*container_id),
                    drain,
                    stop: StopOptions {
                        signal: None,
                        grace_period_seconds,
                    },
                },
            )
            .await
            .map(|_| ()),
    )
    .map_err(|error| machine_error(MachineAction::StopContainer, error))
}

async fn remove<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    drain: Drain,
) -> Result<(), ExecutionError> {
    gone(
        client,
        machine_id,
        ContainerLocator::Id(*container_id),
        drain,
        AnonymousVolumes::Remove,
    )
    .await
}

async fn gone<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container: ContainerLocator,
    drain: Drain,
    anonymous_volumes: AnonymousVolumes,
) -> Result<(), ExecutionError> {
    client
        .ensure_container(
            machine_id,
            ContainerWant::Gone {
                container,
                drain,
                anonymous_volumes,
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| machine_error(MachineAction::RemoveContainer, error))
}

pub(in crate::deploy) fn health_error(
    container_id: ContainerId,
    health: Option<HealthVerdict>,
    spec: &ResolvedServiceSpec,
) -> ExecutionError {
    let out_of_memory = health
        .as_ref()
        .and_then(|health| out_of_memory(health, spec));
    let departing = health.as_ref().and_then(HealthVerdict::departing).cloned();
    let failure = health
        .and_then(HealthVerdict::into_failure)
        .unwrap_or(HealthFailure::TimedOut {
            deadline_millis: None,
            last_check: None,
        });
    ExecutionError::Health {
        container_id,
        failure,
        out_of_memory,
        last_run: None,
        departing,
    }
}

pub(super) async fn inspect<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
) -> Result<ContainerDetails, ExecutionError> {
    client
        .inspect_container(machine_id, container_id)
        .await
        .map_err(|error| machine_error(MachineAction::InspectContainer, error))
}

fn machine_error(action: MachineAction, error: RpcError) -> ExecutionError {
    ExecutionError::Machine { action, error }
}

/// A Service's next gen, and the Machines that left its listing unanswered.
#[derive(Clone)]
struct Numbered {
    r#gen: Gen,
    silent: std::sync::Arc<Vec<Unanswered>>,
}

/// A new copy cannot start while `silent` may run copies the store could not number.
fn unfloored(silent: &Unanswered, unread: &RpcError) -> ExecutionError {
    let error = RpcError::unavailable(format!(
        "Machine '{}' did not answer and the entry Machine's store could not be read, \
             so a new copy might not replace that Machine's copies",
        silent.name
    ))
    .with_causes(
        std::iter::once(unread.message.clone())
            .chain(unread.cause.iter().cloned())
            .collect(),
    );
    machine_error(
        MachineAction::ListContainers,
        naming_machine(error, &silent.failure.machine_id),
    )
}

/// The Machine that answered `error`, for a failure sentence that names the Server.
fn naming_machine(mut error: RpcError, machine_id: &MachineId) -> RpcError {
    if let serde_json::Value::Object(details) = &mut error.details {
        details.insert("machine_id".into(), serde_json::json!(machine_id));
    } else {
        error.details = serde_json::json!({ "machine_id": machine_id });
    }
    error
}

fn ignore_not_found(result: Result<(), RpcError>) -> Result<(), RpcError> {
    match result {
        Err(error) if error.code == RpcErrorCode::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
#[path = "../exec_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../exec_cluster_tests.rs"]
mod cluster_tests;
