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

use crate::cluster::{Call, Deadline};
use ployz_core::error_chain;
use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::fmt;

use futures_util::FutureExt;
use futures_util::future::{BoxFuture, ready};
use ployz_core::authority::deploy::content_addressed;
use ployz_core::{
    AnonymousVolumes, Claim, ContainerId, ContainerKind, ContainerObservation,
    ContainerRefusalReason, ContainerRuntimeObservation, CopyRotation, CreationKey,
    ENSURE_PINNED_CONTAINER_CAPABILITY, HealthObservation, ImageDigest, InspectContainerRequest,
    Machine, MachineId, MachineName, MachineTarget, Namespace, QualifiedService, RawVolumeSource,
    Region, RequestedServiceSpec, ResolvedServiceSpec, ServiceMode, ServicePlacementEligibility,
    op,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};

use crate::deploy::cleanup::{self, Copy, EmptyPolicy, NotIn, RotationFailure};
use crate::deploy::{
    Approved, DeploySnapshot, MachineOperations, MoveEnd, MoveError, PlanError, PlanOptions,
    destinations, health_error, is_active_runtime, move_container, place_one, region_waits,
};

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

impl Unconfirmed {
    pub(crate) fn of(
        failure: RotationFailure,
        name: &(dyn Fn(&MachineId) -> MachineRef + Sync),
    ) -> Self {
        match failure {
            RotationFailure::Refused(refusal) => Unconfirmed::Failed {
                detail: refusal.message,
            },
            RotationFailure::NotConfirmed(unconfirmed) => Unconfirmed::Proxies {
                servers: unconfirmed.machines.iter().map(name).collect(),
            },
            RotationFailure::Cancelled => Unconfirmed::Cancelled,
        }
    }
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

/// What a report from a newer Ployz says where it names a reason this release doesn't know.
const UNRECOGNIZED: &str =
    "a newer Ployz gave a reason this one can't name; upgrade ployz to see it";

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
    /// No Server could take its next Container. `upgrade` names the Servers that could
    /// once upgraded to the release that copies a tag-named image by its ID. A release
    /// before that field reads only `detail`, which says the same.
    NoDestination {
        detail: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(optional, as = "Option<Vec<MachineRef>>")]
        upgrade: Vec<MachineRef>,
    },
    /// Its old copy refused Out: it is the last copy In. The replacement stays as it is
    /// on its destination.
    LastIn,
    /// A reason a later release added, which this one reads without its details.
    #[serde(other)]
    Unrecognized,
}

impl StayReason {
    /// Only `servers` could take its next Container, and each must be upgraded first.
    fn needs_upgrade(servers: Vec<MachineRef>) -> Self {
        let names = servers
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        Self::NoDestination {
            detail: format!(
                "only {names} can take it; upgrade with `ployz server upgrade`, then drain again"
            ),
            upgrade: servers,
        }
    }

    /// The one Server this move tried turned the pinned route down, and must be upgraded
    /// first. Another Server may still fit, so this names only the one it tried.
    fn upgrade_to(server: MachineRef) -> Self {
        Self::NoDestination {
            detail: format!(
                "upgrade {server} to move it there with `ployz server upgrade`, then drain again"
            ),
            upgrade: vec![server],
        }
    }
}

impl fmt::Display for StayReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unobserved { server } => write!(f, "cannot observe {server}"),
            Self::MidRollout => f.write_str("mid-rollout: deploy it first"),
            Self::Global => f.write_str("Global Services run on every Server that accepts them"),
            Self::BindMount { server } => write!(f, "Bind Mount on {server}"),
            Self::Volume { server } => write!(f, "its Volume is on {server}"),
            Self::NoDestination { detail, .. } => write!(f, "no eligible Server: {detail}"),
            Self::LastIn => f.write_str("last copy in rotation; deploy it elsewhere first"),
            Self::Unrecognized => f.write_str(UNRECOGNIZED),
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
    /// there; otherwise it may still be. `needs_upgrade` says `to` turned the move down
    /// until it is upgraded; a release before that field reads it from `detail`.
    NotServing {
        from: MachineRef,
        to: MachineRef,
        detail: String,
        replacement_removed: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(optional, as = "Option<bool>")]
        needs_upgrade: bool,
    },
    /// Cancelled before the new Container served.
    Cancelled {
        from: MachineRef,
        to: MachineRef,
        replacement_removed: bool,
    },
    /// A fresh look refused the move before it started, or its old copy refused Out.
    /// `from` names the copy's Server when the refusal came from its move.
    Refused {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        from: Option<MachineRef>,
        reason: StayReason,
    },
    /// A stage a later release added, which this one reads without its details.
    #[serde(other)]
    Unrecognized,
}

