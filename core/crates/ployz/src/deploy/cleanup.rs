use std::collections::BTreeSet;
use std::time::Duration;

use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerKind, ContainerLocator, ContainerNotFoundReason,
    ContainerNow, ContainerObservation, ContainerRefusal, ContainerRefusalReason, ContainerWant,
    CopyRotation, DeploymentLogId, Drain, EnsuredContainer, HealthVerdict, MachineId,
    QualifiedService, ResolvedServiceSpec, RoutesRead, RpcError, RpcErrorCode, RunRotation,
    RunTarget, StopOptions, Unacked,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::CopyLifecycle;
use crate::connect::{TARGET_RPC_TIMEOUT, in_rpc_timeout, stop_rpc_timeout};
use crate::ui::{then_sentence, with_reason};

pub(crate) const ACK_POLL: Duration = Duration::from_millis(200);
pub(crate) const ACK_WAIT: Duration = Duration::from_secs(300);

const DOCKER_STOP_GRACE_SECONDS: u16 = 10;

pub(crate) fn fresh_run() -> DeploymentLogId {
    DeploymentLogId::parse(uuid::Uuid::new_v4().simple().to_string())
        .expect("a simple uuid is a deployment log id")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Copy {
    pub(crate) machine_id: MachineId,
    pub(crate) service: QualifiedService,
    pub(crate) container_id: ContainerId,
    pub(crate) rotation: CopyRotation,
}

impl Copy {
    pub(crate) fn observed(observation: &ContainerObservation) -> Self {
        Self {
            machine_id: observation.machine_id,
            service: observation.identity(),
            container_id: observation.container_id,
            rotation: observation.rotation().clone(),
        }
    }

    /// A Pending copy was never routed, so it is removed without a drain.
    pub(crate) fn drains(&self) -> bool {
        self.rotation != CopyRotation::Pending
    }

    fn locator(&self) -> ContainerLocator {
        ContainerLocator::Id(self.container_id)
    }
}

#[derive(Debug)]
pub(crate) enum BroughtIn {
    In,
    NotIn { health: Option<HealthVerdict> },
    Refused(ContainerRefusal),
}

/// Why a copy asked In is not In.
#[derive(Debug)]
pub(crate) enum NotIn {
    /// It ran but did not come In; the verdict is what its health check said, if it did.
    Health(Option<HealthVerdict>),
    /// A newer copy is In, so this one is superseded.
    OlderThanIn(ContainerRefusal),
    Refused(ContainerRefusal),
}

impl BroughtIn {
    pub(crate) fn not_in(self) -> Result<(), NotIn> {
        match self {
            BroughtIn::In => Ok(()),
            BroughtIn::NotIn { health } => Err(NotIn::Health(health)),
            BroughtIn::Refused(refusal)
                if refusal.reason == ContainerRefusalReason::OlderThanIn =>
            {
                Err(NotIn::OlderThanIn(refusal))
            }
            BroughtIn::Refused(refusal) => Err(NotIn::Refused(refusal)),
        }
    }
}

/// An unanswered ask re-reads the copy: the owner may have written In after
/// the deadline passed.
pub(crate) async fn bring_in<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    spec: &ResolvedServiceSpec,
    run: Option<DeploymentLogId>,
) -> Result<BroughtIn, RpcError> {
    let want = ContainerWant::Running {
        container: RunTarget::Existing(copy.locator()),
        rotation: RunRotation::In {
            run,
            expect: Some(copy.rotation.clone()),
        },
    };
    let answer = client
        .ensure_unsettled(&copy.machine_id, want, Some(in_rpc_timeout(spec)))
        .await;
    match answer {
        Ok(EnsuredContainer::Ensured { now, health }) => Ok(match now.present() {
            Some(observed) if *observed.rotation() == CopyRotation::In => BroughtIn::In,
            _ => BroughtIn::NotIn { health },
        }),
        Ok(EnsuredContainer::Refused { refusal, .. }) => Ok(BroughtIn::Refused(refusal)),
        Err(error) if error.target_unanswered() => {
            let details = client
                .inspect_container(&copy.machine_id, &copy.container_id)
                .await
                .map_err(|_| error)?;
            if *details.container.rotation() == CopyRotation::In {
                Ok(BroughtIn::In)
            } else {
                Ok(BroughtIn::NotIn { health: None })
            }
        }
        Err(error) => Err(error),
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Waited {
    Done,
    TimedOut(Unconfirmed),
    /// Every proxy confirmed but those on the wait's gap Servers.
    Gapped(Unconfirmed),
    Cancelled,
}

/// Why a rotation did not finish. A site says it in its own words; this is the split.
#[derive(Debug, thiserror::Error)]
pub(crate) enum RotationFailure {
    #[error("{}", .0.message)]
    Refused(ContainerRefusal),
    #[error("the proxies on {} did not confirm", listed(&.0.machines))]
    NotConfirmed(Unconfirmed),
    #[error("the wait for its proxies was cancelled")]
    Cancelled,
}

impl Waited {
    /// A wait that ended at its gap Servers is as unconfirmed as one that timed out; a
    /// site that treats the gap differently matches on the wait itself.
    pub(crate) fn failure(self) -> Result<(), RotationFailure> {
        match self {
            Waited::Done => Ok(()),
            Waited::TimedOut(unconfirmed) | Waited::Gapped(unconfirmed) => {
                Err(RotationFailure::NotConfirmed(unconfirmed))
            }
            Waited::Cancelled => Err(RotationFailure::Cancelled),
        }
    }
}

/// The live Machines whose proxy had not confirmed when the wait ended.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Unconfirmed {
    pub(crate) machines: Vec<MachineId>,
    /// Those whose proxy row never arrived: the only ones that may be away.
    pub(crate) unanswered: Vec<MachineId>,
}

pub(crate) async fn wait_listed<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    copies: &BTreeSet<ContainerId>,
    cancellation: &CancellationToken,
    acked: impl FnMut(&MachineId),
) -> Result<Waited, RpcError> {
    wait_listed_beside(
        client,
        machine_id,
        service,
        copies,
        &BTreeSet::new(),
        cancellation,
        acked,
    )
    .await
}

