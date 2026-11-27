use super::report::{Left, left_by};
use super::{DeployOperation, cleanup};
use super::{OperationFailure, Ran, Shared, health_error, inspect, machine_error, operation_at};
use crate::deploy::cleanup::{ACK_POLL, EmptyPolicy, NotIn, RotationFailure};
use crate::deploy::machine_calls::MachineOperations;
use crate::deploy::progress::Progress;
use crate::deploy::schedule::{ServiceGroup, Stage};
use futures_util::future::join_all;
use ployz_core::{
    AnonymousVolumes, ContainerId, CopyRotation, ExecutionError, MachineAction, MachineId,
    QualifiedService, RpcError, RpcErrorCode, ServiceName, UpdateOrder,
};
use std::collections::{BTreeMap, BTreeSet};
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

/// Each Service whose publish ended with only gap Servers' proxies unconfirmed.
/// Those proxies may still route any of its old copies, so none is retired.
#[derive(Default)]
pub(super) struct GappedPublishes(
    std::sync::Mutex<BTreeMap<QualifiedService, cleanup::Unconfirmed>>,
);

impl GappedPublishes {
    fn record(&self, service: &QualifiedService, gapped: &cleanup::Unconfirmed) {
        self.0
            .lock()
            .expect("gapped lock")
            .insert(service.clone(), gapped.clone());
    }

    /// The proxies that never confirmed `service`'s publish, when only gap Servers'
    /// did not.
    pub(super) fn get(&self, service: &QualifiedService) -> Option<cleanup::Unconfirmed> {
        self.0.lock().expect("gapped lock").get(service).cloned()
    }
}

/// The old copies publishes took out of rotation, or that never went In. No proxy
/// routes them, so they retire once the Deploy's Outcome is out.
struct Retirement {
    copy: cleanup::Copy,
    ask: AnonymousVolumes,
    running: bool,
}

#[derive(Default)]
pub(super) struct Retiring(std::sync::Mutex<Vec<Retirement>>);

impl Retiring {
    fn add(&self, copies: impl IntoIterator<Item = Retirement>) {
        self.0.lock().expect("retiring lock").extend(copies);
    }

    pub(super) fn abandon(&self, progress: &Progress, report: &super::report::Report<'_>) {
        for retirement in std::mem::take(&mut *self.0.lock().expect("retiring lock")) {
            report.unfinished_removal(
                progress,
                &retirement.copy,
                retirement.running,
                Left::Interrupted,
            );
        }
    }

    /// Stop and remove every copy, each Machine's at once, and say when each
    /// Service's are done. A copy that stays is a warning, never a failure: the
    /// Outcome is already out.
    pub(super) async fn retire<C: MachineOperations>(
        &self,
        client: &C,
        progress: &Progress,
        report: &super::report::Report<'_>,
        cancellation: &CancellationToken,
    ) {
        let mut services: BTreeMap<ServiceName, Vec<Retirement>> = BTreeMap::new();
        for retirement in std::mem::take(&mut *self.0.lock().expect("retiring lock")) {
            services
                .entry(retirement.copy.service.name.clone())
                .or_default()
                .push(retirement);
        }
        join_all(services.into_iter().map(|(service, copies)| async move {
            join_all(
                copies
                    .iter()
                    .map(|Retirement { copy, ask, running }| async {
                        let left = if cancellation.is_cancelled() {
                            Some(Left::Interrupted)
                        } else {
                            tokio::select! {
                                biased;
                                removed = cleanup::remove(client, copy, *ask) => {
                                    left_unless_done(removed.map(cleanup::Rotated::from))
                                }
                                () = cancellation.cancelled() => Some(Left::Interrupted),
                            }
                        };
                        if let Some(left) = left {
                            report.unfinished_removal(progress, copy, *running, left);
                        }
                    }),
            )
            .await;
            if !cancellation.is_cancelled() {
                progress.retired(service);
            }
        }))
        .await;
    }
}

pub(super) enum Done {
    Task(usize, Result<Ran, OperationFailure>),
    /// A Rolling step's new copy is In and listed, so the next step may begin.
    Admitted(usize),
    /// The group's publish of its Service ended.
    Publish(usize, ServiceName, Published),
}

