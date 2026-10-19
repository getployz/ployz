use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::Duration,
};

use futures_util::{
    StreamExt,
    future::{Either, join_all},
    stream::FuturesUnordered,
};
use ployz_core::labels::CREATION_KEY;
use ployz_core::{
    AnonymousVolumes, AnswerWhen, ContainerDetails, ContainerId, ContainerKind, ContainerLocator,
    ContainerNow, ContainerObservation, ContainerRefusal, ContainerRefusalReason,
    ContainerRuntimeObservation, ContainerWant, CopyRotation, CreationKey, DependencyHealthFailure,
    DeployEvent, DeployWarning, DeploymentLogId, DockerVolumeId, Drain, EnsuredContainer,
    ExecutionError, ExposedPort, FailedOperation, Gen, HEALTH_POLL_INTERVAL, HealthFailure,
    HealthVerdict, HookFailure, ImagePulled, InspectContainerRequest, ListContainersRequest,
    MachineAction, MachineId, MachineName, MachineTarget, MembershipObservation, Namespace,
    NewContainer, OperationPhase, PortPublication, PullPolicy, QualifiedService, ReadRoutesRequest,
    RemoveContainerPurpose, RemoveVolumeRequest, ResolvedServiceSpec, Retry, RoutesRead, RpcError,
    RpcErrorCode, RunRotation, RunTarget, ServiceId, ServiceName, ServiceOutcome, SlotKey,
    StopOptions, TransportProtocol, op,
};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT, TransportError, stop_rpc_timeout};
use ployz_core::EnvironmentValues;

use super::{
    DeployOperation, DeployOutcome, ReplacementCompensation, ReplacementOperation, RestoreAttempt,
    StopAttempt,
    schedule::{Action, Event, Schedule, ServiceGroup, Step, Task, TaskKind, starts_container},
};

use super::cleanup::{self, ACK_POLL, ACK_WAIT};
pub(super) use super::progress::Progress;

mod health;
use health::wait_healthy;

const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const RESTART_WAIT: Duration = Duration::from_secs(60);
const RESTART_POLL: Duration = Duration::from_millis(250);

