//! Placement convergence: move one replicated Service's Containers off Machines that no
//! longer admit it, one at a time and start-first. No hooks, no Deployment record, no
//! Config Store writes, and each moved Container keeps the image ID and gen it ran.
//!
//! A move runs on rotation: the replacement starts Pending on its destination and goes In,
//! every live proxy lists it, and only then does the old copy go Out, unlisted and Gone.
//! That retirement comes back unawaited, so the next move need not wait for it.
//!
//! Reasons and failures are typed; their `Display` is the CLI's English. Convergence never
//! returns an error: whatever moved before a failure stays in its result.

use std::collections::BTreeSet;
use std::fmt;

use futures_util::FutureExt;
use futures_util::future::{BoxFuture, ready};
use ployz_core::{
    AnonymousVolumes, ContainerId, ContainerKind, ContainerObservation, ContainerRefusalReason,
    CopyRotation, ExecutionError, InspectContainerRequest, Machine, MachineId, MachineName,
    MachineTarget, Namespace, PullPolicy, QualifiedService, RawVolumeSource, ResolvedServiceSpec,
    ServiceMode, ServicePlacementEligibility, op,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};

use super::DeploySnapshot;
use super::cleanup::{self, BroughtIn, Copy, Step, Wait};
use super::exec::{MachineOperations, MoveContainerError};

/// A Server as a move names it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineRef {
    pub id: MachineId,
    pub name: MachineName,
}

impl From<&Machine> for MachineRef {
    fn from(machine: &Machine) -> Self {
        Self {
            id: machine.id,
            name: machine.name.clone(),
        }
    }
}

impl fmt::Display for MachineRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.name.fmt(f)
    }
}

/// One Container moved between Servers: its replacement is In on `to`, and `old` says what
/// became of the copy on `from`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Move {
    pub from: MachineRef,
    pub to: MachineRef,
    pub old: OldCopyEnd,
}

pub(crate) struct Moving {
    pub(crate) from: MachineRef,
    pub(crate) to: MachineRef,
    pub(crate) old: Retiring,
}

pub(crate) type Retiring = BoxFuture<'static, Result<OldCopyEnd, LastIn>>;

#[derive(Debug)]
pub(crate) struct LastIn;

/// What a move left of the old copy on `from`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OldCopyEnd {
    /// Out, unlisted by every live proxy, and Gone.
    Stopped,
    /// Still In: the replacement was not confirmed listed, so both copies serve.
    In { unconfirmed: Unconfirmed },
    /// Left running after Out was asked: its unlisting or removal was not confirmed. The
    /// next Drain cleans it up.
    Running { unconfirmed: Unconfirmed },
}

/// Why a move stopped short of confirming a rotation step.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Unconfirmed {
    /// The proxies on these Servers did not confirm within the ack wait.
    Proxies { servers: Vec<MachineRef> },
    /// The step was cancelled.
    Cancelled,
    /// Asking the Server failed, or it refused: `detail` says how.
    Failed { detail: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Converged<M = Move> {
    /// Every Container that had to move did. Never empty.
    Moved { moves: Vec<M> },
    /// None of its active Containers had to move.
    NothingToMove,
    /// `moves` were made, then `failure` stopped the rest.
    Failed { moves: Vec<M>, failure: MoveFailure },
    /// Nothing moved, and why.
    Stays { reason: StayReason },
    /// It stopped after `moves` without deciding the rest, and the Drain stops with it.
    Stopped { moves: Vec<M>, stop: DrainStop },
}

/// Why a Drain stopped before deciding every Service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DrainStop {
    Cancelled,
    /// The entry Server stopped answering.
    EntryUnreachable {
        detail: String,
    },
}

impl fmt::Display for DrainStop {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("cancelled"),
            Self::EntryUnreachable { detail } => {
                write!(f, "the entry Server stopped answering: {detail}")
            }
        }
    }
}

/// Why a Service's Containers stay where they are. Each holds what the snapshot can't
/// vouch for, or refuses what a move could lose.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StayReason {
    /// A Server's Containers could not be listed.
    Unobserved { server: MachineRef },
    /// Its Containers carry more than one Serving Shape.
    MidRollout,
    /// It is a Global Service.
    Global,
    /// It mounts a Bind Mount on `server`.
    BindMount { server: MachineRef },
    /// It mounts a Volume that lives on `server`.
    Volume { server: MachineRef },
    /// No Server could take its next Container.
    NoDestination { detail: String },
    /// Its old copy refused Out: it is the last copy In. The replacement stays as it is
    /// on its destination.
    LastIn,
}

