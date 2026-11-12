use super::machine_ops::MachineOperations;
use super::{DeployOperation, cleanup};
use super::{
    Left, OperationFailure, Ran, Rows, health_error, inspect, machine_error, operation_at,
};
use crate::deploy::cleanup::{ACK_POLL, EmptyPolicy, NotIn, RotationFailure};
use crate::deploy::progress::Progress;
use crate::deploy::schedule::{ServiceGroup, Stage};
use futures_util::future::join_all;
use ployz_core::{
    AnonymousVolumes, ContainerId, CopyRotation, ExecutionError, MachineAction, MachineId,
    QualifiedService, RpcError, RpcErrorCode, UpdateOrder,
};
use std::collections::BTreeSet;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// The first look at acks is immediate; later ones wait a poll interval.
pub(super) async fn ack_tick(cancellation: &CancellationToken, since: Option<Instant>) {
    if since.is_none() {
        return;
    }
    tokio::select! {
        () = cancellation.cancelled() => {}
        () = tokio::time::sleep(ACK_POLL) => {}
    }
}

pub(super) enum Done {
    Task(usize, Result<Ran, OperationFailure>),
    /// A Rolling step's new copy is In and listed, so the next step may begin.
    Admitted(usize),
    Publish(usize, Published),
}

pub(super) enum Published {
    Listed,
    /// Listed by every proxy but those on gap Servers, which may still route the
    /// old copies, so those stay where they are.
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

/// The copies one group's publish flips: the new ones its tasks started and the
/// old ones they replace.
pub(super) struct GroupCopies<'a> {
    pub(super) service: &'a QualifiedService,
    pub(super) new: Vec<(usize, MachineId, ContainerId)>,
    pub(super) old: Vec<(MachineId, ContainerId)>,
}

