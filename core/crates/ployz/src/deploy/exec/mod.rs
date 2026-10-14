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
use ployz_core::{
    AnonymousVolumes, AnswerWhen, ContainerDetails, ContainerId, ContainerKind, ContainerLocator,
    ContainerNow, ContainerObservation, ContainerRuntimeObservation, ContainerWant, CreationKey,
    DependencyHealthFailure, DeployEvent, DeploymentLogId, DockerVolumeId, Drain, ExecutionError,
    FailedOperation, HEALTH_POLL_INTERVAL, HealthFailure, HealthVerdict, HookFailure,
    InspectContainerRequest, ListContainersRequest, MachineAction, MachineId, MachineTarget,
    MembershipObservation, Namespace, NewContainer, OperationPhase, PublishServingPointerRequest,
    PullPolicy, QualifiedService, RemoveVolumeRequest, ResolvedServiceSpec, RpcError, RpcErrorCode,
    RunTarget, ServingPointer, ServingPointerPublication, ServingPointerRead, SlotKey, StopOptions,
    UpdateOrder, op,
};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::connect::{Client, TARGET_RPC_TIMEOUT, stop_rpc_timeout};
use ployz_core::EnvironmentValues;

use super::{
    DeployOperation, DeployOutcome, ReplacementCompensation, ReplacementOperation, RestartAttempt,
    StopAttempt,
    schedule::{Action, Event, Schedule, ServiceGroup, Task, TaskKind, starts_container},
};
use crate::serving_publish::{AMEND_RETRIES, Applied, Protection, Rotation, succession};

pub(super) use super::progress::Progress;

mod health;
use health::wait_healthy;

const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const RESTART_WAIT: Duration = Duration::from_secs(60);
const RESTART_POLL: Duration = Duration::from_millis(250);
/// How often a Deploy re-reads acks while a dependent waits on them.
const ACK_POLL: Duration = Duration::from_secs(1);
/// How long a dependent waits for one Machine to route its dependency's Deploy.
const ACK_WAIT: Duration = Duration::from_secs(300);

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
            }
        }
        Some(Stop::Superseded(service)) => DeployOutcome::Superseded {
            completed: done(None, true),
            service,
            unexecuted: done(None, false),
        },
    }
}

pub(super) trait MachineOperations {
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
    /// asks, before any Container changes.
    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), RpcError>;
    async fn read_pointer(
        &self,
        service: &QualifiedService,
    ) -> Result<ServingPointerRead, RpcError>;
    async fn publish_pointer(
        &self,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError>;
}

impl MachineOperations for Client {
    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), RpcError> {
        crate::image::pull_cluster_image(
            self,
            machine_id,
            &spec.container.image,
            spec.container.pull_policy,
            self.registry_auth.get(&spec.name).cloned(),
        )
        .await
    }

    async fn read_pointer(
        &self,
        service: &QualifiedService,
    ) -> Result<ServingPointerRead, RpcError> {
        self.clone().serving_pointer_read(service).await
    }

    async fn publish_pointer(
        &self,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError> {
        self.clone().publish_serving_pointer(request).await
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
            ContainerWant::Running { .. } => Some(TARGET_RPC_TIMEOUT),
            ContainerWant::Stopped { stop, .. } => stop_rpc_timeout(stop.grace_period_seconds, 1),
            ContainerWant::Gone { .. } => None,
        };
        Client::ensure_settled(self, machine_id, want, timeout).await
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

enum OperationFailure {
    Ordinary(ExecutionError),
    Replacement {
        error: ExecutionError,
        compensation: Box<ReplacementCompensation<ExecutionError>>,
    },
    /// Another Deploy's row points at the copy this one was about to stop.
    Superseded {
        service: QualifiedService,
        current: ServingPointer,
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
            .read::<op::ListContainers>(
                ListContainersRequest {
                    environment: EnvironmentValues::Included,
                },
                &MachineTarget::from(machine_id),
            )
            .await?
            .containers
            .into_iter()
            .find_map(|container| {
                (container.machine_id == *machine_id
                    && container.kind == slot.kind
                    && container.namespace == slot.namespace
                    && container.resolved_spec == *spec)
                    .then(|| container.labels.get("ployz.creation.key").cloned())
                    .flatten()
            })
            .and_then(|key| CreationKey::try_from(key).ok()))
    }
}