#[allow(clippy::large_enum_variant)]
enum Stop {
    Failed {
        index: usize,
        error: ExecutionError,
        compensation: Option<ReplacementCompensation<ExecutionError>>,
    },
    Superseded(QualifiedService),
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
        Some(Stop::Failed {
            index,
            error,
            compensation,
        }) => {
            let failed = match (operation_at(operations, index), compensation) {
                (DeployOperation::ReplaceContainer(operation), Some(compensation)) => {
                    FailedOperation::Replacement {
                        operation: operation.clone(),
                        error,
                        compensation,
                    }
                }
                (operation, _) => FailedOperation::Operation {
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
        Some(Stop::Superseded(service)) => DeployOutcome::Superseded {
            completed: done(None, true),
            service,
            unexecuted: done(None, false),
            services,
        },
    }
}

pub(crate) trait MachineOperations {
    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError>;
    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError>;
    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError>;
    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError>;
    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError>;
    /// Whether `machine_id` already runs a Container of this Global spec, which a
    /// start into `slot` adopts instead of creating.
    async fn runs_global(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<bool, RpcError>;
    /// Make the image `spec` runs present on `machine_id`, pulled as its policy
    /// asks, before any Container changes, and answer what the Machine reports of it.
    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ImagePulled, RpcError>;
    /// Converge one Container within `timeout`, keeping a refusal as typed.
    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<EnsuredContainer, RpcError>;
    /// What each Machine's proxy routes for `service`, as `machine_id` reads it.
    async fn read_routes(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<RoutesRead, RpcError>;
}

impl MachineOperations for Client {
    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ImagePulled, RpcError> {
        crate::image::pull_cluster_image(
            self,
            machine_id,
            &spec.container.image,
            spec.container.pull_policy,
            self.registry_auth.get(&spec.name).cloned(),
        )
        .await
    }

    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError> {
        self.invoke::<op::PrepareVolumes>(
            ployz_core::PrepareVolumesRequest {
                specs: specs.to_vec(),
            },
            &MachineTarget::from(machine_id),
            Some(Duration::from_secs(120)),
        )
        .await
    }

    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        let mut client = self.clone();
        let machines = client.machines().await.map_err(RpcError::from)?;
        let listed = client
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await
            .map_err(RpcError::from)?
            .containers;
        if let Some(failure) = listed.failures.into_iter().next() {
            return Err(failure.error);
        }
        if let Some(machine_id) = listed.omissions.into_iter().find(|machine_id| {
            omission_requires_observation(
                machines
                    .iter()
                    .find(|machine| machine.machine.id == *machine_id)
                    .map(|machine| &machine.membership),
            )
        }) {
            return Err(RpcError {
                code: RpcErrorCode::Unavailable,
                message: format!("container observation omitted {machine_id}"),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            });
        }
        Ok(listed
            .successes
            .into_iter()
            .flat_map(|success| success.value)
            .filter(|container| {
                container.kind == ContainerKind::ServiceContainer
                    && container.namespace == service.namespace
                    && container.resolved_spec.name == service.name
            })
            .collect())
    }

    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        mut want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        let timeout = match &mut want {
            ContainerWant::Running {
                container: RunTarget::New(new),
                ..
            } => {
                if let Some(key) = self
                    .retained_global_key(machine_id, &new.slot, &new.resolved_spec)
                    .await?
                {
                    new.slot.creation_key = key;
                }
                // A replacement joining a named Deploy keeps that Deploy's label.
                if new.deployment_id.is_none() {
                    new.deployment_id.clone_from(&self.deployment_id);
                }
                None
            }
            ContainerWant::Running { .. }
            | ContainerWant::Out { .. }
            | ContainerWant::Held { .. } => Some(TARGET_RPC_TIMEOUT),
            ContainerWant::Stopped { stop, .. } => stop_rpc_timeout(stop.grace_period_seconds, 1),
            ContainerWant::Gone { .. } => None,
        };
        Client::ensure_settled(self, machine_id, want, timeout).await
    }

    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<EnsuredContainer, RpcError> {
        Client::ensure_unsettled(self, machine_id, want, timeout).await
    }

    async fn read_routes(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<RoutesRead, RpcError> {
        self.invoke::<op::ReadRoutes>(
            ReadRoutesRequest {
                service: service.clone(),
                routes_only: true,
            },
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError> {
        self.invoke::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: *container_id,
            },
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
    }

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        self.invoke::<op::RemoveVolume>(
            RemoveVolumeRequest {
                name: id.name.clone(),
                force: false,
            },
            &MachineTarget::from(&id.machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map(|_| ())
    }

    async fn runs_global(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<bool, RpcError> {
        Ok(self
            .retained_global_key(machine_id, slot, spec)
            .await?
            .is_some())
    }
}

fn omission_requires_observation(membership: Option<&MembershipObservation>) -> bool {
    membership.is_none_or(MembershipObservation::invites_rpc)
}

/// What a completed operation left behind.
#[derive(Default)]
struct Ran {
    /// The new Container it started.
    started: Option<ContainerId>,
    /// The old copy it withdrew and stopped, which a failed Service restores.
    held_old: Option<cleanup::Copy>,
}

impl Ran {
    fn started(id: ContainerId) -> Self {
        Self {
            started: Some(id),
            held_old: None,
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

#[derive(Clone, Copy)]
enum HookInterruption {
    Cancelled,
    TimedOut,
}

impl From<ExecutionError> for OperationFailure {
    fn from(error: ExecutionError) -> Self {
        Self::Ordinary(error)
    }
}

// Wait out a target daemon restart on the same Machine. A new Container's
// creation key makes a resent Running find the Container the lost reply made.
// Do not walk to another connection; that is a different command-entry problem.
struct RestartTolerant<'a, C> {
    inner: &'a C,
    cancellation: &'a CancellationToken,
}

impl Client {
    /// The creation key of a retained Global replacement that a retry replans as a run,
    /// so the run starts that Container rather than making a second one.
    async fn retained_global_key(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<Option<CreationKey>, RpcError> {
        if slot.kind != ContainerKind::ServiceContainer
            || spec.mode != ployz_core::ServiceMode::Global
            || slot.creation_key.as_str() != crate::cluster::global_creation_key(spec)
        {
            return Ok(None);
        }
        Ok(self
            .clone()
            .try_read::<op::ListContainers>(
                ListContainersRequest {
                    environment: EnvironmentValues::Included,
                },
                &MachineTarget::from(machine_id),
            )
            .await
            .map_err(|error| crate::cluster::answered_by(machine_id, error))?
            .containers
            .into_iter()
            .find_map(|container| {
                (container.machine_id == *machine_id
                    && container.kind == slot.kind
                    && container.namespace == slot.namespace
                    && container.resolved_spec == *spec)
                    .then(|| container.labels.get(CREATION_KEY).cloned())
                    .flatten()
            })
            .and_then(|key| CreationKey::try_from(key).ok()))
    }
}

// Each method names its restart policy. `wait` resends while the Machine is
// restarting, so its daemon side must converge on a resend. `once` sends one
// time and names why a resend is unsafe. A row without a policy does not parse.
macro_rules! restart_policy {
    ($($policy:ident $(($reason:literal))? fn $name:ident(&self $(, $arg:ident: $ty:ty)*) -> $ret:ty;)*) => {
        impl<C: MachineOperations> MachineOperations for RestartTolerant<'_, C> {
            $(
                async fn $name(&self $(, $arg: $ty)*) -> $ret {
                    restart_policy!(@send $policy $(($reason))? self $name $($arg)*)
                }
            )*
        }
    };
    (@send wait $self:ident $name:ident $($arg:ident)*) => {
        wait_out_restart($self.cancellation, || $self.inner.$name($(Clone::clone(&$arg)),*)).await
    };
    (@send once($reason:literal) $self:ident $name:ident $($arg:ident)*) => {
        $self.inner.$name($($arg),*).await
    };
}

restart_policy! {
    wait fn prepare_volumes(&self, machine_id: &MachineId, specs: &[ployz_core::ServiceStorageSpec]) -> Result<ployz_core::PreparedVolumes, RpcError>;
    wait fn service_containers(&self, service: &QualifiedService) -> Result<Vec<ContainerObservation>, RpcError>;
    wait fn ensure_container(&self, machine_id: &MachineId, want: ContainerWant) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError>;
    wait fn inspect_container(&self, machine_id: &MachineId, container_id: &ContainerId) -> Result<ContainerDetails, RpcError>;
    wait fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError>;
    wait fn runs_global(&self, machine_id: &MachineId, slot: &SlotKey, spec: &ResolvedServiceSpec) -> Result<bool, RpcError>;
    wait fn ensure_image(&self, machine_id: &MachineId, spec: &ResolvedServiceSpec) -> Result<ImagePulled, RpcError>;
    wait fn ensure_unsettled(&self, machine_id: &MachineId, want: ContainerWant, timeout: Option<Duration>) -> Result<EnsuredContainer, RpcError>;
    wait fn read_routes(&self, machine_id: &MachineId, service: &QualifiedService) -> Result<RoutesRead, RpcError>;
}

async fn wait_out_restart<T, Fut>(
    cancellation: &CancellationToken,
    mut op: impl FnMut() -> Fut,
) -> Result<T, RpcError>
where
    Fut: Future<Output = Result<T, RpcError>>,
{
    let deadline = Instant::now() + RESTART_WAIT;
    // A Machine that answered on an earlier attempt was not silent throughout: its
    // answer stands in for a later silence, as `call_retried` keeps it.
    let mut answer: Option<RpcError> = None;
    loop {
        let outcome = op().await.map_err(|error| {
            if error.target_unanswered() {
                return answer.clone().unwrap_or(error);
            }
            answer = Some(error.clone());
            error
        });
        match outcome {
            Ok(value) => return Ok(value),
            Err(error)
                if error.code.retry() != Retry::Never
                    && Instant::now() < deadline
                    && !cancellation.is_cancelled() =>
            {
                tokio::select! {
                    () = cancellation.cancelled() => return Err(error),
                    () = tokio::time::sleep(RESTART_POLL) => {}
                }
            }
            Err(error) => return Err(error),
        }
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
        let key = if kind == ContainerKind::ServiceContainer
            && spec.mode == ployz_core::ServiceMode::Global
        {
            let key = crate::cluster::global_creation_key(spec);
            // Explicit replacement can overlap even an identical spec.
            match replacing {
                Some(old) => format!("{key}:replace:{old}"),
                None => key,
            }
        } else {
            format!("{}:{index}", self.plan)
        };
        slot_key(self.namespace, kind, key)
    }
}

fn slot_key(namespace: &Namespace, kind: ContainerKind, key: String) -> SlotKey {
    SlotKey {
        namespace: namespace.clone(),
        kind,
        creation_key: CreationKey::try_from(key).expect("creation keys stay within 512 bytes"),
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
    let client = RestartTolerant {
        inner: client,
        cancellation,
    };
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
    let mut started: BTreeMap<usize, Vec<(usize, MachineId, ContainerId)>> = BTreeMap::new();
    let mut stopped: BTreeMap<usize, Vec<(&ReplacementOperation, cleanup::Copy)>> = BTreeMap::new();
    let mut awaiting_flip: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    let mut unread = BTreeSet::new();
    let mut stop = None;
    let mut blocked_since: Option<Instant> = None;
    let mut blocked_before = Vec::new();
    let (progress, client, slots, rows) = (&progress, &client, &slots, &rows);
    let mut inflight = FuturesUnordered::new();

    loop {
        for action in schedule.next() {
            let next = match action {
                Action::Start(index) => Either::Left(async move {
                    let result = if cancellation.is_cancelled() {
                        Err(ExecutionError::Cancelled.into())
                    } else {
                        execute_operation(
                            operation_at(operations, index),
                            index,
                            progress,
                            client,
                            cancellation,
                            slots,
                            rows,
                        )
                        .await
                    };
                    Done::Task(index, result)
                }),
                Action::Publish(group) => {
                    let service = QualifiedService::new(
                        plan.namespace.clone(),
                        schedule
                            .group(group)
                            .service
                            .clone()
                            .expect("only Service groups publish"),
                    );
                    let new = started.get(&group).cloned().unwrap_or_default();
                    let old = old_copies(schedule.group(group), operations);
                    Either::Right(async move {
                        let published = publish_group(
                            client,
                            operations,
                            progress,
                            rows,
                            &service,
                            new,
                            old,
                            cancellation,
                        )
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
                    })) if starts_container(operation) => Some(*container_id),
                    Err(_) => None,
                };
                if let Some(id) = new_container {
                    started
                        .entry(group)
                        .or_default()
                        .push((index, operation.machine_id(), id));
                }
                match result {
                    Ok(ran) => {
                        if let DeployOperation::ReplaceContainer(replacement) = operation
                            && let Some(old) = ran.held_old
                        {
                            stopped.entry(group).or_default().push((replacement, old));
                        }
                        if schedule.group(group).publish && starts_container(operation) {
                            if let Some(container_id) = ran.started {
                                progress.set_running(
                                    index,
                                    OperationPhase::WaitingForSwitch { container_id },
                                );
                            }
                            awaiting_flip.entry(group).or_default().push(index);
                        } else {
                            completed.insert(index);
                            progress.set_completed(index);
                        }
                        schedule.apply(Event::Completed(index));
                    }
                    Err(OperationFailure::Ordinary(error)) => {
                        progress.fail(index, error.clone());
                        stop.get_or_insert(Stop::Failed {
                            index,
                            error,
                            compensation: None,
                        });
                        schedule.apply(Event::Failed(index));
                    }
                    Err(OperationFailure::Replacement {
                        error,
                        compensation,
                    }) => {
                        progress.fail(index, error.clone());
                        stop.get_or_insert(Stop::Failed {
                            index,
                            error,
                            compensation: Some(*compensation),
                        });
                        schedule.apply(Event::Failed(index));
                    }
                }
            }
            Some(Done::Publish(group, published)) => {
                let service = schedule
                    .group(group)
                    .service
                    .clone()
                    .expect("only Service groups publish");
                let qualified = QualifiedService::new(plan.namespace.clone(), service.clone());
                let task = schedule.publish_failure_task(group);
                let mut fail = |error: ExecutionError, event: Event| {
                    progress.fail(task, error.clone());
                    stop.get_or_insert(Stop::Failed {
                        index: task,
                        error,
                        compensation: None,
                    });
                    schedule.apply(event);
                };
                match published {
                    Published::Listed => {
                        for index in awaiting_flip.remove(&group).unwrap_or_default() {
                            completed.insert(index);
                            progress.set_completed(index);
                        }
                        schedule.apply(Event::Routed(service));
                        schedule.apply(Event::Published(group));
                    }
                    Published::Unconfirmed(machines) => {
                        unread.insert(group);
                        fail(
                            machine_error(
                                MachineAction::ReadRoutes,
                                unavailable(rows.not_confirmed(&machines)),
                            ),
                            Event::PublishUnconfirmed(group),
                        );
                    }
                    Published::Superseded => {
                        stop.get_or_insert(Stop::Superseded(qualified));
                        schedule.apply(Event::PublishFailed(group));
                    }
                    Published::Cancelled => {
                        fail(ExecutionError::Cancelled, Event::PublishFailed(group));
                    }
                    Published::Failed(error) => fail(error, Event::PublishFailed(group)),
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
                        progress.fail(task, error.clone());
                        stop.get_or_insert(Stop::Failed {
                            index: task,
                            error,
                            compensation: None,
                        });
                        schedule.apply(Event::Failed(task));
                    }
                } else {
                    blocked_since.get_or_insert_with(Instant::now);
                }
            }
        }
        for group in schedule.failed_groups() {
            let started = if unread.contains(&group) {
                Vec::new()
            } else {
                started
                    .remove(&group)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|(_, machine, id)| (machine, id))
                    .collect()
            };
            restore_group(
                client,
                progress,
                started,
                stopped.remove(&group).unwrap_or_default(),
                rows,
                cancellation,
            )
            .await;
        }
    }

    progress.settle_unexecuted();
    let outcome = outcome_from(operations, &completed, stop, schedule.services());
    progress.outcome(outcome.clone());
    outcome
}

async fn restore_group<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    started: Vec<(MachineId, ContainerId)>,
    stopped: Vec<(&ReplacementOperation, cleanup::Copy)>,
    rows: &Rows<'_>,
    cancellation: &CancellationToken,
) {
    join_all(
        started
            .iter()
            .map(|(machine, id)| discard_new(client, progress, rows, machine, *id, cancellation)),
    )
    .await;
    for (operation, old) in stopped {
        restore_old(client, &old, &operation.spec, rows, cancellation).await;
    }
}

/// The owner refuses Gone on a copy that already came In, so that one leaves
/// through Out and the unlisted wait first.
async fn discard_new<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    rows: &Rows<'_>,
    machine_id: &MachineId,
    container_id: ContainerId,
    cancellation: &CancellationToken,
) {
    let want = ContainerWant::Gone {
        container: ContainerLocator::Id(container_id),
        drain: Drain::NONE,
        anonymous_volumes: AnonymousVolumes::Remove,
    };
    let Ok(EnsuredContainer::Refused { now, refusal }) =
        client.ensure_unsettled(machine_id, want, None).await
    else {
        return;
    };
    let Some(observed) = now
        .present()
        .filter(|_| refusal.reason == ContainerRefusalReason::InRotation)
    else {
        return;
    };
    let copy = cleanup::Copy::observed(observed);
    let left =
        match cleanup::retire(client, &copy, true, AnonymousVolumes::Remove, cancellation).await {
            Ok(step) => Option::<Left>::from(step),
            Err(error) => Some(Left::Failed(error)),
        };
    if let Some(left) = left {
        rows.leave_running(progress, &copy, left);
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
        Some(Stop::Failed {
            index,
            error,
            compensation: None,
        }),
        schedule.services(),
    );
    progress.outcome(outcome.clone());
    outcome
}

/// The first look at acks is immediate; later ones wait a poll interval.
async fn ack_tick(cancellation: &CancellationToken, since: Option<Instant>) {
    if since.is_none() {
        return;
    }
    tokio::select! {
        () = cancellation.cancelled() => {}
        () = tokio::time::sleep(ACK_POLL) => {}
    }
}

enum Done {
    Task(usize, Result<Ran, OperationFailure>),
    Publish(usize, Published),
}

enum Published {
    Listed,
    Unconfirmed(Vec<MachineId>),
    /// A newer gen already serves: the new copies are discarded.
    Superseded,
    Cancelled,
    Failed(ExecutionError),
}

#[expect(
    clippy::too_many_arguments,
    reason = "one call site; the group's copies travel with the rows"
)]
async fn publish_group<C: MachineOperations>(
    client: &C,
    operations: &[DeployOperation],
    progress: &Progress,
    rows: &Rows<'_>,
    service: &QualifiedService,
    new: Vec<(usize, MachineId, ContainerId)>,
    old: Vec<(MachineId, ContainerId)>,
    cancellation: &CancellationToken,
) -> Published {
    if new.is_empty() {
        return Published::Listed;
    }
    // System ingress copies stay Pending for life and no route lists them.
    if *service != QualifiedService::system_ingress()
        && let Err(published) =
            route_new(client, operations, rows, service, &new, cancellation).await
    {
        return published;
    }
    let mut interrupted = false;
    for (machine_id, container_id) in &old {
        let copy = match inspect(client, machine_id, container_id).await {
            Ok(details) => cleanup::Copy::observed(&details.container),
            Err(ExecutionError::Machine { error, .. }) if error.code == RpcErrorCode::NotFound => {
                continue;
            }
            Err(error) => {
                let copy = cleanup::Copy {
                    machine_id: machine_id.clone(),
                    service: service.clone(),
                    container_id: *container_id,
                    rotation: CopyRotation::Pending,
                };
                let left = match error {
                    ExecutionError::Machine { error, .. } => Left::Failed(error),
                    error @ (ExecutionError::Health { .. }
                    | ExecutionError::DependencyHealth { .. }
                    | ExecutionError::Hook { .. }
                    | ExecutionError::Cancelled) => {
                        Left::Failed(RpcError::caused(RpcErrorCode::Internal, &error))
                    }
                };
                rows.leave_running(progress, &copy, left);
                continue;
            }
        };
        if interrupted {
            rows.leave_running(progress, &copy, Left::Interrupted);
            continue;
        }
        let left: Option<Left> =
            match cleanup::retire(client, &copy, false, AnonymousVolumes::Remove, cancellation)
                .await
            {
                Ok(step) => step.into(),
                Err(error) => Some(Left::Failed(error)),
            };
        if let Some(left) = left {
            interrupted = matches!(left, Left::Interrupted);
            rows.leave_running(progress, &copy, left);
        }
    }
    Published::Listed
}

async fn route_new<C: MachineOperations>(
    client: &C,
    operations: &[DeployOperation],
    rows: &Rows<'_>,
    service: &QualifiedService,
    new: &[(usize, MachineId, ContainerId)],
    cancellation: &CancellationToken,
) -> Result<(), Published> {
    let (_, first_machine, _) = new.first().expect("a published group started copies");
    for (task, machine_id, container_id) in new {
        let copy = cleanup::Copy {
            machine_id: machine_id.clone(),
            service: service.clone(),
            container_id: *container_id,
            rotation: CopyRotation::Pending,
        };
        let spec = operation_at(operations, *task)
            .spec()
            .expect("a started copy has a spec");
        match cleanup::bring_in(client, &copy, spec, Some(rows.run.clone())).await {
            Ok(cleanup::BroughtIn::In) => {}
            Ok(cleanup::BroughtIn::NotIn { health, .. }) => {
                let failure = health.and_then(HealthVerdict::into_failure).unwrap_or(
                    HealthFailure::TimedOut {
                        deadline_millis: None,
                        last_check: None,
                    },
                );
                return Err(Published::Failed(health_error(*container_id, failure)));
            }
            Ok(cleanup::BroughtIn::Refused(refusal))
                if refusal.reason == ContainerRefusalReason::OlderThanIn =>
            {
                return Err(Published::Superseded);
            }
            Ok(cleanup::BroughtIn::Refused(refusal)) => {
                return Err(Published::Failed(machine_error(
                    MachineAction::StartContainer,
                    refusal.into_error(),
                )));
            }
            Err(error) => {
                return Err(Published::Failed(machine_error(
                    MachineAction::StartContainer,
                    error,
                )));
            }
        }
    }
    let ids: BTreeSet<ContainerId> = new.iter().map(|(_, _, id)| *id).collect();
    match cleanup::wait_listed(client, first_machine, service, &ids, cancellation, |_| {}).await {
        Ok(cleanup::Wait::Done) => {}
        Ok(cleanup::Wait::TimedOut(machines)) => return Err(Published::Unconfirmed(machines)),
        Ok(cleanup::Wait::Cancelled) => return Err(Published::Cancelled),
        Err(error) => {
            return Err(Published::Failed(machine_error(
                MachineAction::ReadRoutes,
                error,
            )));
        }
    }
    Ok(())
}

fn old_copies(
    group: &ServiceGroup,
    operations: &[DeployOperation],
) -> Vec<(MachineId, ContainerId)> {
    group
        .steps
        .iter()
        .flat_map(|step| match step {
            Step::Serial(index) => vec![*index],
            Step::Parallel(indices) => indices.clone(),
        })
        .filter_map(|index| match operation_at(operations, index) {
            DeployOperation::ReplaceContainer(replacement) => {
                Some((replacement.machine_id.clone(), replacement.old_container_id))
            }
            DeployOperation::PrepareVolumes { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::RunContainer { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        })
        .collect()
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
    #[expect(dead_code, reason = "the operations the rows describe")]
    operations: &'a [DeployOperation],
    run: &'a DeploymentLogId,
    names: BTreeMap<MachineId, MachineName>,
    gens: tokio::sync::Mutex<BTreeMap<QualifiedService, Gen>>,
    stop_first: Vec<StopFirstStep<'a>>,
}

struct StopFirstStep<'a> {
    tasks: Vec<(usize, &'a ployz_core::ReplacementOperation)>,
    ready: tokio::sync::OnceCell<Inspected>,
}

struct Inspected {
    old: BTreeMap<usize, Result<Option<ContainerObservation>, ExecutionError>>,
}

impl<'a> Rows<'a> {
    fn new(plan: &'a super::DeployPlan, run: &'a DeploymentLogId) -> Self {
        let operations = plan.operations();
        let stop_first = plan
            .groups()
            .iter()
            .flat_map(|group| &group.steps)
            .map(|step| {
                let tasks = match step {
                    Step::Serial(index) => vec![*index],
                    Step::Parallel(indices) => indices.clone(),
                };
                tasks
                    .into_iter()
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
        Self {
            namespace: &plan.namespace,
            operations,
            run,
            names,
            gens: tokio::sync::Mutex::new(BTreeMap::new()),
            stop_first,
        }
    }

    async fn inspect_step<C: MachineOperations>(
        &self,
        client: &C,
        index: usize,
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
                Inspected { old }
            })
            .await;
        inspected
            .old
            .get(&index)
            .cloned()
            .expect("every task of the step was inspected")
    }

    fn service(&self, spec: &ResolvedServiceSpec) -> QualifiedService {
        QualifiedService::new(self.namespace.clone(), spec.name.clone())
    }

    async fn gen_for<C: MachineOperations>(
        &self,
        client: &C,
        service: &QualifiedService,
    ) -> Result<Gen, ExecutionError> {
        let mut gens = self.gens.lock().await;
        if let Some(r#gen) = gens.get(service) {
            return Ok(*r#gen);
        }
        let copies = client
            .service_containers(service)
            .await
            .map_err(|error| machine_error(MachineAction::ListContainers, error))?;
        let highest = copies
            .iter()
            .filter(|copy| copy.kind == ContainerKind::ServiceContainer)
            .map(|copy| copy.copy_gen().get())
            .max();
        let r#gen = highest.map_or(Gen::FIRST, |highest| Gen::new(highest + 1));
        gens.insert(service.clone(), r#gen);
        Ok(r#gen)
    }

    fn confirmed(
        &self,
        step: Result<cleanup::Step, RpcError>,
        action: MachineAction,
    ) -> Result<(), ExecutionError> {
        match step {
            Ok(cleanup::Step::Done) => Ok(()),
            Ok(cleanup::Step::Refused(refusal)) => Err(machine_error(action, refusal.into_error())),
            Ok(cleanup::Step::NotConfirmed(machines)) => Err(machine_error(
                action,
                unavailable(self.not_confirmed(&machines)),
            )),
            Ok(cleanup::Step::Cancelled) => Err(ExecutionError::Cancelled),
            Err(error) => Err(machine_error(action, error)),
        }
    }

    fn not_confirmed(&self, machines: &[MachineId]) -> String {
        let names: Vec<String> = machines.iter().map(|id| self.machine_name(id)).collect();
        format!("proxy on {} did not confirm", names.join(", "))
    }

    fn machine_name(&self, id: &MachineId) -> String {
        self.names
            .get(id)
            .map_or_else(|| id.to_string(), |name| name.to_string())
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
            Left::NotConfirmed(machines) => (self.not_confirmed(&machines), machines),
            Left::Interrupted => ("interrupted".into(), Vec::new()),
            Left::Refused(refusal) => (refusal.message, Vec::new()),
            Left::Failed(error) if unanswered(&error) => (
                format!("{} did not answer", self.machine_name(&copy.machine_id)),
                vec![copy.machine_id.clone()],
            ),
            Left::Failed(error) => (error.message, Vec::new()),
            Left::NotRestored(left, why) => {
                let (reason, away) = self.explain(copy, *left);
                (
                    format!("{reason}, and it could not go back In: {why}"),
                    away,
                )
            }
        }
    }
}

fn unanswered(error: &RpcError) -> bool {
    error.code == RpcErrorCode::Unavailable || error.target_unanswered()
}

enum Left {
    NotConfirmed(Vec<MachineId>),
    Interrupted,
    Refused(ContainerRefusal),
    Failed(RpcError),
    /// This run Held the copy and could not bring it back In, for this reason.
    NotRestored(Box<Left>, String),
}

impl From<cleanup::Step> for Option<Left> {
    fn from(step: cleanup::Step) -> Self {
        match step {
            cleanup::Step::Done => None,
            cleanup::Step::Refused(refusal) => Some(Left::Refused(refusal)),
            cleanup::Step::NotConfirmed(machines) => Some(Left::NotConfirmed(machines)),
            cleanup::Step::Cancelled => Some(Left::Interrupted),
        }
    }
}

async fn execute_operation<C: MachineOperations>(
    operation: &DeployOperation,
    index: usize,
    progress: &Progress,
    client: &C,
    cancellation: &CancellationToken,
    slots: &Slots<'_>,
    rows: &Rows<'_>,
) -> Result<Ran, OperationFailure> {
    progress.set_running(index, OperationPhase::Starting);
    match operation {
        DeployOperation::PrepareVolumes { machine_id, specs } => client
            .prepare_volumes(machine_id, specs)
            .await
            .map(|_| Ran::default())
            .map_err(|mut error| {
                if !error.details.is_object() {
                    error.details = serde_json::json!({});
                }
                error
                    .details
                    .as_object_mut()
                    .expect("details normalized to object")
                    .insert("machine_id".into(), serde_json::json!(machine_id));
                machine_error(MachineAction::PrepareVolumes, error).into()
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
            let r#gen = rows.gen_for(client, &rows.service(spec)).await?;
            run_container(
                client,
                index,
                progress,
                machine_id,
                slot,
                spec,
                r#gen,
                *skip_health_monitor,
                cancellation,
            )
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
            .map(|_| Ran::default())
            .map_err(Into::into),
            Err(ExecutionError::Machine { error, .. }) if error.code == RpcErrorCode::NotFound => {
                Ok(Ran::default())
            }
            Err(error) => Err(error.into()),
        },
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
            purpose,
        } => {
            let copy = match inspect(client, machine_id, container_id).await {
                Ok(details) => cleanup::Copy::observed(&details.container),
                Err(ExecutionError::Machine { error, .. })
                    if error.code == RpcErrorCode::NotFound =>
                {
                    return Ok(Ran::default());
                }
                Err(error) => return Err(error.into()),
            };
            progress.set_running(index, OperationPhase::RemovingContainer);
            let allow_empty = matches!(purpose, RemoveContainerPurpose::Retire);
            match cleanup::retire(client, &copy, allow_empty, (*purpose).into(), cancellation).await
            {
                Ok(cleanup::Step::Done) => Ok(Ran::default()),
                Ok(cleanup::Step::Refused(refusal)) => {
                    rows.leave_running(progress, &copy, Left::Refused(refusal.clone()));
                    Err(machine_error(MachineAction::RemoveContainer, refusal.into_error()).into())
                }
                Ok(cleanup::Step::NotConfirmed(machines)) => {
                    rows.leave_running(progress, &copy, Left::NotConfirmed(machines));
                    Ok(Ran::default())
                }
                Ok(cleanup::Step::Cancelled) => {
                    rows.leave_running(progress, &copy, Left::Interrupted);
                    Err(ExecutionError::Cancelled.into())
                }
                Err(error) => Err(machine_error(MachineAction::RemoveContainer, error).into()),
            }
        }
        DeployOperation::ReplaceContainer(replacement) => {
            let slot = slots.slot(
                ContainerKind::ServiceContainer,
                &replacement.spec,
                index,
                Some(replacement.old_container_id),
            );
            let r#gen = rows
                .gen_for(client, &rows.service(&replacement.spec))
                .await?;
            if replacement.spec.update.order.stops_first() {
                let old = rows.inspect_step(client, index).await?;
                return replace_stop_first(
                    client,
                    index,
                    progress,
                    replacement,
                    old,
                    slot,
                    r#gen,
                    rows,
                    cancellation,
                )
                .await;
            }
            // The old Container keeps serving until every new copy of the
            // Service is In and listed; it retires in the group's cleanup.
            start(
                client,
                index,
                progress,
                &replacement.machine_id,
                slot,
                &replacement.spec,
                r#gen,
                answer_when(replacement.skip_health_monitor),
                FailedStart::Discard,
                cancellation,
            )
            .await
            .map(Ran::started)
            .map_err(Into::into)
        }
        DeployOperation::StopHook {
            machine_id,
            container_id,
        } => {
            progress.set_running(index, OperationPhase::StoppingContainer);
            stop(client, machine_id, container_id, Drain::NONE, None)
                .await
                .map(|()| Ran::default())
                .map_err(Into::into)
        }
        DeployOperation::RunHook {
            machine_id,
            spec,
            old_hook_containers,
        } => {
            let slot = slots.slot(ContainerKind::PreDeployHook, spec, index, None);
            run_hook(
                client,
                index,
                progress,
                machine_id,
                slot,
                spec,
                old_hook_containers,
                cancellation,
            )
            .await
            .map(|()| Ran::default())
            .map_err(Into::into)
        }
        DeployOperation::RemoveVolume { id } => {
            progress.set_running(index, OperationPhase::RemovingVolume);
            ignore_not_found(client.remove_volume(id).await)
                .map(|()| Ran::default())
                .map_err(|error| machine_error(MachineAction::RemoveVolume, error).into())
        }
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

/// Why a Placement convergence move did not complete. Each case leaves a different set of
/// Containers behind, so the report tells them apart.
#[derive(Debug, thiserror::Error)]
pub(in crate::deploy) enum MoveContainerError {
    /// The new Container did not start or serve, or the move was cancelled first (then the
    /// error is [`ExecutionError::Cancelled`]). `old` keeps serving. `replacement_removed`
    /// says no new Container is left on `to`: none was made, or its removal was
    /// acknowledged.
    #[error("The new Container did not serve.")]
    NotServing {
        #[source]
        error: ExecutionError,
        replacement_removed: bool,
    },
    /// The new Container serves on `to`, but `old` could not be removed.
    #[error("The old Container was not removed.")]
    OldNotRemoved {
        #[source]
        error: ExecutionError,
        old_stopped: bool,
    },
}

/// Whether a move may stop or remove its old Container. `move_container` asks
/// immediately before each destructive request, so a Serving Pointer published
/// meanwhile keeps the old Container for that pointer's retire path. `old_stopped` says
/// whether the old Container already stopped when it asks.
pub(in crate::deploy) trait RemovesOld {
    async fn removes_old(&mut self, old_stopped: bool) -> bool;
}

impl RemovesOld for bool {
    async fn removes_old(&mut self, _old_stopped: bool) -> bool {
        *self
    }
}

pub(in crate::deploy) async fn move_container<C: MachineOperations>(
    client: &C,
    namespace: &Namespace,
    (spec, deploy): (&ResolvedServiceSpec, Option<&DeploymentLogId>),
    to: &MachineId,
    (from, old): (&MachineId, &ContainerId),
    gate: &mut impl RemovesOld,
    cancellation: &CancellationToken,
) -> Result<ContainerId, MoveContainerError> {
    let client = RestartTolerant {
        inner: client,
        cancellation,
    };
    let global = spec.mode == ployz_core::ServiceMode::Global;
    let key = if global {
        crate::cluster::global_creation_key(spec)
    } else {
        format!("move:{old}")
    };
    let slot = slot_key(namespace, ContainerKind::ServiceContainer, key);
    let adopts = global && adopts_running(&client, to, &slot, spec).await;
    // The moved copy keeps the source copy's gen: a Move is not a new rollout.
    let r#gen = match client.inspect_container(from, old).await {
        Ok(details) => details.container.copy_gen(),
        Err(_) => Gen::FIRST,
    };
    let progress = Progress::new(Vec::new(), None);
    let started = run_new(
        &client,
        0,
        &progress,
        to,
        slot.clone(),
        (spec, deploy),
        r#gen,
        AnswerWhen::Settled,
        cancellation,
    )
    .await;
    let new = match started_container(started, cancellation) {
        Ok(new) => new,
        Err(error) => {
            let keep = adopts || (global && !cancellation.is_cancelled());
            let replacement_removed = !keep && discard_attempt(&client, to, slot).await.is_ok();
            return Err(MoveContainerError::NotServing {
                error,
                replacement_removed,
            });
        }
    };
    if !gate.removes_old(false).await {
        return Ok(new);
    }
    // Stopped first, so a failed removal can say whether the old Container still serves.
    stop(&client, from, old, Drain::DEFAULT, None)
        .await
        .map_err(|error| MoveContainerError::OldNotRemoved {
            error,
            old_stopped: false,
        })?;
    if !gate.removes_old(true).await {
        return Ok(new);
    }
    remove(&client, from, old, Drain::NONE)
        .await
        .map_err(|error| MoveContainerError::OldNotRemoved {
            error,
            old_stopped: true,
        })?;
    Ok(new)
}

/// What a failed Global creation leaves on its Machine. A stop-first replacement
/// retains the keyed candidate so its retry needs no new endpoint; every other
/// start leaves nothing of this run behind.
#[derive(Clone, Copy, PartialEq, Eq)]
enum FailedStart {
    Discard,
    Retain,
}

#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus slot travel together through execute"
)]
async fn start<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    machine_id: &MachineId,
    slot: SlotKey,
    spec: &ResolvedServiceSpec,
    r#gen: Gen,
    answer: AnswerWhen,
    failed: FailedStart,
    cancellation: &CancellationToken,
) -> Result<ContainerId, ExecutionError> {
    let keyed_by_spec = slot.kind == ContainerKind::ServiceContainer
        && spec.mode == ployz_core::ServiceMode::Global;
    let adopts = keyed_by_spec && adopts_running(client, machine_id, &slot, spec).await;
    let started = run_new(
        client,
        index,
        progress,
        machine_id,
        slot.clone(),
        (spec, None),
        r#gen,
        answer,
        cancellation,
    )
    .await;
    let cleanup = match &started {
        Ok(_) => cancellation.is_cancelled(),
        // A slot held by another spec never held this attempt's Container.
        Err(error) => error.code != RpcErrorCode::Conflict,
    };
    let keep = adopts || (keyed_by_spec && failed == FailedStart::Retain);
    if cleanup
        && !keep
        && let Err(error) = discard_attempt(client, machine_id, slot.clone()).await
    {
        tracing::warn!(
            machine = %machine_id,
            ?slot,
            error = %error.message,
            "couldn't remove a failed start's Container"
        );
    }
    started_container(started, cancellation)
}

/// A Global start adopts a running Container of its spec, which a cancel must keep.
/// An unanswered check keeps it too.
async fn adopts_running<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    slot: &SlotKey,
    spec: &ResolvedServiceSpec,
) -> bool {
    client
        .runs_global(machine_id, slot, spec)
        .await
        .unwrap_or(true)
}

/// Remove what a failed creation left in `slot`, giving up if the Machine stays silent.
async fn discard_attempt<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    slot: SlotKey,
) -> Result<(), RpcError> {
    let discarded = client.ensure_container(
        machine_id,
        ContainerWant::Gone {
            container: ContainerLocator::Key(slot),
            drain: Drain::NONE,
            anonymous_volumes: AnonymousVolumes::Remove,
        },
    );
    match tokio::time::timeout(TARGET_RPC_TIMEOUT, discarded).await {
        Ok(result) => result.map(|_| ()),
        Err(_) => Err(crate::cluster::answered_by(
            machine_id,
            ConnectError::Rpc(TransportError::budget_expired(
                "target Machine RPC timed out",
            )),
        )),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus slot travel together through execute"
)]
async fn run_new<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    machine_id: &MachineId,
    slot: SlotKey,
    (spec, deploy): (&ResolvedServiceSpec, Option<&DeploymentLogId>),
    r#gen: Gen,
    answer: AnswerWhen,
    cancellation: &CancellationToken,
) -> Result<(ContainerId, Option<HealthVerdict>), RpcError> {
    progress.set_running(index, OperationPhase::CreatingContainer);
    let want = ContainerWant::Running {
        container: RunTarget::New(NewContainer {
            r#gen,
            slot: slot.clone(),
            resolved_spec: spec.clone(),
            deployment_id: deploy.cloned(),
            registry_auth: None,
            image_prepared: true,
            answer,
        }),
        rotation: RunRotation::Pending,
    };
    let ensured = client.ensure_container(machine_id, want);
    let ensured = match answer {
        AnswerWhen::Started => ensured.await,
        // A cancel abandons the Machine's health wait; the caller then discards
        // whatever the slot holds.
        AnswerWhen::Settled => tokio::select! {
            answered = ensured => answered,
            () = cancellation.cancelled() => return Err(unavailable(format!(
                "the Deploy was cancelled while Machine {machine_id} waited on health"
            ))),
        },
    }?;
    match ensured {
        (ContainerNow::Present(observed), health) => {
            progress.set_display_name(index, observed.display_name.clone());
            Ok((observed.container_id, health))
        }
        (ContainerNow::Absent { .. }, _) => Err(RpcError {
            code: RpcErrorCode::Internal,
            message: format!(
                "the Machine started no Container for slot {}",
                slot.creation_key.as_str()
            ),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }),
    }
}

fn unavailable(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message,
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
}

#[expect(
    clippy::result_large_err,
    reason = "ExecutionError is the Deploy's wire failure value; every step in this file returns it unboxed"
)]
fn started_container(
    started: Result<(ContainerId, Option<HealthVerdict>), RpcError>,
    cancellation: &CancellationToken,
) -> Result<ContainerId, ExecutionError> {
    if cancellation.is_cancelled() {
        return Err(ExecutionError::Cancelled);
    }
    let (container_id, health) =
        started.map_err(|error| machine_error(MachineAction::CreateContainer, error))?;
    match health.and_then(HealthVerdict::into_failure) {
        None => Ok(container_id),
        Some(failure) => Err(health_error(container_id, failure)),
    }
}