/// [`wait_listed`] that ends at [`Waited::Gapped`] once only `gaps`' proxies are
/// unconfirmed. A gap Server is one this Deploy already reports unobserved, such as
/// an Up one whose daemon the entry couldn't dial: nothing there writes its ack.
pub(crate) async fn wait_listed_beside<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    copies: &BTreeSet<ContainerId>,
    gaps: &BTreeSet<MachineId>,
    cancellation: &CancellationToken,
    acked: impl FnMut(&MachineId),
) -> Result<Waited, RpcError> {
    wait_routes(
        || client.read_routes(machine_id, service),
        |routed| copies.is_subset(routed),
        gaps,
        cancellation,
        acked,
    )
    .await
}

/// Read through the entry Machine: every Machine reads the same replicated ack rows,
/// so a cut-off owner in the group does not stop the wait.
pub(crate) async fn wait_none_listed<C: CopyLifecycle>(
    client: &C,
    service: &QualifiedService,
    copies: &BTreeSet<ContainerId>,
    cancellation: &CancellationToken,
) -> Result<Waited, RpcError> {
    wait_routes(
        || client.read_entry_routes(service),
        |routed| routed.is_disjoint(copies),
        &BTreeSet::new(),
        cancellation,
        |_| {},
    )
    .await
}

pub(crate) async fn wait_unlisted<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    container_id: &ContainerId,
    cancellation: &CancellationToken,
) -> Result<Waited, RpcError> {
    wait_routes(
        || client.read_routes(machine_id, service),
        |routed| !routed.contains(container_id),
        &BTreeSet::new(),
        cancellation,
        |_| {},
    )
    .await
}

async fn wait_routes<Read>(
    read: impl Fn() -> Read,
    satisfied: impl Fn(&BTreeSet<ContainerId>) -> bool,
    gaps: &BTreeSet<MachineId>,
    cancellation: &CancellationToken,
    mut acked: impl FnMut(&MachineId),
) -> Result<Waited, RpcError>
where
    Read: Future<Output = Result<RoutesRead, RpcError>>,
{
    let deadline = Instant::now() + ACK_WAIT;
    let mut confirmed = BTreeSet::new();
    loop {
        let read = tokio::select! {
            read = read() => read?,
            () = cancellation.cancelled() => return Ok(Waited::Cancelled),
        };
        let waiting = still_waiting(&read, &satisfied);
        for machine in read.routes.keys() {
            if !waiting.machines.contains(machine) && confirmed.insert(*machine) {
                acked(machine);
            }
        }
        if read.complete && waiting.machines.is_empty() {
            return Ok(Waited::Done);
        }
        if read.complete
            && waiting
                .machines
                .iter()
                .all(|machine| gaps.contains(machine))
        {
            // A gap Server is away even when its stale row arrived.
            let away = waiting.machines.clone();
            return Ok(Waited::Gapped(Unconfirmed {
                machines: away.clone(),
                unanswered: away,
            }));
        }
        if Instant::now() >= deadline {
            return Ok(Waited::TimedOut(waiting));
        }
        tokio::select! {
            () = tokio::time::sleep(ACK_POLL) => {}
            () = cancellation.cancelled() => return Ok(Waited::Cancelled),
        }
    }
}