impl<C: MachineOperations> MachineOperations for RestartTolerant<'_, C> {
    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError> {
        self.inner.prepare_volumes(machine_id, specs).await
    }

    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        wait_out_restart(self.cancellation, || self.inner.service_containers(service)).await
    }

    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner.ensure_container(machine_id, want.clone())
        })
        .await
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner.inspect_container(machine_id, container_id)
        })
        .await
    }

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        wait_out_restart(self.cancellation, || self.inner.remove_volume(id)).await
    }

    async fn runs_global(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<bool, RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner.runs_global(machine_id, slot, spec)
        })
        .await
    }

    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<(), RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner.ensure_image(machine_id, spec)
        })
        .await
    }

    async fn read_pointer(
        &self,
        service: &QualifiedService,
    ) -> Result<ServingPointerRead, RpcError> {
        wait_out_restart(self.cancellation, || self.inner.read_pointer(service)).await
    }

    async fn publish_pointer(
        &self,
        request: PublishServingPointerRequest,
    ) -> Result<ServingPointerPublication, RpcError> {
        self.inner.publish_pointer(request).await
    }
}

fn is_unavailable(error: &RpcError) -> bool {
    error.code == RpcErrorCode::Unavailable
}

async fn wait_out_restart<T, Fut>(
    cancellation: &CancellationToken,
    mut op: impl FnMut() -> Fut,
) -> Result<T, RpcError>
where
    Fut: Future<Output = Result<T, RpcError>>,
{
    let deadline = Instant::now() + RESTART_WAIT;
    loop {
        match op().await {
            Ok(value) => return Ok(value),
            Err(error)
                if is_unavailable(&error)
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

    if let Err((index, error)) = pull_images(&client, operations).await {
        return untouched(&progress, operations, index, error);
    }

    let tasks = operations
        .iter()
        .map(|operation| Task {
            machine: operation.machine_id(),
            kind: TaskKind::of(operation),
        })
        .collect();
    let mut schedule = Schedule::new(plan.groups().to_vec(), tasks);
    let rows = Rows {
        namespace: &plan.namespace,
        groups: plan.groups(),
        admitted: tokio::sync::Mutex::new(BTreeMap::new()),
    };
    for group in 0..plan.groups().len() {
        let Some(service) = schedule.group(group).publishing_service() else {
            continue;
        };
        let service = QualifiedService::new(plan.namespace.clone(), service.clone());
        let first = schedule.first_task(group);
        let machine_id = operation_at(operations, first).machine_id();
        match await_routed_pin(&client, run, &service, machine_id, cancellation).await {
            Ok(Pin::Admitted(row)) => {
                rows.admitted.lock().await.insert(group, row);
            }
            Ok(Pin::Superseded) => {
                progress.settle_unexecuted();
                let outcome = outcome_from(
                    operations,
                    &BTreeSet::new(),
                    Some(Stop::Superseded(service)),
                );
                progress.outcome(outcome.clone());
                return outcome;
            }
            Err(error) => return untouched(&progress, operations, first, error),
        }
    }

    let mut started: BTreeMap<usize, Vec<(MachineId, ContainerId)>> = BTreeMap::new();
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
                Action::Start(index) => Either::Left({
                    let group = schedule.group_of(index);
                    async move {
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
                                group,
                            )
                            .await
                        };
                        Done::Task(index, result)
                    }
                }),
                Action::Publish(group) => {
                    let spec = schedule.group(group).clone();
                    let new = started.get(&group).cloned().unwrap_or_default();
                    let current = rows.admitted.lock().await.get(&group).cloned();
                    let pointer = proposed(current.as_ref(), run, &spec, &new);
                    Either::Right(async move {
                        let result = if cancellation.is_cancelled() {
                            Err(ExecutionError::Cancelled)
                        } else {
                            let service = QualifiedService::new(
                                plan.namespace.clone(),
                                spec.service.clone().expect("only Service groups publish"),
                            );
                            publish_group(client, service, current, &pointer).await
                        };
                        Done::Publish(group, pointer, result)
                    })
                }
            };
            inflight.push(next);
        }
        let blocked = schedule.blocked_on_acks();
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
                    Ok(id) => *id,
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
                        .push((operation.machine_id(), id));
                }
                match result {
                    Ok(_) => {
                        if schedule.group(group).publish && starts_container(operation) {
                            awaiting_flip.entry(group).or_default().push(index);
                        } else {
                            completed.insert(index);
                            progress.set_completed(index);
                        }
                        schedule.apply(Event::Completed(index));
                    }
                    Err(OperationFailure::Superseded { service, current }) => {
                        retain_unpointed(started.entry(group).or_default(), Some(&current));
                        stop.get_or_insert(Stop::Superseded(service));
                        schedule.apply(Event::Failed(index));
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
            Some(Done::Publish(group, pointer, result)) => {
                let service = QualifiedService::new(
                    plan.namespace.clone(),
                    schedule
                        .group(group)
                        .service
                        .clone()
                        .expect("only Service groups publish"),
                );
                let new = started.entry(group).or_default();
                let result = match result {
                    Ok(publication) => Ok(publication),
                    Err(_) if publish_landed(client, &service, &pointer).await => {
                        Ok(ServingPointerPublication::Published)
                    }
                    Err(error) => {
                        unread.insert(group);
                        Err(error)
                    }
                };
                match result {
                    Ok(ServingPointerPublication::Published) => {
                        for index in awaiting_flip.remove(&group).unwrap_or_default() {
                            completed.insert(index);
                            progress.set_completed(index);
                        }
                        schedule.apply(Event::Published(group));
                    }
                    Ok(ServingPointerPublication::Superseded { current }) => {
                        retain_unpointed(new, current.as_ref());
                        stop.get_or_insert(Stop::Superseded(service));
                        schedule.apply(Event::PublishFailed(group));
                    }
                    Err(error) => {
                        let index = schedule.publish_failure_task(group);
                        progress.fail(index, error.clone());
                        stop.get_or_insert(Stop::Failed {
                            index,
                            error,
                            compensation: None,
                        });
                        schedule.apply(Event::PublishFailed(group));
                    }
                }
            }
            None => {
                let expired = blocked_since.is_some_and(|since| since.elapsed() >= ACK_WAIT);
                let mut failed = BTreeSet::new();
                if cancellation.is_cancelled() || expired {
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
                    let services: BTreeSet<_> =
                        blocked.iter().map(|(_, service)| service).collect();
                    for service in services {
                        let qualified =
                            QualifiedService::new(plan.namespace.clone(), service.clone());
                        match client.read_pointer(&qualified).await {
                            Ok(read) => {
                                if read.pointer.is_some_and(|pointer| &pointer.deploy == run) {
                                    if read.all_routed {
                                        schedule.apply(Event::Routed(service.clone()));
                                    }
                                    for (machine, deploy) in read.acks {
                                        if &deploy == run {
                                            schedule.apply(Event::Acked {
                                                service: service.clone(),
                                                machine,
                                            });
                                        }
                                    }
                                }
                            }
                            Err(error) => {
                                let error = machine_error(MachineAction::ReadServingPointer, error);
                                for (task, _) in blocked.iter().filter(|(task, blocker)| {
                                    blocker == service && failed.insert(*task)
                                }) {
                                    progress.fail(*task, error.clone());
                                    stop.get_or_insert(Stop::Failed {
                                        index: *task,
                                        error: error.clone(),
                                        compensation: None,
                                    });
                                    schedule.apply(Event::Failed(*task));
                                }
                            }
                        }
                    }
                }
            }
        }
        for group in schedule.failed_groups() {
            if unread.contains(&group) {
                continue;
            }
            for (machine, id) in started.remove(&group).into_iter().flatten() {
                let _ = gone(
                    client,
                    &machine,
                    ContainerLocator::Id(id),
                    Drain::NONE,
                    AnonymousVolumes::Remove,
                )
                .await;
            }
        }
    }

    progress.settle_unexecuted();
    let outcome = outcome_from(operations, &completed, stop);
    progress.outcome(outcome.clone());
    outcome
}

