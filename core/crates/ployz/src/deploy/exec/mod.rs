use std::collections::{BTreeMap, BTreeSet};

use futures_util::{
    StreamExt,
    future::{Either, join_all},
    stream::FuturesUnordered,
};
use ployz_core::{
    AnonymousVolumes, ContainerDetails, ContainerId, ContainerKind, ContainerLocator,
    ContainerUpdateOperation, ContainerWant, CopyRotation, CreationKey, DependencyHealthFailure,
    DeployEvent, DeployWarning, DeploymentLogId, Drain, ExecutionError, ExposedPort,
    FailedOperation, HealthFailure, HealthVerdict, InPlaceChange, MachineAction, MachineId,
    Namespace, OperationPhase, PortPublication, PullPolicy, QualifiedService,
    RemoveContainerPurpose, ResolvedServiceSpec, RpcError, RpcErrorCode, SealedLogin, ServiceId,
    ServiceName, ServiceOutcome, SlotKey, StopOptions, TransportProtocol,
};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{
    DeployOperation, DeployOutcome, ReplacementCompensation, ReplacementOperation, RestoreAttempt,
    StopAttempt,
    schedule::{Action, Event, GroupRole, Schedule, Stage, Task, TaskKind},
};

use super::cleanup::{self, ACK_WAIT};
pub(super) use super::progress::Progress;

mod gens;
mod health;
mod hooks;
mod publish;
mod report;
mod start;
mod stop_first;

use super::machine_calls::{MachineOperations, RestartTolerant, SlotPolicy, naming_machine};
use gens::Gens;
use health::wait_healthy;
use hooks::run_hook;
use publish::{
    Done, GappedPublishes, GroupCopies, Published, Retiring, Switch, ack_tick, old_copies,
    publish_group, rolling_step, route_new,
};
use report::Report;
use start::{
    Attempt, FailedStart, NewCopy, Row, SlotRun, Unstarted, answer_when, out_of_memory,
    remove_stray, start,
};
pub(crate) use start::{MoveEnd, MoveError, move_container};
use stop_first::{
    StopFirstSteps, remove_container, replace_stop_first, restore_group, stop_old_copy,
};

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
    started: Option<NewCopy>,
    /// The old copy it withdrew and stopped, which a failed Service restores.
    held_old: Option<Box<(ResolvedServiceSpec, cleanup::Copy)>>,
}

impl Ran {
    fn started(copy: NewCopy) -> Self {
        Self {
            started: Some(copy),
            ..Self::default()
        }
    }
}

#[derive(Clone)]
enum OperationFailure {
    Ordinary(ExecutionError),
    /// A start that failed, which may leave a copy that failed its health.
    Start(Unstarted),
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
            Self::Ordinary(error) | Self::Start(Unstarted { error, .. }) => (error, None),
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

impl From<Unstarted> for OperationFailure {
    fn from(unstarted: Unstarted) -> Self {
        Self::Start(unstarted)
    }
}

/// Names the slot each new Container of one Deploy lands on. A resend of the same
/// operation of the same run names the same slot, so a lost reply never doubles a
/// Container; another run or another plan names another slot.
pub(super) struct Slots<'a> {
    run: &'a DeploymentLogId,
    /// Of the plan's operations, so another plan of the same run names other slots.
    digest: String,
    namespace: &'a Namespace,
}

impl<'a> Slots<'a> {
    /// `run` names the attempt: Cloud's deployment id, or a fresh id per CLI run.
    pub(super) fn new(
        run: &'a DeploymentLogId,
        namespace: &'a Namespace,
        operations: &[DeployOperation],
    ) -> Self {
        let digest = Sha256::digest(
            serde_json::to_vec(operations).expect("a Deploy Plan serializes to JSON"),
        );
        let digest = digest
            .iter()
            .take(8)
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        Self {
            run,
            digest,
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
            CreationKey::for_slot(self.run, &self.digest, index)
        })
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
    let slots = Slots::new(run, &plan.namespace, operations);
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

    match pull_images(&client, operations, plan.registry_logins()).await {
        Ok(warnings) => warnings
            .into_iter()
            .for_each(|warning| progress.warn(warning)),
        Err((index, error)) => return untouched(&progress, &schedule, operations, index, error),
    }