fn health_error(container_id: ContainerId, failure: HealthFailure) -> ExecutionError {
    ExecutionError::Health {
        container_id,
        failure,
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus slot travel together through execute"
)]
async fn run_container<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    machine_id: &MachineId,
    slot: SlotKey,
    spec: &ResolvedServiceSpec,
    r#gen: Gen,
    skip_health_monitor: bool,
    cancellation: &CancellationToken,
) -> Result<ContainerId, ExecutionError> {
    start(
        client,
        index,
        progress,
        machine_id,
        slot,
        spec,
        r#gen,
        answer_when(skip_health_monitor),
        FailedStart::Discard,
        cancellation,
    )
    .await
}

fn answer_when(skip_health_monitor: bool) -> AnswerWhen {
    if skip_health_monitor {
        AnswerWhen::Started
    } else {
        AnswerWhen::Settled
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one call site; the rows travel with the slot"
)]
async fn replace_stop_first<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    operation: &ReplacementOperation,
    old: Option<ContainerObservation>,
    slot: SlotKey,
    r#gen: Gen,
    rows: &Rows<'_>,
    cancellation: &CancellationToken,
) -> Result<Ran, OperationFailure> {
    let held_old = match old {
        Some(old) => {
            let was_running = match old.runtime {
                ContainerRuntimeObservation::Created
                | ContainerRuntimeObservation::Exited { .. }
                | ContainerRuntimeObservation::Dead => crate::ingress::is_system_ingress(&old),
                ContainerRuntimeObservation::Removing => false,
                ContainerRuntimeObservation::Running { .. }
                | ContainerRuntimeObservation::Paused
                | ContainerRuntimeObservation::Restarting
                | ContainerRuntimeObservation::Unknown { .. } => true,
            };
            let stopped = stop_old_copy(client, index, progress, &old, rows, cancellation).await?;
            // A copy that was neither serving nor running has nothing to restore.
            (was_running || stopped.rotation != CopyRotation::Pending).then_some(stopped)
        }
        None => None,
    };
    let started = start(
        client,
        index,
        progress,
        &operation.machine_id,
        slot,
        &operation.spec,
        r#gen,
        answer_when(operation.skip_health_monitor),
        FailedStart::Retain,
        cancellation,
    )
    .await;
    match started {
        Ok(new_container) => Ok(Ran {
            started: Some(new_container),
            held_old,
        }),
        Err(error) => Err(compensate(
            client,
            index,
            progress,
            operation,
            held_old,
            rows,
            cancellation,
            error,
        )
        .await),
    }
}