impl fmt::Display for StayReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unobserved { server } => write!(f, "cannot observe {server}"),
            Self::MidRollout => f.write_str("mid-rollout: deploy it first"),
            Self::Global => f.write_str("Global Services run on every Server that accepts them"),
            Self::BindMount { server } => write!(f, "Bind Mount on {server}"),
            Self::Volume { server } => write!(f, "its Volume is on {server}"),
            Self::NoDestination { detail } => write!(f, "no eligible Server: {detail}"),
            Self::LastIn => f.write_str("last copy in rotation; deploy it elsewhere first"),
        }
    }
}

/// Why one move failed, by stage. Every failure leaves the old Container serving where it
/// was.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum MoveFailure {
    /// No Server could take its next Container any more.
    NoDestination { from: MachineRef, detail: String },
    /// `from` predates image IDs.
    SourceTooOld { from: MachineRef },
    /// Reading its running image ID on `from` failed.
    ReadImage {
        from: MachineRef,
        to: MachineRef,
        detail: String,
    },
    /// Copying the image to `to` failed.
    CopyImage {
        from: MachineRef,
        to: MachineRef,
        detail: String,
    },
    /// The new Container on `to` never served. `replacement_removed` says none is left
    /// there; otherwise it may still be.
    NotServing {
        from: MachineRef,
        to: MachineRef,
        detail: String,
        replacement_removed: bool,
    },
    /// Cancelled before the new Container served.
    Cancelled {
        from: MachineRef,
        to: MachineRef,
        replacement_removed: bool,
    },
    /// A fresh look refused the move before it started, or its old copy refused Out.
    Refused { reason: StayReason },
}

impl MoveFailure {
    /// Whether this was a move of another Server's copy that left nothing behind: that
    /// copy still serves there, and no replacement this move started is left. A
    /// cancellation is the Drain's own, so it never qualifies.
    pub(crate) fn clean_elsewhere(&self, server: &MachineId) -> bool {
        match self {
            Self::NoDestination { from, .. }
            | Self::SourceTooOld { from }
            | Self::ReadImage { from, .. }
            | Self::CopyImage { from, .. } => from.id != *server,
            Self::NotServing {
                from,
                replacement_removed,
                ..
            } => from.id != *server && *replacement_removed,
            Self::Cancelled { .. } | Self::Refused { .. } => false,
        }
    }
}

impl fmt::Display for MoveFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoDestination { detail, .. } => write!(f, "no eligible Server: {detail}"),
            Self::SourceTooOld { from } => write!(
                f,
                "Server {from} does not report image IDs; upgrade it with `ployz server upgrade {from}`, then rerun"
            ),
            Self::ReadImage { from, detail, .. } => write!(f, "inspecting it on {from}: {detail}"),
            Self::CopyImage { to, detail, .. } => write!(f, "copying its image to {to}: {detail}"),
            Self::NotServing {
                from, to, detail, ..
            } => write!(f, "moving it from {from} to {to}: {detail}"),
            Self::Cancelled { from, to, .. } => {
                write!(f, "moving it from {from} to {to}: cancelled")
            }
            Self::Refused { reason } => reason.fmt(f),
        }
    }
}

pub(crate) trait ConvergenceClient {
    type Moved;
    async fn observe(&mut self) -> Result<DeploySnapshot, ConnectError>;
    async fn move_one(
        &mut self,
        snapshot: &DeploySnapshot,
        container: &ContainerObservation,
        cancellation: &CancellationToken,
    ) -> Result<Self::Moved, MoveFailure>;
}

impl ConvergenceClient for Client {
    type Moved = Moving;

    async fn observe(&mut self) -> Result<DeploySnapshot, ConnectError> {
        let machines = self.machines().await?;
        self.deploy_snapshot(machines).await
    }

    async fn move_one(
        &mut self,
        snapshot: &DeploySnapshot,
        container: &ContainerObservation,
        cancellation: &CancellationToken,
    ) -> Result<Moving, MoveFailure> {
        move_one(self, snapshot, container, cancellation).await
    }
}