pub(crate) fn still_waiting(
    read: &RoutesRead,
    satisfied: &impl Fn(&BTreeSet<ContainerId>) -> bool,
) -> Unconfirmed {
    let mut waiting = Unconfirmed::default();
    for unacked in read.unacked(satisfied) {
        waiting.machines.push(unacked.machine());
        if let Unacked::Unanswered(machine) = unacked {
            waiting.unanswered.push(machine);
        }
    }
    waiting
}

/// What one rotation ask came to: the copy moved as asked, or why it did not.
#[derive(Debug)]
pub(crate) enum Rotated {
    Done,
    Refused(ContainerRefusal),
    NotConfirmed(Unconfirmed),
    Cancelled,
}

impl Rotated {
    pub(crate) fn failure(self) -> Result<(), RotationFailure> {
        match self {
            Rotated::Done => Ok(()),
            Rotated::Refused(refusal) => Err(RotationFailure::Refused(refusal)),
            Rotated::NotConfirmed(unconfirmed) => Err(RotationFailure::NotConfirmed(unconfirmed)),
            Rotated::Cancelled => Err(RotationFailure::Cancelled),
        }
    }
}

impl From<Waited> for Rotated {
    fn from(wait: Waited) -> Self {
        match wait {
            Waited::Done => Rotated::Done,
            Waited::TimedOut(machines) | Waited::Gapped(machines) => {
                Rotated::NotConfirmed(machines)
            }
            Waited::Cancelled => Rotated::Cancelled,
        }
    }
}

pub(crate) async fn hold<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    run: DeploymentLogId,
    cancellation: &CancellationToken,
) -> Result<Rotated, RpcError> {
    let want = ContainerWant::Held {
        container: copy.locator(),
        run: run.clone(),
        expect: Some(copy.rotation.clone()),
    };
    let answer = client
        .ensure_unsettled(&copy.machine_id, want, Some(TARGET_RPC_TIMEOUT))
        .await;
    match answer {
        Ok(EnsuredContainer::Refused { refusal, .. }) => return Ok(Rotated::Refused(refusal)),
        Ok(EnsuredContainer::Ensured { .. }) => {}
        // As with In, the owner may have written Held after the deadline.
        Err(error) if error.target_unanswered() => {
            let details = client
                .inspect_container(&copy.machine_id, &copy.container_id)
                .await
                .map_err(|_| error.clone())?;
            if *details.container.rotation() != CopyRotation::Held(run) {
                return Err(error);
            }
        }
        Err(error) => return Err(error),
    }
    wait_unlisted(
        client,
        &copy.machine_id,
        &copy.service,
        &copy.container_id,
        cancellation,
    )
    .await
    .map(Rotated::from)
}

/// Whether taking a copy Out may leave its Service with no copy In.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EmptyPolicy {
    RequireOtherIn,
    MayEmpty,
}

impl From<ployz_core::RemoveContainerPurpose> for EmptyPolicy {
    fn from(purpose: ployz_core::RemoveContainerPurpose) -> Self {
        match purpose {
            ployz_core::RemoveContainerPurpose::Rollout => Self::RequireOtherIn,
            ployz_core::RemoveContainerPurpose::Retire => Self::MayEmpty,
        }
    }
}

pub(crate) async fn take_out<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    empty: EmptyPolicy,
    cancellation: &CancellationToken,
) -> Result<Rotated, RpcError> {
    if let Some(refusal) = send_out(client, copy, empty).await? {
        return Ok(Rotated::Refused(refusal));
    }
    wait_unlisted(
        client,
        &copy.machine_id,
        &copy.service,
        &copy.container_id,
        cancellation,
    )
    .await
    .map(Rotated::from)
}

