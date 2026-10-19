//! Rotation moves a Deploy or a user command drives on one copy and the
//! route waits that prove them: `bring_in`, the hold, `retire`, and the
//! listed and unlisted waits over `ReadRoutes`.

use std::collections::BTreeSet;
use std::time::Duration;

use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerKind, ContainerLocator, ContainerObservation,
    ContainerRefusal, ContainerRefusalReason, ContainerWant, CopyRotation, DeploymentLogId, Drain,
    EnsuredContainer, HealthVerdict, MachineId, QualifiedService, ResolvedServiceSpec, RoutesRead,
    RpcError, RpcErrorCode, RunRotation, RunTarget, StopOptions,
};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::MachineOperations;
use crate::connect::{TARGET_RPC_TIMEOUT, in_rpc_timeout, stop_rpc_timeout};

/// How often a wait re-reads the routes.
pub(crate) const ACK_POLL: Duration = Duration::from_secs(1);
/// How long a wait gives every live proxy to list or drop a copy.
pub(crate) const ACK_WAIT: Duration = Duration::from_secs(300);

/// How long Docker gives a Container to stop when neither the user nor its spec
/// names a grace.
const DOCKER_STOP_GRACE_SECONDS: u16 = 10;

/// A run id for a hold or a bring-in outside a Deploy.
pub(crate) fn fresh_run() -> DeploymentLogId {
    DeploymentLogId::parse(uuid::Uuid::new_v4().simple().to_string())
        .expect("a simple uuid is a deployment log id")
}

/// One copy as last observed: where it runs and the rotation every ask
/// carries as `expect`.
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
            machine_id: observation.machine_id.clone(),
            service: observation.identity(),
            container_id: observation.container_id,
            rotation: observation.rotation().clone(),
        }
    }

    fn locator(&self) -> ContainerLocator {
        ContainerLocator::Id(self.container_id)
    }
}

/// What an In ask settled to.
#[derive(Debug)]
pub(crate) enum BroughtIn {
    In,
    /// The owner answered but the copy is not In: it failed health.
    NotIn {
        health: Option<HealthVerdict>,
    },
    Refused(ContainerRefusal),
}

/// Ask the owner to bring `copy` In for `run`. Only an answer whose `now` is
/// In succeeds. An unanswered ask re-reads the copy: the owner may have
/// written In after the deadline passed.
pub(crate) async fn bring_in<C: MachineOperations>(
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

/// How a route wait ended.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Wait {
    Done,
    /// The Machines that had not confirmed when `ACK_WAIT` passed.
    TimedOut(Vec<MachineId>),
    Cancelled,
}

/// Wait until every live Machine's route for `service` lists all of `copies`.
/// `acked` fires once per Machine as it confirms.
pub(crate) async fn wait_listed<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    copies: &BTreeSet<ContainerId>,
    cancellation: &CancellationToken,
    acked: impl FnMut(&MachineId),
) -> Result<Wait, RpcError> {
    wait_routes(
        client,
        machine_id,
        service,
        |routed| copies.is_subset(routed),
        cancellation,
        acked,
    )
    .await
}

/// Wait until no live Machine's route for `service` lists `container_id`.
pub(crate) async fn wait_unlisted<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    container_id: &ContainerId,
    cancellation: &CancellationToken,
) -> Result<Wait, RpcError> {
    wait_routes(
        client,
        machine_id,
        service,
        |routed| !routed.contains(container_id),
        cancellation,
        |_| {},
    )
    .await
}