pub(crate) async fn converge<C: ConvergenceClient>(
    client: &mut C,
    service: &QualifiedService,
    cancellation: &CancellationToken,
) -> Converged<C::Moved> {
    let entry_lost = |moves: Vec<C::Moved>, error: &ConnectError| Converged::Stopped {
        moves,
        stop: DrainStop::EntryUnreachable {
            detail: crate::ui::row(error),
        },
    };
    let snapshot = match client.observe().await {
        Ok(snapshot) => snapshot,
        Err(error) => return entry_lost(Vec::new(), &error),
    };
    let stranded = movable(&snapshot, service);
    if let Some(reason) = refusal(&snapshot, service, &stranded) {
        return Converged::Stays { reason };
    }
    let ids: Vec<ContainerId> = stranded.iter().map(|c| c.container_id).collect();
    let mut moves = Vec::new();
    let mut first = Some(snapshot);
    for id in ids {
        if cancellation.is_cancelled() {
            return Converged::Stopped {
                moves,
                stop: DrainStop::Cancelled,
            };
        }
        let fresh = match first.take() {
            Some(snapshot) => snapshot,
            None => match client.observe().await {
                Ok(snapshot) => snapshot,
                Err(error) => return entry_lost(moves, &error),
            },
        };
        // A snapshot missing a Server's listing can't tell a gone Container from an unlisted one.
        if let Some(reason) = unobserved(&fresh) {
            return Converged::Failed {
                moves,
                failure: MoveFailure::Refused { reason },
            };
        }
        let stranded = movable(&fresh, service);
        let Some(container) = stranded.iter().find(|c| c.container_id == id) else {
            // Gone, exited, or admitted again since the first snapshot: it stays put,
            // whatever the rest would now refuse.
            continue;
        };
        if let Some(reason) = refusal(&fresh, service, &stranded) {
            return Converged::Failed {
                moves,
                failure: MoveFailure::Refused { reason },
            };
        }
        match client.move_one(&fresh, container, cancellation).await {
            Ok(moved) => moves.push(moved),
            Err(failure) => return Converged::Failed { moves, failure },
        }
    }
    if moves.is_empty() {
        Converged::NothingToMove
    } else {
        Converged::Moved { moves }
    }
}

/// Why nothing of this Service may move now, if so. It holds whatever the snapshot can't
/// vouch for, and refuses before anything is removed when no Server can take a Container.
fn refusal(
    snapshot: &DeploySnapshot,
    service: &QualifiedService,
    stranded: &[&ContainerObservation],
) -> Option<StayReason> {
    if let Some(reason) = unobserved(snapshot) {
        return Some(reason);
    }
    let active = snapshot
        .containers
        .iter()
        .filter(|container| {
            is_service_container(container, service) && *container.rotation() != CopyRotation::Out
        })
        .collect::<Vec<_>>();
    let spec = &active.first()?.resolved_spec;
    if active
        .iter()
        .any(|container| container.resolved_spec.serving_shape() != spec.serving_shape())
    {
        return Some(StayReason::MidRollout);
    }
    // The snapshot carries no storage evidence: `stays` pins every storage-dependent spec
    // first. Observe storage once Volumes can move.
    let first = stranded.first()?;
    if let Some(reason) = stays(spec, &machine_ref(snapshot, &first.machine_id)) {
        return Some(reason);
    }
    super::planning::place_one(&spec.to_requested(), &service.namespace, snapshot)
        .err()
        .map(|error| StayReason::NoDestination {
            detail: crate::ui::row(&error),
        })
}

/// The first Server whose Containers the snapshot could not list, if any.
fn unobserved(snapshot: &DeploySnapshot) -> Option<StayReason> {
    snapshot
        .container_failures
        .iter()
        .map(|failure| &failure.machine_id)
        .chain(&snapshot.container_omissions)
        .next()
        .map(|id| StayReason::Unobserved {
            server: machine_ref(snapshot, id),
        })
}

fn is_service_container(container: &ContainerObservation, service: &QualifiedService) -> bool {
    container.kind == ContainerKind::ServiceContainer
        && container.namespace == service.namespace
        && container.resolved_spec.name == service.name
        && super::is_active_runtime(&container.runtime)
}

/// Why this Service's Containers can't move off `machine`, if they can't.
fn stays(spec: &ResolvedServiceSpec, machine: &MachineRef) -> Option<StayReason> {
    pin(spec).map(|pin| match pin {
        Pin::Global => StayReason::Global,
        Pin::BindMount => StayReason::BindMount {
            server: machine.clone(),
        },
        Pin::Volume => StayReason::Volume {
            server: machine.clone(),
        },
    })
}