impl MoveFailure {
    pub(crate) fn of(error: MoveError, from: MachineRef, to: MachineRef) -> Self {
        let MoveError {
            end,
            replacement_removed,
        } = error;
        match end {
            MoveEnd::NotServing { detail } => Self::NotServing {
                from,
                to,
                detail,
                replacement_removed,
                needs_upgrade: false,
            },
            MoveEnd::Cancelled => Self::Cancelled {
                from,
                to,
                replacement_removed,
            },
            MoveEnd::NeedsUpgrade if replacement_removed => Self::Refused {
                from: Some(from),
                reason: StayReason::upgrade_to(to),
            },
            // A send may still have made a Container there, so the refusal can't say
            // nothing is left. It ends as `not_serving`, which every release reads.
            MoveEnd::NeedsUpgrade => Self::NotServing {
                detail: format!(
                    "upgrade {to} to move it there; its new container may still be there"
                ),
                from,
                to,
                replacement_removed,
                needs_upgrade: true,
            },
        }
    }

    /// Whether this was a move of another Server's copy that left nothing behind: that
    /// copy still serves there, and no replacement this move started is left. A
    /// cancellation is the Drain's own, so it never qualifies.
    pub(crate) fn clean_elsewhere(&self, server: &MachineId) -> bool {
        match self {
            Self::NoDestination { from, .. }
            | Self::SourceTooOld { from }
            | Self::ReadImage { from, .. }
            | Self::CopyImage { from, .. }
            | Self::Refused {
                from: Some(from), ..
            } => from.id != *server,
            Self::NotServing {
                from,
                replacement_removed,
                ..
            } => from.id != *server && *replacement_removed,
            Self::Cancelled { .. } | Self::Refused { from: None, .. } | Self::Unrecognized => false,
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
            Self::Refused { reason, .. } => reason.fmt(f),
            Self::Unrecognized => f.write_str(UNRECOGNIZED),
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
    /// Retire `old` behind `new`, the copy an earlier move of it already brought In.
    async fn take_over(
        &mut self,
        snapshot: &DeploySnapshot,
        old: &ContainerObservation,
        new: &ContainerObservation,
        cancellation: &CancellationToken,
    ) -> Result<Self::Moved, MoveFailure>;
}

impl ConvergenceClient for Approved {
    type Moved = Moving;

    async fn observe(&mut self) -> Result<DeploySnapshot, ConnectError> {
        let machines = self.client.machines().await?;
        self.client.deploy_snapshot(machines).await
    }

    async fn move_one(
        &mut self,
        snapshot: &DeploySnapshot,
        container: &ContainerObservation,
        cancellation: &CancellationToken,
    ) -> Result<Moving, MoveFailure> {
        move_one(self, snapshot, container, cancellation).await
    }

    async fn take_over(
        &mut self,
        snapshot: &DeploySnapshot,
        old: &ContainerObservation,
        new: &ContainerObservation,
        cancellation: &CancellationToken,
    ) -> Result<Moving, MoveFailure> {
        let from = machine_ref(snapshot, &old.machine_id);
        let to = machine_ref(snapshot, &new.machine_id);
        let known = known(snapshot);
        let old = Copy::observed(old);
        let relocated = confirm_promoted(
            &*self,
            &old,
            &Copy::observed(new),
            &|id| named(&known, id),
            cancellation,
        )
        .await;
        Ok(moving(
            self,
            (from, to),
            old,
            known,
            relocated,
            cancellation,
        ))
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
            detail: error_chain::row(error),
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
                failure: MoveFailure::Refused { from: None, reason },
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
                failure: MoveFailure::Refused { from: None, reason },
            };
        }
        let moved = match promoted(&fresh, service, container) {
            Some(new) => client.take_over(&fresh, container, new, cancellation).await,
            None => client.move_one(&fresh, container, cancellation).await,
        };
        match moved {
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
    let spec = newest(snapshot, service)?;
    if in_rotation(snapshot, service).any(|container| {
        container.resolved_spec.serving_shape() != spec.serving_shape()
            && !left_behind(snapshot, service, spec, container)
    }) {
        return Some(StayReason::MidRollout);
    }
    let first = match stranded.first() {
        Some(first) => *first,
        None => self::stranded(snapshot, service)
            .into_iter()
            .find(|container| left_behind(snapshot, service, spec, container))?,
    };
    // The snapshot carries no storage evidence: `stays` pins every storage-dependent spec
    // first. Observe storage once Volumes can move.
    if let Some(reason) = stays(spec, &machine_ref(snapshot, &first.machine_id)) {
        return Some(reason);
    }
    // A copy an earlier move already replaced needs no Server.
    let placed = if stranded.is_empty() {
        first
    } else {
        *stranded
            .iter()
            .find(|container| promoted(snapshot, service, container).is_none())?
    };
    let moved = kept_in_region(spec, machine(snapshot, &placed.machine_id));
    // Whether the copy carries a Free receipt is the Config Store's to say, which this
    // plan doesn't read; the move asks before it picks a Server.
    destination(spec, &moved, &service.namespace, snapshot, false).err()
}

/// The copy an earlier drain's move of `old` brought In on a Server that can keep it, if
/// one serves in its place: made under that move's key, `move:{old}` from a release before
/// minted keys or `move:{old}:{nonce}`, of `old`'s Serving Shape, running and healthy. A
/// rerun then creates nothing for `old`, so it never starts a second copy beside that one.
fn promoted<'a>(
    snapshot: &'a DeploySnapshot,
    service: &'a QualifiedService,
    old: &ContainerObservation,
) -> Option<&'a ContainerObservation> {
    let stranded = stranded(snapshot, service);
    in_rotation(snapshot, service).find(|copy| {
        *copy.rotation() == CopyRotation::In
            && copy.machine_id != old.machine_id
            && copy
                .creation_key()
                .is_some_and(|key| CreationKey::made_by_move(key, &old.container_id))
            && copy.resolved_spec.serving_shape() == old.resolved_spec.serving_shape()
            && serves_healthy(&copy.runtime)
            && !stranded
                .iter()
                .any(|other| other.container_id == copy.container_id)
    })
}