pub(super) enum Switch {
    NotNeeded,
    Confirmed,
    Unconfirmed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Permission {
    WithoutCommit,
    Committed,
    Refused,
    Unavailable,
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
    shared: &Shared<'_>,
    copies: GroupCopies<'_>,
    cancellation: &CancellationToken,
    switched: impl AsyncFnOnce(Switch) -> Permission,
) -> Published {
    let GroupCopies { service, new, old } = copies;
    let routed = shared.plan.routed(service);
    let gapped = if routed && !new.is_empty() {
        match route_new(client, operations, shared, service, &new, cancellation).await {
            Ok(gapped) => gapped,
            Err(published) => return published,
        }
    } else {
        None
    };
    let switch = if new.is_empty() {
        Switch::NotNeeded
    } else if gapped.is_some() {
        Switch::Unconfirmed
    } else {
        Switch::Confirmed
    };
    let permission = switched(switch).await;
    match permission {
        Permission::Committed => {}
        Permission::WithoutCommit if !cancellation.is_cancelled() => {}
        Permission::WithoutCommit | Permission::Refused => return Published::Cancelled,
        Permission::Unavailable => {
            return Published::OutUnconfirmed(machine_error(
                MachineAction::ReadRoutes,
                RpcError::unavailable("The Deployment's switch could not be recorded"),
            ));
        }
    }
    if new.is_empty() {
        return Published::Listed;
    }
    if let Some(gapped) = &gapped {
        shared.gapped.record(service, gapped);
    }
    let ids: BTreeSet<ContainerId> = new.iter().map(|(_, _, id)| *id).collect();
    let mut listed_on: BTreeSet<MachineId> = new
        .first()
        .map(|(_, machine, _)| *machine)
        .into_iter()
        .collect();
    let (mut outs, mut gone) = (Vec::new(), Vec::new());
    let mut missing = 0;
    // What each old copy's removal asks for its anonymous Volumes: they go only with a
    // copy its Machine made to take its place that this Deploy keeps.
    let mut running_old = BTreeSet::new();
    let mut asks = BTreeMap::new();
    let inspected = join_all(
        old.iter()
            .map(|(machine_id, container_id)| inspect(client, machine_id, container_id)),
    )
    .await;
    for ((machine_id, container_id), inspected) in old.iter().zip(inspected) {
        let copy = match inspected {
            Ok(details) => {
                if crate::deploy::is_active_runtime(&details.container.runtime) {
                    running_old.insert(*container_id);
                }
                asks.insert(
                    *container_id,
                    AnonymousVolumes::of(&details.container, |id| ids.contains(id)),
                );
                cleanup::Copy::observed(&details.container)
            }
            Err(ExecutionError::Machine { error, .. }) if error.code == RpcErrorCode::NotFound => {
                if unrouted(client, machine_id, service, container_id).await {
                    missing += 1;
                }
                continue;
            }
            Err(error) => {
                let copy = cleanup::Copy {
                    machine_id: *machine_id,
                    service: service.clone(),
                    container_id: *container_id,
                    rotation: CopyRotation::Pending,
                };
                shared.report.leave_running(progress, &copy, left_by(error));
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
            shared
                .report
                .leave_running(progress, &copy, Left::NotConfirmed(routing));
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
                shared.report.leave_running(progress, &copy, left);
                continue;
            }
            listed_on.insert(copy.machine_id);
        }
        outs.push(copy);
    }
    // Every Out goes together and one wait covers the set, so a group's
    // retirement costs one unlisting, not one per copy.
    if cancellation.is_cancelled() {
        if permission != Permission::Committed {
            return Published::Cancelled;
        }
        for copy in outs.iter().chain(&gone) {
            shared.report.unfinished_removal(
                progress,
                copy,
                running_old.contains(&copy.container_id),
                Left::Interrupted,
            );
        }
        return Published::Listed;
    }
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
                shared.report.unfinished_removal(
                    progress,
                    &copy,
                    running_old.contains(&copy.container_id),
                    Left::Refused(refusal),
                );
            }
            Err(error) => {
                unconfirmed.get_or_insert_with(|| {
                    machine_error(MachineAction::StopContainer, error.clone())
                });
                shared.report.unfinished_removal(
                    progress,
                    &copy,
                    running_old.contains(&copy.container_id),
                    Left::Failed(error),
                );
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
                let (left, error) = shared.report.failed(failure, MachineAction::ReadRoutes);
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
                    shared.report.unfinished_removal(
                        progress,
                        copy,
                        running_old.contains(&copy.container_id),
                        left.clone(),
                    );
                }
            }
        }
        if let Some(error) = error {
            unconfirmed.get_or_insert(error);
        }
    }
    // The group retires only once no proxy routes any of its old copies: one left
    // running, such as a copy a gap Server's proxy still routes, holds it switching.
    let switched = unconfirmed.is_none() && gone.len() + missing == old.len();
    if switched && !gone.is_empty() {
        retiring(operations, progress, &new);
    }
    shared.retiring.add(gone.into_iter().map(|copy| {
        let ask = asks
            .get(&copy.container_id)
            .copied()
            .unwrap_or(AnonymousVolumes::keep());
        Retirement {
            running: running_old.contains(&copy.container_id),
            copy,
            ask,
        }
    }));
    match (unconfirmed, gapped) {
        (Some(error), _) => Published::OutUnconfirmed(error),
        (None, Some(_)) => Published::Gapped,
        (None, None) => Published::Listed,
    }
}

/// Whether the owner's route read shows no proxy routing a copy the owner no longer
/// knows. A Machine can lose a copy's record while a stale proxy still routes it.
async fn unrouted<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    container_id: &ContainerId,
) -> bool {
    client
        .read_routes(machine_id, service)
        .await
        .is_ok_and(|read| {
            read.complete
                && cleanup::still_waiting(&read, &|routed| !routed.contains(container_id))
                    .machines
                    .is_empty()
        })
}

/// Move each new copy that started beside the copy it replaces on to retiring it. A
/// stop-first copy's old one already stopped, so its row stays where it is.
fn retiring(
    operations: &[DeployOperation],
    progress: &Progress,
    new: &[(usize, MachineId, ContainerId)],
) {
    for (index, _, container_id) in new {
        if let DeployOperation::ReplaceContainer(replacement) = operation_at(operations, *index)
            && replacement.spec.update.order != UpdateOrder::StopFirst
        {
            progress.set_running(
                *index,
                ployz_core::OperationPhase::RetiringOld {
                    container_id: *container_id,
                },
            );
        }
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
    shared: &Shared<'_>,
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
                    match cleanup::bring_in(client, &copy, spec, Some(shared.plan.run().clone()))
                        .await
                    {
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
    let gaps = shared.gens.gaps(service);
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
