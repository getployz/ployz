//! Rotation moves a Deploy or a user command drives on one copy and the
//! route waits that prove them: `bring_in`, the hold, `retire`, and the
//! listed and unlisted waits over `ReadRoutes`.

use std::collections::BTreeSet;
use std::time::Duration;

use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerLocator, ContainerNow, ContainerObservation,
    ContainerRefusal, ContainerRefusalReason, ContainerWant, CopyRotation, DeploymentLogId, Drain,
    EnsuredContainer, HealthVerdict, MachineId, QualifiedService, ResolvedServiceSpec, RoutesRead,
    RpcError, RunRotation, RunTarget,
};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::MachineOperations;
use crate::connect::{TARGET_RPC_TIMEOUT, in_rpc_timeout};

/// How often a wait re-reads the routes.
pub(crate) const ACK_POLL: Duration = Duration::from_secs(1);
/// How long a wait gives every live proxy to list or drop a copy.
pub(crate) const ACK_WAIT: Duration = Duration::from_secs(300);

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
        now: ContainerNow,
        health: Option<HealthVerdict>,
    },
    Refused(ContainerRefusal),
}

/// Ask the owner to bring `copy` In for `run`. Only an answer whose `now` is
/// In succeeds. An unanswered ask re-reads the copy (D17): the owner may have
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
            _ => BroughtIn::NotIn { now, health },
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
                Ok(BroughtIn::NotIn {
                    now: ContainerNow::Present(details.container),
                    health: None,
                })
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

/// Take `copy` Out, wait until no live proxy routes it, then remove it. A copy
/// never routed (Pending) goes straight to Gone. The copy is removed only after
/// the wait confirms: a copy proxies may still route is left running.
pub(crate) async fn retire<C: MachineOperations>(
    client: &C,
    copy: &Copy,
    allow_empty: bool,
    cancellation: &CancellationToken,
) -> Result<Step, RpcError> {
    match copy.rotation {
        CopyRotation::Pending => return remove(client, copy).await,
        CopyRotation::In | CopyRotation::Held(_) => {
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
        CopyRotation::Out => {}
    }
    match wait_unlisted(
        client,
        &copy.machine_id,
        &copy.service,
        &copy.container_id,
        cancellation,
    )
    .await?
    {
        Wait::Done => remove(client, copy).await,
        wait => Ok(Step::from(wait)),
    }
}

/// Remove `copy`. A copy already gone counts as removed.
pub(crate) async fn remove<C: MachineOperations>(
    client: &C,
    copy: &Copy,
) -> Result<Step, RpcError> {
    let want = ContainerWant::Gone {
        container: copy.locator(),
        drain: Drain::DEFAULT,
        anonymous_volumes: AnonymousVolumes::Remove,
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