/// Whether a copy runs and passes its health check, or has none. A rerun lets only such
/// a copy stand in for the one it retires.
fn serves_healthy(runtime: &ContainerRuntimeObservation) -> bool {
    matches!(
        runtime,
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy | HealthObservation::NotConfigured,
        }
    )
}

/// The Server for one more Container of `spec`. A spec whose image is a tag moves only
/// to a Server that copies its image by ID, so a Server whose row lacks the pinned route
/// is left out; when only such Servers fit, they need upgrading. A copy that carries a
/// Free receipt (`free`) moves only to a Server that takes one, by the same rule.
fn destination(
    spec: &ResolvedServiceSpec,
    requested: &RequestedServiceSpec,
    namespace: &Namespace,
    snapshot: &DeploySnapshot,
    free: bool,
) -> Result<MachineId, StayReason> {
    let options = PlanOptions {
        free: if free {
            BTreeSet::from([requested.name.clone()])
        } else {
            BTreeSet::new()
        },
        ..PlanOptions::default()
    };
    let no_destination = |error: PlanError| {
        if let PlanError::FreeReceiptsUntaken { names, .. } = &error {
            return StayReason::needs_upgrade(
                names
                    .iter()
                    .filter_map(|name| snapshot.machine_named(name))
                    .map(|observed| MachineRef::from(&observed.machine))
                    .collect(),
            );
        }
        StayReason::NoDestination {
            detail: error_chain::row(&error),
            upgrade: Vec::new(),
        }
    };
    if content_addressed(&spec.container.image) {
        return place_one(requested, namespace, snapshot, &options).map_err(no_destination);
    }
    let lacks = |observed: &ployz_core::MachineObservation| {
        observed
            .machine
            .runtime
            .claim(ENSURE_PINNED_CONTAINER_CAPABILITY)
            == Claim::Lacks
    };
    let mut pinned = snapshot.clone();
    pinned.machines.retain(|observed| !lacks(observed));
    let error = match place_one(requested, namespace, &pinned, &options) {
        Ok(id) => return Ok(id),
        Err(error) => error,
    };
    // Every Server that would fit, each one dropped once it is named.
    let mut rest = snapshot.clone();
    let mut servers = Vec::new();
    while let Ok(id) = place_one(requested, namespace, &rest, &options) {
        servers.push(machine_ref(snapshot, &id));
        rest.machines.retain(|observed| observed.machine.id != id);
    }
    if servers.is_empty() {
        Err(no_destination(error))
    } else {
        Err(StayReason::needs_upgrade(servers))
    }
}