/// Asks the copy Out without waiting for proxies to drop it, giving the Machine's
/// refusal when it has one.
pub(crate) async fn send_out<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    empty: EmptyPolicy,
) -> Result<Option<ContainerRefusal>, RpcError> {
    if matches!(copy.rotation, CopyRotation::In | CopyRotation::Held(_)) {
        let want = ContainerWant::Out {
            container: copy.locator(),
            allow_empty: empty == EmptyPolicy::MayEmpty,
            expect: Some(copy.rotation.clone()),
        };
        let answer = client
            .ensure_unsettled(&copy.machine_id, want, Some(TARGET_RPC_TIMEOUT))
            .await?;
        if let EnsuredContainer::Refused { refusal, .. } = answer {
            return Ok(Some(refusal));
        }
    }
    Ok(None)
}

pub(crate) async fn retire<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    empty: EmptyPolicy,
    anonymous_volumes: AnonymousVolumes,
    cancellation: &CancellationToken,
) -> Result<Rotated, RpcError> {
    if !copy.drains() {
        return remove(client, copy, anonymous_volumes)
            .await
            .map(Rotated::from);
    }
    match take_out(client, copy, empty, cancellation).await? {
        Rotated::Done => remove(client, copy, anonymous_volumes)
            .await
            .map(Rotated::from),
        rotated @ (Rotated::Refused(_) | Rotated::NotConfirmed(_) | Rotated::Cancelled) => {
            Ok(rotated)
        }
    }
}

/// Retires the copy without waiting for proxies to drop it. A removal beside a
/// copy on a Server this Deploy cannot reach: every proxy keeps the route while
/// its store lists that copy In, so the drain would never confirm.
pub(crate) async fn retire_unconfirmed<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    empty: EmptyPolicy,
    anonymous_volumes: AnonymousVolumes,
) -> Result<Rotated, RpcError> {
    match send_out(client, copy, empty).await? {
        None => remove(client, copy, anonymous_volumes)
            .await
            .map(Rotated::from),
        Some(refusal) => Ok(Rotated::Refused(refusal)),
    }
}

/// What asking a copy Gone came to. A removal waits on no proxy, so it is never
/// unconfirmed or cancelled.
#[derive(Debug)]
pub(crate) enum Removal {
    Removed,
    Refused(ContainerRefusal),
}

impl Removal {
    pub(crate) fn failure(self) -> Result<(), RotationFailure> {
        Rotated::from(self).failure()
    }
}

impl From<Removal> for Rotated {
    fn from(removal: Removal) -> Self {
        match removal {
            Removal::Removed => Rotated::Done,
            Removal::Refused(refusal) => Rotated::Refused(refusal),
        }
    }
}

pub(crate) async fn remove<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    anonymous_volumes: AnonymousVolumes,
) -> Result<Removal, RpcError> {
    match gone(
        client,
        &copy.machine_id,
        copy.locator(),
        Drain::DEFAULT,
        anonymous_volumes,
        None,
    )
    .await?
    {
        Gone::Removed => Ok(Removal::Removed),
        Gone::InRotation { refusal, .. } | Gone::Refused(refusal) => Ok(Removal::Refused(refusal)),
    }
}

/// What asking a copy Gone came to. A copy that is not there is removed.
#[derive(Debug)]
pub(crate) enum Gone {
    Removed,
    /// The owner refuses Gone on a copy that is In or Held; `now` is what it observes.
    InRotation {
        now: Box<ContainerNow>,
        refusal: ContainerRefusal,
    },
    Refused(ContainerRefusal),
}