/// An In copy is Held for this run so a failed Service can bring it back; a
/// copy another run holds goes Out, since Held never passes between runs.
async fn stop_old_copy<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    old: &ContainerObservation,
    rows: &Rows<'_>,
    cancellation: &CancellationToken,
) -> Result<cleanup::Copy, ExecutionError> {
    let mut copy = cleanup::Copy::observed(old);
    let system_ingress = crate::ingress::is_system_ingress(old);
    let withdrawn = match &copy.rotation {
        CopyRotation::Pending | CopyRotation::Out => None,
        _ if system_ingress => None,
        CopyRotation::In => Some((
            cleanup::hold(client, &copy, rows.run.clone(), cancellation).await,
            CopyRotation::Held(rows.run.clone()),
        )),
        CopyRotation::Held(run) if run == rows.run => Some((
            cleanup::hold(client, &copy, rows.run.clone(), cancellation).await,
            copy.rotation.clone(),
        )),
        CopyRotation::Held(_) => Some((
            cleanup::take_out(client, &copy, false, cancellation).await,
            CopyRotation::Out,
        )),
    };
    if let Some((step, now)) = withdrawn {
        progress.set_running(
            index,
            OperationPhase::WaitingForSwitch {
                container_id: copy.container_id,
            },
        );
        let (left, error) = match step {
            Ok(cleanup::Step::NotConfirmed(machines)) => {
                let error = machine_error(
                    MachineAction::StopContainer,
                    unavailable(rows.not_confirmed(&machines)),
                );
                (Left::NotConfirmed(machines), error)
            }
            Ok(cleanup::Step::Cancelled) => (Left::Interrupted, ExecutionError::Cancelled),
            step => {
                rows.confirmed(step, MachineAction::StopContainer)?;
                copy.rotation = now;
                return stop_withdrawn(client, index, progress, copy).await;
            }
        };
        copy.rotation = now;
        leave_unstopped(client, progress, &copy, &old.resolved_spec, rows, left).await;
        return Err(error);
    }
    stop_withdrawn(client, index, progress, copy).await
}