/// Whether converging `service` on `snapshot` could start a copy on another Server: some
/// Server is among the ones a move of it may pick. That copy needs a signed version whatever
/// it reaches, since a move copies its image in under pull policy never, and a Server runs
/// an unsigned spec only from an image it pulled itself. A copy that only waits Out, is
/// held, or has nowhere to go starts nothing.
pub(crate) fn needs_version(snapshot: &DeploySnapshot, service: &QualifiedService) -> bool {
    let stranded = movable(snapshot, service);
    if refusal(snapshot, service, &stranded).is_some() {
        return false;
    }
    stranded.iter().any(|container| {
        destinations(
            &kept_in_region(
                &container.resolved_spec,
                machine(snapshot, &container.machine_id),
            ),
            &container.namespace,
            snapshot,
            &PlanOptions::default(),
        )
        .is_ok_and(|machines| !machines.is_empty())
    })
}

fn kept_in_region(spec: &ResolvedServiceSpec, source: &Machine) -> RequestedServiceSpec {
    let requested = spec.to_requested();
    match Region::of(source).filter(|region| requested.regions.contains_key(region)) {
        Some(region) => requested.held_to(&region),
        None => requested,
    }
}

fn in_rotation<'a>(
    snapshot: &'a DeploySnapshot,
    service: &'a QualifiedService,
) -> impl Iterator<Item = &'a ContainerObservation> {
    snapshot.containers.iter().filter(|container| {
        is_service_container(container, service) && *container.rotation() != CopyRotation::Out
    })
}

/// The spec the Service's last Deploy gave it, read off its newest copy.
fn newest<'a>(
    snapshot: &'a DeploySnapshot,
    service: &'a QualifiedService,
) -> Option<&'a ResolvedServiceSpec> {
    in_rotation(snapshot, service)
        .min_by_key(|container| Reverse(container.copy_gen()))
        .map(|container| &container.resolved_spec)
}

