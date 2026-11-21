use super::cleanup;
use super::machine_ops::CopyLifecycle;
use super::machine_ops::MachineOperations;
use super::stop_first::{Withdrawn, leave_unstopped};
use super::transport::{Outstanding, RESTART_WAIT, RestartTolerant};
use super::{Left, Rows, SlotPolicy, left_by, machine_error, stop};
use crate::connect::{ConnectError, TransportError};
use crate::deploy::cleanup::EmptyPolicy;
use crate::deploy::progress::Progress;
use ployz_core::{
    AnonymousVolumes, AnswerWhen, ContainerId, ContainerKind, ContainerLocator, ContainerNow,
    ContainerRefusalReason, ContainerWant, CopyRotation, DeploymentLogId, Drain,
    ENSURE_PINNED_CONTAINER_CAPABILITY, EnsuredContainer, ExecutionError, Gen, HealthVerdict,
    ImageDigest, LastRun, MachineAction, MachineId, Namespace, NewContainer, OperationPhase,
    OutOfMemory, QualifiedService, ResolvedServiceSpec, RpcError, RpcErrorCode, RunRotation,
    RunTarget, SlotKey, error_chain,
};
use tokio_util::sync::CancellationToken;

/// The owner refuses Gone on a copy that already came In. With `hold`, that
/// copy is Held and stopped, so it can still come back; otherwise it leaves
/// through Out and the unlisted wait.
pub(super) async fn discard_new<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    rows: &Rows<'_>,
    (service, machine_id, container_id): (Option<&QualifiedService>, &MachineId, ContainerId),
    hold: Option<&DeploymentLogId>,
    cancellation: &CancellationToken,
) -> Withdrawn {
    let running = || Withdrawn::Running(*machine_id);
    let gone = cleanup::gone(
        client,
        machine_id,
        ContainerLocator::Id(container_id),
        Drain::NONE,
        AnonymousVolumes::Remove,
        None,
    )
    .await;
    let now = match gone {
        Ok(cleanup::Gone::Removed) => return Withdrawn::Gone,
        Ok(cleanup::Gone::InRotation { now, .. }) => now,
        Ok(cleanup::Gone::Refused(_)) => return running(),
        Err(error) => {
            if let Some(service) = service {
                rows.leave_unrouted(progress, service, *machine_id, container_id, error);
            }
            return running();
        }
    };
    let Some(observed) = now.present() else {
        return running();
    };
    let mut copy = cleanup::Copy::observed(observed);
    let step = match hold {
        Some(run) => cleanup::hold(client, &copy, run.clone(), cancellation).await,
        None => {
            cleanup::retire(
                client,
                &copy,
                EmptyPolicy::MayEmpty,
                AnonymousVolumes::Remove,
                cancellation,
            )
            .await
        }
    };
    let left = match step {
        Ok(cleanup::Rotated::Done) => None,
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

/// Remove a copy an ended run left Pending. The owner refuses Gone on a copy that is
/// In or Held, which the run's runner may have made it after losing its claim: that
/// copy serves, so it stays, named because it serves beside this Deploy's new copies.
/// One that stays for any other reason is named too, and the Deploy goes on without it.
pub(super) async fn remove_stray<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    rows: &Rows<'_>,
    (service, machine_id, container_id): (&QualifiedService, &MachineId, ContainerId),
    cancellation: &CancellationToken,
) {
    let removed = tokio::time::timeout(
        RESTART_WAIT,
        cleanup::gone(
            client,
            machine_id,
            ContainerLocator::Id(container_id),
            Drain::NONE,
            AnonymousVolumes::Remove,
            Some(RESTART_WAIT),
        ),
    );
    let answer = tokio::select! {
        answer = removed => answer.unwrap_or_else(|_| {
            Err(crate::cluster::answered_by(
                machine_id,
                ConnectError::Rpc(TransportError::budget_expired(
                    "target Machine RPC timed out",
                )),
            ))
        }),
        () = cancellation.cancelled() => {
            rows.name_left(progress, service, *machine_id, container_id, "interrupted".into());
            return;
        }
    };
    let error = match answer {
        Ok(cleanup::Gone::Removed) => return,
        Ok(cleanup::Gone::InRotation { .. }) => {
            rows.name_left(
                progress,
                service,
                *machine_id,
                container_id,
                "it came into routing after this deploy planned".into(),
            );
            return;
        }
        Ok(cleanup::Gone::Refused(refusal)) => refusal.into_error(),
        Err(error) => error,
    };
    rows.leave_unrouted(progress, service, *machine_id, container_id, error);
}

/// Why a move's replacement never served, and whether none of it is left on its
/// destination; otherwise it may still be there.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct MoveError {
    pub(crate) end: MoveEnd,
    pub(crate) replacement_removed: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum MoveEnd {
    Cancelled,
    NotServing {
        detail: String,
    },
    /// The destination predates the pinned route, so it refused every send.
    NeedsUpgrade,
}

impl From<ExecutionError> for MoveEnd {
    fn from(error: ExecutionError) -> Self {
        match error {
            ExecutionError::Cancelled => Self::Cancelled,
            error @ (ExecutionError::Machine { .. }
            | ExecutionError::Health { .. }
            | ExecutionError::DependencyHealth { .. }
            | ExecutionError::Hook { .. }
            | ExecutionError::NewerServing { .. }) => Self::NotServing {
                detail: error_chain::row(&error),
            },
        }
    }
}

/// A move's replacement, Pending and healthy on its destination.
#[derive(Debug)]
pub(crate) struct Started {
    pub(crate) container_id: ContainerId,
    pub(crate) deploy: Option<DeploymentLogId>,
    to: MachineId,
    /// The slot the move minted. Only its own sends made what it holds.
    slot: SlotKey,
    /// A send of the move lost its answer or was cut off while out, so it may still make
    /// a Container after any removal.
    unsettled: bool,
}

impl Started {
    /// Remove the replacement by the move's slot, and say whether none of it is left.
    pub(crate) async fn discard<C: CopyLifecycle>(
        self,
        client: &C,
        cancellation: &CancellationToken,
    ) -> bool {
        let client = RestartTolerant::new(client, cancellation);
        emptied(&client, &self.to, self.slot).await && !self.unsettled
    }
}

/// Create the replacement of `old` on `to` and wait for its health. `image` is the image
/// ID the old copy runs. A spec whose image is a tag goes on the pinned route, pinned to
/// it, so whatever the tag names on `to` never decides what runs; any other spec goes on
/// ensure.v1 as it is.
///
/// Every send of one move carries the slot it mints, `move:{old}:{nonce}`, which no other
/// caller names, so a resend adopts what an earlier send made. After any end but a healthy
/// Container, the move asks that slot Gone by key, resent only while the Machine restarts:
/// it can take only what its own sends made, whatever an error says the Machine did.
pub(crate) async fn move_container<C: MachineOperations>(
    client: &C,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    to: &MachineId,
    (from, old): (&MachineId, &ContainerId),
    image: &ImageDigest,
    cancellation: &CancellationToken,
) -> Result<Started, MoveError> {
    let client = RestartTolerant::new(client, cancellation);
    let policy = SlotPolicy::of(ContainerKind::ServiceContainer, &spec.mode);
    let global = policy.keyed_by_spec();
    let slot = policy.slot(
        namespace,
        ContainerKind::ServiceContainer,
        spec,
        None,
        || format!("move:{old}:{}", uuid::Uuid::new_v4().simple()),
    );
    let adopts = global && adopts_running(&client, to, &slot, spec).await;
    let source = match client.inspect_container(from, old).await {
        Ok(details) => details.container,
        Err(error) => {
            return Err(MoveError {
                end: machine_error(MachineAction::InspectContainer, error).into(),
                replacement_removed: !adopts,
            });
        }
    };
    let deploy = source.deploy_label();
    let want = ContainerWant::Running {
        container: RunTarget::New(NewContainer {
            r#gen: source.copy_gen(),
            slot: slot.clone(),
            resolved_spec: spec.clone(),
            deployment_id: deploy.clone(),
            registry_auth: None,
            image_prepared: true,
            answer: AnswerWhen::Settled,
            image_id: (!ployz_core::authority::deploy::content_addressed(&spec.container.image))
                .then(|| image.clone()),
        }),
        rotation: RunRotation::Pending,
    };
    let outstanding = Outstanding::default();
    let answered = tokio::select! {
        answered = client.ensure_new_noting(to, want, &outstanding) => Some(answered),
        () = cancellation.cancelled() => None,
    };
    let end = match answered {
        Some(Ok(EnsuredContainer::Ensured {
            now: ContainerNow::Present(observed),
            health,
            ..
        })) => {
            let id = observed.container_id;
            let failed = if cancellation.is_cancelled() {
                Some(MoveEnd::Cancelled)
            } else {
                started_container(Ok((id, health)), spec, cancellation)
                    .err()
                    .map(MoveEnd::from)
            };
            match failed {
                None => {
                    return Ok(Started {
                        container_id: id,
                        deploy,
                        to: *to,
                        slot,
                        unsettled: outstanding.any(),
                    });
                }
                Some(end) => end,
            }
        }
        Some(Ok(EnsuredContainer::Ensured {
            now: ContainerNow::Absent { .. },
            ..
        })) => MoveEnd::NotServing {
            detail: format!(
                "the Machine started no Container for slot {}",
                slot.creation_key.as_str()
            ),
        },
        Some(Ok(EnsuredContainer::Refused { refusal, .. })) => MoveEnd::NotServing {
            detail: error_chain::row(&machine_error(
                MachineAction::CreateContainer,
                refusal.into_error(),
            )),
        },
        Some(Err(error))
            if error.refusal().is_some_and(|refusal| {
                refusal.capability == ENSURE_PINNED_CONTAINER_CAPABILITY
            }) =>
        {
            MoveEnd::NeedsUpgrade
        }
        Some(Err(error)) => machine_error(MachineAction::CreateContainer, error).into(),
        None => MoveEnd::Cancelled,
    };
    let keep = adopts || (global && !cancellation.is_cancelled());
    let replacement_removed = !keep && emptied(&client, to, slot).await && !outstanding.any();
    Err(MoveError {
        end,
        replacement_removed,
    })
}

/// Ask `slot` on `machine_id` Gone, sent again while the Machine restarts, and say whether
/// the Machine answered that nothing of it is there. A removal unanswered for as long as a
/// restart takes may not have happened, and one that reads back a Container left one.
async fn emptied<C: CopyLifecycle>(
    client: &RestartTolerant<'_, C>,
    machine_id: &MachineId,
    slot: SlotKey,
) -> bool {
    let want = ContainerWant::Gone {
        container: ContainerLocator::Key(slot),
        drain: Drain::NONE,
        anonymous_volumes: AnonymousVolumes::Remove,
    };
    let gone = client.ensure_unsettled(machine_id, want, Some(RESTART_WAIT));
    match tokio::time::timeout(RESTART_WAIT, gone).await {
        Ok(Ok(EnsuredContainer::Ensured { now, .. })) => now.present().is_none(),
        Ok(Ok(EnsuredContainer::Refused { now, refusal })) => {
            refusal.reason == ContainerRefusalReason::NotFound && now.present().is_none()
        }
        Ok(Err(_)) | Err(_) => false,
    }
}

/// What a failed Global creation leaves on its Machine. A stop-first replacement
/// retains the keyed candidate so its retry needs no new endpoint; every other
/// start leaves nothing of this run behind.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum FailedStart {
    Discard,
    Retain,
}