pub(crate) async fn gone<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    container: ContainerLocator,
    drain: Drain,
    anonymous_volumes: AnonymousVolumes,
    budget: Option<Duration>,
) -> Result<Gone, RpcError> {
    let want = ContainerWant::Gone {
        container,
        drain,
        anonymous_volumes,
    };
    Ok(
        match client.ensure_unsettled(machine_id, want, budget).await? {
            EnsuredContainer::Ensured { .. } => Gone::Removed,
            EnsuredContainer::Refused { refusal, .. }
                if refusal.reason == ContainerRefusalReason::NotFound =>
            {
                Gone::Removed
            }
            EnsuredContainer::Refused { now, refusal }
                if refusal.reason == ContainerRefusalReason::InRotation =>
            {
                Gone::InRotation {
                    now: Box::new(now),
                    refusal,
                }
            }
            EnsuredContainer::Refused { refusal, .. } => Gone::Refused(refusal),
        },
    )
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UserStarted {
    In,
    /// Started outside rotation; its health is the caller's to wait for.
    Running,
    NotIn(Option<HealthVerdict>),
}

pub(crate) async fn user_stop<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    stop: StopOptions,
    queued: u32,
    cancellation: &CancellationToken,
) -> Result<(), RpcError> {
    let observed = observe(client, machine_id, container_id).await?;
    let copy = Copy::observed(&observed);
    if routable(&observed) && copy.rotation != CopyRotation::Out {
        let run = match &copy.rotation {
            CopyRotation::Held(run) => run.clone(),
            // Under the run that created it, so start can tell it never served.
            CopyRotation::Pending => observed.deploy_label().unwrap_or_else(fresh_run),
            CopyRotation::In | CopyRotation::Out => fresh_run(),
        };
        let step = hold(client, &copy, run.clone(), cancellation).await;
        if copy.rotation == CopyRotation::In {
            let failed = match step {
                Ok(Rotated::Cancelled) => Some(RpcError::unavailable(
                    "the wait for its proxies was cancelled".to_owned(),
                )),
                // The ask may have landed, so the copy is restored as if Held.
                Err(ref error) => Some(error.clone()),
                Ok(Rotated::Done | Rotated::Refused(_) | Rotated::NotConfirmed(_)) => None,
            };
            if let Some(error) = failed {
                return Err(not_stopped(client, &copy, &observed.resolved_spec, run, error).await);
            }
        }
        // A proxy that never confirmed after the wait, such as one on a Server whose
        // daemon is stopped, does not veto a stop the user asked for: stopping on is
        // the owner's own decision, and holding the copy there would leave it running
        // and unrouted everywhere, for good (#3995).
        let step = match step? {
            Rotated::NotConfirmed(_) => Rotated::Done,
            step @ (Rotated::Done | Rotated::Refused(_) | Rotated::Cancelled) => step,
        };
        left_running(&copy, "Held", step)?;
    }
    stopped(client, &copy, &observed.resolved_spec, stop, queued).await
}

pub(crate) async fn user_start<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
) -> Result<UserStarted, RpcError> {
    let observed = observe(client, machine_id, container_id).await?;
    let copy = Copy::observed(&observed);
    if !routable(&observed) {
        return run_pending(client, &copy).await;
    }
    if never_in(&observed) {
        run_pending(client, &copy).await?;
        return Err(not_admitted(&copy));
    }
    let run = match &copy.rotation {
        CopyRotation::Out => return Err(deploy_again(&copy)),
        CopyRotation::Held(run) => Some(run.clone()),
        CopyRotation::Pending | CopyRotation::In => None,
    };
    started(
        &copy,
        bring_in(client, &copy, &observed.resolved_spec, run).await?,
    )
}

pub(crate) async fn user_restart<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    stop: StopOptions,
    queued: u32,
    cancellation: &CancellationToken,
) -> Result<UserStarted, RpcError> {
    let observed = observe(client, machine_id, container_id).await?;
    let copy = Copy::observed(&observed);
    let spec = &observed.resolved_spec;
    if routable(&observed) && never_in(&observed) {
        restart_pending(client, &copy, spec, stop, queued).await?;
        return Err(not_admitted(&copy));
    }
    let run = match (routable(&observed), &copy.rotation) {
        (true, CopyRotation::Out) => return Err(deploy_again(&copy)),
        (true, CopyRotation::Held(run)) => {
            return started(
                &copy,
                bring_in(client, &copy, spec, Some(run.clone())).await?,
            );
        }
        (true, CopyRotation::In) => fresh_run(),
        (false, _) | (true, CopyRotation::Pending) => {
            return restart_pending(client, &copy, spec, stop, queued).await;
        }
    };
    let held = Copy {
        rotation: CopyRotation::Held(run.clone()),
        ..copy.clone()
    };
    let stopped = match hold(client, &copy, run.clone(), cancellation).await {
        Ok(rotated) => match rotated.failure() {
            Ok(()) => stopped(client, &held, spec, stop, queued).await,
            // Answered before the Machine acted, so there is nothing to put back.
            Err(RotationFailure::Refused(refusal)) => return Err(refusal.into_error()),
            Err(failure) => Err(left_unrouted(
                &format!("Container {} was not restarted", copy.container_id),
                failure,
            )),
        },
        Err(error) if error.code.as_str() == ployz_core::FORBIDDEN_CODE => {
            let message = with_reason(
                format_args!("Container {} was not restarted", copy.container_id),
                &error.message,
            );
            return Err(error.with_message(message));
        }
        // The ask may have landed, so the copy is restored as if Held.
        Err(error) => {
            let message = with_reason(
                format_args!("Container {} was not restarted", copy.container_id),
                &error.message,
            );
            Err(error.with_message(message))
        }
    };
    let brought = bring_in(client, &held, spec, Some(run)).await;
    if let Err(error) = stopped {
        return Err(say_restored(
            error,
            &brought,
            "It is left Held and not routed.",
        ));
    }
    started(&held, brought?)
}