/// Whether the last Deploy left `copy` as it was: it runs an older Serving Shape in a region
/// of the Service's counts where no Server can run the Service now. No Deploy replaces it
/// and no move has a destination until one can, so it holds nothing else.
fn left_behind(
    snapshot: &DeploySnapshot,
    service: &QualifiedService,
    newest: &ResolvedServiceSpec,
    copy: &ContainerObservation,
) -> bool {
    if copy.resolved_spec.serving_shape() == newest.serving_shape() {
        return false;
    }
    let requested = newest.to_requested();
    Region::of(machine(snapshot, &copy.machine_id))
        .filter(|region| requested.regions.contains_key(region))
        .is_some_and(|region| region_waits(&requested, &region, &service.namespace, snapshot))
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
        && is_active_runtime(&container.runtime)
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
    let newest = newest(snapshot, service);
    let mut movable = stranded(snapshot, service);
    movable.retain(|container| {
        !matches!(
            container.rotation(),
            CopyRotation::Held(_) | CopyRotation::Out
        ) && !newest.is_some_and(|spec| left_behind(snapshot, service, spec, container))
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

#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Relocated {
    Listed,
    Unlisted(Unconfirmed),
}

pub(crate) async fn relocate<C: MachineOperations>(
    client: &C,
    namespace: &Namespace,
    (spec, image): (&ResolvedServiceSpec, &ImageDigest),
    to: &MachineId,
    old: &Copy,
    name: &(dyn Fn(&MachineId) -> MachineRef + Sync),
    cancellation: &CancellationToken,
) -> Result<Relocated, MoveError> {
    let started = move_container(
        client,
        namespace,
        spec,
        to,
        (&old.machine_id, &old.container_id),
        image,
        cancellation,
    )
    .await?;
    let new = Copy {
        machine_id: *to,
        service: old.service.clone(),
        container_id: started.container_id,
        rotation: CopyRotation::Pending,
    };
    let not_in = match cleanup::bring_in(client, &new, spec, started.deploy.clone())
        .await
        .map(cleanup::BroughtIn::not_in)
    {
        Ok(Ok(())) => None,
        Ok(Err(NotIn::OlderThanIn(refusal) | NotIn::Refused(refusal))) => Some(refusal.message),
        Ok(Err(NotIn::Health(health))) => Some(error_chain::row(&health_error(
            new.container_id,
            health,
            spec,
        ))),
        Err(error) => Some(error_chain::row(&error)),
    };
    if let Some(detail) = not_in {
        return Err(MoveError {
            end: MoveEnd::NotServing { detail },
            replacement_removed: started.discard(client, cancellation).await,
        });
    }
    Ok(listed(client, &new, name, cancellation).await)
}

/// Confirm `new`, the copy an earlier move of `old` brought In, in `old`'s place: it runs
/// `old`'s image, is still In and healthy, and every proxy lists it. Nothing is created
/// or removed. Whatever can't be confirmed leaves `old` serving beside it.
pub(crate) async fn confirm_promoted<C: MachineOperations>(
    client: &C,
    old: &Copy,
    new: &Copy,
    name: &(dyn Fn(&MachineId) -> MachineRef + Sync),
    cancellation: &CancellationToken,
) -> Relocated {
    let unconfirmed = |detail| Relocated::Unlisted(Unconfirmed::Failed { detail });
    let (source, copy) = match (
        client
            .inspect_container(&old.machine_id, &old.container_id)
            .await,
        client
            .inspect_container(&new.machine_id, &new.container_id)
            .await,
    ) {
        (Ok(source), Ok(copy)) => (source, copy),
        (Err(error), _) | (_, Err(error)) => return unconfirmed(error_chain::row(&error)),
    };
    let server = name(&new.machine_id);
    if source.image_id.is_none() || copy.image_id != source.image_id {
        return unconfirmed(format!(
            "Container {} came In for it on {server}, but runs another image",
            new.container_id
        ));
    }
    if !serves_healthy(&copy.container.runtime) {
        return unconfirmed(format!(
            "Container {} came In for it on {server}, but isn't healthy",
            new.container_id
        ));
    }
    if *copy.container.rotation() != CopyRotation::In {
        return unconfirmed(format!(
            "Container {} that came In for it on {server} is no longer In",
            new.container_id
        ));
    }
    listed(client, new, name, cancellation).await
}

/// Whether every live proxy lists `new`, which is In.
async fn listed<C: MachineOperations>(
    client: &C,
    new: &Copy,
    name: &(dyn Fn(&MachineId) -> MachineRef + Sync),
    cancellation: &CancellationToken,
) -> Relocated {
    let listed = cleanup::wait_listed(
        client,
        &new.machine_id,
        &new.service,
        &BTreeSet::from([new.container_id]),
        cancellation,
        |_| {},
    )
    .await;
    let unconfirmed = match listed {
        Ok(waited) => waited
            .failure()
            .err()
            .map(|failure| Unconfirmed::of(failure, name)),
        Err(error) => Some(Unconfirmed::Failed {
            detail: error_chain::row(&error),
        }),
    };
    if let Some(unconfirmed) = unconfirmed {
        return Relocated::Unlisted(unconfirmed);
    }
    // The listed wait returns Done when every proxy already lists the copy, even under a
    // cancel that arrived meanwhile. Between In and Out, Ctrl-C leaves both copies In.
    if cancellation.is_cancelled() {
        return Relocated::Unlisted(Unconfirmed::Cancelled);
    }
    Relocated::Listed
}

pub(crate) async fn retire_old<C: MachineOperations>(
    client: &C,
    old: &Copy,
    name: &(dyn Fn(&MachineId) -> MachineRef + Sync),
    cancellation: &CancellationToken,
) -> Result<OldCopyEnd, LastIn> {
    let retired = cleanup::retire(
        client,
        old,
        EmptyPolicy::RequireOtherIn,
        AnonymousVolumes::keep(),
        cancellation,
    )
    .await;
    let unconfirmed = match retired.map(cleanup::Rotated::failure) {
        Ok(Ok(())) => return Ok(OldCopyEnd::Stopped),
        Ok(Err(RotationFailure::Refused(refusal)))
            if refusal.reason == ContainerRefusalReason::LastIn =>
        {
            return Err(LastIn);
        }
        Ok(Err(failure)) => Unconfirmed::of(failure, name),
        Err(error) => Unconfirmed::Failed {
            detail: error_chain::row(&error),
        },
    };
    Ok(OldCopyEnd::Running { unconfirmed })
}

async fn move_one(
    client: &Approved,
    snapshot: &DeploySnapshot,
    container: &ContainerObservation,
    cancellation: &CancellationToken,
) -> Result<Moving, MoveFailure> {
    let source = machine(snapshot, &container.machine_id);
    let from = MachineRef::from(source);
    let spec = &container.resolved_spec;
    let free = client.carries_free(&container.namespace, spec).await;
    let dest = destination(
        spec,
        &kept_in_region(spec, source),
        &container.namespace,
        snapshot,
        free,
    )
    .map_err(|reason| match reason {
        StayReason::NoDestination { detail, upgrade } if upgrade.is_empty() => {
            MoveFailure::NoDestination {
                from: from.clone(),
                detail,
            }
        }
        reason @ (StayReason::Unobserved { .. }
        | StayReason::MidRollout
        | StayReason::Global
        | StayReason::BindMount { .. }
        | StayReason::Volume { .. }
        | StayReason::NoDestination { .. }
        | StayReason::LastIn
        | StayReason::Unrecognized) => MoveFailure::Refused {
            from: Some(from.clone()),
            reason,
        },
    })?;
    let dest = machine(snapshot, &dest);
    let to = MachineRef::from(dest);
    let image_id = image_id(&client.client, &container.container_id, &from, &to).await?;
    let image = ImageDigest::parse(&image_id).map_err(|error| MoveFailure::ReadImage {
        from: from.clone(),
        to: to.clone(),
        detail: error_chain::row(&error),
    })?;
    crate::image::copy_running_image(
        &client.client,
        source,
        dest,
        &spec.container.image,
        &image_id,
        client.client.image_naming(&dest.id),
    )
    .await
    .map_err(|error| MoveFailure::CopyImage {
        from: from.clone(),
        to: to.clone(),
        detail: error_chain::row(&error),
    })?;
    let old = Copy::observed(container);
    let known = known(snapshot);
    let relocated = relocate(
        client,
        &container.namespace,
        (spec, &image),
        &dest.id,
        &old,
        &|id| named(&known, id),
        cancellation,
    )
    .await;
    match relocated {
        Ok(relocated) => Ok(moving(
            client,
            (from, to),
            old,
            known,
            relocated,
            cancellation,
        )),
        Err(error) => Err(MoveFailure::of(error, from, to)),
    }
}

/// Every Server of `snapshot`, as a move names it.
fn known(snapshot: &DeploySnapshot) -> Vec<MachineRef> {
    snapshot
        .machines
        .iter()
        .map(|machine| MachineRef::from(&machine.machine))
        .collect()
}

/// A move whose replacement is In: `old` retires once every proxy listed the replacement,
/// and otherwise keeps serving beside it.
fn moving(
    client: &Approved,
    (from, to): (MachineRef, MachineRef),
    old: Copy,
    known: Vec<MachineRef>,
    relocated: Relocated,
    cancellation: &CancellationToken,
) -> Moving {
    let old = match relocated {
        Relocated::Listed => {
            let client = client.clone();
            let cancellation = cancellation.clone();
            async move { retire_old(&client, &old, &|id| named(&known, id), &cancellation).await }
                .boxed()
        }
        Relocated::Unlisted(unconfirmed) => ready(Ok(OldCopyEnd::In { unconfirmed })).boxed(),
    };
    Moving { from, to, old }
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
        .call_shared::<op::InspectContainer>(
            InspectContainerRequest {
                container_id: *container,
            },
            Call::once()
                .to(&MachineTarget::from(&from.id))
                .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
        )
        .await
        .map_err(crate::connect::rpc_error)
        .map_err(|error| MoveFailure::ReadImage {
            from: from.clone(),
            to: to.clone(),
            detail: error_chain::row(&error),
        })?
        .image_id
        .ok_or_else(|| MoveFailure::SourceTooOld { from: from.clone() })
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerId, ContainerKind, ContainerObservation, ContainerObservationParts,
        ContainerRuntimeObservation, CopyRotation, DeploymentLogId, Gen, HealthObservation,
        Machine, MachineId, MachineName, MachineObservation, MembershipObservation, Namespace,
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
        /// Each old copy retired behind a copy an earlier move brought In, with that copy.
        taken_over: Vec<(ContainerId, ContainerId)>,
    }

    impl Scripted {
        fn new(snapshots: impl IntoIterator<Item = Result<DeploySnapshot, ConnectError>>) -> Self {
            Self {
                snapshots: snapshots.into_iter().collect(),
                taken_over: Vec::new(),
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

        async fn take_over(
            &mut self,
            snapshot: &DeploySnapshot,
            old: &ContainerObservation,
            new: &ContainerObservation,
            _cancellation: &CancellationToken,
        ) -> Result<Move, MoveFailure> {
            self.taken_over.push((old.container_id, new.container_id));
            Ok(Move {
                from: super::machine_ref(snapshot, &old.machine_id),
                to: super::machine_ref(snapshot, &new.machine_id),
                old: OldCopyEnd::Stopped,
            })
        }
    }

    /// A rerun after a drain whose move brought its copy In on the only Server left, then
    /// stopped before retiring the old copy. That Server has no bridge endpoint free, which
    /// the copy holds, yet the old copy retires behind it and nothing is started.
    #[tokio::test]
    async fn a_rerun_retires_the_old_copy_behind_the_copy_an_earlier_move_brought_in() {
        let service = QualifiedService::parse("app/api").unwrap();
        let in_with_key = |hex, server, key: Option<String>| {
            let parts = ContainerObservationParts::from(container(hex, server, stateless()));
            let mut labels = parts.labels.clone();
            if let Some(key) = key {
                labels.insert(ployz_core::labels::CREATION_KEY.into(), key);
            }
            ContainerObservation::try_from(ContainerObservationParts {
                rotation: Some(CopyRotation::In),
                labels,
                ..parts
            })
            .unwrap()
        };
        for key in [
            format!("move:{}", id('1')),
            format!("move:{}:0f3a", id('1')),
        ] {
            let snapshot = DeploySnapshot {
                machines: vec![machine('a', false), machine('b', true)],
                containers: vec![
                    in_with_key('1', 'a', None),
                    in_with_key('2', 'b', Some(key)),
                ],
                capacity: Some(
                    [(
                        machine('b', true).machine.id,
                        ployz_core::BridgeEndpointCapacity::new(1, 1),
                    )]
                    .into(),
                ),
                ..DeploySnapshot::default()
            };
            let mut client = Scripted::new([Ok(snapshot)]);
            let converged = converge(&mut client, &service, &CancellationToken::new()).await;
            assert_eq!(converged, Converged::Moved { moves: one_move() });
            assert_eq!(client.taken_over, [(id('1'), id('2'))]);
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

    #[test]
    fn a_replica_counted_by_region_moves_only_to_a_server_in_its_region() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut by_region = stateless();
        by_region.regions = serde_json::from_value(json!({ "fsn": 1, "syd": 1 })).unwrap();
        let refused = |recorded: &ResolvedServiceSpec, free_server_region| {
            let snapshot = DeploySnapshot {
                machines: vec![
                    in_region('a', false, "syd"),
                    in_region('b', true, free_server_region),
                ],
                containers: vec![container('1', 'a', recorded.clone())],
                ..DeploySnapshot::default()
            };
            refusal(&snapshot, &service, &super::stranded(&snapshot, &service))
                .map(|reason| reason.to_string())
        };

        assert_eq!(refused(&by_region, "syd"), None);
        assert!(
            refused(&by_region, "fsn")
                .is_some_and(|reason| reason.starts_with("no eligible Server")),
            "{:?}",
            refused(&by_region, "fsn")
        );
        assert_eq!(refused(&stateless(), "fsn"), None);
    }

    #[tokio::test]
    async fn an_old_copy_no_deploy_can_reach_does_not_hold_the_others_moves() {
        let service = QualifiedService::parse("app/api").unwrap();
        let mut newer = stateless();
        newer.regions = serde_json::from_value(json!({ "fsn": 2, "syd": 1 })).unwrap();
        let mut older = newer.clone();
        older.container.image = "alpine:3.22.0".into();
        let deployed = |id, server, spec: &ResolvedServiceSpec, deploy| {
            let parts = ContainerObservationParts::from(container(id, server, spec.clone()));
            ContainerObservation::try_from(ContainerObservationParts {
                r#gen: Some(Gen::new(deploy)),
                ..parts
            })
            .unwrap()
        };
        let snapshot = DeploySnapshot {
            machines: vec![
                in_region('a', false, "syd"),
                in_region('b', false, "fsn"),
                in_region('c', true, "fsn"),
            ],
            containers: vec![
                deployed('1', 'a', &older, 1),
                deployed('2', 'b', &newer, 2),
                deployed('3', 'c', &newer, 2),
            ],
            ..DeploySnapshot::default()
        };

        let mut client = Scripted::new([Ok(snapshot.clone())]);
        let converged = converge(&mut client, &service, &CancellationToken::new()).await;
        let Converged::Moved { moves } = &converged else {
            panic!("{converged:?}");
        };
        assert_eq!(
            moves.iter().map(|moved| &moved.from).collect::<Vec<_>>(),
            [&MachineRef::from(&machine('b', false).machine)]
        );

        let mut alone = snapshot.clone();
        alone
            .machines
            .retain(|server| server.machine.id != machine('b', false).machine.id);
        alone.containers.retain(|copy| copy.container_id != id('2'));
        assert!(
            matches!(
                refusal(&alone, &service, &movable(&alone, &service)),
                Some(super::StayReason::NoDestination { .. })
            ),
            "a Drain of its own Server says why nothing takes it"
        );

        let mut reachable = snapshot;
        reachable.machines.push(in_region('d', true, "syd"));
        assert_eq!(
            refusal(&reachable, &service, &movable(&reachable, &service)),
            Some(super::StayReason::MidRollout),
            "a Server in syd that can run it lets a Deploy replace the old copy"
        );
    }

    fn in_region(hex: char, accepts_services: bool, region: &str) -> MachineObservation {
        let mut server = machine(hex, accepts_services);
        server
            .machine
            .labels
            .insert("region".parse().unwrap(), region.parse().unwrap());
        server
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
                join_approval: None,
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
            log_driver: None,
            admission: None,
            replaced_by: None,
        })
        .unwrap()
    }
}

#[cfg(test)]
#[path = "placement_tests.rs"]
mod destination_tests;