/// One creation of a Container: the client that sends it, the row that shows it
/// (none when no row of the Deploy does) and the Deploy's cancel.
pub(super) struct Attempt<'a, C> {
    pub(super) client: &'a C,
    pub(super) row: Option<Row<'a>>,
    pub(super) cancellation: &'a CancellationToken,
}

impl<C> Attempt<'_, C> {
    pub(super) fn running(&self, phase: OperationPhase) {
        if let Some(row) = self.row {
            row.running(phase);
        }
    }
}

/// The progress row of one operation.
#[derive(Clone, Copy)]
pub(super) struct Row<'a> {
    pub(super) progress: &'a Progress,
    pub(super) index: usize,
}

impl Row<'_> {
    pub(super) fn running(self, phase: OperationPhase) {
        self.progress.set_running(self.index, phase);
    }
}

/// The Container one start creates and how its Machine answers.
pub(super) struct SlotRun<'a> {
    pub(super) machine_id: &'a MachineId,
    pub(super) slot: SlotKey,
    pub(super) spec: &'a ResolvedServiceSpec,
    pub(super) deploy: Option<&'a DeploymentLogId>,
    pub(super) r#gen: Gen,
    pub(super) answer: AnswerWhen,
}

pub(super) async fn start<C: MachineOperations>(
    attempt: &Attempt<'_, C>,
    run: SlotRun<'_>,
    failed: FailedStart,
) -> Result<ContainerId, ExecutionError> {
    let Attempt {
        client,
        cancellation,
        ..
    } = *attempt;
    let (machine_id, slot, spec) = (run.machine_id, run.slot.clone(), run.spec);
    let keyed_by_spec = SlotPolicy::of(slot.kind, &spec.mode).keyed_by_spec();
    let adopts = keyed_by_spec && adopts_running(client, machine_id, &slot, spec).await;
    let started = run_new(attempt, run).await;
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
            machine = machine_id.to_string().as_str(),
            ?slot,
            code = error.code.as_str(),
            error = error.message.as_str(),
            "couldn't remove a failed start's Container"
        );
    }
    started_container(started, spec, cancellation)
}