pub(crate) async fn user_remove<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    cancellation: &CancellationToken,
) -> Result<(), RpcError> {
    let observed = match observe(client, machine_id, container_id).await {
        Err(error) if error.code == RpcErrorCode::NotFound => {
            return match error.evidence::<ContainerNotFoundReason>() {
                Some(ContainerNotFoundReason::NoResolvedSpec) => {
                    Err(kept_without_spec(container_id, error))
                }
                None => Ok(()),
            };
        }
        observed => observed?,
    };
    let copy = Copy::observed(&observed);
    let rotated = if routable(&observed) {
        retire(
            client,
            &copy,
            EmptyPolicy::MayEmpty,
            AnonymousVolumes::Remove,
            cancellation,
        )
        .await?
    } else {
        remove(client, &copy, AnonymousVolumes::Remove)
            .await?
            .into()
    };
    left_running(&copy, "Out", rotated)
}

/// The inspect saw Docker answer for `container_id` while its Machine holds no Resolved
/// Service Spec under that ID. The remove asks for nothing, so the Container stays.
fn kept_without_spec(container_id: &ContainerId, inspected: RpcError) -> RpcError {
    RpcError::conflict(format!(
            "Container {container_id} was not removed: Docker on its Server has it, and the Server holds no Resolved Service Spec under its ID."
        )).with_evidence(&ContainerNotFoundReason::NoResolvedSpec).with_causes(std::iter::once(inspected.message)
            .chain(inspected.cause)
            .collect())
}

async fn observe<C: CopyLifecycle>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
) -> Result<ContainerObservation, RpcError> {
    client
        .inspect_container(machine_id, container_id)
        .await
        .map(|details| details.container)
}

/// No Deploy brought this copy In: it is Pending, or a user stop withdrew it from
/// Pending under its own Deploy's run. A failed Deploy leaves such copies beside
/// the configured ones, and a user command that routed one would leave two
/// versions answering, then refuse the configured copies as older.
fn never_in(observed: &ContainerObservation) -> bool {
    match observed.rotation() {
        CopyRotation::Pending => true,
        CopyRotation::Held(run) => observed.deploy_label().as_ref() == Some(run),
        CopyRotation::In | CopyRotation::Out => false,
    }
}

fn not_admitted(copy: &Copy) -> RpcError {
    RpcError::conflict(format!(
        "Container {} is left {} and not routed: no Deploy brought it In. Deploy the Service again to route it.",
        copy.container_id,
        rotation_name(&copy.rotation)
    ))
}

/// Hooks stay Pending for life, and neither Ingress door is ever routed.
fn routable(observed: &ContainerObservation) -> bool {
    observed.kind == ContainerKind::ServiceContainer && !observed.identity().is_ingress_door()
}

async fn stopped<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    spec: &ResolvedServiceSpec,
    stop: StopOptions,
    queued: u32,
) -> Result<(), RpcError> {
    let grace = stop
        .grace_period_seconds
        .or_else(|| {
            spec.container
                .stop_timeout_secs
                .and_then(|seconds| u16::try_from(seconds).ok())
        })
        .unwrap_or(DOCKER_STOP_GRACE_SECONDS);
    let want = ContainerWant::Stopped {
        container: copy.locator(),
        drain: Drain::DEFAULT,
        stop,
    };
    client
        .ensure_unsettled(
            &copy.machine_id,
            want,
            stop_rpc_timeout(Some(grace), queued),
        )
        .await?
        .into_settled()
        .map(|_| ())
}

async fn restart_pending<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    spec: &ResolvedServiceSpec,
    stop: StopOptions,
    queued: u32,
) -> Result<UserStarted, RpcError> {
    let stopped = stopped(client, copy, spec, stop, queued).await;
    let started = run_pending(client, copy).await;
    stopped?;
    started
}