/// The Deploy ends at `index` before any Container changed.
fn untouched(
    progress: &Progress,
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

/// Whether a publish whose reply was lost wrote `pointer`. A row from an
/// earlier attempt of the same Deploy carries its id but not this membership,
/// so only the whole row counts. Any other answer leaves the write pending, and
/// the group keeps every started copy a late commit may still point at.
async fn publish_landed<C: MachineOperations>(
    client: &C,
    service: &QualifiedService,
    pointer: &ServingPointer,
) -> bool {
    client
        .read_pointer(service)
        .await
        .is_ok_and(|read| read.pointer.as_ref() == Some(pointer))
}

/// Keeps only the started Containers `pointer` does not name, so cleanup never
/// removes a routed copy.
fn retain_unpointed(started: &mut Vec<(MachineId, ContainerId)>, pointer: Option<&ServingPointer>) {
    let Some(pointer) = pointer else {
        return;
    };
    started.retain(|(_, id)| !pointer.containers.contains(id) && !pointer.retiring.contains(id));
}

/// The row each later publish of a Service replaces, or Superseded when
/// another Deploy wrote one first.
enum Pin {
    Admitted(ServingPointer),
    Superseded,
}

async fn await_routed_pin<C: MachineOperations>(
    client: &C,
    run: &DeploymentLogId,
    service: &QualifiedService,
    machine_id: MachineId,
    cancellation: &CancellationToken,
) -> Result<Pin, ExecutionError> {
    let read = client
        .read_pointer(service)
        .await
        .map_err(|error| machine_error(MachineAction::ReadServingPointer, error))?;
    let pointer = match read.pointer {
        Some(pointer) if read.all_routed => return Ok(Pin::Admitted(pointer)),
        current => match live_pin(client, run, service, current).await? {
            Some(pointer) => pointer,
            None => return Ok(Pin::Superseded),
        },
    };
    let since = Instant::now();
    loop {
        let read = client
            .read_pointer(service)
            .await
            .map_err(|error| machine_error(MachineAction::ReadServingPointer, error))?;
        if read.pointer.as_ref() != Some(&pointer) {
            return Ok(Pin::Superseded);
        }
        if read.all_routed {
            return Ok(Pin::Admitted(pointer));
        }
        if cancellation.is_cancelled() {
            return Err(ExecutionError::Cancelled);
        }
        if since.elapsed() >= ACK_WAIT {
            return Err(ExecutionError::DependencyHealth {
                dependency: service.clone(),
                failure: DependencyHealthFailure::NotRouted { machine_id },
            });
        }
        ack_tick(cancellation, Some(since)).await;
    }
}

async fn live_pin<C: MachineOperations>(
    client: &C,
    run: &DeploymentLogId,
    service: &QualifiedService,
    current: Option<ServingPointer>,
) -> Result<Option<ServingPointer>, ExecutionError> {
    let observed = client
        .service_containers(service)
        .await
        .map_err(|error| machine_error(MachineAction::ListContainers, error))?;
    let members = ployz_core::service_containers(observed);
    let serving = ployz_core::serving_containers(&members, |_| None);
    let deploy = serving
        .iter()
        .map(|serving| serving.as_observation())
        .max_by_key(|observation| (observation.created_at_unix_nanos, observation.container_id))
        .and_then(|newest| newest.labels.get(ployz_core::DEPLOYMENT_ID_LABEL))
        .and_then(|label| DeploymentLogId::parse(label).ok())
        .unwrap_or_else(|| {
            DeploymentLogId::parse(format!("pin-{run}")).expect("a run id prefixed stays valid")
        });
    let pointer = ServingPointer {
        deploy,
        containers: serving
            .iter()
            .map(|serving| serving.as_observation().container_id)
            .collect(),
        previous: None,
        retiring: Vec::new(),
    };
    if current.as_ref().is_some_and(|current| {
        current
            .containers
            .iter()
            .all(|id| pointer.containers.contains(id))
    }) {
        return Ok(current);
    }
    let publication = client
        .publish_pointer(PublishServingPointerRequest {
            service: service.clone(),
            read: current,
            pointer: pointer.clone(),
        })
        .await
        .map_err(|error| machine_error(MachineAction::PublishServingPointer, error))?;
    Ok(match publication {
        ServingPointerPublication::Published => Some(pointer),
        ServingPointerPublication::Superseded { .. } => None,
    })
}

enum Done {
    Task(usize, Result<Option<ContainerId>, OperationFailure>),
    Publish(
        usize,
        ServingPointer,
        Result<ServingPointerPublication, ExecutionError>,
    ),
}

async fn pull_images<C: MachineOperations>(
    client: &C,
    operations: &[DeployOperation],
) -> Result<(), (usize, ExecutionError)> {
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
    results
        .into_iter()
        .filter_map(Result::err)
        .min_by_key(|(index, _)| *index)
        .map_or(Ok(()), Err)
}

fn strength(policy: PullPolicy) -> u8 {
    match policy {
        PullPolicy::Never => 0,
        PullPolicy::Missing => 1,
        PullPolicy::Always => 2,
    }
}

/// The row this run publishes for `group`: the kept copies plus every started one.
fn proposed(
    current: Option<&ServingPointer>,
    run: &DeploymentLogId,
    group: &ServiceGroup,
    started: &[(MachineId, ContainerId)],
) -> ServingPointer {
    let mut containers = group.kept.clone();
    for (_, id) in started {
        if !containers.contains(id) {
            containers.push(*id);
        }
    }
    succession(current, run.clone(), containers)
}

async fn publish_group<C: MachineOperations>(
    client: &C,
    service: QualifiedService,
    current: Option<ServingPointer>,
    pointer: &ServingPointer,
) -> Result<ServingPointerPublication, ExecutionError> {
    client
        .publish_pointer(PublishServingPointerRequest {
            service,
            read: current,
            pointer: pointer.clone(),
        })
        .await
        .map_err(|error| machine_error(MachineAction::PublishServingPointer, error))
}

struct Rows<'a> {
    namespace: &'a Namespace,
    groups: &'a [ServiceGroup],
    admitted: tokio::sync::Mutex<BTreeMap<usize, ServingPointer>>,
}