/// A Global start adopts a running Container of its spec, which a cancel must keep.
/// An unanswered check keeps it too.
pub(super) async fn adopts_running<C: MachineOperations>(
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

/// Remove what a failed creation left in `slot`, resending while its Machine is away
/// for as long as a restart takes, then giving up.
pub(super) async fn discard_attempt<C: MachineOperations>(
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
    match tokio::time::timeout(RESTART_WAIT, discarded).await {
        Ok(result) => result.map(|_| ()),
        Err(_) => Err(crate::cluster::answered_by(
            machine_id,
            ConnectError::Rpc(TransportError::budget_expired(
                "target Machine RPC timed out",
            )),
        )),
    }
}

pub(super) async fn run_new<C: MachineOperations>(
    attempt: &Attempt<'_, C>,
    run: SlotRun<'_>,
) -> Result<(ContainerId, Option<HealthVerdict>), RpcError> {
    let SlotRun {
        machine_id,
        slot,
        spec,
        deploy,
        r#gen,
        answer,
    } = run;
    attempt.running(OperationPhase::CreatingContainer);
    let want = ContainerWant::Running {
        container: RunTarget::New(NewContainer {
            r#gen,
            slot: slot.clone(),
            resolved_spec: spec.clone(),
            deployment_id: deploy.cloned(),
            registry_auth: None,
            image_prepared: true,
            answer,
            image_id: None,
        }),
        rotation: RunRotation::Pending,
    };
    let ensured = attempt.client.ensure_container(machine_id, want);
    let ensured = match answer {
        AnswerWhen::Started => ensured.await,
        // A cancel abandons the Machine's health wait; the caller then discards
        // whatever the slot holds.
        AnswerWhen::Settled => tokio::select! {
            answered = ensured => answered,
            () = attempt.cancellation.cancelled() => return Err(RpcError::unavailable(format!(
                "the Deploy was cancelled while Machine {machine_id} waited on health"
            ))),
        },
    }?;
    match ensured {
        (ContainerNow::Present(observed), health) => {
            if let Some(row) = attempt.row {
                row.progress
                    .set_display_name(row.index, observed.display_name.clone());
            }
            Ok((observed.container_id, health))
        }
        (ContainerNow::Absent { .. }, _) => Err(RpcError::internal(format!(
            "the Machine started no Container for slot {}",
            slot.creation_key.as_str()
        ))),
    }
}

pub(super) fn out_of_memory(
    health: &HealthVerdict,
    spec: &ResolvedServiceSpec,
) -> Option<OutOfMemory> {
    health.oom_killed().then_some(OutOfMemory {
        memory_bytes: spec.container.resources.memory_bytes,
    })
}

#[expect(
    clippy::result_large_err,
    reason = "ExecutionError is the Deploy's wire failure value; every step in this file returns it unboxed"
)]
pub(super) fn started_container(
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
    let last_run = health.last_exit_code().map(|code| LastRun {
        exit_code: Some(code),
        last_line: None,
    });
    match health.into_failure() {
        None => Ok(container_id),
        Some(failure) => Err(ExecutionError::Health {
            container_id,
            failure,
            out_of_memory,
            last_run,
            departing,
        }),
    }
}

pub(super) fn answer_when(skip_health_monitor: bool) -> AnswerWhen {
    if skip_health_monitor {
        AnswerWhen::Started
    } else {
        AnswerWhen::Settled
    }
}