async fn wait_routes<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    satisfied: impl Fn(&BTreeSet<ContainerId>) -> bool,
    cancellation: &CancellationToken,
    mut acked: impl FnMut(&MachineId),
) -> Result<Wait, RpcError> {
    let deadline = Instant::now() + ACK_WAIT;
    let mut confirmed = BTreeSet::new();
    loop {
        let read = tokio::select! {
            read = client.read_routes(machine_id, service) => read?,
            () = cancellation.cancelled() => return Ok(Wait::Cancelled),
        };
        let waiting = still_waiting(&read, &satisfied);
        for machine in read.routes.keys() {
            if !waiting.contains(machine) && confirmed.insert(machine.clone()) {
                acked(machine);
            }
        }
        if read.complete && waiting.is_empty() {
            return Ok(Wait::Done);
        }
        if Instant::now() >= deadline {
            return Ok(Wait::TimedOut(waiting));
        }
        tokio::select! {
            () = tokio::time::sleep(ACK_POLL) => {}
            () = cancellation.cancelled() => return Ok(Wait::Cancelled),
        }
    }
}

/// The live Machines whose route does not yet satisfy the wait. A missing or
/// unreadable route is not satisfied.
fn still_waiting(
    read: &RoutesRead,
    satisfied: &impl Fn(&BTreeSet<ContainerId>) -> bool,
) -> Vec<MachineId> {
    read.routes
        .iter()
        .filter(|(machine, _)| !read.down.contains(*machine))
        .filter(|(_, routed)| !routed.as_ref().is_some_and(satisfied))
        .map(|(machine, _)| machine.clone())
        .collect()
}

/// How a hold or a retirement ended.
#[derive(Debug)]
pub(crate) enum Step {
    Done,
    Refused(ContainerRefusal),
    /// The copy keeps running: these Machines never confirmed the route change.
    NotConfirmed(Vec<MachineId>),
    Cancelled,
}

impl From<Wait> for Step {
    fn from(wait: Wait) -> Self {
        match wait {
            Wait::Done => Step::Done,
            Wait::TimedOut(machines) => Step::NotConfirmed(machines),
            Wait::Cancelled => Step::Cancelled,
        }
    }
}

/// Hold `copy` for `run` and wait until no live proxy routes it.
pub(crate) async fn hold<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    run: DeploymentLogId,
    cancellation: &CancellationToken,
) -> Result<Step, RpcError> {
    let want = ContainerWant::Held {
        container: copy.locator(),
        run,
        expect: Some(copy.rotation.clone()),
    };
    let answer = client
        .ensure_unsettled(&copy.machine_id, want, Some(TARGET_RPC_TIMEOUT))
        .await?;
    if let EnsuredContainer::Refused { refusal, .. } = answer {
        return Ok(Step::Refused(refusal));
    }
    wait_unlisted(
        client,
        &copy.machine_id,
        &copy.service,
        &copy.container_id,
        cancellation,
    )
    .await
    .map(Step::from)
}

/// Take `copy` Out and wait until no live proxy routes it. A copy never
/// routed (Pending) or already Out needs no ask; the wait still runs.
pub(crate) async fn take_out<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    allow_empty: bool,
    cancellation: &CancellationToken,
) -> Result<Step, RpcError> {
    if matches!(copy.rotation, CopyRotation::In | CopyRotation::Held(_)) {
        let want = ContainerWant::Out {
            container: copy.locator(),
            allow_empty,
            expect: Some(copy.rotation.clone()),
        };
        let answer = client
            .ensure_unsettled(&copy.machine_id, want, Some(TARGET_RPC_TIMEOUT))
            .await?;
        if let EnsuredContainer::Refused { refusal, .. } = answer {
            return Ok(Step::Refused(refusal));
        }
    }
    wait_unlisted(
        client,
        &copy.machine_id,
        &copy.service,
        &copy.container_id,
        cancellation,
    )
    .await
    .map(Step::from)
}

/// Take `copy` Out, wait until no live proxy routes it, then remove it. A copy
/// never routed (Pending) goes straight to Gone. The copy is removed only after
/// the wait confirms: a copy proxies may still route is left running.
pub(crate) async fn retire<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    allow_empty: bool,
    anonymous_volumes: AnonymousVolumes,
    cancellation: &CancellationToken,
) -> Result<Step, RpcError> {
    if copy.rotation == CopyRotation::Pending {
        return remove(client, copy, anonymous_volumes).await;
    }
    match take_out(client, copy, allow_empty, cancellation).await? {
        Step::Done => remove(client, copy, anonymous_volumes).await,
        step => Ok(step),
    }
}

