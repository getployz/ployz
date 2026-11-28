//! Drain: turn a Machine's services role off, run Placement convergence for each chosen
//! replicated Service with a Container on it, then observe what still runs there. One
//! bounded command. A user Service on the Server with a Global copy anywhere, chosen or not,
//! refuses the Drain in its preflight: a Drain retires no Global, and lets go of no Global's
//! copy.
//!
//! Each move's old copy retires alongside the moves after it; the report waits for every
//! retirement and still lists the Services in the order they were handled.
//!
//! A preflight that may refuse comes first; nothing has moved when it does. After it the
//! Drain cannot fail: `execute` returns a [`DrainReport`], not a `Result`, so whatever moved
//! is always in the report.
//!
//! Callers: `ployz server drain` (streams a line per step through `progress`) and
//! [`crate::sdk::Session::drain_machine`] (Cloud stores the final report).

use ployz_core::error_chain;
use std::collections::VecDeque;
use std::pin::pin;

use futures_util::future::BoxFuture;
use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, StreamExt};
use ployz_core::{
    ContainerKind, CopyRotation, EnvironmentValues, LiveServices, Machine, MachineId, MachineName,
    MachineTarget, MachineUpdate, Namespace, QualifiedService, RpcError, RpcErrorCode, ServiceMode,
    UpdateMachineRequest, op,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::cluster::{Call, Deadline, RoleSetting, RoleWaitError, visible_machine, wait_for_role};
use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};
use crate::deploy::cleanup::Copy;
use crate::deploy::{Approvals, Approved, CopyLifecycle, DeploySnapshot};

pub(crate) mod placement;
pub(crate) mod retirement;

use placement::{Converged, LastIn, Moving, can_move, converge, named};
use retirement::let_go;

pub use placement::{
    DrainStop, MachineRef, Move, MoveFailure, OldCopyEnd, StayReason, Unconfirmed,
};

/// Which user Services a Drain acts on. Those of reserved Namespaces never are.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum DrainScope {
    /// Every user Namespace: a Standalone Cluster records no ownership.
    EveryNamespace,
    /// Only these Namespaces, which some Project owns. Empty selects nothing. Containers of
    /// other Namespaces stay where they are and are not reported.
    Owned { namespaces: Vec<Namespace> },
    /// Only these Services. Containers of every other Service stay where they are and are
    /// not reported.
    Services { services: Vec<QualifiedService> },
}

impl DrainScope {
    fn includes(&self, service: &QualifiedService) -> bool {
        !service.namespace.is_system()
            && match self {
                Self::EveryNamespace => true,
                Self::Owned { namespaces } => namespaces.contains(&service.namespace),
                Self::Services { services } => services.contains(service),
            }
    }
}

/// One step of a Drain as it lands, for a caller that shows progress. The report repeats
/// all of it.
#[derive(Clone, Copy, Debug)]
pub enum DrainStep<'a> {
    /// The services role is off: this Drain turned it off, or found it off.
    ServicesOff {
        server: &'a Machine,
        role: ServicesRole,
    },
    /// One Service is done with, in report order.
    Service {
        server: &'a Machine,
        service: &'a ServiceDrain,
    },
}

/// What one Drain observed and did, in order.
///
/// `services` lists each chosen Service that had a Container on `server` when the Drain
/// looked, in the order handled. When `stopped` is set, the Service it stopped at reads
/// `interrupted` or `not_attempted`, and every one after it reads `not_attempted`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(into = "DrainReportWire")]
#[ts(as = "DrainReportWire")]
pub struct DrainReport {
    /// The drained Server's record as selected, before this Drain changed its role.
    pub server: Machine,
    /// Whether this Drain turned the services role off or found it off.
    pub services_role: ServicesRole,
    /// Each chosen Service and what the Drain did with it.
    pub services: Vec<ServiceDrain>,
    /// Why the Drain ended before handling every Service.
    pub stopped: Option<DrainStop>,
    /// What still runs on the Server after the Drain.
    pub remaining: Remaining,
}

impl DrainReport {
    /// Every chosen Service left the Server, every moved copy's old copy stopped, every
    /// copy left Out there is gone, the Drain ran to its end, and what remains was
    /// observed. Another Server's move that failed and left nothing behind is reported,
    /// but is that Server's to finish.
    #[must_use]
    pub fn complete(&self) -> bool {
        let Remaining::Observed { services, .. } = &self.remaining else {
            return false;
        };
        let done = |outcome: &DrainOutcome| {
            if let DrainOutcome::Failed { moves, failure } = outcome
                && failure.clean_elsewhere(&self.server.id)
            {
                return moves.iter().all(|step| step.old == OldCopyEnd::Stopped);
            }
            outcome.complete()
        };
        self.stopped.is_none()
            && self
                .services
                .iter()
                .all(|service| done(&service.outcome) && !services.contains(&service.service))
    }
}

