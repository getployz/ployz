use super::publish::{gaps_still_routing, left_unless_done};
use super::report::{Left, left_by};
use super::start::{Attempt, FailedStart, Row, SlotRun, discard_new, start};
use super::{
    DeployOperation, ReplacementCompensation, ReplacementOperation, RestoreAttempt, StopAttempt,
    cleanup,
};
use super::{
    GroupRecord, OperationFailure, Ran, Shared, health_error, inspect, machine_error, operation_at,
    remove_by_id, stop,
};
use crate::deploy::cleanup::{EmptyPolicy, NotIn, RotationFailure};
use crate::deploy::machine_calls::MachineOperations;
use crate::deploy::progress::Progress;
use crate::deploy::schedule::ServiceGroup;
use futures_util::future::join_all;
use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerLocator, ContainerObservation, ContainerRefusalReason,
    ContainerRuntimeObservation, ContainerWant, CopyRotation, Drain, ExecutionError, MachineAction,
    MachineId, OperationPhase, QualifiedService, RemoveContainerPurpose, ResolvedServiceSpec,
    RpcError, RpcErrorCode, RunRotation, RunTarget,
};
use std::collections::{BTreeMap, BTreeSet};
use tokio_util::sync::CancellationToken;

/// A stop-first copy shares its Volume with the new copy on its Machine, so
/// each new copy is gone or stopped before an old copy runs again. Cleanup
/// that must finish after a cancel waits on its own token; every wait is
/// bounded by `ACK_WAIT`.
pub(super) async fn restore_group<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    service: Option<&QualifiedService>,
    record: GroupRecord,
    shared: &Shared<'_>,
) {
    let GroupRecord {
        started,
        stopped,
        unread,
        ..
    } = record;
    let started: Vec<_> = started
        .into_iter()
        .map(|(_, machine, id)| (machine, id))
        .collect();
    let cleanup = CancellationToken::new();
    if stopped.is_empty() {
        if !unread {
            join_all(started.iter().map(|(machine, id)| {
                discard_new(
                    client,
                    progress,
                    shared,
                    (service, machine, *id),
                    None,
                    &cleanup,
                )
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
                shared,
                (service, machine, *id),
                Some(shared.plan.run()),
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
            let restore = restore_old(client, progress, old, spec, shared, &cleanup).await;
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
            shared.report.leave_running(
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
            let left = match cleanup::retire(
                client,
                &copy,
                EmptyPolicy::RequireOtherIn,
                AnonymousVolumes::Remove,
                &cleanup,
            )
            .await
            {
                Ok(step) => Option::<Left>::from(step),
                Err(error) => Some(Left::Failed(error)),
            };
            if let Some(left) = left {
                shared.report.leave_running(progress, &copy, left);
            }
        } else if may_run.contains(&copy.machine_id) {
            // Its old copy may be writing the Volume, so it stays stopped.
            shared.report.leave_running(
                progress,
                &copy,
                Left::NotRestored(
                    Box::new(Left::DeployFailed),
                    "its old copy may be serving".to_owned(),
                ),
            );
        } else {
            // No old copy came back, so the healthy new copy serves again.
            leave_unstopped(client, progress, &copy, &spec, shared, Left::DeployFailed).await;
        }
    }
}

pub(super) enum Withdrawn {
    Gone,
    /// Held by this run and stopped: it can go back In if no old copy does.
    Held(Box<ResolvedServiceSpec>, cleanup::Copy),
    /// It may still run on this Machine.
    Running(MachineId),
}

pub(super) async fn replace_stop_first<C: MachineOperations>(
    client: &C,
    row: Row<'_>,
    operation: &ReplacementOperation,
    old: Option<ContainerObservation>,
    run: SlotRun<'_>,
    shared: &Shared<'_>,
    cancellation: &CancellationToken,
) -> Result<Ran, OperationFailure> {
    let Row { progress, index } = row;
    let held_old = match old {
        Some(old) => {
            let was_running = match old.runtime {
                ContainerRuntimeObservation::Created
                | ContainerRuntimeObservation::Exited { .. }
                | ContainerRuntimeObservation::Dead => !shared.plan.routed(&old.identity()),
                ContainerRuntimeObservation::Removing => false,
                ContainerRuntimeObservation::Running { .. }
                | ContainerRuntimeObservation::Paused
                | ContainerRuntimeObservation::Restarting
                | ContainerRuntimeObservation::Unknown { .. } => true,
            };
            let stopped =
                stop_old_copy(client, index, progress, &old, shared, cancellation).await?;
            // A copy that was neither serving nor running has nothing to restore.
            (was_running || stopped.rotation != CopyRotation::Pending)
                .then(|| (old.resolved_spec.clone(), stopped))
        }
        None => None,
    };
    let attempt = Attempt {
        client,
        row: Some(row),
        cancellation,
    };
    let started = start(&attempt, run, FailedStart::Retain).await;
    match started {
        Ok(new_container) => Ok(Ran {
            started: Some(new_container),
            held_old: held_old.map(Box::new),
        }),
        Err(error) => Err(compensate(
            client,
            row,
            operation,
            held_old,
            shared,
            cancellation,
            error,
        )
        .await),
    }
}

/// An In copy is Held for this run so a failed Service can bring it back; a
/// copy another run holds goes Out, since Held never passes between runs.
/// Returns the copy as it now is, or `None` when it needs no withdrawal.
pub(super) async fn withdraw<C: MachineOperations>(
    client: &C,
    old: &ContainerObservation,
    shared: &Shared<'_>,
    cancellation: &CancellationToken,
) -> Option<(cleanup::Copy, Result<cleanup::Rotated, RpcError>)> {
    let mut copy = cleanup::Copy::observed(old);
    let (step, now) = match &copy.rotation {
        CopyRotation::Pending | CopyRotation::Out => return None,
        _ if !shared.plan.routed(&old.identity()) => return None,
        CopyRotation::In => (
            cleanup::hold(client, &copy, shared.plan.run().clone(), cancellation).await,
            CopyRotation::Held(shared.plan.run().clone()),
        ),
        CopyRotation::Held(run) if run == shared.plan.run() => (
            cleanup::hold(client, &copy, shared.plan.run().clone(), cancellation).await,
            copy.rotation.clone(),
        ),
        CopyRotation::Held(_) => (
            cleanup::take_out(client, &copy, EmptyPolicy::RequireOtherIn, cancellation).await,
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
pub(super) async fn remove_container<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    (machine_id, container_id, purpose): (&MachineId, &ContainerId, RemoveContainerPurpose),
    shared: &Shared<'_>,
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
                (&error, shared.plan.removed_by(index))
            {
                let copy = cleanup::Copy {
                    machine_id: *machine_id,
                    service,
                    container_id: *container_id,
                    rotation: CopyRotation::In,
                };
                shared
                    .report
                    .leave_running(progress, &copy, Left::Failed(cause.clone()));
            }
            return Err(error.into());
        }
    };
    let gapped = copy
        .drains()
        .then(|| shared.gapped.get(&copy.service))
        .flatten();
    if let Some(gapped) = &gapped
        && let Some(routing) =
            gaps_still_routing(client, &copy, gapped.machines.iter().copied()).await
    {
        shared
            .report
            .leave_running(progress, &copy, Left::NotConfirmed(routing));
        return Ok(Ran::default());
    }
    // A copy an earlier gapped Deploy left may still be routed by a gap Server's
    // proxy, which never acks its drain (#3676).
    if copy.drains()
        && matches!(purpose, RemoveContainerPurpose::Rollout)
        && let Some(gaps) = shared
            .gens
            .gaps_routing(client, progress, &copy, cancellation)
            .await
    {
        shared
            .report
            .leave_running(progress, &copy, Left::NotConfirmed(gaps));
        return Ok(Ran::default());
    }
    let empty = EmptyPolicy::from(purpose);
    let unreached = shared
        .gens
        .unreached(client, &copy.service, cancellation)
        .await?;
    let retired = if copy.drains() && unreached.may_keep_route() {
        cleanup::retire_unconfirmed(client, &copy, empty, purpose.into()).await
    } else {
        cleanup::retire(client, &copy, empty, purpose.into(), cancellation).await
    };
    match retired.map(cleanup::Rotated::failure) {
        Ok(Ok(())) => {
            shared.report.leave_unreached(progress, unreached.copies());
            Ok(Ran::default())
        }
        // The run already ends with its Service's publish gapped, and no In copy
        // the gap proxy routes is acked by every proxy: the owner keeps this one In
        // for the next Deploy.
        Ok(Err(RotationFailure::Refused(refusal)))
            if gapped.is_some() && refusal.reason == ContainerRefusalReason::LastIn =>
        {
            shared
                .report
                .leave_running(progress, &copy, Left::Refused(refusal));
            Ok(Ran::default())
        }
        Ok(Err(failure)) => {
            let (left, error) = shared
                .report
                .failed(failure, MachineAction::RemoveContainer);
            shared.report.leave_running(progress, &copy, left);
            Err(error.into())
        }
        Err(error) => {
            shared
                .report
                .leave_running(progress, &copy, Left::Failed(error.clone()));
            Err(machine_error(MachineAction::RemoveContainer, error).into())
        }
    }
}

pub(super) async fn stop_old_copy<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    old: &ContainerObservation,
    shared: &Shared<'_>,
    cancellation: &CancellationToken,
) -> Result<cleanup::Copy, ExecutionError> {
    match shared.stop_first.claim(index) {
        Claim::Withdrawn(copy) => {
            return stop_held(client, index, progress, old, shared, copy).await;
        }
        Claim::Restored => return Err(ExecutionError::Cancelled),
        Claim::Own => {}
    }
    let mut copy = cleanup::Copy::observed(old);
    if let Some((withdrawn, step)) = withdraw(client, old, shared, cancellation).await {
        let now = withdrawn.rotation;
        progress.set_running(
            index,
            OperationPhase::WaitingForSwitch {
                container_id: copy.container_id,
            },
        );
        let (left, error) = match step.map(cleanup::Rotated::failure) {
            Ok(Ok(())) => {
                copy.rotation = now;
                return stop_held(client, index, progress, old, shared, copy).await;
            }
            // Answered before the Machine acted, so there is nothing to put back.
            Ok(Err(RotationFailure::Refused(refusal))) => {
                return Err(machine_error(
                    MachineAction::StopContainer,
                    refusal.into_error(),
                ));
            }
            Ok(Err(failure)) => shared.report.failed(failure, MachineAction::StopContainer),
            // The ask may have landed, so the copy is restored as if Held.
            Err(error) => (
                Left::Failed(error.clone()),
                machine_error(MachineAction::StopContainer, error),
            ),
        };
        copy.rotation = now;
        leave_unstopped(client, progress, &copy, &old.resolved_spec, shared, left).await;
        return Err(error);
    }
    stop_withdrawn(client, index, progress, &copy).await?;
    Ok(copy)
}

pub(super) async fn stop_held<C: MachineOperations>(
    client: &C,
    index: usize,
    progress: &Progress,
    old: &ContainerObservation,
    shared: &Shared<'_>,
    copy: cleanup::Copy,
) -> Result<cleanup::Copy, ExecutionError> {
    match stop_withdrawn(client, index, progress, &copy).await {
        Ok(()) => Ok(copy),
        Err(error) => {
            let left = left_by(error.clone());
            leave_unstopped(client, progress, &copy, &old.resolved_spec, shared, left).await;
            Err(error)
        }
    }
}

pub(super) async fn stop_withdrawn<C: MachineOperations>(
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
pub(super) async fn leave_unstopped<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    copy: &cleanup::Copy,
    spec: &ResolvedServiceSpec,
    shared: &Shared<'_>,
    left: Left,
) {
    if copy.rotation != CopyRotation::Held(shared.plan.run().clone()) {
        shared.report.leave_running(progress, copy, left);
        return;
    }
    let not_restored =
        match cleanup::bring_in(client, copy, spec, Some(shared.plan.run().clone())).await {
            Ok(brought) => match brought.not_in() {
                Ok(()) => None,
                Err(NotIn::Health(_)) => Some("it failed health".to_owned()),
                Err(NotIn::OlderThanIn(_)) => {
                    return retire_older(client, progress, copy, shared).await;
                }
                Err(NotIn::Refused(refusal)) => Some(refusal.message),
            },
            Err(error) => Some(error.message),
        };
    match not_restored {
        None => shared.report.leave_running(progress, copy, left),
        Some(why) => {
            shared
                .report
                .leave_running(progress, copy, Left::NotRestored(Box::new(left), why))
        }
    }
}

pub(super) async fn retire_older<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    copy: &cleanup::Copy,
    shared: &Shared<'_>,
) {
    let retired = cleanup::retire(
        client,
        copy,
        EmptyPolicy::RequireOtherIn,
        AnonymousVolumes::Remove,
        &CancellationToken::new(),
    )
    .await;
    if let Some(left) = left_unless_done(retired) {
        shared
            .report
            .leave_running(progress, copy, Left::OlderNotRemoved(Box::new(left)));
    }
}

pub(super) async fn compensate<C: MachineOperations>(
    client: &C,
    row: Row<'_>,
    operation: &ReplacementOperation,
    held_old: Option<(ResolvedServiceSpec, cleanup::Copy)>,
    shared: &Shared<'_>,
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
    let Row { progress, .. } = row;
    row.running(OperationPhase::Compensating);
    let stop_new_container = match new_container {
        Some(container_id) => Some(StopAttempt::from(
            remove_by_id(client, &operation.machine_id, &container_id, Drain::NONE).await,
        )),
        None => None,
    };
    let compensation = match held_old {
        Some((spec, old)) => ReplacementCompensation::OldStopped {
            stop_new_container,
            restore_old_container: restore_old(client, progress, &old, &spec, shared, cancellation)
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
pub(super) struct Restore {
    pub(super) attempt: RestoreAttempt<ExecutionError>,
    pub(super) may_run: bool,
}

pub(super) async fn restore_old<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    old: &cleanup::Copy,
    spec: &ResolvedServiceSpec,
    shared: &Shared<'_>,
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
    match cleanup::bring_in(client, old, spec, Some(shared.plan.run().clone()))
        .await
        .map(cleanup::BroughtIn::not_in)
    {
        Ok(Ok(())) => {}
        Ok(Err(NotIn::Health(health))) => {
            return ended(failed(health_error(old.container_id, health, spec)), true);
        }
        Ok(Err(NotIn::Refused(refusal)))
            if refusal.reason == ployz_core::ContainerRefusalReason::HeldByOtherRun =>
        {
            return ended(RestoreAttempt::LeftHeld, false);
        }
        Ok(Err(NotIn::OlderThanIn(_))) => {
            retire_older(client, progress, old, shared).await;
            return ended(RestoreAttempt::NewerServing, false);
        }
        Ok(Err(NotIn::Refused(refusal))) => {
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
        Ok(wait) => match wait.failure() {
            Ok(()) => RestoreAttempt::Restored,
            Err(failure) => failed(shared.report.failed(failure, MachineAction::ReadRoutes).1),
        },
        Err(error) => failed(machine_error(MachineAction::ReadRoutes, error)),
    };
    ended(attempt, true)
}

/// The stop-first steps of one Deploy, each withdrawing its old copies together
/// before any of its tasks creates a copy.
pub(super) struct StopFirstSteps<'a>(Vec<StopFirstStep<'a>>);

struct StopFirstStep<'a> {
    tasks: Vec<(usize, &'a ReplacementOperation)>,
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

pub(super) enum Claim {
    Withdrawn(cleanup::Copy),
    Restored,
    /// Not withdrawn for a step: the task withdraws it itself.
    Own,
}

impl<'a> StopFirstSteps<'a> {
    pub(super) fn new(operations: &'a [DeployOperation], groups: &[ServiceGroup]) -> Self {
        let steps = groups
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
        Self(steps)
    }

    fn step_of(&self, index: usize) -> Option<&StopFirstStep<'a>> {
        self.0
            .iter()
            .find(|step| step.tasks.iter().any(|(task, _)| *task == index))
    }

    /// The old copy of stop-first task `index`, once its step withdrew every old copy.
    pub(super) async fn inspect<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        shared: &Shared<'_>,
        index: usize,
        cancellation: &CancellationToken,
    ) -> Result<Option<ContainerObservation>, ExecutionError> {
        let step = self
            .step_of(index)
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
                withdraw_step(client, progress, shared, old, cancellation).await
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

    pub(super) fn claim(&self, index: usize) -> Claim {
        let Some(inspected) = self.step_of(index).and_then(|step| step.ready.get()) else {
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
    pub(super) async fn restore_unclaimed<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        shared: &Shared<'_>,
        in_group: impl Fn(usize) -> bool,
    ) {
        let mut unclaimed = Vec::new();
        for step in &self.0 {
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
            leave_unstopped(client, progress, copy, spec, shared, Left::Interrupted)
        }))
        .await;
    }
}

/// Withdraw every old copy of a stop-first step together. If one is not
/// withdrawn, bring back each one this run Held, before any copy starts.
async fn withdraw_step<C: MachineOperations>(
    client: &C,
    progress: &Progress,
    shared: &Shared<'_>,
    mut old: BTreeMap<usize, Result<Option<ContainerObservation>, ExecutionError>>,
    cancellation: &CancellationToken,
) -> Inspected {
    // A dead run's Pending copies were never routed, so when the step
    // withdraws a routed copy, they go first with no proxy wait.
    let withdraws = old
        .values()
        .filter_map(|observed| observed.as_ref().ok()?.as_ref())
        .any(|observed| {
            shared.plan.routed(&observed.identity())
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
                    && shared.plan.routed(&observed.identity())
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
            let (copy, step) = withdraw(client, observed, shared, cancellation).await?;
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
                let (left, error) = shared.report.failed(failure, MachineAction::StopContainer);
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
            leave_unstopped(
                client,
                progress,
                copy,
                &observed.resolved_spec,
                shared,
                left,
            )
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