async fn run_pending<C: CopyLifecycle>(client: &C, copy: &Copy) -> Result<UserStarted, RpcError> {
    let want = ContainerWant::Running {
        container: RunTarget::Existing(copy.locator()),
        rotation: RunRotation::Pending,
    };
    client
        .ensure_unsettled(&copy.machine_id, want, Some(TARGET_RPC_TIMEOUT))
        .await?
        .into_settled()?;
    Ok(UserStarted::Running)
}

/// `copy` carries the rotation the In ask expected, which a refusal leaves in place.
fn started(copy: &Copy, brought: BroughtIn) -> Result<UserStarted, RpcError> {
    match brought {
        BroughtIn::In => Ok(UserStarted::In),
        BroughtIn::NotIn { health } => Ok(UserStarted::NotIn(health)),
        // The user rows of the caller table never retire a copy, so a superseded one
        // is named with the rotation it keeps.
        BroughtIn::Refused(refusal) if refusal.reason == ContainerRefusalReason::OlderThanIn => {
            let mut error = refusal.into_error();
            error.message = then_sentence(
                &format!(
                    "Container {} is left {} and not routed: {}",
                    copy.container_id,
                    rotation_name(&copy.rotation),
                    error.message
                ),
                &format!(
                    "The newer copy serves the Service; ployz deploy {} replaces this one.",
                    copy.service.name
                ),
            );
            Err(error)
        }
        BroughtIn::Refused(refusal) => Err(refusal.into_error()),
    }
}

fn rotation_name(rotation: &CopyRotation) -> &'static str {
    match rotation {
        CopyRotation::Pending => "Pending",
        CopyRotation::In => "In",
        CopyRotation::Held(_) => "Held",
        CopyRotation::Out => "Out",
    }
}

/// A stop that ends while its proxies drop the copy withdrew a serving copy for
/// nothing, so it goes back In, as a restart does when its wait fails. A refusal was
/// answered before the Machine acted, so the copy is as it was and only the reason is said.
async fn not_stopped<C: CopyLifecycle>(
    client: &C,
    copy: &Copy,
    spec: &ResolvedServiceSpec,
    run: DeploymentLogId,
    mut error: RpcError,
) -> RpcError {
    error.message = with_reason(
        format_args!("Container {} was not stopped", copy.container_id),
        &error.message,
    );
    if error.code.as_str() == ployz_core::FORBIDDEN_CODE {
        return error;
    }
    let held = Copy {
        rotation: CopyRotation::Held(run.clone()),
        ..copy.clone()
    };
    let brought = bring_in(client, &held, spec, Some(run)).await;
    say_restored(error, &brought, "It is left running and Held.")
}

/// Ends a failed user command with whether its copy is back In or what it was left as.
fn say_restored(
    mut error: RpcError,
    brought: &Result<BroughtIn, RpcError>,
    left: &str,
) -> RpcError {
    let tail = if matches!(brought, Ok(BroughtIn::In)) {
        "It is back In."
    } else {
        left
    };
    error.message = then_sentence(&error.message, tail);
    error
}

/// A user command's line for a proxy wait that did not finish, after `lead` says what
/// became of the copy.
fn left_unrouted(lead: &str, failure: RotationFailure) -> RpcError {
    match failure {
        RotationFailure::Refused(refusal) => refusal.into_error(),
        RotationFailure::NotConfirmed(unconfirmed) => RpcError::unavailable(format!(
            "{lead}: the proxies on {} did not confirm they stopped routing it.",
            listed(&unconfirmed.machines)
        )),
        RotationFailure::Cancelled => {
            RpcError::unavailable(format!("{lead}: the wait for its proxies was cancelled."))
        }
    }
}

fn left_running(copy: &Copy, rotation: &str, rotated: Rotated) -> Result<(), RpcError> {
    rotated.failure().map_err(|failure| {
        left_unrouted(
            &format!(
                "Container {} is left running and {rotation}",
                copy.container_id
            ),
            failure,
        )
    })
}

fn deploy_again(copy: &Copy) -> RpcError {
    RpcError::conflict(format!(
        "Container {} is out of rotation and cannot start again. Deploy the Service again.",
        copy.container_id
    ))
}

fn listed(machines: &[MachineId]) -> String {
    machines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