impl Rows<'_> {
    fn service_of(&self, group: usize, operation: &DeployOperation) -> Option<QualifiedService> {
        let group = self.groups.get(group)?;
        let name = match (&group.service, group.dependencies.as_slice()) {
            (Some(name), _) => name,
            (None, [followed]) if TaskKind::of(operation) == TaskKind::Unroute => followed,
            (None, _) => return None,
        };
        Some(QualifiedService::new(self.namespace.clone(), name.clone()))
    }

    async fn unpointed<C: MachineOperations>(
        &self,
        client: &C,
        group: usize,
        operation: &DeployOperation,
        copy: &ContainerId,
    ) -> Result<(), ExecutionError> {
        let Some(service) = self.service_of(group, operation) else {
            return Ok(());
        };
        let read = client
            .read_pointer(&service)
            .await
            .map_err(|error| machine_error(MachineAction::ReadServingPointer, error))?;
        match Protection::of(read.pointer, copy) {
            Protection::Unnamed | Protection::Retiring(_) => Ok(()),
            Protection::Pointed(row) => Err(pointed(copy, &row)),
        }
    }

    async fn retire_before_stop<C: MachineOperations>(
        &self,
        client: &C,
        group: usize,
        operation: &DeployOperation,
        copy: &ContainerId,
    ) -> Result<(), OperationFailure> {
        let Some(service) = self.service_of(group, operation) else {
            return Ok(());
        };
        let mut admitted = self.admitted.lock().await;
        let mut current = client
            .read_pointer(&service)
            .await
            .map_err(|error| machine_error(MachineAction::ReadServingPointer, error))?
            .pointer;
        for _ in 0..=AMEND_RETRIES {
            let row = match Protection::of(current, copy) {
                Protection::Unnamed | Protection::Retiring(_) => return Ok(()),
                Protection::Pointed(row) => row,
            };
            if admitted
                .get(&group)
                .is_some_and(|admitted| admitted.deploy != row.deploy)
            {
                return Err(OperationFailure::Superseded {
                    service,
                    current: row,
                });
            }
            let Applied::Next(next) = Rotation::leaving([*copy]).apply(&row) else {
                return Ok(());
            };
            let publication = client
                .publish_pointer(PublishServingPointerRequest {
                    service: service.clone(),
                    read: Some(row),
                    pointer: next.clone(),
                })
                .await
                .map_err(|error| machine_error(MachineAction::PublishServingPointer, error))?;
            match publication {
                ServingPointerPublication::Published => {
                    admitted.insert(group, next);
                    return Ok(());
                }
                ServingPointerPublication::Superseded { current: newer } => current = newer,
            }
        }
        Err(machine_error(
            MachineAction::PublishServingPointer,
            RpcError {
                code: RpcErrorCode::Conflict,
                message: format!(
                    "lost the Serving Pointer of {service} to other writers {} times",
                    AMEND_RETRIES + 1
                ),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            },
        )
        .into())
    }
}