/// A [`DrainReport`] as it is sent, with [`DrainReport::complete`] worked out from it.
#[derive(Serialize, TS)]
#[ts(rename = "DrainReport")]
struct DrainReportWire {
    /// The drained Server's record as selected, before this Drain changed its role.
    server: Machine,
    /// Whether this Drain turned the services role off or found it off.
    services_role: ServicesRole,
    /// Each chosen Service and what the Drain did with it.
    services: Vec<ServiceDrain>,
    /// Why the Drain ended before handling every Service.
    stopped: Option<DrainStop>,
    /// What still runs on the Server after the Drain.
    remaining: Remaining,
    /// Every chosen Service left the Server, every old copy stopped, the Drain ran to its
    /// end, and what remains was observed.
    complete: bool,
}

impl From<DrainReport> for DrainReportWire {
    fn from(report: DrainReport) -> Self {
        let complete = report.complete();
        let DrainReport {
            server,
            services_role,
            services,
            stopped,
            remaining,
        } = report;
        Self {
            server,
            services_role,
            services,
            stopped,
            remaining,
            complete,
        }
    }
}

/// The drained Server's services role, which the Drain turns off first.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ServicesRole {
    /// This Drain turned it off.
    TurnedOff,
    /// It was already off.
    AlreadyOff,
}

/// One chosen Service and what the Drain did with it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ServiceDrain {
    /// The Service, by Namespace and name.
    pub service: QualifiedService,
    #[serde(flatten)]
    #[ts(flatten)]
    pub outcome: DrainOutcome,
    /// The Managed volume Cloud moves, or tried to move, with this Service, by name. The
    /// Engine never sets it; Cloud adds it to the report it keeps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub volume: Option<String>,
    /// Cloud's line for that Volume as last read: where it went, or why it stays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub detail: Option<String>,
    /// The Volume Run Cloud admitted for that Volume, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub run: Option<String>,
}

impl ServiceDrain {
    /// The Engine's own entry, before Cloud says anything about a Volume.
    #[must_use]
    pub fn of(service: QualifiedService, outcome: DrainOutcome) -> Self {
        Self {
            service,
            outcome,
            volume: None,
            detail: None,
            run: None,
        }
    }
}

/// One Service's outcome, flat on `result`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum DrainOutcome {
    /// Replicated: every Container that had to move did. Never empty.
    Moved { moves: Vec<Move> },
    /// Replicated: none of its active Containers had to move when it was handled.
    NothingToMove,
    /// Replicated: `moves` were made, then `failure` stopped the rest.
    Failed {
        moves: Vec<Move>,
        failure: MoveFailure,
    },
    /// Replicated: nothing moved, and why.
    Stays { reason: StayReason },
    /// Replicated: a copy the user stopped stays on the Server until they start or remove
    /// it. The Drain never moves one.
    Held,
    /// Replicated: the copies an earlier move left Out on the Server, each waited unlisted
    /// by every live proxy and then removed. Never empty.
    OldCopies { ends: Vec<OldCopyEnd> },
    /// Global: its removal from the Server was acknowledged, or none of it was left there.
    /// Only a Drain from before user Globals refused it made one; Cloud keeps their reports.
    Retired,
    /// Global: it may still run on the Server, and why. Only made as `Retired` is.
    NotRetired { error: String },
    /// Replicated: the Drain stopped while handling it, after `moves`. The report's
    /// `stopped` says why.
    Interrupted { moves: Vec<Move> },
    /// The Drain stopped before reaching it.
    NotAttempted,
}

impl DrainOutcome {
    /// Whether a move's old copy, or one an earlier move left Out, is still on the Server.
    #[must_use]
    pub fn left_old_copy(&self) -> bool {
        match self {
            Self::Moved { moves } => moves.iter().any(|step| step.old != OldCopyEnd::Stopped),
            Self::OldCopies { ends } => ends.iter().any(|end| *end != OldCopyEnd::Stopped),
            Self::NothingToMove
            | Self::Failed { .. }
            | Self::Stays { .. }
            | Self::Held
            | Self::Retired
            | Self::NotRetired { .. }
            | Self::Interrupted { .. }
            | Self::NotAttempted => false,
        }
    }

    /// Whether the Drain went wrong on this Service: a Global may still run, or a move
    /// failed or was interrupted.
    #[must_use]
    pub fn failed(&self) -> bool {
        matches!(
            self,
            Self::NotRetired { .. } | Self::Failed { .. } | Self::Interrupted { .. }
        )
    }

    fn complete(&self) -> bool {
        match self {
            Self::Moved { .. } | Self::OldCopies { .. } => !self.left_old_copy(),
            Self::NothingToMove | Self::Retired => true,
            Self::Failed { .. }
            | Self::Stays { .. }
            | Self::Held
            | Self::NotRetired { .. }
            | Self::Interrupted { .. }
            | Self::NotAttempted => false,
        }
    }
}