/// Whether a Drain could move this Service's Containers, storage and mode permitting.
pub(crate) fn can_move(spec: &ResolvedServiceSpec) -> bool {
    pin(spec).is_none()
}

/// What holds a Service's Containers on their Server. It refuses anything a move could lose.
enum Pin {
    Global,
    BindMount,
    Volume,
}

fn pin(spec: &ResolvedServiceSpec) -> Option<Pin> {
    if spec.mode == ServiceMode::Global {
        return Some(Pin::Global);
    }
    spec.volume_graph()
        .mounted_volumes()
        .find_map(|volume| match volume.source.kind() {
            RawVolumeSource::Tmpfs { .. } => None,
            RawVolumeSource::Bind { .. } => Some(Pin::BindMount),
            RawVolumeSource::External { .. }
            | RawVolumeSource::Ordinary { .. }
            | RawVolumeSource::Provisioned { .. } => Some(Pin::Volume),
        })
}

/// The Service's active Containers on Machines its own spec definitely rules out.
fn stranded<'a>(
    snapshot: &'a DeploySnapshot,
    service: &QualifiedService,
) -> Vec<&'a ContainerObservation> {
    snapshot
        .containers
        .iter()
        .filter(|container| {
            is_service_container(container, service)
                && snapshot
                    .machines
                    .iter()
                    .find(|machine| machine.machine.id == container.machine_id)
                    .is_some_and(|machine| {
                        matches!(
                            container
                                .resolved_spec
                                .to_requested()
                                .placement_eligibility_in_namespace(
                                    &service.namespace,
                                    &machine.machine,
                                    machine.storage.as_ref(),
                                ),
                            ServicePlacementEligibility::Ineligible(_)
                        )
                    })
        })
        .collect()
}

fn movable<'a>(
    snapshot: &'a DeploySnapshot,
    service: &QualifiedService,
) -> Vec<&'a ContainerObservation> {
    let mut movable = stranded(snapshot, service);
    movable.retain(|container| {
        !matches!(
            container.rotation(),
            CopyRotation::Held(_) | CopyRotation::Out
        )
    });
    movable
}

/// The snapshot's Machine `id`. Container listings fan out to the snapshot's Machines, so
/// every id a snapshot carries names one of them.
fn machine<'a>(snapshot: &'a DeploySnapshot, id: &MachineId) -> &'a Machine {
    &snapshot
        .machines
        .iter()
        .find(|machine| machine.machine.id == *id)
        .expect("snapshot evidence names the snapshot's Machines")
        .machine
}

fn machine_ref(snapshot: &DeploySnapshot, id: &MachineId) -> MachineRef {
    MachineRef::from(machine(snapshot, id))
}

