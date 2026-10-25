use std::collections::BTreeSet;
use std::time::Duration;

use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerKind, ContainerLocator, ContainerObservation,
    ContainerRefusal, ContainerRefusalReason, ContainerWant, CopyRotation, DeploymentLogId, Drain,
    EnsuredContainer, HealthVerdict, MachineId, QualifiedService, ResolvedServiceSpec, RoutesRead,
    RpcError, RpcErrorCode, RunRotation, RunTarget, StopOptions, Unacked,
};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::MachineOperations;
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

/// An unanswered ask re-reads the copy: the owner may have written In after
/// the deadline passed.
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

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Wait {
    Done,
    TimedOut(Unconfirmed),
    Cancelled,
}

/// The live Machines whose proxy had not confirmed when the wait ended.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Unconfirmed {
    pub(crate) machines: Vec<MachineId>,
    /// Those whose proxy row never arrived: the only ones that may be away.
    pub(crate) unanswered: Vec<MachineId>,
}

pub(crate) async fn wait_listed<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    copies: &BTreeSet<ContainerId>,
    cancellation: &CancellationToken,
    acked: impl FnMut(&MachineId),
) -> Result<Wait, RpcError> {
    wait_routes(
        || client.read_routes(machine_id, service),
        |routed| copies.is_subset(routed),
        cancellation,
        acked,
    )
    .await
}

/// Read through the entry Machine: every Machine reads the same replicated ack rows,
/// so a cut-off owner in the group does not stop the wait.
pub(crate) async fn wait_none_listed<C: MachineOperations>(
    client: &C,
    service: &QualifiedService,
    copies: &BTreeSet<ContainerId>,
    cancellation: &CancellationToken,
) -> Result<Wait, RpcError> {
    wait_routes(
        || client.read_entry_routes(service),
        |routed| routed.is_disjoint(copies),
        cancellation,
        |_| {},
    )
    .await
}

pub(crate) async fn wait_unlisted<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    service: &QualifiedService,
    container_id: &ContainerId,
    cancellation: &CancellationToken,
) -> Result<Wait, RpcError> {
    wait_routes(
        || client.read_routes(machine_id, service),
        |routed| !routed.contains(container_id),
        cancellation,
        |_| {},
    )
    .await
}

async fn wait_routes<Read>(
    read: impl Fn() -> Read,
    satisfied: impl Fn(&BTreeSet<ContainerId>) -> bool,
    cancellation: &CancellationToken,
    mut acked: impl FnMut(&MachineId),
) -> Result<Wait, RpcError>
where
    Read: Future<Output = Result<RoutesRead, RpcError>>,
{
    let deadline = Instant::now() + ACK_WAIT;
    let mut confirmed = BTreeSet::new();
    loop {
        let read = tokio::select! {
            read = read() => read?,
            () = cancellation.cancelled() => return Ok(Wait::Cancelled),
        };
        let waiting = still_waiting(&read, &satisfied);
        for machine in read.routes.keys() {
            if !waiting.machines.contains(machine) && confirmed.insert(*machine) {
                acked(machine);
            }
        }
        if read.complete && waiting.machines.is_empty() {
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

#[derive(Debug)]
pub(crate) enum Step {
    Done,
    Refused(ContainerRefusal),
    NotConfirmed(Unconfirmed),
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

pub(crate) async fn hold<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    run: DeploymentLogId,
    cancellation: &CancellationToken,
) -> Result<Step, RpcError> {
    let want = ContainerWant::Held {
        container: copy.locator(),
        run: run.clone(),
        expect: Some(copy.rotation.clone()),
    };
    let answer = client
        .ensure_unsettled(&copy.machine_id, want, Some(TARGET_RPC_TIMEOUT))
        .await;
    match answer {
        Ok(EnsuredContainer::Refused { refusal, .. }) => return Ok(Step::Refused(refusal)),
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
    .map(Step::from)
}

pub(crate) async fn take_out<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    allow_empty: bool,
    cancellation: &CancellationToken,
) -> Result<Step, RpcError> {
    if let Some(refusal) = send_out(client, copy, allow_empty).await? {
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

/// Asks the copy Out without waiting for proxies to drop it, giving the Machine's
/// refusal when it has one.
pub(crate) async fn send_out<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    allow_empty: bool,
) -> Result<Option<ContainerRefusal>, RpcError> {
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
            return Ok(Some(refusal));
        }
    }
    Ok(None)
}

pub(crate) async fn retire<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    allow_empty: bool,
    anonymous_volumes: AnonymousVolumes,
    cancellation: &CancellationToken,
) -> Result<Step, RpcError> {
    if !copy.drains() {
        return remove(client, copy, anonymous_volumes).await;
    }
    match take_out(client, copy, allow_empty, cancellation).await? {
        Step::Done => remove(client, copy, anonymous_volumes).await,
        step @ (Step::Refused(_) | Step::NotConfirmed(_) | Step::Cancelled) => Ok(step),
    }
}

/// Retires the copy without waiting for proxies to drop it. A removal beside a
/// copy on a Server this Deploy cannot reach: every proxy keeps the route while
/// its store lists that copy In, so the drain would never confirm.
pub(crate) async fn retire_unconfirmed<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    allow_empty: bool,
    anonymous_volumes: AnonymousVolumes,
) -> Result<Step, RpcError> {
    match send_out(client, copy, allow_empty).await? {
        None => remove(client, copy, anonymous_volumes).await,
        Some(refusal) => Ok(Step::Refused(refusal)),
    }
}

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

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum UserStarted {
    In,
    /// Started outside rotation; its health is the caller's to wait for.
    Running,
    NotIn(Option<HealthVerdict>),
}