/// Remove `copy`. A copy already gone counts as removed.
pub(crate) async fn remove<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    anonymous_volumes: AnonymousVolumes,
) -> Result<Step, RpcError> {
    let want = ContainerWant::Gone {
        container: copy.locator(),
        drain: Drain::DEFAULT,
        anonymous_volumes,
    };
    match client
        .ensure_unsettled(&copy.machine_id, want, None)
        .await?
    {
        EnsuredContainer::Ensured { .. } => Ok(Step::Done),
        EnsuredContainer::Refused { refusal, .. }
            if refusal.reason == ContainerRefusalReason::NotFound =>
        {
            Ok(Step::Done)
        }
        EnsuredContainer::Refused { refusal, .. } => Ok(Step::Refused(refusal)),
    }
}

/// Where a user start or restart left the copy.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UserStarted {
    /// In and healthy.
    In,
    /// Started outside rotation: a proxy copy, a hook, or a restarted Pending
    /// copy. Its health is the caller's to wait for.
    Running,
    /// Started but not In: its health failed or could not be read.
    NotIn(Option<HealthVerdict>),
}

/// A user stop. A routed or routable copy is Held (under its own run when it is
/// Held already) and stopped once no live proxy lists it.
/// A copy proxies still list is left running and Held.
pub(crate) async fn user_stop<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    stop: StopOptions,
    queued: u32,
) -> Result<(), RpcError> {
    let observed = observe(client, machine_id, container_id).await?;
    let copy = Copy::observed(&observed);
    if routable(&observed) && copy.rotation != CopyRotation::Out {
        let run = match &copy.rotation {
            CopyRotation::Held(run) => run.clone(),
            _ => fresh_run(),
        };
        let step = hold(client, &copy, run, &CancellationToken::new()).await?;
        left_running(&copy, "Held", step)?;
    }
    stopped(client, &copy, &observed.resolved_spec, stop, queued).await
}

/// A user start: a Held, Pending or stopped In copy is
/// brought In once healthy. An Out copy is refused.
pub(crate) async fn user_start<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
) -> Result<UserStarted, RpcError> {
    let observed = observe(client, machine_id, container_id).await?;
    let copy = Copy::observed(&observed);
    if !routable(&observed) {
        return run_pending(client, &copy).await;
    }
    let run = match &copy.rotation {
        CopyRotation::Out => return Err(deploy_again(&copy)),
        CopyRotation::Held(run) => Some(run.clone()),
        CopyRotation::Pending | CopyRotation::In => None,
    };
    started(bring_in(client, &copy, &observed.resolved_spec, run).await?)
}

/// A user restart. An In copy is Held, stopped once no
/// live proxy lists it, and brought back In under the same run; it comes back
/// In even when the stop fails. A Held copy is only brought In. A Pending copy
/// is stopped and started outside rotation. An Out copy is refused.
pub(crate) async fn user_restart<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    stop: StopOptions,
) -> Result<UserStarted, RpcError> {
    let observed = observe(client, machine_id, container_id).await?;
    let copy = Copy::observed(&observed);
    let spec = &observed.resolved_spec;
    let run = match (routable(&observed), &copy.rotation) {
        (true, CopyRotation::Out) => return Err(deploy_again(&copy)),
        (true, CopyRotation::Held(run)) => {
            return started(bring_in(client, &copy, spec, Some(run.clone())).await?);
        }
        (true, CopyRotation::In) => fresh_run(),
        (false, _) | (true, CopyRotation::Pending) => {
            let stopped = stopped(client, &copy, spec, stop, 1).await;
            let started = run_pending(client, &copy).await;
            stopped?;
            return started;
        }
    };
    let held = Copy {
        rotation: CopyRotation::Held(run.clone()),
        ..copy.clone()
    };
    let stopped = match hold(client, &copy, run.clone(), &CancellationToken::new()).await? {
        Step::Refused(refusal) => return Err(refusal.into_error()),
        Step::Done => stopped(client, &held, spec, stop, 1).await,
        Step::NotConfirmed(machines) => Err(unavailable(format!(
            "Container {} was not restarted: the proxies on {} did not confirm they stopped routing it.",
            copy.container_id,
            listed(&machines)
        ))),
        Step::Cancelled => Err(unavailable(format!(
            "Container {} was not restarted: the wait for its proxies was cancelled.",
            copy.container_id
        ))),
    };
    let started = started(bring_in(client, &held, spec, Some(run)).await?);
    stopped?;
    started
}