#[derive(Debug)]
pub(in crate::deploy) enum RelocateError {
    NotServing {
        detail: String,
        replacement_removed: bool,
    },
    Cancelled {
        replacement_removed: bool,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub(in crate::deploy) enum Relocated {
    Listed,
    Unlisted(Unconfirmed),
}

pub(in crate::deploy) async fn relocate<C: MachineOperations>(
    client: &C,
    namespace: &Namespace,
    spec: &ResolvedServiceSpec,
    to: &MachineId,
    old: &Copy,
    name: &(dyn Fn(&MachineId) -> MachineRef + Sync),
    cancellation: &CancellationToken,
) -> Result<Relocated, RelocateError> {
    let (new, deploy) = super::exec::move_container(
        client,
        namespace,
        spec,
        to,
        (&old.machine_id, &old.container_id),
        cancellation,
    )
    .await
    .map_err(
        |MoveContainerError {
             error,
             replacement_removed,
         }| match error {
            ExecutionError::Cancelled => RelocateError::Cancelled {
                replacement_removed,
            },
            error @ (ExecutionError::Machine { .. }
            | ExecutionError::Health { .. }
            | ExecutionError::DependencyHealth { .. }
            | ExecutionError::Hook { .. }
            | ExecutionError::NewerServing { .. }) => RelocateError::NotServing {
                detail: crate::ui::row(&error),
                replacement_removed,
            },
        },
    )?;
    let new = Copy {
        machine_id: *to,
        service: old.service.clone(),
        container_id: new,
        rotation: CopyRotation::Pending,
    };
    let not_in = match cleanup::bring_in(client, &new, spec, deploy).await {
        Ok(BroughtIn::In) => None,
        Ok(BroughtIn::Refused(refusal)) => Some(refusal.message),
        Ok(BroughtIn::NotIn { health }) => Some(crate::ui::row(&super::exec::health_error(
            new.container_id,
            health,
            spec,
        ))),
        Err(error) => Some(crate::ui::row(&error)),
    };
    if let Some(detail) = not_in {
        let removed = cleanup::remove(client, &new, AnonymousVolumes::Remove).await;
        return Err(RelocateError::NotServing {
            detail,
            replacement_removed: matches!(removed, Ok(Step::Done)),
        });
    }
    let listed = cleanup::wait_listed(
        client,
        to,
        &new.service,
        &BTreeSet::from([new.container_id]),
        cancellation,
        |_| {},
    )
    .await;
    let unconfirmed = match listed {
        Ok(Wait::Done) => None,
        Ok(Wait::TimedOut(machines) | Wait::Gapped(machines)) => Some(Unconfirmed::Proxies {
            servers: machines.machines.iter().map(name).collect(),
        }),
        Ok(Wait::Cancelled) => Some(Unconfirmed::Cancelled),
        Err(error) => Some(Unconfirmed::Failed {
            detail: crate::ui::row(&error),
        }),
    };
    if let Some(unconfirmed) = unconfirmed {
        return Ok(Relocated::Unlisted(unconfirmed));
    }
    // The listed wait returns Done when every proxy already lists the copy, even under a
    // cancel that arrived meanwhile. Between In and Out, Ctrl-C leaves both copies In.
    if cancellation.is_cancelled() {
        return Ok(Relocated::Unlisted(Unconfirmed::Cancelled));
    }
    Ok(Relocated::Listed)
}

pub(in crate::deploy) async fn retire_old<C: MachineOperations>(
    client: &C,
    old: &Copy,
    name: &(dyn Fn(&MachineId) -> MachineRef + Sync),
    cancellation: &CancellationToken,
) -> Result<OldCopyEnd, LastIn> {
    let retired = cleanup::retire(client, old, false, AnonymousVolumes::Remove, cancellation).await;
    let unconfirmed = match retired {
        Ok(Step::Done) => return Ok(OldCopyEnd::Stopped),
        Ok(Step::Refused(refusal)) if refusal.reason == ContainerRefusalReason::LastIn => {
            return Err(LastIn);
        }
        Ok(Step::Refused(refusal)) => Unconfirmed::Failed {
            detail: refusal.message,
        },
        Ok(Step::NotConfirmed(machines)) => Unconfirmed::Proxies {
            servers: machines.machines.iter().map(name).collect(),
        },
        Ok(Step::Cancelled) => Unconfirmed::Cancelled,
        Err(error) => Unconfirmed::Failed {
            detail: crate::ui::row(&error),
        },
    };
    Ok(OldCopyEnd::Running { unconfirmed })
}

async fn move_one(
    client: &mut Client,
    snapshot: &DeploySnapshot,
    container: &ContainerObservation,
    cancellation: &CancellationToken,
) -> Result<Moving, MoveFailure> {
    let source = machine(snapshot, &container.machine_id);
    let from = MachineRef::from(source);
    let spec = &container.resolved_spec;
    let dest = super::planning::place_one(&spec.to_requested(), &container.namespace, snapshot)
        .map_err(|error| MoveFailure::NoDestination {
            from: from.clone(),
            detail: crate::ui::row(&error),
        })?;
    let dest = machine(snapshot, &dest);
    let to = MachineRef::from(dest);
    let image_id = image_id(client, &container.container_id, &from, &to).await?;
    crate::image::copy_running_image(client, source, dest, &spec.container.image, &image_id)
        .await
        .map_err(|error| MoveFailure::CopyImage {
            from: from.clone(),
            to: to.clone(),
            detail: crate::ui::row(&error),
        })?;
    // The image is on `dest` by now; a registry pull could fetch a different one.
    let mut spec = spec.clone();
    spec.container.pull_policy = PullPolicy::Never;
    let old = Copy::observed(container);
    let known = snapshot
        .machines
        .iter()
        .map(|machine| MachineRef::from(&machine.machine))
        .collect::<Vec<_>>();
    let relocated = relocate(
        &*client,
        &container.namespace,
        &spec,
        &dest.id,
        &old,
        &|id| named(&known, id),
        cancellation,
    )
    .await;
    match relocated {
        Ok(Relocated::Listed) => {
            let client = client.clone();
            let cancellation = cancellation.clone();
            let old = async move {
                retire_old(&client, &old, &|id| named(&known, id), &cancellation).await
            };
            Ok(Moving {
                from,
                to,
                old: old.boxed(),
            })
        }
        Ok(Relocated::Unlisted(unconfirmed)) => Ok(Moving {
            from,
            to,
            old: ready(Ok(OldCopyEnd::In { unconfirmed })).boxed(),
        }),
        Err(RelocateError::NotServing {
            detail,
            replacement_removed,
        }) => Err(MoveFailure::NotServing {
            from,
            to,
            detail,
            replacement_removed,
        }),
        Err(RelocateError::Cancelled {
            replacement_removed,
        }) => Err(MoveFailure::Cancelled {
            from,
            to,
            replacement_removed,
        }),
    }
}

pub(crate) fn named(known: &[MachineRef], id: &MachineId) -> MachineRef {
    known
        .iter()
        .find(|machine| machine.id == *id)
        .cloned()
        .unwrap_or_else(|| MachineRef {
            id: *id,
            name: MachineName::parse(id.to_string())
                .or_else(|_| MachineName::parse(format!("m{id}")))
                .expect("a lowercase hex id is a DNS label"),
        })
}

async fn image_id(
    client: &Client,
    container: &ContainerId,
    from: &MachineRef,
    to: &MachineRef,
) -> Result<String, MoveFailure> {
    client
        .invoke::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: *container,
            },
            &MachineTarget::from(&from.id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map_err(|error| MoveFailure::ReadImage {
            from: from.clone(),
            to: to.clone(),
            detail: crate::ui::row(&error),
        })?
        .image_id
        .ok_or_else(|| MoveFailure::SourceTooOld { from: from.clone() })
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerId, ContainerKind, ContainerObservation, ContainerObservationParts,
        ContainerRuntimeObservation, CopyRotation, DeploymentLogId, HealthObservation, Machine,
        MachineId, MachineName, MachineObservation, MembershipObservation, Namespace,
        QualifiedService, ResolvedServiceSpec, WireGuardPublicKey,
    };
    use serde_json::json;

    use tokio_util::sync::CancellationToken;

    use super::{
        Converged, ConvergenceClient, DeploySnapshot, DrainStop, MachineRef, Move, MoveFailure,
        OldCopyEnd, converge, movable, refusal, stays,
    };
    use crate::connect::ConnectError;

    fn spec(mode: serde_json::Value, source: serde_json::Value) -> ResolvedServiceSpec {
        serde_json::from_value(json!({
            "service_id": "a".repeat(32),
            "name": "api",
            "mode": mode,
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" },
            "volumes": [{ "reference": "data", "source": source }],
            "mounts": [{ "volume": "data", "target": "/data" }],
        }))
        .unwrap()
    }

    #[test]
    fn only_what_a_move_cannot_lose_stays() {
        let web = super::MachineRef::from(&machine('2', false).machine);
        let replicated = json!({ "mode": "replicated", "replicas": 2 });
        let tmpfs = json!({ "kind": "tmpfs", "size_bytes": 4096 });
        let stays_with = |mode: &serde_json::Value, source| {
            stays(&spec(mode.clone(), source), &web).map(|reason| reason.to_string())
        };
        assert_eq!(stays_with(&replicated, tmpfs.clone()), None);
        assert_eq!(
            stays_with(
                &replicated,
                json!({ "kind": "bind", "machine_path": "/srv" })
            )
            .as_deref(),
            Some("Bind Mount on machine-2")
        );
        assert_eq!(
            stays_with(&replicated, json!({ "kind": "external", "name": "app_db" })).as_deref(),
            Some("its Volume is on machine-2")
        );
        assert!(stays_with(&json!({ "mode": "global" }), tmpfs).is_some());
    }

    #[test]
    fn what_the_snapshot_cannot_vouch_for_is_held() {
        let replicated = json!({ "mode": "replicated", "replicas": 2 });
        let stateless = spec(replicated.clone(), json!({ "kind": "tmpfs" }));
        let service = QualifiedService::parse("app/api").unwrap();
        let refused = |snapshot: &DeploySnapshot| {
            let stranded = super::stranded(snapshot, &service);
            refusal(snapshot, &service, &stranded).map(|reason| reason.to_string())
        };
        let base = || DeploySnapshot {
            machines: vec![machine('a', false), machine('b', true)],
            containers: vec![container('1', 'a', stateless.clone())],
            ..DeploySnapshot::default()
        };
        assert_eq!(refused(&base()), None);

        let mut newer = stateless.clone();
        newer.container.image = "alpine:3.24".into();
        let mut mixed = base();
        mixed.containers.push(container('2', 'b', newer));
        assert_eq!(
            refused(&mixed).as_deref(),
            Some("mid-rollout: deploy it first")
        );

        let provisioned = spec(
            replicated,
            json!({
                "kind": "provisioned",
                "name": "app_data",
                "maximum_bytes": 1_073_741_824,
                "scope": { "namespace": "app", "logical_name": "data" }
            }),
        );
        let mut no_storage_evidence = base();
        no_storage_evidence.containers = vec![container('1', 'a', provisioned)];
        assert_eq!(
            refused(&no_storage_evidence).as_deref(),
            Some("its Volume is on machine-a"),
            "a Managed volume holds its Service whatever the snapshot knows of storage"
        );

        let mut unobserved = base();
        unobserved
            .container_omissions
            .push(machine('b', true).machine.id);
        assert_eq!(
            refused(&unobserved).as_deref(),
            Some("cannot observe machine-b")
        );

        let mut nowhere = base();
        nowhere.machines = vec![machine('a', false), machine('b', false)];
        assert!(
            refused(&nowhere).is_some_and(|reason| reason.starts_with("no eligible Server")),
            "{:?}",
            refused(&nowhere)
        );
    }

    struct Scripted {
        snapshots: std::collections::VecDeque<Result<DeploySnapshot, ConnectError>>,
    }

    impl Scripted {
        fn new(snapshots: impl IntoIterator<Item = Result<DeploySnapshot, ConnectError>>) -> Self {
            Self {
                snapshots: snapshots.into_iter().collect(),
            }
        }
    }

    impl ConvergenceClient for Scripted {
        type Moved = Move;

        async fn observe(&mut self) -> Result<DeploySnapshot, ConnectError> {
            self.snapshots
                .pop_front()
                .expect("one snapshot per observation")
        }

        async fn move_one(
            &mut self,
            snapshot: &DeploySnapshot,
            container: &ContainerObservation,
            _cancellation: &CancellationToken,
        ) -> Result<Move, MoveFailure> {
            Ok(Move {
                from: super::machine_ref(snapshot, &container.machine_id),
                to: MachineRef::from(&machine('b', true).machine),
                old: OldCopyEnd::Stopped,
            })
        }
    }

    fn stateless() -> ResolvedServiceSpec {
        spec(
            json!({ "mode": "replicated", "replicas": 2 }),
            json!({ "kind": "tmpfs" }),
        )
    }

    fn two_on_a() -> DeploySnapshot {
        DeploySnapshot {
            machines: vec![machine('a', false), machine('b', true)],
            containers: vec![
                container('1', 'a', stateless()),
                container('2', 'a', stateless()),
            ],
            ..DeploySnapshot::default()
        }
    }

    fn one_move() -> Vec<Move> {
        vec![Move {
            from: MachineRef::from(&machine('a', false).machine),
            to: MachineRef::from(&machine('b', true).machine),
            old: OldCopyEnd::Stopped,
        }]
    }

    #[test]
    fn held_and_out_copies_are_not_moved() {
        let service = QualifiedService::parse("app/api").unwrap();
        let run = DeploymentLogId::parse("run").unwrap();
        let mut snapshot = two_on_a();
        snapshot
            .containers
            .push(rotating('3', CopyRotation::Held(run)));
        snapshot.containers.push(rotating('4', CopyRotation::Out));
        snapshot.containers.push(rotating('5', CopyRotation::In));
        let moved: Vec<_> = movable(&snapshot, &service)
            .into_iter()
            .map(|container| container.container_id)
            .collect();
        assert_eq!(moved, [id('1'), id('2'), id('5')]);
    }

    #[test]
    fn only_a_serving_copy_of_another_shape_holds_a_move() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut older = serde_json::to_value(stateless()).unwrap();
        *older.pointer_mut("/container/image").unwrap() = json!("alpine:3.22.0");
        let older: ResolvedServiceSpec = serde_json::from_value(older).unwrap();
        let with_older = |rotation| {
            let mut snapshot = two_on_a();
            let parts = ContainerObservationParts::from(container('3', 'a', older.clone()));
            snapshot.containers.push(
                ContainerObservation::try_from(ContainerObservationParts {
                    rotation: Some(rotation),
                    ..parts
                })
                .unwrap(),
            );
            snapshot
        };
        let out = with_older(CopyRotation::Out);
        assert_eq!(refusal(&out, &service, &movable(&out, &service)), None);
        let serving = with_older(CopyRotation::In);
        assert_eq!(
            refusal(&serving, &service, &movable(&serving, &service)),
            Some(super::StayReason::MidRollout)
        );
    }