    let shared = Shared::new(plan, run);
    let mut records: BTreeMap<usize, GroupRecord> = BTreeMap::new();
    let mut stop = None;
    let switches = std::sync::Mutex::new(Switches::new(plan));
    let switches = &switches;
    let mut blocked_since: Option<Instant> = None;
    let mut blocked_before = Vec::new();
    let (progress, client, slots, shared) = (&progress, &client, &slots, &shared);
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
                            shared,
                        )
                        .await
                    };
                    Done::Task(index, result)
                })),
                Action::Publish(group, service) => {
                    let service = QualifiedService::new(plan.namespace.clone(), service);
                    let new = records
                        .get(&group)
                        .map(|record| {
                            record
                                .started
                                .iter()
                                .map(|(task, machine, copy)| (*task, *machine, copy.container_id))
                                .collect()
                        })
                        .unwrap_or_default();
                    let old = old_copies(schedule.group(group), operations);
                    Either::Right(async move {
                        let copies = GroupCopies {
                            service: &service,
                            new,
                            old,
                        };
                        let published = publish_group(
                            client,
                            operations,
                            progress,
                            shared,
                            copies,
                            cancellation,
                            |switch| {
                                if let Some(at) =
                                    switches.lock().expect("switch lock").observe(group, switch)
                                {
                                    progress.emit_switch(Some(at));
                                }
                            },
                        )
                        .await;
                        Done::Publish(group, service.name, published)
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
                let new_copy = match &result {
                    Ok(ran) => ran.started,
                    Err(OperationFailure::Start(unstarted)) => unstarted.unhealthy(),
                    Err(OperationFailure::Ordinary(_) | OperationFailure::Replacement { .. }) => {
                        None
                    }
                };
                let record = records.entry(group).or_default();
                if let Some(copy) = new_copy {
                    record.started.push((index, operation.machine_id(), copy));
                }
                match result {
                    Ok(ran) => {
                        if let Some(old) = ran.held_old {
                            record.stopped.push(*old);
                        }
                        let publishing = schedule.group(group).publishing_service();
                        if let (Some(_), Some(copy)) = (publishing, ran.started) {
                            progress.set_running(
                                index,
                                OperationPhase::WaitingForSwitch {
                                    container_id: copy.container_id,
                                },
                            );
                            record.awaiting_flip.push(index);
                        } else {
                            completed.insert(index);
                            progress.set_completed(index);
                        }
                        let service = publishing.map(|service| {
                            QualifiedService::new(plan.namespace.clone(), service.clone())
                        });
                        if let Some((service, new)) = service
                            .filter(|service| shared.plan.routed(service))
                            .zip(rolling_step(
                                operation,
                                ran.started.map(|copy| copy.container_id),
                            ))
                        {
                            inflight.push(Either::Left(Either::Right(async move {
                                let new = [(index, new.0, new.1)];
                                match route_new(
                                    client,
                                    operations,
                                    shared,
                                    &service,
                                    &new,
                                    cancellation,
                                )
                                .await
                                {
                                    Ok(_) => Done::Admitted(index),
                                    Err(published) => Done::Publish(group, service.name, published),
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
            Some(Done::Publish(group, service, published)) => {
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
                            RpcError::unavailable(shared.report.not_confirmed(&machines)),
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
        if stop.is_some() {
            switches.lock().expect("switch lock").blocked = true;
        }
        for group in schedule.failed_groups() {
            shared
                .stop_first
                .restore_unclaimed(client, progress, shared, |task| {
                    schedule.group_of(task) == group
                })
                .await;
            let service = schedule
                .group(group)
                .service()
                .map(|name| QualifiedService::new(plan.namespace.clone(), name.clone()));
            let record = records.remove(&group).unwrap_or_default();
            restore_group(client, progress, service.as_ref(), record, shared).await;
        }
    }

    progress.settle_unexecuted();
    let outcome = outcome_from(operations, &completed, stop, schedule.services());
    let live_at = if matches!(outcome, DeployOutcome::Success { .. }) {
        switches
            .lock()
            .expect("switch lock")
            .finished(!completed.is_empty(), progress)
    } else {
        None
    };
    progress.outcome(outcome.clone(), live_at);
    shared
        .retiring
        .retire(client, progress, &shared.report)
        .await;
    outcome
}

struct Switches {
    awaiting: BTreeSet<usize>,
    last_at: Option<i64>,
    blocked: bool,
}

impl Switches {
    fn new(plan: &super::DeployPlan) -> Self {
        let awaiting = plan
            .groups()
            .iter()
            .enumerate()
            .filter(|(_, group)| {
                group.publishing_service().is_some()
                    && group.steps.iter().flat_map(Stage::tasks).any(|&index| {
                        super::schedule::creates_container(operation_at(plan.operations(), index))
                    })
            })
            .map(|(index, _)| index)
            .collect();
        Self {
            awaiting,
            last_at: None,
            blocked: plan.preview().waits().next().is_some(),
        }
    }

    fn observe(&mut self, group: usize, switch: Switch) -> Option<i64> {
        if matches!(switch, Switch::Unconfirmed) {
            self.blocked = true;
        }
        if self.blocked || !self.awaiting.remove(&group) {
            return None;
        }
        if matches!(switch, Switch::Confirmed) {
            self.last_at = Some(ployz_core::deploy_steps::now_ms() / 1000);
        }
        self.awaiting.is_empty().then_some(self.last_at).flatten()
    }

    fn finished(&self, ran_steps: bool, progress: &Progress) -> Option<i64> {
        if self.blocked || !self.awaiting.is_empty() {
            return None;
        }
        self.last_at.or_else(|| {
            // A removal-only or in-place plan takes its time from its last successful step.
            if !ran_steps {
                return None;
            }
            let at = ployz_core::deploy_steps::now_ms() / 1000;
            progress.emit_switch(Some(at));
            Some(at)
        })
    }
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
    started: Vec<(usize, MachineId, NewCopy)>,
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
    progress.outcome(outcome.clone(), None);
    outcome
}

/// Pull every image the plan runs, then warn about each Service whose image exposes
/// TCP ports none of its domains send to.
async fn pull_images<C: MachineOperations>(
    client: &C,
    operations: &[DeployOperation],
    auth: &BTreeMap<ServiceName, SealedLogin>,
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
                    .ensure_image(machine, spec, auth.get(&spec.name).cloned())
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

/// What every task of one Deploy shares.
struct Shared<'a> {
    plan: PlanFacts<'a>,
    report: Report<'a>,
    gens: Gens<'a>,
    gapped: GappedPublishes,
    retiring: Retiring,
    stop_first: StopFirstSteps<'a>,
}

impl<'a> Shared<'a> {
    fn new(plan: &'a super::DeployPlan, run: &'a DeploymentLogId) -> Self {
        let facts = plan.facts();
        Self {
            plan: PlanFacts::new(plan, run),
            report: Report::new(&facts.machine_names, &facts.dropped_large_packets),
            gens: Gens::new(&facts.unobserved_containers, &facts.retiring),
            gapped: GappedPublishes::default(),
            retiring: Retiring::default(),
            stop_first: StopFirstSteps::new(plan.operations(), plan.groups()),
        }
    }
}

/// Who runs this Deploy, and the Services its removals take copies of.
struct PlanFacts<'a> {
    namespace: &'a Namespace,
    run: &'a DeploymentLogId,
    /// The Service each rollout removal retires a copy of, so one that cannot
    /// inspect its copy can still name it.
    removals: BTreeMap<usize, &'a ServiceName>,
    /// The removals of copies ended runs left Pending, with each copy's Service.
    strays: &'a BTreeMap<usize, ServiceName>,
}

impl<'a> PlanFacts<'a> {
    fn new(plan: &'a super::DeployPlan, run: &'a DeploymentLogId) -> Self {
        let operations = plan.operations();
        let removals = plan
            .groups()
            .iter()
            .filter_map(|group| match &group.role {
                GroupRole::Removals(service) => Some((service, &group.steps)),
                GroupRole::Prelude | GroupRole::Service { .. } | GroupRole::Tail(_) => None,
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
            run,
            removals,
            strays: &plan.facts().strays,
        }
    }

    fn run(&self) -> &'a DeploymentLogId {
        self.run
    }

    fn service(&self, name: &ServiceName) -> QualifiedService {
        QualifiedService::new(self.namespace.clone(), name.clone())
    }

    /// Whether proxies route `service`'s copies. The Ingress Proxy's stay Pending for
    /// life and no route lists them, so none is withdrawn, routed or waited on.
    fn routed(&self, service: &QualifiedService) -> bool {
        *service != QualifiedService::system_ingress()
    }

    /// The Service rollout removal `index` retires a copy of.
    fn removed_by(&self, index: usize) -> Option<QualifiedService> {
        self.removals.get(&index).map(|name| self.service(name))
    }

    /// The Service of the copy an ended run left Pending that removal `index` removes.
    fn stray(&self, index: usize) -> Option<QualifiedService> {
        self.strays.get(&index).map(|name| self.service(name))
    }
}

async fn execute_operation<C: MachineOperations>(
    operation: &DeployOperation,
    row: Row<'_>,
    client: &C,
    cancellation: &CancellationToken,
    slots: &Slots<'_>,
    shared: &Shared<'_>,
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
        DeployOperation::WaitHealthy { dependency, .. } => wait_healthy(
            client,
            index,
            progress,
            dependency,
            shared.plan.run(),
            cancellation,
        )
        .await
        .map(|()| Ran::default())
        .map_err(Into::into),
        DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor,
        } => {
            let slot = slots.slot(ContainerKind::ServiceContainer, spec, index, None);
            let r#gen = shared
                .gens
                .gen_for(
                    client,
                    progress,
                    &shared.plan.service(&spec.name),
                    machine_id,
                    cancellation,
                )
                .await?;
            let run = SlotRun {
                machine_id,
                slot,
                spec,
                deploy: Some(shared.plan.run()),
                r#gen,
                answer: answer_when(*skip_health_monitor),
                replaces: None,
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
                shared,
                cancellation,
            )
            .await
            .map(|copy| Ran {
                held_old: (copy.rotation == CopyRotation::Held(shared.plan.run().clone()))
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
        } if let Some(service) = shared.plan.stray(index) => {
            remove_stray(
                client,
                progress,
                shared,
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
            anonymous_volumes,
        } => {
            remove_container(
                client,
                index,
                progress,
                (machine_id, container_id, *purpose, *anonymous_volumes),
                shared,
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
            let r#gen = shared
                .gens
                .gen_for(
                    client,
                    progress,
                    &shared.plan.service(&replacement.spec.name),
                    &replacement.machine_id,
                    cancellation,
                )
                .await?;
            if replacement.spec.update.order.stops_first() {
                let old = shared
                    .stop_first
                    .inspect(client, progress, shared, index, cancellation)
                    .await?;
                let run = SlotRun {
                    machine_id: &replacement.machine_id,
                    slot,
                    spec: &replacement.spec,
                    deploy: Some(shared.plan.run()),
                    r#gen,
                    answer: answer_when(replacement.skip_health_monitor),
                    replaces: Some(replacement.old_container_id),
                };
                return replace_stop_first(
                    client,
                    row,
                    replacement,
                    old,
                    run,
                    shared,
                    cancellation,
                )
                .await;
            }
            // The old Container keeps serving until every new copy of the
            // Service is In and listed; it retires in the group's cleanup.
            let run = SlotRun {
                machine_id: &replacement.machine_id,
                slot,
                spec: &replacement.spec,
                deploy: Some(shared.plan.run()),
                r#gen,
                answer: answer_when(replacement.skip_health_monitor),
                replaces: Some(replacement.old_container_id),
            };
            start(&attempt, run, FailedStart::Discard)
                .await
                .map(Ran::started)
                .map_err(Into::into)
        }
        // The copy stays In under its own id whether the update succeeds or fails.
        DeployOperation::UpdateContainer(update) => {
            let ContainerUpdateOperation {
                machine_id,
                container_id,
                spec,
                change,
            } = update;
            let (updated, action) = match change {
                InPlaceChange::Limits => (
                    client
                        .update_container_limits(machine_id, container_id, spec)
                        .await
                        .map(drop),
                    MachineAction::UpdateContainerLimits,
                ),
                InPlaceChange::Placement => (
                    client
                        .update_container_placement(machine_id, container_id, spec)
                        .await
                        .map(drop),
                    MachineAction::UpdateContainerPlacement,
                ),
            };
            updated
                .map(|()| Ran::default())
                .map_err(|error| machine_error(action, naming_machine(error, machine_id)).into())
        }
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
                (spec, shared.plan.run()),
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

/// Remove a Container by its id, keeping its anonymous Volumes: an old hook, or a new
/// copy that failed its health, neither of which a copy replaced.
async fn remove_by_id<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    drain: Drain,
) -> Result<(), ExecutionError> {
    client
        .ensure_container(
            machine_id,
            ContainerWant::Gone {
                container: ContainerLocator::Id(*container_id),
                drain,
                anonymous_volumes: AnonymousVolumes::keep(),
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| machine_error(MachineAction::RemoveContainer, error))
}

pub(crate) fn health_error(
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