/// What runs on the Server once the Drain finished.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Remaining {
    /// A fresh observation of the Server answered.
    Observed {
        /// Every Service still with a Container there, reserved and unchosen Namespaces
        /// included.
        services: Vec<QualifiedService>,
        /// The user Services of `services` that the Drain's scope left alone.
        unchosen: Vec<QualifiedService>,
    },
    /// The Server could not be observed, and why.
    Unobserved { error: String },
}

/// Why a Drain stopped before anything moved. A Drain that had turned the services role
/// off says so in every error after that, by its type.
#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    /// Nothing changed.
    #[error(transparent)]
    Unchanged(Stop),
    /// This Drain turned `server`'s services role off, then stopped before anything moved.
    /// The role stays off until someone turns it back on. The role is idempotent; a rerun
    /// finds it off.
    #[error("{}", left_off(.server, .stop))]
    LeftOff { server: MachineName, stop: Stop },
    /// Turning `server`'s services role off timed out or was cancelled, so the daemon may
    /// have turned it off anyway.
    #[error("{}", maybe_off(.server, .stop))]
    MaybeOff { server: MachineName, stop: Stop },
}

/// What stopped a Drain before anything moved.
#[derive(Debug, thiserror::Error)]
pub enum Stop {
    /// The target names no visible Server, or more than one, a user Global runs on it, or
    /// turning its services role off failed.
    #[error(transparent)]
    Refused(RpcError),
    /// The entry did not observe the services role off within 30 s.
    #[error("This entry Server has not yet observed the new services role.")]
    RoleNotObserved,
    /// The Server's Containers could not be listed.
    #[error("Cannot observe Services on Server {server}: {detail}")]
    Unobservable { server: MachineName, detail: String },
    /// Cancellation came before anything moved.
    #[error("Cancelled before anything moved.")]
    Cancelled,
    /// The entry could not be reached.
    #[error(transparent)]
    Connect(#[from] ConnectError),
}

impl From<ConnectError> for DrainError {
    fn from(error: ConnectError) -> Self {
        Self::Unchanged(error.into())
    }
}

/// What a Drain that turned `server`'s services off says when `stop` ended it.
fn left_off(server: &MachineName, stop: &Stop) -> String {
    let stopped = stop.to_string();
    let end = if stopped.ends_with('.') { "" } else { "." };
    format!(
        "{stopped}{end} Services are now off on {server}: turn them back on to let it run \
         services again."
    )
}

/// What a Drain says when `stop` ended its turning `server`'s services off without an
/// answer.
fn maybe_off(server: &MachineName, stop: &Stop) -> String {
    let stopped = stop.to_string();
    let end = if stopped.ends_with('.') { "" } else { "." };
    format!(
        "{stopped}{end} Services may now be off on {server}: if they are, turn them back on \
         to let it run services again."
    )
}

impl From<Stop> for RpcError {
    fn from(stop: Stop) -> Self {
        let coded = |code, stop: &Stop| Self::new(code, stop.to_string());
        match stop {
            Stop::Refused(error) => error,
            Stop::Connect(error) => error.into(),
            stop @ (Stop::RoleNotObserved | Stop::Unobservable { .. } | Stop::Cancelled) => {
                coded(RpcErrorCode::Unavailable, &stop)
            }
        }
    }
}

impl From<DrainError> for RpcError {
    fn from(error: DrainError) -> Self {
        match error {
            DrainError::Unchanged(stop) => stop.into(),
            DrainError::LeftOff { server, stop } => {
                let message = left_off(&server, &stop);
                Self::from(stop).with_message(message)
            }
            DrainError::MaybeOff { server, stop } => {
                let message = maybe_off(&server, &stop);
                Self::from(stop).with_message(message)
            }
        }
    }
}

impl Client {
    /// Drain `target` within `scope`, handing each Server `approvals` with each copy it
    /// moves there.
    ///
    /// Cancelling `cancellation` ends the Drain at its next safe point: a move in flight
    /// removes its new Container again, the Services not reached read `not_attempted`,
    /// and the report still comes back. `progress` sees each step as it lands.
    ///
    /// # Errors
    ///
    /// Only before anything moved: `target` is not a visible Server, the services role
    /// can't be turned off or isn't observed off by the entry within 30 s, the Server's
    /// Services can't be observed, or `cancellation` fired first.
    pub(crate) async fn drain(
        &mut self,
        target: &MachineTarget,
        scope: &DrainScope,
        approvals: Approvals,
        cancellation: &CancellationToken,
        progress: &mut (dyn FnMut(DrainStep<'_>) + Send),
    ) -> Result<DrainReport, DrainError> {
        let ready = preflight(self, target, scope, cancellation, progress).await?;
        let mut approved = Approved::new(self.clone(), approvals);
        Ok(execute(&mut approved, ready, cancellation, progress).await)
    }
}

/// The Services a Drain of `target` within `scope` could move onto another Server, where
/// each new copy needs a signed version. Reads only, so a caller can refuse before the Drain
/// changes anything.
///
/// # Errors
///
/// `target` is not a visible Server, or its Containers can't be listed.
pub(crate) async fn needing_versions(
    client: &mut Client,
    target: &MachineTarget,
    scope: &DrainScope,
) -> Result<Vec<QualifiedService>, DrainError> {
    let machines = client.machines().await?;
    let server = visible_machine(target, &machines)
        .map_err(|error| DrainError::Unchanged(Stop::Refused(error)))?
        .machine
        .clone();
    let snapshot = client.deploy_snapshot(machines).await?;
    unsigned_moves(snapshot, &server.id, scope).map_err(|detail| {
        DrainError::Unchanged(Stop::Unobservable {
            server: server.name,
            detail,
        })
    })
}

/// [`needing_versions`] on `snapshot`, taken before the Drain turns `server`'s role off.
/// The detail when `server`'s own Containers went unlisted, since then nothing says what
/// would move.
fn unsigned_moves(
    mut snapshot: DeploySnapshot,
    server: &MachineId,
    scope: &DrainScope,
) -> Result<Vec<QualifiedService>, String> {
    if let Some(failure) = snapshot
        .container_failures
        .iter()
        .find(|failure| failure.machine_id == *server)
    {
        return Err(error_chain::row(&failure.error));
    }
    if snapshot.container_omissions.contains(server) {
        return Err("no terminal response".into());
    }
    // Another Server may answer by the time a move looks, and the move then goes ahead.
    snapshot.container_failures.clear();
    snapshot.container_omissions.clear();
    for observed in &mut snapshot.machines {
        if observed.machine.id == *server {
            observed.machine.accepts_services = false;
        }
    }
    let mut services: Vec<QualifiedService> = snapshot
        .containers
        .iter()
        .filter(|container| {
            container.machine_id == *server && container.kind == ContainerKind::ServiceContainer
        })
        .map(|container| QualifiedService {
            namespace: container.namespace.clone(),
            name: container.resolved_spec.name.clone(),
        })
        .filter(|service| scope.includes(service))
        .collect();
    services.sort();
    services.dedup();
    services.retain(|service| placement::needs_version(&snapshot, service));
    Ok(services)
}

/// Everything decided before the first move.
struct Ready {
    server: Machine,
    scope: DrainScope,
    services_role: ServicesRole,
    replicated: Vec<QualifiedService>,
}

/// Select, turn the role off, wait until the entry observes it, list the chosen Services.
/// Each listing must answer for the Server, and each refuses a user Service there with a
/// Global copy, so no Drain retires one. Once this Drain turned the role off, every error
/// says so.
async fn preflight(
    client: &mut Client,
    target: &MachineTarget,
    scope: &DrainScope,
    cancellation: &CancellationToken,
    progress: &mut (dyn FnMut(DrainStep<'_>) + Send),
) -> Result<Ready, DrainError> {
    let machines = client.machines().await?;
    let server = visible_machine(target, &machines)
        .map_err(|error| DrainError::Unchanged(Stop::Refused(error)))?
        .machine
        .clone();
    if cancellation.is_cancelled() {
        return Err(DrainError::Unchanged(Stop::Cancelled));
    }
    let live = client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await?;
    chosen_on(&server, &live, scope).map_err(DrainError::Unchanged)?;
    let services_role = if server.accepts_services {
        turn_services_off(client, &server.id, cancellation)
            .await
            .map_err(|unset| match unset {
                Unset::Refused(stop) => DrainError::Unchanged(stop),
                Unset::Unknown(stop) => DrainError::MaybeOff {
                    server: server.name.clone(),
                    stop,
                },
            })?;
        ServicesRole::TurnedOff
    } else {
        ServicesRole::AlreadyOff
    };
    let replicated = services_off(
        client,
        &server,
        scope,
        services_role,
        cancellation,
        progress,
    )
    .await
    .map_err(|stop| match services_role {
        ServicesRole::TurnedOff => DrainError::LeftOff {
            server: server.name.clone(),
            stop,
        },
        ServicesRole::AlreadyOff => DrainError::Unchanged(stop),
    })?;
    Ok(Ready {
        server,
        scope: scope.clone(),
        services_role,
        replicated,
    })
}

/// The preflight once `server`'s services role is off as `role` says: wait until the entry
/// observes a role this Drain turned off, then list the chosen Services again.
async fn services_off(
    client: &mut Client,
    server: &Machine,
    scope: &DrainScope,
    role: ServicesRole,
    cancellation: &CancellationToken,
    progress: &mut (dyn FnMut(DrainStep<'_>) + Send),
) -> Result<Vec<QualifiedService>, Stop> {
    if role == ServicesRole::TurnedOff {
        wait_for_role(client, &server.id, RoleSetting::ServicesOff, cancellation)
            .await
            .map_err(|error| match error {
                RoleWaitError::NotObserved(_) | RoleWaitError::ChangeNotObserved => {
                    Stop::RoleNotObserved
                }
                RoleWaitError::Cancelled => Stop::Cancelled,
                RoleWaitError::Connect(error) => Stop::Connect(error),
            })?;
    }
    progress(DrainStep::ServicesOff { server, role });
    let machines = client.machines().await?;
    let live = client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await?;
    chosen_on(server, &live, scope)
}

/// The Services of `scope` with a Container on `server`. Refuses when `server` did not
/// answer the listing, or when any user Service there has a Global copy anywhere, whether
/// `scope` chose it or not: a copy stays In while it crash-loops, so the copy on `server`
/// could be the last that serves, and one left Out there would be let go. One the scope
/// left out would still be left on a Server whose services are off.
fn chosen_on(
    server: &Machine,
    live: &LiveServices<RpcError>,
    scope: &DrainScope,
) -> Result<Vec<QualifiedService>, Stop> {
    observed(live, &server.id).map_err(|detail| Stop::Unobservable {
        server: server.name.clone(),
        detail,
    })?;
    let (globals, replicated): (Vec<_>, Vec<_>) = live
        .services()
        .into_iter()
        .filter(|service| {
            DrainScope::EveryNamespace.includes(&service.identity)
                && service
                    .containers
                    .iter()
                    .any(|container| container.as_observation().machine_id == server.id)
        })
        .partition(|service| {
            service
                .members()
                .any(|member| member.as_observation().resolved_spec.mode == ServiceMode::Global)
        });
    if globals.is_empty() {
        return Ok(replicated
            .into_iter()
            .map(|service| service.identity)
            .filter(|service| scope.includes(service))
            .collect());
    }
    let globals = globals
        .into_iter()
        .map(|service| service.identity)
        .collect::<Vec<_>>();
    Err(Stop::Refused(user_globals(server, &globals)))
}

/// The refusal of a Drain whose Server runs `globals`. Compose made a user Global until
/// 0.1.2-beta.42.
fn user_globals(server: &Machine, globals: &[QualifiedService]) -> RpcError {
    let names = globals
        .iter()
        .map(|service| format!("`{service}`"))
        .collect::<Vec<_>>()
        .join(", ");
    let (copies, each, them) = if globals.len() == 1 {
        ("its last working copy", "it", "it")
    } else {
        ("their last working copies", "each", "them")
    };
    RpcError::new(
        RpcErrorCode::Conflict,
        format!(
            "Server {} runs {names} on every Server, and draining could stop {copies}. \
             Remove {each}, deploy, then add {them} back with a replica count.",
            server.name
        ),
    )
}

/// How turning the services role off failed.
enum Unset {
    /// The daemon answered no, or the call never left this client: nothing changed.
    Refused(Stop),
    /// The call went out and no answer came back, or the caller cancelled while it was
    /// pending. The daemon finishes an update it admitted, so the role may be off. A
    /// `Cancelled` status counts here even off the wire, since an HTTP/2 reset arrives in
    /// that shape: erring this way offers an idempotent undo, the other way hides a change.
    Unknown(Stop),
}

/// Turn the services role off on `id`. Whether the entry observes it is the next phase's.
async fn turn_services_off(
    client: &mut Client,
    id: &MachineId,
    cancellation: &CancellationToken,
) -> Result<(), Unset> {
    let target = MachineTarget::from(id);
    let update = client.call_shared::<op::UpdateMachine>(
        UpdateMachineRequest {
            update: MachineUpdate {
                accepts_services: Some(false),
                ..MachineUpdate::default()
            },
        },
        Call::once()
            .to(&target)
            .within(Deadline::Target(TARGET_RPC_TIMEOUT)),
    );
    tokio::select! {
        biased;
        () = cancellation.cancelled() => Err(Unset::Unknown(Stop::Cancelled)),
        updated = update => updated.map(drop).map_err(|error| {
            let unreplied = error.unreplied();
            let stop = Stop::Refused(crate::connect::rpc_error(error));
            if unreplied {
                Unset::Unknown(stop)
            } else {
                Unset::Refused(stop)
            }
        }),
    }
}

/// What a Drain's execution needs from a Cluster.
trait DrainClient {
    /// The Services with a Container on `id`, from a fresh observation.
    async fn still_on(&mut self, id: &MachineId) -> Result<Vec<QualifiedService>, Lost>;
    async fn converge(
        &mut self,
        service: &QualifiedService,
        cancellation: &CancellationToken,
    ) -> Converged<Moving>;
    /// `service`'s copies on `server`, leaving out any Global copy, which may be the last
    /// that serves.
    async fn copies_on(
        &mut self,
        server: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<Copy>, RpcError>;
    async fn let_go(&mut self, copy: &Copy, cancellation: &CancellationToken) -> OldCopyEnd;
}

impl DrainClient for Approved {
    async fn still_on(&mut self, id: &MachineId) -> Result<Vec<QualifiedService>, Lost> {
        let machines = self
            .client
            .machines()
            .await
            .map_err(|error| Lost::Entry(error_chain::row(&error)))?;
        let live = self
            .client
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await
            .map_err(|error| Lost::Entry(error_chain::row(&error)))?;
        observed(&live, id).map_err(Lost::Server)?;
        Ok(services_on(id, &live))
    }

    async fn converge(
        &mut self,
        service: &QualifiedService,
        cancellation: &CancellationToken,
    ) -> Converged<Moving> {
        converge(self, service, cancellation).await
    }

    async fn copies_on(
        &mut self,
        server: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<Copy>, RpcError> {
        Ok(self
            .service_containers(service)
            .await?
            .iter()
            .filter(|observation| {
                observation.machine_id == *server
                    && observation.resolved_spec.mode != ServiceMode::Global
            })
            .map(Copy::observed)
            .collect())
    }

    async fn let_go(&mut self, copy: &Copy, cancellation: &CancellationToken) -> OldCopyEnd {
        let known: Vec<MachineRef> = self
            .client
            .machines()
            .await
            .unwrap_or_default()
            .iter()
            .map(|machine| MachineRef::from(&machine.machine))
            .collect();
        let name = |id: &MachineId| named(&known, id);
        let_go(self, copy, &name, cancellation).await
    }
}

/// Each replicated Service, then what remains. Infallible: every problem here is recorded,
/// never propagated.
async fn execute<C: DrainClient>(
    client: &mut C,
    ready: Ready,
    cancellation: &CancellationToken,
    progress: &mut (dyn FnMut(DrainStep<'_>) + Send),
) -> DrainReport {
    let Ready {
        server,
        scope,
        services_role,
        replicated,
    } = ready;
    let mut record = Record {
        server: &server,
        services: Vec::new(),
        stopped: None,
        progress,
    };
    let mut behind = Behind::default();
    'work: {
        let mut rest = replicated.as_slice();
        while let Some((service, after)) = rest.split_first() {
            if cancellation.is_cancelled() {
                behind.finish(&mut record).await;
                record.stop(DrainStop::Cancelled, rest);
                break 'work;
            }
            // Listed before the move, so a copy the move itself leaves Out is reported
            // once, by the move, and not waited on twice. A failed listing leaves the
            // copies to `remaining`, which still finds the Service there.
            let left_behind = behind
                .alongside(&mut record, client.copies_on(&server.id, service))
                .await
                .unwrap_or_default();
            let converged = behind
                .alongside(&mut record, client.converge(service, cancellation))
                .await;
            let (shape, moves) = match converged {
                Converged::Moved { moves } => (Shape::Moved, moves),
                Converged::NothingToMove => (Shape::Done(DrainOutcome::NothingToMove), Vec::new()),
                Converged::Failed { moves, failure } => (Shape::Failed(failure), moves),
                Converged::Stays { reason } => {
                    (Shape::Done(DrainOutcome::Stays { reason }), Vec::new())
                }
                Converged::Stopped { moves, stop } => {
                    behind.moved(
                        service.clone(),
                        Shape::Interrupted,
                        moves,
                        Leftovers::default(),
                    );
                    behind.finish(&mut record).await;
                    record.stop(stop, after);
                    break 'work;
                }
            };
            let leftovers = Leftovers::of(&left_behind);
            behind.moved(service.clone(), shape, moves, leftovers);
            if leftovers.held {
                behind.done(service.clone(), DrainOutcome::Held);
            }
            if leftovers.out {
                let out = left_behind
                    .iter()
                    .filter(|copy| copy.rotation == CopyRotation::Out)
                    .collect::<Vec<_>>();
                let mut ends = Vec::with_capacity(out.len());
                for copy in out {
                    ends.push(if cancellation.is_cancelled() {
                        OldCopyEnd::Running {
                            unconfirmed: Unconfirmed::Cancelled,
                        }
                    } else {
                        behind
                            .alongside(&mut record, client.let_go(copy, cancellation))
                            .await
                    });
                }
                behind.done(service.clone(), DrainOutcome::OldCopies { ends });
            }
            rest = after;
        }
    }
    behind.finish(&mut record).await;
    let remaining = match client.still_on(&server.id).await {
        Ok(services) => Remaining::Observed {
            unchosen: services
                .iter()
                .filter(|service| !service.namespace.is_system() && !scope.includes(service))
                .cloned()
                .collect(),
            services,
        },
        Err(Lost::Server(error) | Lost::Entry(error)) => Remaining::Unobserved { error },
    };
    let Record {
        services, stopped, ..
    } = record;
    DrainReport {
        server,
        services_role,
        services,
        stopped,
        remaining,
    }
}

#[derive(Default)]
struct Behind {
    entries: VecDeque<Pending>,
    flushed: usize,
    retiring: FuturesUnordered<BoxFuture<'static, Ended>>,
}

struct Pending {
    service: QualifiedService,
    shape: Shape,
    moves: Vec<Step>,
    leftovers: Leftovers,
}

/// What an earlier move left on the Server for one Service, listed before its convergence.
/// Rows for them follow the Service's own, so a convergence that moved only other Servers'
/// copies adds none of its own.
#[derive(Clone, Copy, Default)]
struct Leftovers {
    /// A copy the user stopped.
    held: bool,
    /// A copy rotated Out.
    out: bool,
}

impl Leftovers {
    fn of(copies: &[Copy]) -> Self {
        Self {
            held: copies
                .iter()
                .any(|copy| matches!(copy.rotation, CopyRotation::Held(_))),
            out: copies.iter().any(|copy| copy.rotation == CopyRotation::Out),
        }
    }

    fn any(self) -> bool {
        self.held || self.out
    }
}

enum Shape {
    Done(DrainOutcome),
    Moved,
    Failed(MoveFailure),
    Interrupted,
}

struct Step {
    from: MachineRef,
    to: MachineRef,
    old: Option<Result<OldCopyEnd, LastIn>>,
}

struct Ended {
    entry: usize,
    index: usize,
    old: Result<OldCopyEnd, LastIn>,
}

impl Behind {
    fn done(&mut self, service: QualifiedService, outcome: DrainOutcome) {
        self.moved(
            service,
            Shape::Done(outcome),
            Vec::new(),
            Leftovers::default(),
        );
    }

    fn moved(
        &mut self,
        service: QualifiedService,
        shape: Shape,
        moves: Vec<Moving>,
        leftovers: Leftovers,
    ) {
        let entry = self.flushed + self.entries.len();
        let moves = moves
            .into_iter()
            .enumerate()
            .map(|(index, Moving { from, to, old })| {
                let ended = old.map(move |old| Ended { entry, index, old });
                self.retiring.push(ended.boxed());
                Step {
                    from,
                    to,
                    old: None,
                }
            })
            .collect();
        self.entries.push_back(Pending {
            service,
            shape,
            moves,
            leftovers,
        });
    }

    async fn alongside<T>(&mut self, record: &mut Record<'_>, work: impl Future<Output = T>) -> T {
        let mut work = pin!(work);
        loop {
            tokio::select! {
                done = &mut work => {
                    self.flush(record);
                    return done;
                }
                Some(ended) = self.retiring.next() => self.end(ended),
            }
        }
    }

    async fn finish(&mut self, record: &mut Record<'_>) {
        while let Some(ended) = self.retiring.next().await {
            self.end(ended);
        }
        self.flush(record);
    }

    fn end(&mut self, Ended { entry, index, old }: Ended) {
        let step = self
            .entries
            .get_mut(entry - self.flushed)
            .and_then(|pending| pending.moves.get_mut(index))
            .expect("an ended retirement's entry waits for it");
        step.old = Some(old);
    }

    fn flush(&mut self, record: &mut Record<'_>) {
        while self
            .entries
            .front()
            .is_some_and(|pending| pending.moves.iter().all(|step| step.old.is_some()))
        {
            let Pending {
                service,
                shape,
                moves,
                leftovers,
            } = self.entries.pop_front().expect("just checked");
            self.flushed += 1;
            let mut last_in = false;
            // A convergence moves every draining Server's copies. A concurrent Drain's
            // moves are its own to report, though this one waited for their old copies;
            // another Server's old copy left behind is still news here.
            let moves = moves
                .into_iter()
                .map(|Step { from, to, old }| {
                    let refused = old.as_ref().is_some_and(Result::is_err);
                    let old = match old.expect("flushed once every old copy ended") {
                        Ok(old) => old,
                        Err(LastIn) => OldCopyEnd::In {
                            unconfirmed: Unconfirmed::Failed {
                                detail: format!(
                                    "{} still counted it as the last copy serving",
                                    from.name
                                ),
                            },
                        },
                    };
                    (Move { from, to, old }, refused)
                })
                .filter(|(step, _)| {
                    step.from.id == record.server.id || step.old != OldCopyEnd::Stopped
                })
                .map(|(step, refused)| {
                    last_in |= refused;
                    step
                })
                .collect::<Vec<_>>();
            for outcome in rows(shape, moves, last_in, leftovers) {
                record.push(service.clone(), outcome);
            }
        }
    }
}

/// The rows one Service's convergence reports, in order. `moves` are the ones this Drain
/// reports and `last_in` says an old copy was refused out as the last one serving. The
/// Service's own row is none when its leftover rows say all there is, and a Failed or
/// Interrupted one is followed by a Failed refusal when an old copy was refused out.
fn rows(shape: Shape, moves: Vec<Move>, last_in: bool, leftovers: Leftovers) -> Vec<DrainOutcome> {
    let refused = || MoveFailure::Refused {
        from: None,
        reason: StayReason::LastIn,
    };
    let then_refused = |outcome| {
        let mut rows = vec![outcome];
        if last_in {
            rows.push(DrainOutcome::Failed {
                moves: Vec::new(),
                failure: refused(),
            });
        }
        rows
    };
    match shape {
        Shape::Done(DrainOutcome::NothingToMove) if leftovers.any() => Vec::new(),
        Shape::Done(outcome) => vec![outcome],
        Shape::Moved if last_in => vec![DrainOutcome::Failed {
            moves,
            failure: refused(),
        }],
        Shape::Moved if moves.is_empty() && leftovers.any() => Vec::new(),
        Shape::Moved if moves.is_empty() => vec![DrainOutcome::NothingToMove],
        Shape::Moved => vec![DrainOutcome::Moved { moves }],
        Shape::Failed(failure) => then_refused(DrainOutcome::Failed { moves, failure }),
        Shape::Interrupted => then_refused(DrainOutcome::Interrupted { moves }),
    }
}

/// The report's Services as they land, each handed to `progress`.
struct Record<'progress> {
    server: &'progress Machine,
    services: Vec<ServiceDrain>,
    stopped: Option<DrainStop>,
    progress: &'progress mut (dyn FnMut(DrainStep<'_>) + Send),
}

impl Record<'_> {
    fn push(&mut self, service: QualifiedService, outcome: DrainOutcome) {
        self.services.push(ServiceDrain::of(service, outcome));
        let entry = self.services.last().expect("just pushed");
        (self.progress)(DrainStep::Service {
            server: self.server,
            service: entry,
        });
    }

    fn stop<'rest>(
        &mut self,
        stop: DrainStop,
        rest: impl IntoIterator<Item = &'rest QualifiedService>,
    ) {
        for service in rest {
            self.push(service.clone(), DrainOutcome::NotAttempted);
        }
        self.stopped = Some(stop);
    }
}

/// Why a follow-up observation failed: the entry (the Drain stops) or the Server (one
/// Service's evidence).
enum Lost {
    Entry(String),
    Server(String),
}

/// Whether `id` answered the Container listing; the detail when it didn't. A listing that
/// never asked `id`, because the Machine list left it out, has no answer from it either.
fn observed(live: &LiveServices<RpcError>, id: &MachineId) -> Result<(), String> {
    if live
        .containers
        .successes
        .iter()
        .any(|success| success.machine_id == *id)
    {
        return Ok(());
    }
    if let Some(failure) = live
        .containers
        .failures
        .iter()
        .find(|failure| failure.machine_id == *id)
    {
        return Err(error_chain::row(&failure.error));
    }
    if live.containers.omissions.contains(id) {
        return Err("no terminal response".into());
    }
    Err("not in the Machine list".into())
}

pub(crate) fn services_on(
    machine_id: &MachineId,
    live: &LiveServices<RpcError>,
) -> Vec<QualifiedService> {
    live.services()
        .into_iter()
        .filter(|service| {
            service
                .containers
                .iter()
                .any(|container| container.as_observation().machine_id == *machine_id)
        })
        .map(|service| service.identity)
        .collect()
}

/// The Services a Drain would move off `machine_id`: user Services whose Container there
/// the planner may move. Global, Volume and Bind Mount pinned Services stay put.
pub(crate) fn movable_services_on(
    machine_id: &MachineId,
    live: &LiveServices<RpcError>,
) -> Vec<QualifiedService> {
    live.services()
        .into_iter()
        .filter(|service| {
            DrainScope::EveryNamespace.includes(&service.identity)
                && service.containers.iter().any(|container| {
                    let observation = container.as_observation();
                    observation.machine_id == *machine_id && can_move(&observation.resolved_spec)
                })
        })
        .map(|service| service.identity)
        .collect()
}

pub(crate) fn replicated_services_on(
    machine_id: &MachineId,
    live: &LiveServices<RpcError>,
) -> Vec<QualifiedService> {
    live.services()
        .into_iter()
        .filter(|service| {
            service.containers.iter().any(|container| {
                let observation = container.as_observation();
                observation.machine_id == *machine_id
                    && matches!(
                        observation.resolved_spec.mode,
                        ServiceMode::Replicated { .. }
                    )
            })
        })
        .map(|service| service.identity)
        .collect()
}

#[cfg(test)]
#[path = "drain_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "drain/version_tests.rs"]
mod version_tests;