    fn rotating(hex: char, rotation: CopyRotation) -> ContainerObservation {
        let parts = ContainerObservationParts::from(container(hex, 'a', stateless()));
        ContainerObservation::try_from(ContainerObservationParts {
            rotation: Some(rotation),
            ..parts
        })
        .unwrap()
    }

    fn id(hex: char) -> ContainerId {
        ContainerId::parse(hex.to_string().repeat(64)).unwrap()
    }

    #[tokio::test]
    async fn losing_the_entry_stops_and_keeps_the_moves_already_made() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut client = Scripted::new([
            Ok(two_on_a()),
            Err(ConnectError::Attempt("entry went away".into())),
        ]);
        assert_eq!(
            converge(&mut client, &service, &CancellationToken::new()).await,
            Converged::Stopped {
                moves: one_move(),
                stop: DrainStop::EntryUnreachable {
                    detail: "connection attempt failed: entry went away".into(),
                },
            }
        );

        let mut client = Scripted::new([Err(ConnectError::Attempt("down".into()))]);
        assert_eq!(
            converge(&mut client, &service, &CancellationToken::new()).await,
            Converged::Stopped {
                moves: Vec::new(),
                stop: DrainStop::EntryUnreachable {
                    detail: "connection attempt failed: down".into(),
                },
            },
            "an entry lost before the first look moves nothing"
        );
    }

    #[tokio::test]
    async fn a_fresh_snapshot_that_refuses_keeps_the_moves_already_made() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut unobserved = two_on_a();
        unobserved
            .container_omissions
            .push(machine('b', true).machine.id);
        let mut client = Scripted::new([Ok(two_on_a()), Ok(unobserved)]);
        let converged = converge(&mut client, &service, &CancellationToken::new()).await;
        let Converged::Failed { moves, failure } = &converged else {
            panic!("{converged:?}");
        };
        assert_eq!(moves, &one_move());
        assert_eq!(failure.to_string(), "cannot observe machine-b");
    }

    #[tokio::test]
    async fn a_container_gone_since_the_first_snapshot_is_skipped_before_any_refusal() {
        let service = QualifiedService::parse("app/api").unwrap();
        // The moved Container now sits on machine-b, which takes nothing more: a refusal
        // would read "no eligible Server", but the one left to move is gone.
        let after = DeploySnapshot {
            machines: vec![machine('a', false), machine('b', false)],
            containers: vec![container('3', 'b', stateless())],
            ..DeploySnapshot::default()
        };
        let mut client = Scripted::new([Ok(two_on_a()), Ok(after)]);
        assert_eq!(
            converge(&mut client, &service, &CancellationToken::new()).await,
            Converged::Moved { moves: one_move() }
        );
    }

    #[tokio::test]
    async fn a_cancelled_convergence_stops_before_moving() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut client = Scripted::new([Ok(two_on_a())]);
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        assert_eq!(
            converge(&mut client, &service, &cancelled).await,
            Converged::Stopped {
                moves: Vec::new(),
                stop: DrainStop::Cancelled,
            }
        );
    }

    fn machine(hex: char, accepts_services: bool) -> MachineObservation {
        MachineObservation::new(
            Machine {
                labels: Default::default(),
                accepts_builds: true,
                accepts_services,
                accepts_ingress: false,
                id: MachineId::parse(hex.to_string().repeat(32)).unwrap(),
                name: MachineName::parse(format!("machine-{hex}")).unwrap(),
                subnet: format!("10.210.{}.0/24", hex.to_digit(16).unwrap())
                    .parse()
                    .unwrap(),
                public_key: WireGuardPublicKey([hex as u8; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                build_concurrency: None,
            },
            MembershipObservation::Up,
        )
    }

    fn container(id: char, machine: char, spec: ResolvedServiceSpec) -> ContainerObservation {
        ContainerObservation::try_from(ContainerObservationParts {
            rotation: None,
            r#gen: None,
            container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: "api".into(),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
            namespace: Namespace::parse("app").unwrap(),
            kind: ContainerKind::ServiceContainer,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec: spec,
            address: None,
            labels: Default::default(),
            stopped: false,
            run: None,
        })
        .unwrap()
    }
}