async fn stop_withdrawn<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    copy: cleanup::Copy,
) -> Result<cleanup::Copy, ExecutionError> {
    progress.set_running(index, OperationPhase::StoppingContainer);
    stop(
        client,
        &copy.machine_id,
        &copy.container_id,
        Drain::DEFAULT,
        None,
    )
    .await?;
    Ok(copy)
}

/// A copy this run Held goes back In, so a Service it was serving keeps serving.
async fn leave_unstopped<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    copy: &cleanup::Copy,
    spec: &ResolvedServiceSpec,
    rows: &Rows<'_>,
    left: Left,
) {
    if copy.rotation != CopyRotation::Held(rows.run.clone()) {
        rows.leave_running(progress, copy, left);
        return;
    }
    let not_restored = match cleanup::bring_in(client, copy, spec, Some(rows.run.clone())).await {
        Ok(cleanup::BroughtIn::In) => None,
        Ok(cleanup::BroughtIn::NotIn { .. }) => Some("it failed health".to_owned()),
        Ok(cleanup::BroughtIn::Refused(refusal)) => Some(refusal.message),
        Err(error) => Some(error.message),
    };
    match not_restored {
        None => rows.leave_running(progress, copy, left),
        Some(why) => rows.leave_running(progress, copy, Left::NotRestored(Box::new(left), why)),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one call site; the failure travels with what it must undo"
)]
async fn compensate<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    operation: &ReplacementOperation,
    held_old: Option<cleanup::Copy>,
    rows: &Rows<'_>,
    cancellation: &CancellationToken,
    error: ExecutionError,
) -> OperationFailure {
    let new_container = if let ExecutionError::Health { container_id, .. } = &error {
        Some(*container_id)
    } else {
        None
    };
    if held_old.is_none() && new_container.is_none() {
        return error.into();
    }
    progress.set_running(index, OperationPhase::Compensating);
    let stop_new_container = match new_container {
        Some(container_id) => Some(StopAttempt::from(
            remove(client, &operation.machine_id, &container_id, Drain::NONE).await,
        )),
        None => None,
    };
    let compensation = match held_old {
        Some(old) => ReplacementCompensation::OldStopped {
            stop_new_container,
            restore_old_container: restore_old(client, &old, &operation.spec, rows, cancellation)
                .await,
        },
        None => match stop_new_container {
            Some(stop_new_container) => {
                ReplacementCompensation::OldUntouched { stop_new_container }
            }
            None => return error.into(),
        },
    };
    OperationFailure::Replacement {
        error,
        compensation: Box::new(compensation),
    }
}

