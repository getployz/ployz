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
    AnonymousVolumes, AnswerWhen, ContainerDetails, ContainerId, ContainerKind,
    ContainerLimitsUpdated, ContainerLocator, ContainerNow, ContainerObservation, ContainerRefusal,
    ContainerRefusalReason, ContainerRuntimeObservation, ContainerWant, CopyRotation, CreationKey,
    DependencyHealthFailure, DeployEvent, DeployWarning, DeploymentLogId, DockerVolumeId, Drain,
    EnsuredContainer, ExecutionError, ExposedPort, FailedOperation, Gen, HEALTH_POLL_INTERVAL,
    HealthFailure, HealthVerdict, HookFailure, ImagePulled, InspectContainerRequest,
    ListContainersRequest, MachineAction, MachineFailure, MachineId, MachineName, MachineTarget,
    MembershipObservation, Namespace, NewContainer, ObservationGap, ObservationGapReason,
    ObservationKind, OperationPhase, OutOfMemory, PortPublication, PullPolicy, QualifiedService,
    ReadRoutesRequest, RemoveContainerPurpose, RemoveVolumeRequest, ResolvedServiceSpec, Retry,
    RoutesRead, RpcError, RpcErrorCode, RunRotation, RunTarget, ServiceId, ServiceName,
    ServiceOutcome, SlotKey, StopOptions, TransportProtocol, UpdateContainerLimitsRequest,
    UpdateOrder, op,
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
/// How long a Deploy waits on the entry Machine's store for its gen floor.
const STORED_GENS_WAIT: Duration = Duration::from_secs(5);
const RESTART_POLL: Duration = Duration::from_millis(250);

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

/// One Service's copies on the Machines that answered, and each Machine whose silence
/// is a [`crate::deploy::listing_gap`].
pub(crate) struct Listed {
    pub(crate) copies: Vec<ContainerObservation>,
    pub(crate) unanswered: Vec<Unanswered>,
}

/// A Machine whose listing went unanswered, a [`crate::deploy::listing_gap`].
pub(crate) struct Unanswered {
    pub(crate) name: MachineName,
    pub(crate) failure: MachineFailure<RpcError>,
    /// The failure as planning words it too, so their notices are one.
    pub(crate) message: String,
}

