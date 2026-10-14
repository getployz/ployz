use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    time::Duration,
};

use futures_util::future::{Either, join_all};
use ployz_core::{
    AnonymousVolumes, AnswerWhen, ContainerDetails, ContainerId, ContainerKind, ContainerLocator,
    ContainerNow, ContainerObservation, ContainerRuntimeObservation, ContainerWant, CreationKey,
    DependencyHealthFailure, DeployEvent, DeploymentLogId, DockerVolumeId, Drain, ExecutionError,
    FailedOperation, HEALTH_POLL_INTERVAL, HealthFailure, HealthVerdict, HookFailure,
    InspectContainerRequest, ListContainersRequest, MachineAction, MachineId, MachineTarget,
    MembershipObservation, Namespace, NewContainer, OperationPhase, PublishServingPointerRequest,
    PullPolicy, QualifiedService, RemoveVolumeRequest, ResolvedServiceSpec, RpcError, RpcErrorCode,
    RunTarget, ServiceOutcome, ServingPointer, ServingPointerPublication, ServingPointerRead,
    SlotKey, StopOptions, op,
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
    schedule::{Action, Event, Schedule, ServiceGroup, Task, starts_container},
};

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
    /// Make `image` present on `machine_id` before any Container changes.
    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        image: &str,
        policy: PullPolicy,
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
        image: &str,
        policy: PullPolicy,
    ) -> Result<(), RpcError> {
        crate::image::ensure_cluster_image(self, machine_id, image, policy).await
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
                new.deployment_id.clone_from(&self.deployment_id);
                new.registry_auth = self.registry_auth.get(&new.resolved_spec.name).cloned();
                crate::image::ensure_cluster_image(
                    self,
                    machine_id,
                    &new.resolved_spec.container.image,
                    new.resolved_spec.container.pull_policy,
                )
                .await?;
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

/// What a completed operation left behind.
#[derive(Default)]
struct Ran {
    /// The new Container it started.
    started: Option<ContainerId>,
    /// It stopped an old Container that a failed Service starts again.
    stopped_old: bool,
}

impl Ran {
    fn started(id: ContainerId) -> Self {
        Self {
            started: Some(id),
            stopped_old: false,
        }
    }
}

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
        image: &str,
        policy: PullPolicy,
    ) -> Result<(), RpcError> {
        wait_out_restart(self.cancellation, || {
            self.inner.ensure_image(machine_id, image, policy)
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
    let tasks = operations
        .iter()
        .map(|operation| Task {
            machine: operation.machine_id(),
            starts: starts_container(operation),
        })
        .collect();
    let mut schedule = Schedule::new(plan.groups().to_vec(), tasks);

    if let Err((index, error)) = pull_images(&client, operations).await {
        progress.fail(index, error.clone());
        progress.settle_unexecuted();
        let outcome = outcome_from(
            operations,
            &completed,
            Some(Stop::Failed {
                index,
                error,
                compensation: None,
            }),
            schedule.services(),
        );
        progress.outcome(outcome.clone());
        return outcome;
    }

    let mut started: BTreeMap<usize, Vec<(MachineId, ContainerId)>> = BTreeMap::new();
    let mut replaced: BTreeMap<usize, Vec<ContainerId>> = BTreeMap::new();
    let mut stopped: BTreeMap<usize, Vec<&ReplacementOperation>> = BTreeMap::new();
    let mut cleaned = BTreeSet::new();
    let mut stop = None;
    let mut blocked_since: Option<tokio::time::Instant> = None;

    while !schedule.finished() {
        let actions = schedule.next();
        if actions.is_empty() {
            let blocked = schedule.blocked_on_acks();
            if blocked.is_empty() {
                break;
            }
            let expired = blocked_since.is_some_and(|since| since.elapsed() >= ACK_WAIT);
            if cancellation.is_cancelled() || expired {
                for (task, dependency) in blocked {
                    let error = if cancellation.is_cancelled() {
                        ExecutionError::Cancelled
                    } else {
                        ExecutionError::DependencyHealth {
                            dependency: QualifiedService::new(plan.namespace.clone(), dependency),
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
                continue;
            }
            if blocked_since.is_some() {
                tokio::select! {
                    () = cancellation.cancelled() => {}
                    () = tokio::time::sleep(ACK_POLL) => {}
                }
            } else {
                blocked_since = Some(tokio::time::Instant::now());
            }
            let services: BTreeSet<_> = blocked.iter().map(|(_, service)| service).collect();
            for service in services {
                let qualified = QualifiedService::new(plan.namespace.clone(), service.clone());
                match client.read_pointer(&qualified).await {
                    Ok(read) => {
                        if read.pointer.is_some_and(|pointer| &pointer.deploy == run) {
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
                        for (task, _) in blocked.iter().filter(|(_, blocker)| blocker == service) {
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
            continue;
        }
        blocked_since = None;

        let (progress, client, slots) = (&progress, &client, &slots);
        let round = actions.into_iter().map(|action| match action {
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
                    )
                    .await
                };
                Done::Task(index, result)
            }),
            Action::Publish(group) => {
                let spec = schedule.group(group).clone();
                let new = started.get(&group).cloned().unwrap_or_default();
                let old = replaced.get(&group).cloned().unwrap_or_default();
                Either::Right(async move {
                    let result = if cancellation.is_cancelled() {
                        Err(ExecutionError::Cancelled)
                    } else {
                        publish_group(client, run, &plan.namespace, &spec, &new, &old).await
                    };
                    Done::Publish(group, result)
                })
            }
        });
        for done in join_all(round).await {
            match done {
                Done::Task(index, result) => {
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
                            .push((operation.machine_id(), id));
                    }
                    match result {
                        Ok(ran) => {
                            completed.insert(index);
                            progress.set_completed(index);
                            if let DeployOperation::ReplaceContainer(replacement) = operation {
                                replaced
                                    .entry(group)
                                    .or_default()
                                    .push(replacement.old_container_id);
                                if ran.stopped_old {
                                    stopped.entry(group).or_default().push(replacement);
                                }
                            }
                            schedule.apply(Event::Completed(index));
                        }
                        Err(failure) => {
                            let (error, compensation) = match failure {
                                OperationFailure::Ordinary(error) => (error, None),
                                OperationFailure::Replacement {
                                    error,
                                    compensation,
                                } => (error, Some(*compensation)),
                            };
                            progress.fail(index, error.clone());
                            stop.get_or_insert(Stop::Failed {
                                index,
                                error,
                                compensation,
                            });
                            schedule.apply(Event::Failed(index));
                        }
                    }
                }
                Done::Publish(group, Ok(ServingPointerPublication::Published)) => {
                    schedule.apply(Event::Published(group));
                }
                Done::Publish(group, Ok(ServingPointerPublication::Superseded { .. })) => {
                    let service = schedule.group(group).service.clone();
                    stop.get_or_insert(Stop::Superseded(QualifiedService::new(
                        plan.namespace.clone(),
                        service.expect("only Service groups publish"),
                    )));
                    schedule.apply(Event::PublishFailed(group));
                }
                Done::Publish(group, Err(error)) => {
                    // A publish has no row of its own; its Service's last
                    // operation carries the failure.
                    let index = schedule.last_task(group);
                    completed.remove(&index);
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
        for group in schedule.failed_groups() {
            if cleaned.insert(group) {
                restore_group(
                    client,
                    started.get(&group).map_or(&[], Vec::as_slice),
                    stopped.get(&group).map_or(&[], Vec::as_slice),
                )
                .await;
            }
        }
    }

    progress.settle_unexecuted();
    let outcome = outcome_from(operations, &completed, stop, schedule.services());
    progress.outcome(outcome.clone());
    outcome
}

/// Put a Service that did not switch back on its old copy: remove every new
/// Container, then start every old one a stop-first replacement stopped.
async fn restore_group<C: MachineOperations>(
    client: &C,
    started: &[(MachineId, ContainerId)],
    stopped: &[&ReplacementOperation],
) {
    join_all(started.iter().map(|(machine, id)| {
        gone(
            client,
            machine,
            ContainerLocator::Id(*id),
            Drain::NONE,
            AnonymousVolumes::Remove,
        )
    }))
    .await;
    join_all(
        stopped
            .iter()
            .map(|operation| restore_old_container(client, operation)),
    )
    .await;
}

enum Done {
    Task(usize, Result<Ran, OperationFailure>),
    Publish(usize, Result<ServingPointerPublication, ExecutionError>),
}

async fn pull_images<C: MachineOperations>(
    client: &C,
    operations: &[DeployOperation],
) -> Result<(), (usize, ExecutionError)> {
    let mut pulls: BTreeMap<(MachineId, String), (usize, PullPolicy)> = BTreeMap::new();
    for (index, operation) in operations.iter().enumerate() {
        let Some(spec) = operation.spec() else {
            continue;
        };
        pulls
            .entry((operation.machine_id(), spec.container.image.clone()))
            .or_insert((index, spec.container.pull_policy));
    }
    let results = join_all(
        pulls
            .iter()
            .map(|((machine, image), (index, policy))| async move {
                client
                    .ensure_image(machine, image, *policy)
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

async fn publish_group<C: MachineOperations>(
    client: &C,
    run: &DeploymentLogId,
    namespace: &Namespace,
    group: &ServiceGroup,
    started: &[(MachineId, ContainerId)],
    replaced: &[ContainerId],
) -> Result<ServingPointerPublication, ExecutionError> {
    let service = QualifiedService::new(
        namespace.clone(),
        group.service.clone().expect("only Service groups publish"),
    );
    let read = client
        .read_pointer(&service)
        .await
        .map_err(|error| machine_error(MachineAction::ReadServingPointer, error))?;
    let mut containers: Vec<ContainerId> = read
        .pointer
        .iter()
        .flat_map(|current| current.containers.iter().copied())
        .filter(|id| !replaced.contains(id))
        .collect();
    containers.extend(started.iter().map(|(_, id)| *id));
    let mut retiring: Vec<ContainerId> = read
        .pointer
        .iter()
        .flat_map(|current| current.retiring.iter().copied())
        .collect();
    for id in replaced {
        if !retiring.contains(id) {
            retiring.push(*id);
        }
    }
    let pointer = ServingPointer {
        deploy: run.clone(),
        containers,
        previous: read.pointer.as_ref().map_or_else(
            || group.previous.clone(),
            |current| Some(current.deploy.clone()),
        ),
        retiring,
    };
    client
        .publish_pointer(PublishServingPointerRequest {
            service,
            read: read.pointer,
            pointer,
        })
        .await
        .map_err(|error| machine_error(MachineAction::PublishServingPointer, error))
}

async fn execute_operation<C: MachineOperations>(
    operation: &DeployOperation,
    index: usize,
    progress: &Progress,
    client: &C,
    cancellation: &CancellationToken,
    slots: &Slots<'_>,
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
            wait_healthy(client, index, progress, dependency, cancellation)
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
            .map(Ran::started)
            .map_err(Into::into)
        }
        DeployOperation::StopContainer {
            machine_id,
            container_id,
            ..
        } => {
            progress.set_running(index, OperationPhase::StoppingContainer);
            stop(client, machine_id, container_id, Drain::DEFAULT, None)
                .await
                .map(|()| Ran::default())
                .map_err(Into::into)
        }
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
            purpose,
        } => {
            progress.set_running(index, OperationPhase::RemovingContainer);
            gone(
                client,
                machine_id,
                ContainerLocator::Id(*container_id),
                Drain::DEFAULT,
                (*purpose).into(),
            )
            .await
            .map(|()| Ran::default())
            .map_err(Into::into)
        }
        DeployOperation::ReplaceContainer(replacement) => {
            let slot = slots.slot(
                ContainerKind::ServiceContainer,
                &replacement.spec,
                index,
                Some(replacement.old_container_id),
            );
            if replacement.spec.update.order.stops_first() {
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

/// Placement convergence's one move: start `spec` on `to`, wait until it is healthy,
/// then remove `old` from `from`. A new Container that never gets healthy is removed
/// again, so `old` keeps serving. No hooks run.
///
/// # Errors
///
/// [`MoveContainerError::NotServing`] when the new Container never served;
/// [`MoveContainerError::OldNotRemoved`] when it serves but `old` could not be removed.
pub(in crate::deploy) async fn move_container<C: MachineOperations>(
    client: &C,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    to: &MachineId,
    (from, old): (&MachineId, &ContainerId),
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
        spec,
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
    // Stopped first, so a failed removal can say whether the old Container still serves.
    stop(&client, from, old, Drain::DEFAULT, None)
        .await
        .map_err(|error| MoveContainerError::OldNotRemoved {
            error,
            old_stopped: false,
        })?;
    remove(&client, from, old, Drain::NONE)
        .await
        .map_err(|error| MoveContainerError::OldNotRemoved {
            error,
            old_stopped: true,
        })?;
    Ok(new)
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
        spec,
        answer,
        cancellation,
    )
    .await;
    let cleanup = match &started {
        Ok(_) => cancellation.is_cancelled(),
        // A slot held by another spec never held this attempt's Container.
        Err(error) => error.code != RpcErrorCode::Conflict,
    };
    let keep = adopts || (keyed_by_spec && !cancellation.is_cancelled());
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
    spec: &ResolvedServiceSpec,
    answer: AnswerWhen,
    cancellation: &CancellationToken,
) -> Result<(ContainerId, Option<HealthVerdict>), RpcError> {
    progress.set_running(index, OperationPhase::CreatingContainer);
    let want = ContainerWant::Running {
        container: RunTarget::New(NewContainer {
            slot: slot.clone(),
            resolved_spec: spec.clone(),
            deployment_id: None,
            registry_auth: None,
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
) -> Result<Ran, OperationFailure> {
    // Stops, not removes, the old Container: it retires with the Service's
    // pointer, and a failed Service starts it again.
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
        cancellation,
    )
    .await;
    match started {
        Ok(new_container) => Ok(Ran {
            started: Some(new_container),
            stopped_old: old_stopped,
        }),
        Err(error) => {
            let new_container = if let ExecutionError::Health { container_id, .. } = &error {
                Some(*container_id)
            } else {
                None
            };
            Err(compensate(
                client,
                index,
                progress,
                operation,
                old_stopped,
                new_container,
                error,
            )
            .await)
        }
    }
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