pub(crate) async fn user_stop<C: MachineOperations>(
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
                Ok(Step::Cancelled) => Some(unavailable(
                    "the wait for its proxies was cancelled".to_owned(),
                )),
                // The ask may have landed, so the copy is restored as if Held.
                Err(ref error) => Some(error.clone()),
                Ok(Step::Done | Step::Refused(_) | Step::NotConfirmed(_)) => None,
            };
            if let Some(error) = failed {
                return Err(not_stopped(client, &copy, &observed.resolved_spec, run, error).await);
            }
        }
        left_running(&copy, "Held", step?)?;
    }
    stopped(client, &copy, &observed.resolved_spec, stop, queued).await
}

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

pub(crate) async fn user_restart<C: MachineOperations>(
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
        Ok(Step::Refused(refusal)) => return Err(refusal.into_error()),
        Ok(Step::Done) => stopped(client, &held, spec, stop, queued).await,
        Ok(Step::NotConfirmed(unconfirmed)) => Err(unavailable(format!(
            "Container {} was not restarted: the proxies on {} did not confirm they stopped routing it.",
            copy.container_id,
            listed(&unconfirmed.machines)
        ))),
        Ok(Step::Cancelled) => Err(unavailable(format!(
            "Container {} was not restarted: the wait for its proxies was cancelled.",
            copy.container_id
        ))),
        // The ask may have landed, so the copy is restored as if Held.
        Err(error) => Err(RpcError {
            message: with_reason(
                format_args!("Container {} was not restarted", copy.container_id),
                &error.message,
            ),
            ..error
        }),
    };
    let brought = bring_in(client, &held, spec, Some(run)).await;
    if let Err(mut error) = stopped {
        let left = match brought {
            Ok(BroughtIn::In) => "It is back In.",
            Ok(BroughtIn::NotIn { .. } | BroughtIn::Refused(_)) | Err(_) => {
                "It is left Held and not routed."
            }
        };
        error.message = then_sentence(&error.message, left);
        return Err(error);
    }
    started(&held, brought?)
}

pub(crate) async fn user_remove<C: MachineOperations>(
    client: &C,
    machine_id: &MachineId,
    container_id: &ContainerId,
    cancellation: &CancellationToken,
) -> Result<(), RpcError> {
    let observed = match observe(client, machine_id, container_id).await {
        Err(error) if error.code == RpcErrorCode::NotFound => return Ok(()),
        observed => observed?,
    };
    let copy = Copy::observed(&observed);
    let step = if routable(&observed) {
        retire(client, &copy, true, AnonymousVolumes::Remove, cancellation).await?
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
    RpcError {
        code: RpcErrorCode::Conflict,
        message: format!(
            "Container {} is left {} and not routed: no Deploy brought it In. Deploy the Service again to route it.",
            copy.container_id,
            rotation_name(&copy.rotation)
        ),
        details: Value::Null,
        cause: Vec::new(),
    }
}

/// Hooks stay Pending for life, and proxy copies are never routed.
fn routable(observed: &ContainerObservation) -> bool {
    observed.kind == ContainerKind::ServiceContainer
        && observed.identity() != QualifiedService::system_ingress()
}

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

async fn restart_pending<C: MachineOperations>(
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
/// nothing, so it goes back In, as a restart does when its wait fails.
async fn not_stopped<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    spec: &ResolvedServiceSpec,
    run: DeploymentLogId,
    mut error: RpcError,
) -> RpcError {
    let held = Copy {
        rotation: CopyRotation::Held(run.clone()),
        ..copy.clone()
    };
    let back = match bring_in(client, &held, spec, Some(run)).await {
        Ok(BroughtIn::In) => "It is back In.",
        Ok(BroughtIn::NotIn { .. } | BroughtIn::Refused(_)) | Err(_) => {
            "It is left running and Held."
        }
    };
    error.message = then_sentence(
        &with_reason(
            format_args!("Container {} was not stopped", copy.container_id),
            &error.message,
        ),
        back,
    );
    error
}

fn left_running(copy: &Copy, rotation: &str, step: Step) -> Result<(), RpcError> {
    match step {
        Step::Done => Ok(()),
        Step::Refused(refusal) => Err(refusal.into_error()),
        Step::NotConfirmed(unconfirmed) => Err(unavailable(format!(
            "Container {} is left running and {rotation}: the proxies on {} did not confirm they stopped routing it.",
            copy.container_id,
            listed(&unconfirmed.machines)
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