/// A user removal: an In or Held copy goes Out, and is
/// removed once no live proxy lists it. A Pending or Out copy, a hook and a
/// proxy copy are removed directly. A copy already gone counts as removed.
pub(crate) async fn user_remove<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
) -> Result<(), RpcError> {
    let observed = match observe(client, machine_id, container_id).await {
        Err(error) if error.code == RpcErrorCode::NotFound => return Ok(()),
        observed => observed?,
    };
    let copy = Copy::observed(&observed);
    let step = if routable(&observed) {
        retire(
            client,
            &copy,
            true,
            AnonymousVolumes::Remove,
            &CancellationToken::new(),
        )
        .await?
    } else {
        remove(client, &copy, AnonymousVolumes::Remove).await?
    };
    left_running(&copy, "Out", step)
}

async fn observe<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
) -> Result<ContainerObservation, RpcError> {
    client
        .inspect_container(machine_id, container_id)
        .await
        .map(|details| details.container)
}

/// Whether the copy takes part in rotation. Hooks stay Pending for life, and
/// proxy copies are never routed.
fn routable(observed: &ContainerObservation) -> bool {
    observed.kind == ContainerKind::ServiceContainer
        && observed.identity() != QualifiedService::system_ingress()
}

/// Stop `copy` behind the default drain, bounded by its grace.
async fn stopped<C: MachineOperations>(
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

async fn run_pending<C: MachineOperations>(
    client: &C,
    copy: &Copy,
) -> Result<UserStarted, RpcError> {
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

fn started(brought: BroughtIn) -> Result<UserStarted, RpcError> {
    match brought {
        BroughtIn::In => Ok(UserStarted::In),
        BroughtIn::NotIn { health } => Ok(UserStarted::NotIn(health)),
        BroughtIn::Refused(refusal) => Err(refusal.into_error()),
    }
}

fn left_running(copy: &Copy, rotation: &str, step: Step) -> Result<(), RpcError> {
    match step {
        Step::Done => Ok(()),
        Step::Refused(refusal) => Err(refusal.into_error()),
        Step::NotConfirmed(machines) => Err(unavailable(format!(
            "Container {} is left running and {rotation}: the proxies on {} did not confirm they stopped routing it.",
            copy.container_id,
            listed(&machines)
        ))),
        Step::Cancelled => Err(unavailable(format!(
            "Container {} is left running and {rotation}: the wait for its proxies was cancelled.",
            copy.container_id
        ))),
    }
}

fn deploy_again(copy: &Copy) -> RpcError {
    RpcError {
        code: RpcErrorCode::Conflict,
        message: format!(
            "Container {} is out of rotation and cannot start again. Deploy the Service again.",
            copy.container_id
        ),
        details: Value::Null,
        cause: Vec::new(),
    }
}

fn unavailable(message: String) -> RpcError {
    RpcError {
        code: RpcErrorCode::Unavailable,
        message,
        details: Value::Null,
        cause: Vec::new(),
    }
}

fn listed(machines: &[MachineId]) -> String {
    machines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}