async fn restore_old<C: MachineOperations>(
    client: &C,
    old: &cleanup::Copy,
    spec: &ResolvedServiceSpec,
    rows: &Rows<'_>,
    cancellation: &CancellationToken,
) -> RestoreAttempt<ExecutionError> {
    let failed = |error| RestoreAttempt::Failed { error };
    match &old.rotation {
        CopyRotation::Out => return RestoreAttempt::NotServing,
        CopyRotation::Pending => {
            let want = ContainerWant::Running {
                container: RunTarget::Existing(ContainerLocator::Id(old.container_id)),
                rotation: RunRotation::Pending,
            };
            return match client.ensure_container(&old.machine_id, want).await {
                Ok(_) => RestoreAttempt::Restored,
                Err(error) => failed(machine_error(MachineAction::StartContainer, error)),
            };
        }
        CopyRotation::In | CopyRotation::Held(_) => {}
    }
    match cleanup::bring_in(client, old, spec, Some(rows.run.clone())).await {
        Ok(cleanup::BroughtIn::In) => {}
        Ok(cleanup::BroughtIn::NotIn { health, .. }) => {
            let failure =
                health
                    .and_then(HealthVerdict::into_failure)
                    .unwrap_or(HealthFailure::TimedOut {
                        deadline_millis: None,
                        last_check: None,
                    });
            return failed(health_error(old.container_id, failure));
        }
        Ok(cleanup::BroughtIn::Refused(refusal))
            if refusal.reason == ployz_core::ContainerRefusalReason::HeldByOtherRun =>
        {
            return RestoreAttempt::LeftHeld;
        }
        Ok(cleanup::BroughtIn::Refused(refusal)) => {
            return failed(machine_error(
                MachineAction::StartContainer,
                refusal.into_error(),
            ));
        }
        Err(error) => return failed(machine_error(MachineAction::StartContainer, error)),
    }
    let copies = BTreeSet::from([old.container_id]);
    match cleanup::wait_listed(
        client,
        &old.machine_id,
        &old.service,
        &copies,
        cancellation,
        |_| {},
    )
    .await
    {
        Ok(wait) => {
            match rows.confirmed(Ok(cleanup::Step::from(wait)), MachineAction::ReadRoutes) {
                Ok(()) => RestoreAttempt::Restored,
                Err(error) => failed(error),
            }
        }
        Err(error) => failed(machine_error(MachineAction::ReadRoutes, error)),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "progress row plus slot travel together through execute"
)]
async fn run_hook<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    machine_id: &MachineId,
    slot: SlotKey,
    spec: &ResolvedServiceSpec,
    old_hook_containers: &[(MachineId, ContainerId)],
    cancellation: &CancellationToken,
) -> Result<(), ExecutionError> {
    progress.set_running(index, OperationPhase::RemovingContainer);
    for (old_machine_id, old_container_id) in old_hook_containers {
        remove(client, old_machine_id, old_container_id, Drain::NONE).await?;
    }

    let container_id = start(
        client,
        index,
        progress,
        machine_id,
        slot,
        spec,
        Gen::FIRST,
        AnswerWhen::Started,
        FailedStart::Discard,
        cancellation,
    )
    .await?;

    let timeout = spec
        .pre_deploy
        .as_ref()
        .and_then(|hook| hook.timeout_millis)
        .map_or(DEFAULT_HOOK_TIMEOUT, Duration::from_millis);
    let started = Instant::now();
    let deadline = started + timeout;
    let deadline_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    loop {
        progress.set_running(
            index,
            OperationPhase::WaitingForHook {
                container_id,
                elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                deadline_ms,
            },
        );
        let observed = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(interrupt_hook(
                    client,
                    machine_id,
                    &container_id,
                    HookInterruption::Cancelled,
                ).await);
            }
            () = tokio::time::sleep_until(deadline) => {
                return Err(interrupt_hook(
                    client,
                    machine_id,
                    &container_id,
                    HookInterruption::TimedOut,
                ).await);
            }
            observed = inspect(client, machine_id, &container_id) => observed?.container,
        };
        match observed.runtime {
            ContainerRuntimeObservation::Exited { code: 0 } => return Ok(()),
            ContainerRuntimeObservation::Exited { code } => {
                return Err(ExecutionError::Hook {
                    container_id,
                    failure: HookFailure::Exit { code },
                });
            }
            ContainerRuntimeObservation::Created
            | ContainerRuntimeObservation::Running { .. }
            | ContainerRuntimeObservation::Paused
            | ContainerRuntimeObservation::Restarting
            | ContainerRuntimeObservation::Removing
            | ContainerRuntimeObservation::Dead
            | ContainerRuntimeObservation::Unknown { .. } => {}
        }
        let wake = std::cmp::min(Instant::now() + HEALTH_POLL_INTERVAL, deadline);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                return Err(interrupt_hook(
                    client,
                    machine_id,
                    &container_id,
                    HookInterruption::Cancelled,
                ).await);
            }
            () = tokio::time::sleep_until(deadline) => {
                return Err(interrupt_hook(
                    client,
                    machine_id,
                    &container_id,
                    HookInterruption::TimedOut,
                ).await);
            }
            () = tokio::time::sleep_until(wake) => {}
        }
    }
}

async fn interrupt_hook<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    interruption: HookInterruption,
) -> ExecutionError {
    let stop_error = client
        .ensure_container(
            machine_id,
            ContainerWant::Stopped {
                container: ContainerLocator::Id(*container_id),
                drain: Drain::NONE,
                stop: StopOptions {
                    signal: None,
                    grace_period_seconds: Some(0),
                },
            },
        )
        .await
        .err();
    let failure = match interruption {
        HookInterruption::Cancelled => HookFailure::Cancelled { stop_error },
        HookInterruption::TimedOut => HookFailure::TimedOut { stop_error },
    };
    ExecutionError::Hook {
        container_id: *container_id,
        failure,
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