pub(super) async fn publish_group<C: MachineOperations>(
    client: &C,
    operations: &[DeployOperation],
    progress: &Progress,
    rows: &Rows<'_>,
    copies: GroupCopies<'_>,
    cancellation: &CancellationToken,
) -> Published {
    let GroupCopies { service, new, old } = copies;
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
    if let Some(gapped) = &gapped {
        rows.gapped
            .lock()
            .expect("gapped lock")
            .insert(service.clone(), gapped.clone());
    }
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
        // Any copy proxies ever routed, Out ones included, may still be on a gap
        // Server's proxy. A copy its last routes leave out, such as one an earlier
        // gapped Deploy started, drains as usual (#3683).
        if let Some(gapped) = &gapped
            && copy.drains()
            && let Some(routing) =
                gaps_still_routing(client, &copy, gapped.machines.iter().copied()).await
        {
            rows.leave_running(progress, &copy, Left::NotConfirmed(routing));
            continue;
        }
        // The owner refuses this Out (LastIn) until its own view lists a new copy.
        // A gap Server's proxy never does; the copy it routes stays In beside.
        if routed && copy.rotation == CopyRotation::In && !listed_on.contains(&copy.machine_id) {
            let gaps = gapped
                .iter()
                .flat_map(|gapped| gapped.machines.iter().copied())
                .collect();
            let left = match cleanup::wait_listed_beside(
                client,
                &copy.machine_id,
                service,
                &ids,
                &gaps,
                cancellation,
                |_| {},
            )
            .await
            {
                Ok(cleanup::Waited::Done | cleanup::Waited::Gapped(_)) => None,
                Ok(cleanup::Waited::TimedOut(_)) => Some(Left::NotListed),
                Ok(cleanup::Waited::Cancelled) => Some(Left::Interrupted),
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
            .map(|copy| cleanup::send_out(client, copy, EmptyPolicy::RequireOtherIn)),
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
        let (left, error) = match unlisted.map(cleanup::Waited::failure) {
            Ok(Ok(())) => (None, None),
            // A cancelled wait leaves the copies running but is no failure to report.
            Ok(Err(RotationFailure::Cancelled)) => (Some(Left::Interrupted), None),
            Ok(Err(failure)) => {
                let (left, error) = rows.failed(failure, MachineAction::ReadRoutes);
                (Some(left), Some(error))
            }
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
        if let Some(left) = left_unless_done(removed.map(cleanup::Rotated::from)) {
            rows.leave_running(progress, copy, left);
        }
    }
    match (unconfirmed, gapped) {
        (Some(error), _) => Published::OutUnconfirmed(error),
        (None, Some(_)) => Published::Gapped,
        (None, None) => Published::Listed,
    }
}

/// Of `gaps`, the Servers whose stale proxy may still route `copy`: one the owner's
/// route read doesn't report Down whose row lists the copy or is missing, or any
/// when the read failed. None when there are none: those proxies' last routes
/// already leave the copy out, so it drains as it would without a gap.
pub(super) async fn gaps_still_routing<C: MachineOperations>(
    client: &C,
    copy: &cleanup::Copy,
    gaps: impl IntoIterator<Item = MachineId>,
) -> Option<cleanup::Unconfirmed> {
    let read = client
        .read_routes(&copy.machine_id, &copy.service)
        .await
        .ok();
    let routing: Vec<MachineId> = gaps
        .into_iter()
        .filter(|gap| {
            // A Down Server's proxy no longer counts, as in any drain.
            read.as_ref().is_none_or(|read| {
                !read.down.contains(gap)
                    && read
                        .routes
                        .get(gap)
                        .cloned()
                        .flatten()
                        .is_none_or(|routed| routed.contains(&copy.container_id))
            })
        })
        .collect();
    (!routing.is_empty()).then(|| cleanup::Unconfirmed {
        machines: routing.clone(),
        unanswered: routing,
    })
}

pub(super) fn left_unless_done(step: Result<cleanup::Rotated, RpcError>) -> Option<Left> {
    match step {
        Ok(step) => step.into(),
        Err(error) => Some(Left::Failed(error)),
    }
}

/// A Rolling replacement's new copy, which goes In before the next Machine's
/// old copy is withdrawn: while it stays Pending, proxies keep that old copy.
pub(super) fn rolling_step(
    operation: &DeployOperation,
    started: Option<ContainerId>,
) -> Option<(MachineId, ContainerId)> {
    let DeployOperation::ReplaceContainer(replacement) = operation else {
        return None;
    };
    (replacement.spec.update.order == UpdateOrder::Rolling)
        .then_some((replacement.machine_id, started?))
}

pub(super) async fn route_new<C: MachineOperations>(
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
                        Ok(brought) => match brought.not_in() {
                            Ok(()) => return Ok(()),
                            Err(NotIn::Health(health)) => health_error(*container_id, health, spec),
                            Err(NotIn::OlderThanIn(_)) => ExecutionError::NewerServing {
                                service: service.clone(),
                            },
                            Err(NotIn::Refused(refusal)) => {
                                machine_error(MachineAction::StartContainer, refusal.into_error())
                            }
                        },
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
        Ok(cleanup::Waited::Done) => Ok(None),
        Ok(cleanup::Waited::Gapped(machines)) => Ok(Some(machines)),
        Ok(cleanup::Waited::TimedOut(machines)) => Err(Published::Unconfirmed(machines)),
        Ok(cleanup::Waited::Cancelled) => Err(Published::Cancelled),
        Err(error) => Err(Published::Failed(machine_error(
            MachineAction::ReadRoutes,
            error,
        ))),
    }
}

/// The copies the group's publish retires. A copy updated in place is one of
/// the Service's serving copies and stays.
pub(super) fn old_copies(
    group: &ServiceGroup,
    operations: &[DeployOperation],
) -> Vec<(MachineId, ContainerId)> {
    group
        .steps
        .iter()
        .flat_map(|step| match step {
            Stage::Serial(index) => vec![*index],
            Stage::Parallel(indices) => indices.clone(),
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