impl Unanswered {
    /// Report this Machine as an observation gap, which leaves the Deploy partial.
    fn warn(&self, progress: &Progress) {
        progress.warn(DeployWarning::ObservationFailed {
            gap: Some(ObservationGap {
                machine_name: self.name.clone(),
                reason: ObservationGapReason::Failed,
            }),
            kind: ObservationKind::Container,
            machine_id: self.failure.machine_id,
            machine_name: None,
            message: self.message.clone(),
        });
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
    /// [`Self::service_containers`], keeping each Machine whose silence is a
    /// [`crate::deploy::listing_gap`] beside the copies the others list instead of
    /// failing on it. An answered error, or any other silence, still fails. The default
    /// reports no Machine as unanswered.
    async fn listed_copies(&self, service: &QualifiedService) -> Result<Listed, RpcError> {
        Ok(Listed {
            copies: self.service_containers(service).await?,
            unanswered: Vec::new(),
        })
    }
    /// Every Container row in the entry Machine's store, which keeps the rows of
    /// a Machine that does not answer.
    async fn replicated_containers(&self) -> Result<Vec<ContainerObservation>, RpcError>;
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
    /// Apply `spec`'s CPU and memory limits to the running `container_id` in place.
    async fn update_container_limits(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerLimitsUpdated, RpcError>;
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
    /// [`Self::read_routes`] as the entry Machine this client reaches reads it.
    async fn read_entry_routes(&self, service: &QualifiedService) -> Result<RoutesRead, RpcError>;
    /// Dial the entry Machine again on the same connection, after the session a call
    /// went out on closed. None when this client can't.
    async fn redial(&self) -> Option<Result<(), RpcError>> {
        None
    }
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
        let listed = self.listed_copies(service).await?;
        match listed.unanswered.into_iter().next() {
            Some(silent) => Err(naming_machine(
                crate::cluster::unanswered_by(&silent.name, silent.failure.error),
                &silent.failure.machine_id,
            )),
            None => Ok(listed.copies),
        }
    }

    async fn listed_copies(&self, service: &QualifiedService) -> Result<Listed, RpcError> {
        let mut client = self.clone();
        let machines = client.machines().await.map_err(RpcError::from)?;
        let listed = client
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await
            .map_err(RpcError::from)?
            .containers;
        let mut unanswered = Vec::new();
        for failure in listed.failures {
            match machines
                .iter()
                .find(|machine| machine.machine.id == failure.machine_id)
            {
                Some(machine) if crate::deploy::listing_gap(machine, &failure.error) => {
                    unanswered.push(Unanswered {
                        name: machine.machine.name.clone(),
                        message: crate::deploy::unanswered_message(machine, &failure.error),
                        failure,
                    });
                }
                Some(machine) if failure.error.target_unanswered() => {
                    return Err(naming_machine(
                        crate::cluster::unanswered_by(&machine.machine.name, failure.error),
                        &failure.machine_id,
                    ));
                }
                Some(_) | None => return Err(failure.error),
            }
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
        let copies = listed
            .successes
            .into_iter()
            .flat_map(|success| success.value)
            .filter(|container| {
                container.kind == ContainerKind::ServiceContainer
                    && container.namespace == service.namespace
                    && container.resolved_spec.name == service.name
            })
            .collect();
        Ok(Listed { copies, unanswered })
    }

    async fn replicated_containers(&self) -> Result<Vec<ContainerObservation>, RpcError> {
        self.runtime_frame().await.map(|frame| frame.containers)
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
            },
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
    }

    async fn redial(&self) -> Option<Result<(), RpcError>> {
        Some(
            self.redial_shared()
                .await
                .map_err(crate::connect::rpc_error),
        )
    }

    async fn read_entry_routes(&self, service: &QualifiedService) -> Result<RoutesRead, RpcError> {
        self.invoke_entry::<op::ReadRoutes>(
            ReadRoutesRequest {
                service: service.clone(),
            },
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

    async fn update_container_limits(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerLimitsUpdated, RpcError> {
        self.invoke::<op::UpdateContainerLimits>(
            UpdateContainerLimitsRequest {
                container_id: *container_id,
                spec: spec.clone(),
            },
            &MachineTarget::from(machine_id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
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
// An entry daemon restart closes the session every call goes out on, so the
// same connection is dialed again. Do not walk to another connection; that is
// a different command-entry problem.
struct RestartTolerant<'a, C> {
    inner: &'a C,
    cancellation: &'a CancellationToken,
    /// How many times the entry was dialed again. Calls that met the same closed
    /// session dial it once between them.
    sessions: tokio::sync::Mutex<u64>,
}

impl<'a, C: MachineOperations> RestartTolerant<'a, C> {
    fn new(inner: &'a C, cancellation: &'a CancellationToken) -> Self {
        Self {
            inner,
            cancellation,
            sessions: tokio::sync::Mutex::new(0),
        }
    }

    async fn session(&self) -> u64 {
        *self.sessions.lock().await
    }

    /// Dial the entry again, unless another call already has since `seen`.
    async fn redial(&self, seen: u64) -> Option<Result<(), RpcError>> {
        let mut sessions = self.sessions.lock().await;
        if *sessions != seen {
            return Some(Ok(()));
        }
        let answer = self.inner.redial().await;
        if matches!(answer, Some(Ok(()))) {
            *sessions += 1;
        }
        answer
    }
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
    (@send wait $self:ident $name:ident $($arg:ident)*) => {{
        let seen = &std::sync::atomic::AtomicU64::new($self.session().await);
        wait_out_restart(
            $self.cancellation,
            || $self.inner.$name($(Clone::clone(&$arg)),*),
            move || async move {
                let answer = $self.redial(seen.load(std::sync::atomic::Ordering::Relaxed)).await;
                seen.store($self.session().await, std::sync::atomic::Ordering::Relaxed);
                answer
            },
        )
        .await
    }};
    (@send once($reason:literal) $self:ident $name:ident $($arg:ident)*) => {
        $self.inner.$name($($arg),*).await
    };
}

restart_policy! {
    wait fn prepare_volumes(&self, machine_id: &MachineId, specs: &[ployz_core::ServiceStorageSpec]) -> Result<ployz_core::PreparedVolumes, RpcError>;
    wait fn service_containers(&self, service: &QualifiedService) -> Result<Vec<ContainerObservation>, RpcError>;
    wait fn listed_copies(&self, service: &QualifiedService) -> Result<Listed, RpcError>;
    once("a failed read leaves the gen to the live answers") fn replicated_containers(&self) -> Result<Vec<ContainerObservation>, RpcError>;
    wait fn ensure_container(&self, machine_id: &MachineId, want: ContainerWant) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError>;
    wait fn inspect_container(&self, machine_id: &MachineId, container_id: &ContainerId) -> Result<ContainerDetails, RpcError>;
    wait fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError>;
    wait fn update_container_limits(&self, machine_id: &MachineId, container_id: &ContainerId, spec: &ResolvedServiceSpec) -> Result<ContainerLimitsUpdated, RpcError>;
    wait fn runs_global(&self, machine_id: &MachineId, slot: &SlotKey, spec: &ResolvedServiceSpec) -> Result<bool, RpcError>;
    wait fn ensure_image(&self, machine_id: &MachineId, spec: &ResolvedServiceSpec) -> Result<ImagePulled, RpcError>;
    wait fn ensure_unsettled(&self, machine_id: &MachineId, want: ContainerWant, timeout: Option<Duration>) -> Result<EnsuredContainer, RpcError>;
    wait fn read_routes(&self, machine_id: &MachineId, service: &QualifiedService) -> Result<RoutesRead, RpcError>;
    wait fn read_entry_routes(&self, service: &QualifiedService) -> Result<RoutesRead, RpcError>;
}

async fn wait_out_restart<T, Fut, Redial>(
    cancellation: &CancellationToken,
    mut op: impl FnMut() -> Fut,
    mut redial: impl FnMut() -> Redial,
) -> Result<T, RpcError>
where
    Fut: Future<Output = Result<T, RpcError>>,
    Redial: Future<Output = Option<Result<(), RpcError>>>,
{
    let deadline = Instant::now() + RESTART_WAIT;
    // What earlier attempts showed of the Machine. A real answer clears the other two, and
    // a lost reply and a silence each clear the other. A Machine that answered was not
    // silent throughout: its answer stands in for a later silence, as `call_retried` keeps it.
    let mut answer: Option<RpcError> = None;
    // A send whose answer was lost: the Machine may have done it.
    let mut lost: Option<RpcError> = None;
    // A Machine that didn't answer: stronger news than an earlier lost reply, as
    // `keep_lost_call` holds.
    let mut silent: Option<RpcError> = None;
    loop {
        let error = match op().await {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let closed = crate::connect::on_closed_session(&error);
        // A send that never reached the Machine, or met a closed session, says nothing of
        // it, so it is never kept.
        let unreached = error.never_reached() || closed;
        if error.reply_lost() {
            silent = None;
            lost = Some(error.clone());
        } else if error.target_unanswered() && !unreached {
            lost = None;
            silent = Some(error.clone());
        } else if !unreached {
            lost = None;
            silent = None;
            answer = Some(error.clone());
        }
        let reported = lost
            .clone()
            .or_else(|| answer.clone())
            .or_else(|| silent.clone())
            .unwrap_or_else(|| error.clone());
        // Whether to send again follows what this attempt met.
        let retry = if unreached { &error } else { &reported }.code.retry();
        let error = reported;
        let again = retry != Retry::Never
            && Instant::now() < deadline
            && !cancellation.is_cancelled()
            // Nothing sent on a closed session reaches a daemon again.
            && (!closed || redialed(&mut redial, deadline, cancellation).await);
        if !again {
            return Err(error);
        }
        tokio::select! {
            () = cancellation.cancelled() => return Err(error),
            () = tokio::time::sleep(RESTART_POLL) => {}
        }
    }
}

/// Dial the entry again until it answers, `deadline` passes or the Deploy is
/// cancelled. False then, or when the client can't dial again.
async fn redialed<Redial>(
    redial: &mut impl FnMut() -> Redial,
    deadline: Instant,
    cancellation: &CancellationToken,
) -> bool
where
    Redial: Future<Output = Option<Result<(), RpcError>>>,
{
    loop {
        let answer = tokio::select! {
            () = cancellation.cancelled() => return false,
            answer = tokio::time::timeout_at(deadline, redial()) => answer,
        };
        match answer {
            Ok(Some(Ok(()))) => return true,
            Ok(Some(Err(_))) => {}
            Ok(None) | Err(_) => return false,
        }
        tokio::select! {
            () = cancellation.cancelled() => return false,
            () = tokio::time::sleep_until(deadline) => return false,
            () = tokio::time::sleep(RESTART_POLL) => {}
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
        slot_key(self.namespace, &spec.name, kind, key)
    }
}

fn slot_key(
    namespace: &Namespace,
    service: &ServiceName,
    kind: ContainerKind,
    key: String,
) -> SlotKey {
    SlotKey {
        namespace: namespace.clone(),
        service: service.clone(),
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
    let mut started: BTreeMap<usize, Vec<(usize, MachineId, ContainerId)>> = BTreeMap::new();
    let mut stopped: BTreeMap<usize, Vec<(ResolvedServiceSpec, cleanup::Copy)>> = BTreeMap::new();
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
                Action::Start(index) => Either::Left(Either::Left(async move {
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
                        if let Some(old) = ran.held_old {
                            stopped.entry(group).or_default().push(*old);
                        }
                        if let (true, Some(container_id)) =
                            (schedule.group(group).publish, ran.started)
                        {
                            progress.set_running(
                                index,
                                OperationPhase::WaitingForSwitch { container_id },
                            );
                            awaiting_flip.entry(group).or_default().push(index);
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
                    Err(OperationFailure::Ordinary(error)) => {
                        if lost_to_newer(&error, None) {
                            schedule.apply(Event::NewerServing(schedule.group_of(index)));
                        }
                        progress.fail(index, error.clone());
                        stop.get_or_insert(Stop {
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
                        if lost_to_newer(&error, Some(compensation.as_ref())) {
                            schedule.apply(Event::NewerServing(schedule.group_of(index)));
                        }
                        progress.fail(index, error.clone());
                        stop.get_or_insert(Stop {
                            index,
                            error,
                            compensation: Some(*compensation),
                        });
                        schedule.apply(Event::Failed(index));
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
                let mut fail = |task: usize, error: ExecutionError, event: Event| {
                    if lost_to_newer(&error, None) {
                        schedule.apply(Event::NewerServing(group));
                    }
                    progress.fail(task, error.clone());
                    stop.get_or_insert(Stop {
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
                    // Its old copies were left In beside the new ones and named.
                    Published::Gapped => {
                        for index in awaiting_flip.remove(&group).unwrap_or_default() {
                            completed.insert(index);
                            progress.set_completed(index);
                        }
                        schedule.apply(Event::Routed(service));
                        schedule.apply(Event::PublishGapped(group));
                    }
                    Published::Unconfirmed(machines) => {
                        unread.insert(group);
                        fail(
                            last,
                            machine_error(
                                MachineAction::ReadRoutes,
                                unavailable(rows.not_confirmed(&machines)),
                            ),
                            Event::PublishUnconfirmed(group),
                        );
                    }
                    // The new copies serve and the old ones went Out: none
                    // is removed or brought back.
                    Published::OutUnconfirmed(error) => {
                        unread.insert(group);
                        stopped.remove(&group);
                        fail(last, error, Event::PublishUnconfirmed(group));
                    }
                    Published::Cancelled => {
                        fail(last, ExecutionError::Cancelled, Event::PublishFailed(group));
                    }
                    Published::CopyFailed { task, error } => {
                        fail(task, error, Event::PublishFailed(group));
                    }
                    Published::Failed(error) => fail(last, error, Event::PublishFailed(group)),
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
                        stop.get_or_insert(Stop {
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
            rows.restore_unclaimed(client, progress, |task| schedule.group_of(task) == group)
                .await;
            let started = started
                .remove(&group)
                .unwrap_or_default()
                .into_iter()
                .map(|(_, machine, id)| (machine, id))
                .collect();
            restore_group(
                client,
                progress,
                started,
                unread.contains(&group),
                stopped.remove(&group).unwrap_or_default(),
                rows,
            )
            .await;
        }
    }

    progress.settle_unexecuted();
    let outcome = outcome_from(operations, &completed, stop, schedule.services());
    progress.outcome(outcome.clone());
    outcome
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

/// A stop-first copy shares its Volume with the new copy on its Machine, so
/// each new copy is gone or stopped before an old copy runs again. Cleanup
/// that must finish after a cancel waits on its own token; every wait is
/// bounded by `ACK_WAIT`.
async fn restore_group<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    started: Vec<(MachineId, ContainerId)>,
    unread: bool,
    stopped: Vec<(ResolvedServiceSpec, cleanup::Copy)>,
    rows: &Rows<'_>,
) {
    let cleanup = CancellationToken::new();
    if stopped.is_empty() {
        if !unread {
            join_all(started.iter().map(|(machine, id)| {
                discard_new(client, progress, rows, machine, *id, None, &cleanup)
            }))
            .await;
        }
        return;
    }
    // Unconfirmed new copies may serve, so they stay and block their Machine.
    let withdrawn = if unread {
        started
            .iter()
            .map(|(machine, _)| Withdrawn::Running(*machine))
            .collect()
    } else {
        join_all(started.iter().map(|(machine, id)| {
            discard_new(
                client,
                progress,
                rows,
                machine,
                *id,
                Some(rows.run),
                &cleanup,
            )
        }))
        .await
    };
    let blocked: BTreeSet<&MachineId> = withdrawn
        .iter()
        .filter_map(|withdrawn| match withdrawn {
            Withdrawn::Running(machine) => Some(machine),
            Withdrawn::Gone | Withdrawn::Held(..) => None,
        })
        .collect();
    let mut restored = false;
    let mut may_run = BTreeSet::new();
    for (spec, old) in &stopped {
        let why = if blocked.contains(&old.machine_id) {
            Some("its new copy may be serving".to_owned())
        } else {
            let restore = restore_old(client, progress, old, spec, rows, &cleanup).await;
            if restore.may_run {
                may_run.insert(old.machine_id);
            }
            match restore.attempt {
                RestoreAttempt::Restored => {
                    restored = true;
                    None
                }
                RestoreAttempt::NotServing | RestoreAttempt::NewerServing => None,
                RestoreAttempt::LeftHeld => Some("another Deploy holds it".to_owned()),
                RestoreAttempt::Failed { error } => Some(error.to_string()),
            }
        };
        if let Some(why) = why {
            rows.leave_running(
                progress,
                old,
                Left::NotRestored(Box::new(Left::DeployFailed), why),
            );
        }
    }
    for withdrawn in withdrawn {
        let Withdrawn::Held(spec, copy) = withdrawn else {
            continue;
        };
        if restored {
            let left =
                match cleanup::retire(client, &copy, false, AnonymousVolumes::Remove, &cleanup)
                    .await
                {
                    Ok(step) => Option::<Left>::from(step),
                    Err(error) => Some(Left::Failed(error)),
                };
            if let Some(left) = left {
                rows.leave_running(progress, &copy, left);
            }
        } else if may_run.contains(&copy.machine_id) {
            // Its old copy may be writing the Volume, so it stays stopped.
            rows.leave_running(
                progress,
                &copy,
                Left::NotRestored(
                    Box::new(Left::DeployFailed),
                    "its old copy may be serving".to_owned(),
                ),
            );
        } else {
            // No old copy came back, so the healthy new copy serves again.
            leave_unstopped(client, progress, &copy, &spec, rows, Left::DeployFailed).await;
        }
    }
}

enum Withdrawn {
    Gone,
    /// Held by this run and stopped: it can go back In if no old copy does.
    Held(Box<ResolvedServiceSpec>, cleanup::Copy),
    /// It may still run on this Machine.
    Running(MachineId),
}

/// The owner refuses Gone on a copy that already came In. With `hold`, that
/// copy is Held and stopped, so it can still come back; otherwise it leaves
/// through Out and the unlisted wait.
async fn discard_new<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    rows: &Rows<'_>,
    machine_id: &MachineId,
    container_id: ContainerId,
    hold: Option<&DeploymentLogId>,
    cancellation: &CancellationToken,
) -> Withdrawn {
    let running = || Withdrawn::Running(*machine_id);
    let want = ContainerWant::Gone {
        container: ContainerLocator::Id(container_id),
        drain: Drain::NONE,
        anonymous_volumes: AnonymousVolumes::Remove,
    };
    let (now, refusal) = match client.ensure_unsettled(machine_id, want, None).await {
        Ok(EnsuredContainer::Ensured { .. }) => return Withdrawn::Gone,
        Ok(EnsuredContainer::Refused { now, refusal }) => (now, refusal),
        Err(_) => return running(),
    };
    if refusal.reason == ContainerRefusalReason::NotFound {
        return Withdrawn::Gone;
    }
    let Some(observed) = now
        .present()
        .filter(|_| refusal.reason == ContainerRefusalReason::InRotation)
    else {
        return running();
    };
    let mut copy = cleanup::Copy::observed(observed);
    let step = match hold {
        Some(run) => cleanup::hold(client, &copy, run.clone(), cancellation).await,
        None => cleanup::retire(client, &copy, true, AnonymousVolumes::Remove, cancellation).await,
    };
    let left = match step {
        Ok(cleanup::Step::Done) => None,
        Ok(step) => Option::<Left>::from(step),
        Err(error) => Some(Left::Failed(error)),
    };
    let spec = observed.resolved_spec.clone();
    match (hold, left) {
        (None, None) => Withdrawn::Gone,
        (Some(run), None) => {
            copy.rotation = CopyRotation::Held(run.clone());
            match stop(client, machine_id, &container_id, Drain::DEFAULT, None).await {
                Ok(()) => Withdrawn::Held(Box::new(spec), copy),
                Err(error) => {
                    leave_unstopped(client, progress, &copy, &spec, rows, left_by(error)).await;
                    running()
                }
            }
        }
        (None, Some(left)) => {
            rows.leave_running(progress, &copy, left);
            running()
        }
        (Some(run), Some(left)) => {
            copy.rotation = CopyRotation::Held(run.clone());
            leave_unstopped(client, progress, &copy, &spec, rows, left).await;
            running()
        }
    }
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
    /// A Rolling step's new copy is In and listed, so the next step may begin.
    Admitted(usize),
    Publish(usize, Published),
}

enum Published {
    Listed,
    /// Listed by every proxy but those on gap Servers, which may still route the
    /// old copies, so those stay In.
    Gapped,
    Unconfirmed(cleanup::Unconfirmed),
    /// The new copies are listed, but an old copy's Out was not seen through.
    OutUnconfirmed(ExecutionError),
    Cancelled,
    CopyFailed {
        task: usize,
        error: ExecutionError,
    },
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
    let routed = *service != QualifiedService::system_ingress();
    // System ingress copies stay Pending for life and no route lists them.
    let gapped = if routed {
        match route_new(client, operations, rows, service, &new, cancellation).await {
            Ok(gapped) => gapped,
            Err(published) => return published,
        }
    } else {
        None
    };
    let ids: BTreeSet<ContainerId> = new.iter().map(|(_, _, id)| *id).collect();
    let mut listed_on: BTreeSet<MachineId> = new
        .first()
        .map(|(_, machine, _)| *machine)
        .into_iter()
        .collect();
    let (mut outs, mut gone) = (Vec::new(), Vec::new());
    let inspected = join_all(
        old.iter()
            .map(|(machine_id, container_id)| inspect(client, machine_id, container_id)),
    )
    .await;
    for ((machine_id, container_id), inspected) in old.iter().zip(inspected) {
        let copy = match inspected {
            Ok(details) => cleanup::Copy::observed(&details.container),
            Err(ExecutionError::Machine { error, .. }) if error.code == RpcErrorCode::NotFound => {
                continue;
            }
            Err(error) => {
                let copy = cleanup::Copy {
                    machine_id: *machine_id,
                    service: service.clone(),
                    container_id: *container_id,
                    rotation: CopyRotation::Pending,
                };
                let left = match error {
                    ExecutionError::Machine { error, .. } => Left::Failed(error),
                    error @ (ExecutionError::Health { .. }
                    | ExecutionError::DependencyHealth { .. }
                    | ExecutionError::Hook { .. }
                    | ExecutionError::NewerServing { .. }
                    | ExecutionError::Cancelled) => {
                        Left::Failed(RpcError::caused(RpcErrorCode::Internal, &error))
                    }
                };
                rows.leave_running(progress, &copy, left);
                continue;
            }
        };
        if copy.rotation == CopyRotation::Pending {
            gone.push(copy);
            continue;
        }
        if let Some(gapped) = &gapped
            && copy.rotation == CopyRotation::In
        {
            rows.leave_running(progress, &copy, Left::NotConfirmed(gapped.clone()));
            continue;
        }
        // The owner refuses this Out (LastIn) until its own view lists a new copy.
        if routed && copy.rotation == CopyRotation::In && !listed_on.contains(&copy.machine_id) {
            let left = match cleanup::wait_listed(
                client,
                &copy.machine_id,
                service,
                &ids,
                cancellation,
                |_| {},
            )
            .await
            {
                Ok(cleanup::Wait::Done) => None,
                Ok(cleanup::Wait::TimedOut(_) | cleanup::Wait::Gapped(_)) => Some(Left::NotListed),
                Ok(cleanup::Wait::Cancelled) => Some(Left::Interrupted),
                Err(error) => Some(Left::Failed(error)),
            };
            if let Some(left) = left {
                rows.leave_running(progress, &copy, left);
                continue;
            }
            listed_on.insert(copy.machine_id);
        }
        outs.push(copy);
    }
    // Every Out goes together and one wait covers the set, so a group's
    // retirement costs one unlisting, not one per copy.
    let sent = join_all(
        outs.iter()
            .map(|copy| cleanup::send_out(client, copy, false)),
    )
    .await;
    // An Out that may have landed but was never seen unlisted admits no
    // later rotation change, so the group does not publish.
    let mut unconfirmed = None;
    let mut unlisting = Vec::new();
    for (copy, sent) in outs.into_iter().zip(sent) {
        match sent {
            Ok(None) => unlisting.push(copy),
            Ok(Some(refusal)) => {
                rows.leave_running(progress, &copy, Left::Refused(refusal));
            }
            Err(error) => {
                unconfirmed.get_or_insert_with(|| {
                    machine_error(MachineAction::StopContainer, error.clone())
                });
                rows.leave_running(progress, &copy, Left::Failed(error));
            }
        }
    }
    if !unlisting.is_empty() {
        let ids: BTreeSet<ContainerId> = unlisting.iter().map(|copy| copy.container_id).collect();
        let unlisted = cleanup::wait_none_listed(client, service, &ids, cancellation).await;
        let (left, error) = match unlisted {
            Ok(cleanup::Wait::Done) => (None, None),
            Ok(cleanup::Wait::TimedOut(machines) | cleanup::Wait::Gapped(machines)) => {
                let error = machine_error(
                    MachineAction::ReadRoutes,
                    unavailable(rows.not_confirmed(&machines)),
                );
                (Some(Left::NotConfirmed(machines)), Some(error))
            }
            Ok(cleanup::Wait::Cancelled) => (Some(Left::Interrupted), None),
            Err(error) => (
                Some(Left::Unread(error.clone())),
                Some(machine_error(MachineAction::ReadRoutes, error)),
            ),
        };
        match left {
            None => gone.append(&mut unlisting),
            Some(left) => {
                for copy in &unlisting {
                    rows.leave_running(progress, copy, left.clone());
                }
            }
        }
        if let Some(error) = error {
            unconfirmed.get_or_insert(error);
        }
    }
    // Each Gone drains on its own Machine; they overlap and are all reported.
    let removed = join_all(
        gone.iter()
            .map(|copy| cleanup::remove(client, copy, AnonymousVolumes::Remove)),
    )
    .await;
    for (copy, removed) in gone.iter().zip(removed) {
        if let Some(left) = left_unless_done(removed) {
            rows.leave_running(progress, copy, left);
        }
    }
    match (unconfirmed, gapped) {
        (Some(error), _) => Published::OutUnconfirmed(error),
        (None, Some(_)) => Published::Gapped,
        (None, None) => Published::Listed,
    }
}

fn left_unless_done(step: Result<cleanup::Step, RpcError>) -> Option<Left> {
    match step {
        Ok(step) => step.into(),
        Err(error) => Some(Left::Failed(error)),
    }
}

/// A Rolling replacement's new copy, which goes In before the next Machine's
/// old copy is withdrawn: while it stays Pending, proxies keep that old copy.
fn rolling_step(
    operation: &DeployOperation,
    started: Option<ContainerId>,
) -> Option<(MachineId, ContainerId)> {
    let DeployOperation::ReplaceContainer(replacement) = operation else {
        return None;
    };
    (replacement.spec.update.order == UpdateOrder::Rolling)
        .then_some((replacement.machine_id, started?))
}

async fn route_new<C: MachineOperations>(
    client: &C,
    operations: &[DeployOperation],
    rows: &Rows<'_>,
    service: &QualifiedService,
    new: &[(usize, MachineId, ContainerId)],
    cancellation: &CancellationToken,
) -> Result<Option<cleanup::Unconfirmed>, Published> {
    let (_, first_machine, _) = new.first().expect("a published group started copies");
    let brought = join_all(
        new.iter()
            .map(|(task, machine_id, container_id)| async move {
                let copy = cleanup::Copy {
                    machine_id: *machine_id,
                    service: service.clone(),
                    container_id: *container_id,
                    rotation: CopyRotation::Pending,
                };
                let spec = operation_at(operations, *task)
                    .spec()
                    .expect("a started copy has a spec");
                let error =
                    match cleanup::bring_in(client, &copy, spec, Some(rows.run.clone())).await {
                        Ok(cleanup::BroughtIn::In) => return Ok(()),
                        Ok(cleanup::BroughtIn::NotIn { health, .. }) => {
                            health_error(*container_id, health, spec)
                        }
                        Ok(cleanup::BroughtIn::Refused(refusal))
                            if refusal.reason == ContainerRefusalReason::OlderThanIn =>
                        {
                            ExecutionError::NewerServing {
                                service: service.clone(),
                            }
                        }
                        Ok(cleanup::BroughtIn::Refused(refusal)) => {
                            machine_error(MachineAction::StartContainer, refusal.into_error())
                        }
                        Err(error) => machine_error(MachineAction::StartContainer, error),
                    };
                Err(Published::CopyFailed { task: *task, error })
            }),
    )
    .await;
    brought.into_iter().collect::<Result<Vec<()>, _>>()?;
    let ids: BTreeSet<ContainerId> = new.iter().map(|(_, _, id)| *id).collect();
    let gaps = rows.gaps(service);
    match cleanup::wait_listed_beside(
        client,
        first_machine,
        service,
        &ids,
        &gaps,
        cancellation,
        |_| {},
    )
    .await
    {
        Ok(cleanup::Wait::Done) => Ok(None),
        Ok(cleanup::Wait::Gapped(machines)) => Ok(Some(machines)),
        Ok(cleanup::Wait::TimedOut(machines)) => Err(Published::Unconfirmed(machines)),
        Ok(cleanup::Wait::Cancelled) => Err(Published::Cancelled),
        Err(error) => Err(Published::Failed(machine_error(
            MachineAction::ReadRoutes,
            error,
        ))),
    }
}

/// The copies the group's publish retires. A copy updated in place is one of
/// the Service's serving copies and stays.
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
                Some((replacement.machine_id, replacement.old_container_id))
            }
            DeployOperation::PrepareVolumes { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::RunContainer { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::UpdateContainer(_)
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
    /// The Service each rollout removal retires a copy of, so one that cannot
    /// inspect its copy can still name it.
    services: BTreeMap<usize, &'a ServiceName>,
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
    Unclaimed(cleanup::Copy),
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
                    .flat_map(|step| match step {
                        Step::Serial(index) => std::slice::from_ref(index),
                        Step::Parallel(indices) => indices.as_slice(),
                    })
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
            operations,
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
                Ok(cleanup::Step::Done) => {
                    old.insert(task, Ok(None));
                    continue;
                }
                Ok(cleanup::Step::Refused(refusal)) => {
                    machine_error(MachineAction::RemoveContainer, refusal.into_error())
                }
                Ok(step @ (cleanup::Step::NotConfirmed(_) | cleanup::Step::Cancelled)) => {
                    unreachable!("a removal waits on no proxy: {step:?}")
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
            let (left, error) = match step {
                Ok(cleanup::Step::Done) => (None, None),
                Ok(cleanup::Step::NotConfirmed(machines)) => {
                    let error = machine_error(
                        MachineAction::StopContainer,
                        unavailable(self.not_confirmed(&machines)),
                    );
                    (Some(Left::NotConfirmed(machines)), Some(error))
                }
                Ok(cleanup::Step::Cancelled) => {
                    (Some(Left::Interrupted), Some(ExecutionError::Cancelled))
                }
                Ok(cleanup::Step::Refused(refusal)) => {
                    copy.rotation = observed.rotation().clone();
                    let error =
                        machine_error(MachineAction::StopContainer, refusal.clone().into_error());
                    (Some(Left::Refused(refusal)), Some(error))
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
            .map(|(task, _, copy, _)| (task, StepCopy::Unclaimed(copy)))
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
            Some(StepCopy::Unclaimed(copy)) => Claim::Withdrawn(copy),
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
                if let StepCopy::Unclaimed(copy) = std::mem::replace(entry, StepCopy::Restored) {
                    let spec = match inspected.old.get(task) {
                        Some(Ok(Some(observed))) => observed.resolved_spec.clone(),
                        Some(Ok(None) | Err(_)) | None => {
                            unreachable!("only an inspected copy is withdrawn")
                        }
                    };
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
                            error = %error.message,
                            "couldn't read the entry Machine's store; new gens follow the live answers only"
                        );
                        return Ok(Err(error));
                    }
                    Err(_) => {
                        tracing::warn!(
                            wait = ?STORED_GENS_WAIT,
                            "the entry Machine's store didn't answer in time; new gens follow the live answers only"
                        );
                        return Ok(Err(RpcError {
                            code: RpcErrorCode::Unavailable,
                            message: format!("the store didn't answer in {STORED_GENS_WAIT:?}"),
                            details: serde_json::Value::Null,
                            cause: Vec::new(),
                        }));
                    }
                };
                Ok(Ok(containers
                    .into_iter()
                    .filter(|copy| copy.kind == ContainerKind::ServiceContainer)
                    .collect()))
            })
            .await
    }

    #[expect(
        clippy::result_large_err,
        reason = "ExecutionError is the Deploy's wire failure value; every step in this file returns it unboxed"
    )]
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

    /// The Machines `service`'s listing left unanswered. They are observation gaps,
    /// so no publish waits on their proxies.
    fn gaps(&self, service: &QualifiedService) -> BTreeSet<MachineId> {
        let cell = self.gens.lock().expect("gens lock").get(service).cloned();
        cell.and_then(|cell| {
            cell.get().map(|numbered| {
                numbered
                    .silent
                    .iter()
                    .map(|silent| silent.failure.machine_id)
                    .collect()
            })
        })
        .unwrap_or_default()
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
    progress.set_running(
        index,
        match operation {
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
        },
    );
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
            run_container(
                client,
                index,
                progress,
                machine_id,
                slot,
                (spec, rows.run),
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
                (&replacement.spec, rows.run),
                r#gen,
                answer_when(replacement.skip_health_monitor),
                FailedStart::Discard,
                cancellation,
            )
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
                index,
                progress,
                machine_id,
                slot,
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

#[derive(Debug, thiserror::Error)]
#[error("The new Container did not serve.")]
pub(in crate::deploy) struct MoveContainerError {
    #[source]
    pub(in crate::deploy) error: ExecutionError,
    pub(in crate::deploy) replacement_removed: bool,
}

pub(in crate::deploy) async fn move_container<C: MachineOperations>(
    client: &C,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    to: &MachineId,
    (from, old): (&MachineId, &ContainerId),
    cancellation: &CancellationToken,
) -> Result<(ContainerId, Option<DeploymentLogId>), MoveContainerError> {
    let client = RestartTolerant::new(client, cancellation);
    let global = spec.mode == ployz_core::ServiceMode::Global;
    let key = if global {
        crate::cluster::global_creation_key(spec)
    } else {
        format!("move:{old}")
    };
    let slot = slot_key(namespace, &spec.name, ContainerKind::ServiceContainer, key);
    let adopts = global && adopts_running(&client, to, &slot, spec).await;
    let source = match client.inspect_container(from, old).await {
        Ok(details) => details.container,
        Err(error) => {
            return Err(MoveContainerError {
                error: machine_error(MachineAction::InspectContainer, error),
                replacement_removed: !adopts,
            });
        }
    };
    let deploy = source.deploy_label();
    let progress = Progress::new(Vec::new(), None);
    let started = run_new(
        &client,
        0,
        &progress,
        to,
        slot.clone(),
        (spec, deploy.as_ref()),
        source.copy_gen(),
        AnswerWhen::Settled,
        cancellation,
    )
    .await;
    match started_container(started, spec, cancellation) {
        Ok(new) => Ok((new, deploy)),
        Err(error) => {
            let keep = adopts || (global && !cancellation.is_cancelled());
            let replacement_removed = !keep && discard_attempt(&client, to, slot).await.is_ok();
            Err(MoveContainerError {
                error,
                replacement_removed,
            })
        }
    }
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
    (spec, run): (&ResolvedServiceSpec, &DeploymentLogId),
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
        (spec, Some(run)),
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
    started_container(started, spec, cancellation)
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

fn out_of_memory(health: &HealthVerdict, spec: &ResolvedServiceSpec) -> Option<OutOfMemory> {
    health.oom_killed().then_some(OutOfMemory {
        memory_bytes: spec.container.resources.memory_bytes,
    })
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

#[expect(
    clippy::result_large_err,
    reason = "ExecutionError is the Deploy's wire failure value; every step in this file returns it unboxed"
)]
fn started_container(
    started: Result<(ContainerId, Option<HealthVerdict>), RpcError>,
    spec: &ResolvedServiceSpec,
    cancellation: &CancellationToken,
) -> Result<ContainerId, ExecutionError> {
    if cancellation.is_cancelled() {
        return Err(ExecutionError::Cancelled);
    }
    let (container_id, health) =
        started.map_err(|error| machine_error(MachineAction::CreateContainer, error))?;
    let Some(health) = health else {
        return Ok(container_id);
    };
    let out_of_memory = out_of_memory(&health, spec);
    let departing = health.departing().cloned();
    match health.into_failure() {
        None => Ok(container_id),
        Some(failure) => Err(ExecutionError::Health {
            container_id,
            failure,
            out_of_memory,
            last_run: None,
            departing,
        }),
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
    spec: (&ResolvedServiceSpec, &DeploymentLogId),
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
            (was_running || stopped.rotation != CopyRotation::Pending)
                .then(|| (old.resolved_spec.clone(), stopped))
        }
        None => None,
    };
    let started = start(
        client,
        index,
        progress,
        &operation.machine_id,
        slot,
        (&operation.spec, rows.run),
        r#gen,
        answer_when(operation.skip_health_monitor),
        FailedStart::Retain,
        cancellation,
    )
    .await;
    match started {
        Ok(new_container) => Ok(Ran {
            started: Some(new_container),
            held_old: held_old.map(Box::new),
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
/// Returns the copy as it now is, or `None` when it needs no withdrawal.
async fn withdraw<C: MachineOperations>(
    client: &C,
    old: &ContainerObservation,
    rows: &Rows<'_>,
    cancellation: &CancellationToken,
) -> Option<(cleanup::Copy, Result<cleanup::Step, RpcError>)> {
    let mut copy = cleanup::Copy::observed(old);
    let (step, now) = match &copy.rotation {
        CopyRotation::Pending | CopyRotation::Out => return None,
        _ if crate::ingress::is_system_ingress(old) => return None,
        CopyRotation::In => (
            cleanup::hold(client, &copy, rows.run.clone(), cancellation).await,
            CopyRotation::Held(rows.run.clone()),
        ),
        CopyRotation::Held(run) if run == rows.run => (
            cleanup::hold(client, &copy, rows.run.clone(), cancellation).await,
            copy.rotation.clone(),
        ),
        CopyRotation::Held(_) => (
            cleanup::take_out(client, &copy, false, cancellation).await,
            CopyRotation::Out,
        ),
    };
    copy.rotation = now;
    Some((copy, step))
}

/// Removes one Container. A copy that is not there is already removed. Otherwise
/// what the removal must reach decides how it goes:
///
/// - A `Rollout` removal takes an old copy a new one replaced, with its anonymous
///   Volumes. The Service keeps running, so a copy still In goes Out first, and
///   the drain waits for every proxy to confirm.
/// - A `Retire` keeps the anonymous Volumes and may leave the Service empty.
///   - For a Service this Deploy keeps, such as an exited pre-deploy hook of one
///     that stays, it is plain cleanup. The Service's other copies are its own,
///     wherever they run, so none is named.
///   - For a Service this Deploy removes, named or pruned, it must take the
///     Service out everywhere. Every proxy keeps the route while the entry
///     Machine's store lists an In or Pending copy on a Machine this Deploy did
///     not observe. So when the store lists such a copy, or could not be read, a
///     live copy that drains goes Out and is removed without waiting for the
///     proxies to confirm. Once the live copy is gone, each copy the store listed
///     is named `LeftRunning`, once per Deploy, for the next full Deploy to prune.
///     A store that could not be read names nothing here. The plan's observation
///     gap names those Machines, and the Deploy ends partial.
///
/// A removal that fails, is refused, is cancelled or goes unconfirmed names the
/// live copy as left running and no unreached copy, so a retry names them all.
async fn remove_container<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    (machine_id, container_id, purpose): (&MachineId, &ContainerId, RemoveContainerPurpose),
    rows: &Rows<'_>,
    cancellation: &CancellationToken,
) -> Result<Ran, OperationFailure> {
    let copy = match inspect(client, machine_id, container_id).await {
        Ok(details) => cleanup::Copy::observed(&details.container),
        Err(ExecutionError::Machine { error, .. }) if error.code == RpcErrorCode::NotFound => {
            return Ok(Ran::default());
        }
        Err(error) => {
            // Unread, the copy may still be In and serving the old version.
            if let (ExecutionError::Machine { error: cause, .. }, Some(service)) =
                (&error, rows.services.get(&index))
            {
                let copy = cleanup::Copy {
                    machine_id: *machine_id,
                    service: QualifiedService::new(rows.namespace.clone(), (*service).clone()),
                    container_id: *container_id,
                    rotation: CopyRotation::In,
                };
                rows.leave_running(progress, &copy, Left::Failed(cause.clone()));
            }
            return Err(error.into());
        }
    };
    let allow_empty = matches!(purpose, RemoveContainerPurpose::Retire);
    let unreached = rows.unreached(client, &copy.service, cancellation).await?;
    let retired = if copy.drains() && unreached.may_keep_route() {
        cleanup::retire_unconfirmed(client, &copy, allow_empty, purpose.into()).await
    } else {
        cleanup::retire(client, &copy, allow_empty, purpose.into(), cancellation).await
    };
    match retired {
        Ok(cleanup::Step::Done) => {
            rows.leave_unreached(progress, unreached.copies());
            Ok(Ran::default())
        }
        Ok(cleanup::Step::Refused(refusal)) => {
            rows.leave_running(progress, &copy, Left::Refused(refusal.clone()));
            Err(machine_error(MachineAction::RemoveContainer, refusal.into_error()).into())
        }
        Ok(cleanup::Step::NotConfirmed(machines)) => {
            let error = machine_error(
                MachineAction::RemoveContainer,
                unavailable(rows.not_confirmed(&machines)),
            );
            rows.leave_running(progress, &copy, Left::NotConfirmed(machines));
            Err(error.into())
        }
        Ok(cleanup::Step::Cancelled) => {
            rows.leave_running(progress, &copy, Left::Interrupted);
            Err(ExecutionError::Cancelled.into())
        }
        Err(error) => {
            rows.leave_running(progress, &copy, Left::Failed(error.clone()));
            Err(machine_error(MachineAction::RemoveContainer, error).into())
        }
    }
}

async fn stop_old_copy<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    old: &ContainerObservation,
    rows: &Rows<'_>,
    cancellation: &CancellationToken,
) -> Result<cleanup::Copy, ExecutionError> {
    match rows.claim(index) {
        Claim::Withdrawn(copy) => return stop_held(client, index, progress, old, rows, copy).await,
        Claim::Restored => return Err(ExecutionError::Cancelled),
        Claim::Own => {}
    }
    let mut copy = cleanup::Copy::observed(old);
    if let Some((withdrawn, step)) = withdraw(client, old, rows, cancellation).await {
        let now = withdrawn.rotation;
        progress.set_running(
            index,
            OperationPhase::WaitingForSwitch {
                container_id: copy.container_id,
            },
        );
        let (left, error) = match step {
            Ok(cleanup::Step::Done) => {
                copy.rotation = now;
                return stop_held(client, index, progress, old, rows, copy).await;
            }
            Ok(cleanup::Step::Refused(refusal)) => {
                return Err(machine_error(
                    MachineAction::StopContainer,
                    refusal.into_error(),
                ));
            }
            Ok(cleanup::Step::NotConfirmed(machines)) => {
                let error = machine_error(
                    MachineAction::StopContainer,
                    unavailable(rows.not_confirmed(&machines)),
                );
                (Left::NotConfirmed(machines), error)
            }
            Ok(cleanup::Step::Cancelled) => (Left::Interrupted, ExecutionError::Cancelled),
            // The ask may have landed, so the copy is restored as if Held.
            Err(error) => (
                Left::Failed(error.clone()),
                machine_error(MachineAction::StopContainer, error),
            ),
        };
        copy.rotation = now;
        leave_unstopped(client, progress, &copy, &old.resolved_spec, rows, left).await;
        return Err(error);
    }
    stop_withdrawn(client, index, progress, &copy).await?;
    Ok(copy)
}

async fn stop_held<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    old: &ContainerObservation,
    rows: &Rows<'_>,
    copy: cleanup::Copy,
) -> Result<cleanup::Copy, ExecutionError> {
    match stop_withdrawn(client, index, progress, &copy).await {
        Ok(()) => Ok(copy),
        Err(error) => {
            let left = left_by(error.clone());
            leave_unstopped(client, progress, &copy, &old.resolved_spec, rows, left).await;
            Err(error)
        }
    }
}

async fn stop_withdrawn<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    copy: &cleanup::Copy,
) -> Result<(), ExecutionError> {
    progress.set_running(index, OperationPhase::StoppingContainer);
    stop(
        client,
        &copy.machine_id,
        &copy.container_id,
        Drain::DEFAULT,
        None,
    )
    .await
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
        Ok(cleanup::BroughtIn::Refused(refusal))
            if refusal.reason == ContainerRefusalReason::OlderThanIn =>
        {
            return retire_older(client, progress, copy, rows).await;
        }
        Ok(cleanup::BroughtIn::Refused(refusal)) => Some(refusal.message),
        Err(error) => Some(error.message),
    };
    match not_restored {
        None => rows.leave_running(progress, copy, left),
        Some(why) => rows.leave_running(progress, copy, Left::NotRestored(Box::new(left), why)),
    }
}

async fn retire_older<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    copy: &cleanup::Copy,
    rows: &Rows<'_>,
) {
    let retired = cleanup::retire(
        client,
        copy,
        false,
        AnonymousVolumes::Remove,
        &CancellationToken::new(),
    )
    .await;
    if let Some(left) = left_unless_done(retired) {
        rows.leave_running(progress, copy, Left::OlderNotRemoved(Box::new(left)));
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
    held_old: Option<(ResolvedServiceSpec, cleanup::Copy)>,
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
        Some((spec, old)) => ReplacementCompensation::OldStopped {
            stop_new_container,
            restore_old_container: restore_old(client, progress, &old, &spec, rows, cancellation)
                .await
                .attempt,
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

/// A restore that failed may still have started the old copy: `may_run`
/// stays true unless the Machine refused it. An error, even one the Machine
/// answered, can come after the copy already runs.
struct Restore {
    attempt: RestoreAttempt<ExecutionError>,
    may_run: bool,
}

async fn restore_old<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    old: &cleanup::Copy,
    spec: &ResolvedServiceSpec,
    rows: &Rows<'_>,
    cancellation: &CancellationToken,
) -> Restore {
    let ended = |attempt, may_run| Restore { attempt, may_run };
    let failed = |error| RestoreAttempt::Failed { error };
    match &old.rotation {
        CopyRotation::Out => return ended(RestoreAttempt::NotServing, false),
        CopyRotation::Pending => {
            let want = ContainerWant::Running {
                container: RunTarget::Existing(ContainerLocator::Id(old.container_id)),
                rotation: RunRotation::Pending,
            };
            return match client.ensure_container(&old.machine_id, want).await {
                Ok(_) => ended(RestoreAttempt::Restored, true),
                Err(error) => ended(
                    failed(machine_error(MachineAction::StartContainer, error)),
                    true,
                ),
            };
        }
        CopyRotation::In | CopyRotation::Held(_) => {}
    }
    match cleanup::bring_in(client, old, spec, Some(rows.run.clone())).await {
        Ok(cleanup::BroughtIn::In) => {}
        Ok(cleanup::BroughtIn::NotIn { health, .. }) => {
            return ended(failed(health_error(old.container_id, health, spec)), true);
        }
        Ok(cleanup::BroughtIn::Refused(refusal))
            if refusal.reason == ployz_core::ContainerRefusalReason::HeldByOtherRun =>
        {
            return ended(RestoreAttempt::LeftHeld, false);
        }
        Ok(cleanup::BroughtIn::Refused(refusal))
            if refusal.reason == ContainerRefusalReason::OlderThanIn =>
        {
            retire_older(client, progress, old, rows).await;
            return ended(RestoreAttempt::NewerServing, false);
        }
        Ok(cleanup::BroughtIn::Refused(refusal)) => {
            return ended(
                failed(machine_error(
                    MachineAction::StartContainer,
                    refusal.into_error(),
                )),
                false,
            );
        }
        Err(error) => {
            return ended(
                failed(machine_error(MachineAction::StartContainer, error)),
                true,
            );
        }
    }
    let copies = BTreeSet::from([old.container_id]);
    let attempt = match cleanup::wait_listed(
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
    };
    ended(attempt, true)
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
    (spec, run): (&ResolvedServiceSpec, &DeploymentLogId),
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
        (spec, run),
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

/// A Service's next gen, and the Machines that left its listing unanswered.
#[derive(Clone)]
struct Numbered {
    r#gen: Gen,
    silent: std::sync::Arc<Vec<Unanswered>>,
}

/// A new copy cannot start while `silent` may run copies the store could not number.
fn unfloored(silent: &Unanswered, unread: &RpcError) -> ExecutionError {
    let error = RpcError {
        code: RpcErrorCode::Unavailable,
        message: format!(
            "Machine '{}' did not answer and the entry Machine's store could not be read, \
             so a new copy might not replace that Machine's copies",
            silent.name
        ),
        details: serde_json::Value::Null,
        cause: std::iter::once(unread.message.clone())
            .chain(unread.cause.iter().cloned())
            .collect(),
    };
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