fn pointed(copy: &ContainerId, row: &ServingPointer) -> ExecutionError {
    machine_error(
        MachineAction::RemoveContainer,
        RpcError {
            code: RpcErrorCode::Conflict,
            message: format!(
                "Container {copy} is pointed at by Deploy {}; a pointed copy is not removed",
                row.deploy
            ),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        },
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "one call site; the rows travel with the slots"
)]
async fn execute_operation<C: MachineOperations>(
    operation: &DeployOperation,
    index: usize,
    progress: &Progress,
    client: &C,
    cancellation: &CancellationToken,
    slots: &Slots<'_>,
    rows: &Rows<'_>,
    group: usize,
) -> Result<Option<ContainerId>, OperationFailure> {
    progress.set_running(index, OperationPhase::Starting);
    match operation {
        DeployOperation::PrepareVolumes { machine_id, specs } => client
            .prepare_volumes(machine_id, specs)
            .await
            .map(|_| None)
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
            wait_healthy(client, index, progress, dependency, cancellation)
                .await
                .map(|()| None)
                .map_err(Into::into)
        }
        DeployOperation::RunContainer {
            machine_id,
            spec,
            skip_health_monitor,
        } => {
            let slot = slots.slot(ContainerKind::ServiceContainer, spec, index, None);
            run_container(
                client,
                index,
                progress,
                machine_id,
                slot,
                spec,
                *skip_health_monitor,
                cancellation,
            )
            .await
            .map(Some)
            .map_err(Into::into)
        }
        DeployOperation::StopContainer {
            machine_id,
            container_id,
            ..
        } => {
            rows.retire_before_stop(client, group, operation, container_id)
                .await?;
            progress.set_running(index, OperationPhase::StoppingContainer);
            stop(client, machine_id, container_id, Drain::DEFAULT, None)
                .await
                .map(|()| None)
                .map_err(Into::into)
        }
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
            purpose,
        } => {
            rows.unpointed(client, group, operation, container_id)
                .await?;
            progress.set_running(index, OperationPhase::RemovingContainer);
            gone(
                client,
                machine_id,
                ContainerLocator::Id(*container_id),
                Drain::DEFAULT,
                (*purpose).into(),
            )
            .await
            .map(|()| None)
            .map_err(Into::into)
        }
        DeployOperation::ReplaceContainer(replacement) => {
            let slot = slots.slot(
                ContainerKind::ServiceContainer,
                &replacement.spec,
                index,
                Some(replacement.old_container_id),
            );
            if replacement.spec.update.order == UpdateOrder::StopFirst {
                rows.retire_before_stop(client, group, operation, &replacement.old_container_id)
                    .await?;
                return replace_stop_first(
                    client,
                    index,
                    progress,
                    replacement,
                    slot,
                    cancellation,
                )
                .await;
            }
            // The old Container keeps serving until the Service's pointer flips;
            // it retires with the pointer, not here.
            start(
                client,
                index,
                progress,
                &replacement.machine_id,
                slot,
                &replacement.spec,
                answer_when(replacement.skip_health_monitor),
                FailedStart::Discard,
                cancellation,
            )
            .await
            .map(Some)
            .map_err(Into::into)
        }
        DeployOperation::StopHook {
            machine_id,
            container_id,
        } => {
            progress.set_running(index, OperationPhase::StoppingContainer);
            stop(client, machine_id, container_id, Drain::NONE, None)
                .await
                .map(|()| None)
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
            .map(|()| None)
            .map_err(Into::into)
        }
        DeployOperation::RemoveVolume { id } => {
            progress.set_running(index, OperationPhase::RemovingVolume);
            ignore_not_found(client.remove_volume(id).await)
                .map(|()| None)
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
    let progress = Progress::new(Vec::new(), None);
    let started = run_new(
        &client,
        0,
        &progress,
        to,
        slot.clone(),
        (spec, deploy),
        AnswerWhen::Settled,
        cancellation,
    )
    .await;
    let new = match started_container(started, cancellation) {
        Ok(new) => new,
        Err(error) => {
            let keep = adopts || (global && !cancellation.is_cancelled());
            let replacement_removed = !keep && discard_attempt(&client, to, slot).await;
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
    if cleanup && !keep {
        discard_attempt(client, machine_id, slot).await;
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
) -> bool {
    let discarded = gone(
        client,
        machine_id,
        ContainerLocator::Key(slot),
        Drain::NONE,
        AnonymousVolumes::Remove,
    );
    matches!(
        tokio::time::timeout(TARGET_RPC_TIMEOUT, discarded).await,
        Ok(Ok(_))
    )
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
    answer: AnswerWhen,
    cancellation: &CancellationToken,
) -> Result<(ContainerId, Option<HealthVerdict>), RpcError> {
    progress.set_running(index, OperationPhase::CreatingContainer);
    let want = ContainerWant::Running {
        container: RunTarget::New(NewContainer {
            slot: slot.clone(),
            resolved_spec: spec.clone(),
            deployment_id: deploy.cloned(),
            registry_auth: None,
            image_prepared: true,
            answer,
        }),
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

async fn replace_stop_first<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    operation: &ReplacementOperation,
    slot: SlotKey,
    cancellation: &CancellationToken,
) -> Result<Option<ContainerId>, OperationFailure> {
    // Stops, not removes, the old Container first, so a failed new one can hand
    // the slot back to it.
    let old_stopped = {
        let old = match client
            .inspect_container(&operation.machine_id, &operation.old_container_id)
            .await
        {
            Ok(old) => Some(old.container),
            Err(error) if error.code == RpcErrorCode::NotFound => None,
            Err(error) => {
                return Err(machine_error(MachineAction::InspectContainer, error).into());
            }
        };
        match old {
            // A stop holds an old container at rest too: its Server restarts an
            // ingress whose start failed unless a request stopped it.
            Some(old) => {
                progress.set_running(index, OperationPhase::StoppingContainer);
                stop(
                    client,
                    &operation.machine_id,
                    &operation.old_container_id,
                    Drain::DEFAULT,
                    None,
                )
                .await?;
                // An ingress at rest is handed back by a start, which lifts the
                // hold even when the start fails again. That includes one a user
                // stopped. Any other container at rest stays down, and one in a
                // runtime this client does not recognize is handed back.
                match old.runtime {
                    ContainerRuntimeObservation::Created
                    | ContainerRuntimeObservation::Exited { .. }
                    | ContainerRuntimeObservation::Dead => crate::ingress::is_system_ingress(&old),
                    ContainerRuntimeObservation::Removing => false,
                    ContainerRuntimeObservation::Running { .. }
                    | ContainerRuntimeObservation::Paused
                    | ContainerRuntimeObservation::Restarting
                    | ContainerRuntimeObservation::Unknown { .. } => true,
                }
            }
            None => false,
        }
    };

    let started = start(
        client,
        index,
        progress,
        &operation.machine_id,
        slot,
        &operation.spec,
        answer_when(operation.skip_health_monitor),
        FailedStart::Retain,
        cancellation,
    )
    .await;
    let new = match started {
        Ok(new) => new,
        Err(error) => {
            let new_container = if let ExecutionError::Health { container_id, .. } = &error {
                Some(*container_id)
            } else {
                None
            };
            return Err(compensate(
                client,
                index,
                progress,
                operation,
                old_stopped,
                new_container,
                error,
            )
            .await);
        }
    };

    progress.set_running(index, OperationPhase::RemovingContainer);
    remove(
        client,
        &operation.machine_id,
        &operation.old_container_id,
        Drain::DEFAULT,
    )
    .await
    .map(|()| Some(new))
    .map_err(Into::into)
}

// A failure after the old container stopped restores it; otherwise only a health
// failure is compensated, by removing the new container.
async fn compensate<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    operation: &ReplacementOperation,
    old_stopped: bool,
    new_container: Option<ContainerId>,
    error: ExecutionError,
) -> OperationFailure {
    if !old_stopped && !matches!(&error, ExecutionError::Health { .. }) {
        return error.into();
    }
    progress.set_running(index, OperationPhase::Compensating);
    let stop_new_container = match new_container {
        Some(container_id) => Some(StopAttempt::from(
            remove(client, &operation.machine_id, &container_id, Drain::NONE).await,
        )),
        None => None,
    };
    let compensation = match (old_stopped, stop_new_container) {
        (true, stop_new_container) => ReplacementCompensation::OldStopped {
            stop_new_container,
            restart_old_container: restore_old_container(client, operation).await,
        },
        (false, Some(stop_new_container)) => {
            ReplacementCompensation::OldUntouched { stop_new_container }
        }
        // A health failure always follows a started container.
        (false, None) => return error.into(),
    };
    OperationFailure::Replacement {
        error,
        compensation: Box::new(compensation),
    }
}

async fn restore_old_container<C: MachineOperations>(
    client: &C,
    operation: &ReplacementOperation,
) -> RestartAttempt<ExecutionError> {
    let restored = client
        .ensure_container(
            &operation.machine_id,
            ContainerWant::Running {
                container: RunTarget::Existing(ContainerLocator::Id(operation.old_container_id)),
            },
        )
        .await
        .map(|_| ())
        .map_err(|error| machine_error(MachineAction::StartContainer, error));
    RestartAttempt::from(restored)
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
