//! Drain: turn a Machine's services role off, retire its chosen Globals there, run
//! Placement convergence for each chosen replicated Service with a Container on it, then
//! observe what still runs there. One bounded command.
//!
//! Each move's old copy retires alongside the moves after it, and each Service's last new
//! copy goes In while the next Service starts its own; the report waits for every
//! retirement and still lists the Services in the order they were handled.
//!
//! A preflight that may refuse comes first; nothing has moved when it does. After it the
//! Drain cannot fail: `execute` returns a [`DrainReport`], not a `Result`, so whatever moved
//! is always in the report.
//!
//! Callers: `ployz server drain` (streams a line per step through `progress`) and
//! [`crate::sdk::Session::drain_machine`] (Cloud stores the final report).

use std::collections::VecDeque;
use std::future::poll_fn;
use std::pin::{Pin, pin};

use futures_util::future::{BoxFuture, MaybeDone, maybe_done};
use futures_util::stream::FuturesUnordered;
use futures_util::{FutureExt, StreamExt};
use ployz_core::{
    CopyRotation, EnvironmentValues, LiveServices, Machine, MachineId, MachineName, MachineTarget,
    MachineUpdate, Namespace, QualifiedService, RpcError, RpcErrorCode, ServiceMode,
    UpdateMachineRequest, op,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::cluster::{RoleSetting, RoleWaitError, visible_machine, wait_for_role};
use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};
use crate::deploy::cleanup::Copy;
use crate::deploy::{Converged, Converging, LastIn, Moving, Reservations, converge};

pub(crate) mod retirement;

use retirement::{Retirement, copies_on, let_go, retire_globals};

pub use crate::deploy::{
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
        !service.namespace.is_reserved()
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
/// looked: Globals first, then replicated Services, in the order handled. When `stopped` is
/// set, the Service it stopped at reads `interrupted` or `not_attempted`, and every one
/// after it reads `not_attempted`.
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
    /// observed.
    #[must_use]
    pub fn complete(&self) -> bool {
        let Remaining::Observed { services, .. } = &self.remaining else {
            return false;
        };
        self.stopped.is_none()
            && self
                .services
                .iter()
                .all(|service| service.outcome.complete() && !services.contains(&service.service))
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
    Retired,
    /// Global: it may still run on the Server, and why.
    NotRetired { error: String },
    /// Replicated: the Drain stopped while handling it, after `moves`. The report's
    /// `stopped` says why.
    Interrupted { moves: Vec<Move> },
    /// The Drain stopped before reaching it.
    NotAttempted,
}

impl DrainOutcome {
    fn complete(&self) -> bool {
        match self {
            Self::Moved { moves } => moves.iter().all(|step| step.old == OldCopyEnd::Stopped),
            Self::OldCopies { ends } => ends.iter().all(|end| *end == OldCopyEnd::Stopped),
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

/// Why a Drain did nothing past, at most, turning the services role off. The role is
/// idempotent; a rerun finds it off.
#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    /// The target names no visible Server, or more than one, or turning its services role
    /// off failed.
    #[error(transparent)]
    Refused(RpcError),
    /// The entry did not observe the services role off within 30 s.
    #[error("this entry Server has not yet observed the new services role")]
    RoleNotObserved,
    /// The Server's Containers could not be listed.
    #[error("Cannot observe Services on Server {server}: {detail}")]
    Unobservable { server: MachineName, detail: String },
    /// Cancellation came before anything moved.
    #[error("cancelled before the Drain began")]
    Cancelled,
    /// The entry could not be reached.
    #[error(transparent)]
    Connect(#[from] ConnectError),
}

impl From<DrainError> for RpcError {
    fn from(error: DrainError) -> Self {
        let coded = |code, error: &DrainError| Self {
            code,
            message: error.to_string(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        };
        match error {
            DrainError::Refused(error) => error,
            DrainError::Connect(error) => error.into(),
            error @ (DrainError::RoleNotObserved
            | DrainError::Unobservable { .. }
            | DrainError::Cancelled) => coded(RpcErrorCode::Unavailable, &error),
        }
    }
}

impl Client {
    /// Drain `target` within `scope`.
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
    pub async fn drain(
        &mut self,
        target: &MachineTarget,
        scope: &DrainScope,
        cancellation: &CancellationToken,
        progress: &mut (dyn FnMut(DrainStep<'_>) + Send),
    ) -> Result<DrainReport, DrainError> {
        let ready = preflight(self, target, scope, cancellation, progress).await?;
        Ok(execute(self, ready, cancellation, progress).await)
    }
}

/// Everything decided before the first move.
struct Ready {
    server: Machine,
    scope: DrainScope,
    services_role: ServicesRole,
    globals: Vec<QualifiedService>,
    replicated: Vec<QualifiedService>,
}

/// Select, turn the role off, wait until the entry observes it, list the chosen Services.
async fn preflight(
    client: &mut Client,
    target: &MachineTarget,
    scope: &DrainScope,
    cancellation: &CancellationToken,
    progress: &mut (dyn FnMut(DrainStep<'_>) + Send),
) -> Result<Ready, DrainError> {
    let machines = client.machines().await?;
    let server = visible_machine(target, &machines)
        .map_err(DrainError::Refused)?
        .machine
        .clone();
    if cancellation.is_cancelled() {
        return Err(DrainError::Cancelled);
    }
    let services_role = if server.accepts_services {
        turn_services_off(client, &server.id, cancellation).await?;
        ServicesRole::TurnedOff
    } else {
        ServicesRole::AlreadyOff
    };
    progress(DrainStep::ServicesOff {
        server: &server,
        role: services_role,
    });
    let machines = client.machines().await?;
    let live = client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await?;
    observed(&live, &server.id).map_err(|detail| DrainError::Unobservable {
        server: server.name.clone(),
        detail,
    })?;
    let replicated = replicated_services_on(&server.id, &live);
    let (replicated, globals) = services_on(&server.id, &live)
        .into_iter()
        .filter(|service| scope.includes(service))
        .partition(|service| replicated.contains(service));
    Ok(Ready {
        server,
        scope: scope.clone(),
        services_role,
        globals,
        replicated,
    })
}

/// Turn the services role off on `id`, then wait until the entry observes it off.
async fn turn_services_off(
    client: &mut Client,
    id: &MachineId,
    cancellation: &CancellationToken,
) -> Result<(), DrainError> {
    let target = MachineTarget::from(id);
    let update = client.invoke::<op::UpdateMachine>(
        UpdateMachineRequest {
            update: MachineUpdate {
                accepts_services: Some(false),
                ..MachineUpdate::default()
            },
        },
        &target,
        Some(TARGET_RPC_TIMEOUT),
    );
    tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(DrainError::Cancelled),
        updated = update => updated.map_err(DrainError::Refused)?,
    };
    wait_for_role(client, id, RoleSetting::ServicesOff, cancellation)
        .await
        .map_err(|error| match error {
            RoleWaitError::NotObserved(_) => DrainError::RoleNotObserved,
            RoleWaitError::Cancelled => DrainError::Cancelled,
            RoleWaitError::Connect(error) => DrainError::Connect(error),
        })
}

/// What a Drain's execution needs from a Cluster.
trait DrainClient {
    /// The Services with a Container on `id`, from a fresh observation.
    async fn still_on(&mut self, id: &MachineId) -> Result<Vec<QualifiedService>, Lost>;
    async fn retire(
        &mut self,
        server: &Machine,
        globals: &[QualifiedService],
        cancellation: &CancellationToken,
    ) -> Vec<(QualifiedService, Retirement)>;
    /// Converges `service` up to its last move's In, placing beside `reservations`.
    async fn converge(
        &mut self,
        service: &QualifiedService,
        reservations: &Reservations,
        cancellation: &CancellationToken,
    ) -> Converging<Moving>;
    /// Every copy of `service` on `server`, from a fresh listing.
    async fn copies_on(
        &mut self,
        server: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<Copy>, RpcError>;
    /// Wait until no live proxy lists the Out copy, then remove it.
    async fn let_go(&mut self, copy: &Copy, cancellation: &CancellationToken) -> OldCopyEnd;
}

impl DrainClient for Client {
    async fn still_on(&mut self, id: &MachineId) -> Result<Vec<QualifiedService>, Lost> {
        let machines = self
            .machines()
            .await
            .map_err(|error| Lost::Entry(crate::ui::row(&error)))?;
        let live = self
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await
            .map_err(|error| Lost::Entry(crate::ui::row(&error)))?;
        observed(&live, id).map_err(Lost::Server)?;
        Ok(services_on(id, &live))
    }

    async fn retire(
        &mut self,
        server: &Machine,
        globals: &[QualifiedService],
        cancellation: &CancellationToken,
    ) -> Vec<(QualifiedService, Retirement)> {
        retire_globals(self, server, globals, cancellation).await
    }

    async fn converge(
        &mut self,
        service: &QualifiedService,
        reservations: &Reservations,
        cancellation: &CancellationToken,
    ) -> Converging<Moving> {
        converge(self, service, reservations, cancellation).await
    }

    async fn copies_on(
        &mut self,
        server: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<Copy>, RpcError> {
        copies_on(self, server, service).await
    }

    async fn let_go(&mut self, copy: &Copy, cancellation: &CancellationToken) -> OldCopyEnd {
        let machines = self.machines().await.unwrap_or_default();
        let name = |id: &MachineId| {
            machines
                .iter()
                .find(|machine| machine.machine.id == *id)
                .map_or_else(
                    || MachineRef {
                        id: *id,
                        name: MachineName::parse(id.to_string())
                            .or_else(|_| MachineName::parse(format!("m{id}")))
                            .expect("a lowercase hex id is a DNS label"),
                    },
                    |machine| MachineRef::from(&machine.machine),
                )
        };
        let_go(self, copy, &name, cancellation).await
    }
}

/// Globals, then each replicated Service, then what remains. Infallible: every problem
/// here is recorded, never propagated.
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
        globals,
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
        if !globals.is_empty() {
            if cancellation.is_cancelled() {
                record.stop(DrainStop::Cancelled, globals.iter().chain(&replicated));
                break 'work;
            }
            let retirements = client.retire(&server, &globals, cancellation).await;
            let still = client.still_on(&server.id).await;
            let cancelled = retirements
                .iter()
                .any(|(_, retirement)| *retirement == Retirement::NotAttempted);
            for (global, retirement) in retirements {
                let outcome = match retirement {
                    // Only a fresh observation that still finds it undoes an acknowledged
                    // retirement; a lost one leaves that to `remaining`.
                    Retirement::Retired => match &still {
                        Ok(still) if still.contains(&global) => DrainOutcome::NotRetired {
                            error: format!("still running on {}", server.name),
                        },
                        Ok(_) | Err(_) => DrainOutcome::Retired,
                    },
                    Retirement::NotRetired(error) => DrainOutcome::NotRetired { error },
                    Retirement::NotAttempted => DrainOutcome::NotAttempted,
                };
                record.push(global, outcome);
            }
            if cancelled {
                record.stop(DrainStop::Cancelled, &replicated);
                break 'work;
            }
            if let Err(Lost::Entry(detail)) = still {
                record.stop(DrainStop::EntryUnreachable { detail }, &replicated);
                break 'work;
            }
        }
        let reservations = Reservations::default();
        let mut settling: Option<Settling> = None;
        let mut rest = replicated.as_slice();
        while let Some((service, after)) = rest.split_first() {
            if cancellation.is_cancelled() {
                if let Some(previous) = settling.take() {
                    settle(
                        client,
                        &mut behind,
                        &mut record,
                        previous,
                        &mut None,
                        cancellation,
                    )
                    .await;
                }
                behind.finish(&mut record).await;
                record.stop(DrainStop::Cancelled, rest);
                break 'work;
            }
            // Listed before the move, so a copy the move itself leaves Out is reported
            // once, by the move, and not waited on twice. A failed listing leaves the
            // copies to `remaining`, which still finds the Service there.
            let left_behind = behind
                .alongside(
                    &mut record,
                    beside(client.copies_on(&server.id, service), &mut settling),
                )
                .await
                .unwrap_or_default();
            let converging = behind
                .alongside(
                    &mut record,
                    beside(
                        client.converge(service, &reservations, cancellation),
                        &mut settling,
                    ),
                )
                .await;
            let decided = match converging {
                Converging::Decided(Converged::Moved { moves }) => {
                    MaybeDone::Done((Shape::Moved, moves))
                }
                Converging::Decided(Converged::NothingToMove) => {
                    MaybeDone::Done((Shape::Done(DrainOutcome::NothingToMove), Vec::new()))
                }
                Converging::Decided(Converged::Failed { moves, failure }) => {
                    MaybeDone::Done((Shape::Failed(failure), moves))
                }
                Converging::Decided(Converged::Stays { reason }) => {
                    MaybeDone::Done((Shape::Done(DrainOutcome::Stays { reason }), Vec::new()))
                }
                Converging::Decided(Converged::Stopped { moves, stop }) => {
                    if let Some(previous) = settling.take() {
                        settle(
                            client,
                            &mut behind,
                            &mut record,
                            previous,
                            &mut None,
                            cancellation,
                        )
                        .await;
                    }
                    behind.moved(service.clone(), Shape::Interrupted, moves);
                    behind.finish(&mut record).await;
                    record.stop(stop, after);
                    break 'work;
                }
                Converging::Admitting { mut moves, last } => maybe_done(
                    async move {
                        match last.await {
                            Ok(moved) => {
                                moves.push(moved);
                                (Shape::Moved, moves)
                            }
                            Err(failure) => (Shape::Failed(failure), moves),
                        }
                    }
                    .boxed(),
                ),
            };
            let next = Settling {
                service: service.clone(),
                left_behind,
                decided,
            };
            if let Some(previous) = settling.replace(next) {
                settle(
                    client,
                    &mut behind,
                    &mut record,
                    previous,
                    &mut settling,
                    cancellation,
                )
                .await;
            }
            rest = after;
        }
        if let Some(last) = settling.take() {
            settle(
                client,
                &mut behind,
                &mut record,
                last,
                &mut None,
                cancellation,
            )
            .await;
        }
    }
    behind.finish(&mut record).await;
    let remaining = match client.still_on(&server.id).await {
        Ok(services) => Remaining::Observed {
            unchosen: services
                .iter()
                .filter(|service| !service.namespace.is_reserved() && !scope.includes(service))
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

/// A Service handled up to its last move's In, with its copies on the drained Server
/// listed before it moved.
struct Settling {
    service: QualifiedService,
    left_behind: Vec<Copy>,
    decided: MaybeDone<BoxFuture<'static, (Shape, Vec<Moving>)>>,
}

/// Waits, beside `next`, until `settling` is decided, then hands its moves to `behind` and
/// lets go of the Out copies it left on the drained Server.
async fn settle<C: DrainClient>(
    client: &mut C,
    behind: &mut Behind,
    record: &mut Record<'_>,
    settling: Settling,
    next: &mut Option<Settling>,
    cancellation: &CancellationToken,
) {
    let Settling {
        service,
        left_behind,
        mut decided,
    } = settling;
    behind.alongside(record, beside(&mut decided, next)).await;
    let (shape, moves) = Pin::new(&mut decided)
        .take_output()
        .expect("a settled Service was just decided");
    let held = left_behind
        .iter()
        .any(|copy| matches!(copy.rotation, CopyRotation::Held(_)));
    let out = left_behind
        .iter()
        .filter(|copy| copy.rotation == CopyRotation::Out)
        .collect::<Vec<_>>();
    let only_leftovers =
        matches!(shape, Shape::Done(DrainOutcome::NothingToMove)) && (held || !out.is_empty());
    if !only_leftovers {
        behind.moved(service.clone(), shape, moves);
    }
    if held {
        behind.done(service.clone(), DrainOutcome::Held);
    }
    if !out.is_empty() {
        let mut ends = Vec::with_capacity(out.len());
        for copy in out {
            ends.push(if cancellation.is_cancelled() {
                OldCopyEnd::Running {
                    unconfirmed: Unconfirmed::Cancelled,
                }
            } else {
                behind
                    .alongside(record, beside(client.let_go(copy, cancellation), next))
                    .await
            });
        }
        behind.done(service, DrainOutcome::OldCopies { ends });
    }
}

/// Runs `work` while `next`'s last move keeps going In.
async fn beside<T>(work: impl Future<Output = T>, next: &mut Option<Settling>) -> T {
    let mut work = pin!(work);
    poll_fn(|cx| {
        if let Some(next) = next.as_mut() {
            let _ = Pin::new(&mut next.decided).poll(cx);
        }
        work.as_mut().poll(cx)
    })
    .await
}

/// The replicated Services' entries in the order they were handled, each held until the
/// old copies its moves left have retired. Those retirements run alongside later work.
#[derive(Default)]
struct Behind {
    entries: VecDeque<Pending>,
    /// How many entries went to the report already: an `Ended`'s entry counts from here.
    flushed: usize,
    retiring: FuturesUnordered<BoxFuture<'static, Ended>>,
}

struct Pending {
    service: QualifiedService,
    shape: Shape,
    moves: Vec<Step>,
}

/// The outcome an entry becomes once its moves' old copies have ended.
enum Shape {
    Done(DrainOutcome),
    Moved,
    Failed(MoveFailure),
    Interrupted,
}

struct Step {
    from: MachineRef,
    to: MachineRef,
    /// `None` while the old copy is still retiring.
    old: Option<Result<OldCopyEnd, LastIn>>,
}

/// How the old copy of an entry's `index`th move ended.
struct Ended {
    entry: usize,
    index: usize,
    old: Result<OldCopyEnd, LastIn>,
}

impl Behind {
    fn done(&mut self, service: QualifiedService, outcome: DrainOutcome) {
        self.moved(service, Shape::Done(outcome), Vec::new());
    }

    fn moved(&mut self, service: QualifiedService, shape: Shape, moves: Vec<Moving>) {
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
        });
    }

    /// Runs `work` while the old copies keep retiring, and reports the entries that ended.
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

    /// Waits for every retirement still running, then reports every entry.
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

    /// Reports the leading entries whose old copies have all ended. An Out refused as the
    /// last copy In drops that move and says the copy stays.
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
            } = self.entries.pop_front().expect("just checked");
            self.flushed += 1;
            let mut last_in = false;
            let moves = moves
                .into_iter()
                .filter_map(|Step { from, to, old }| {
                    match old.expect("flushed once every old copy ended") {
                        Ok(old) => Some(Move { from, to, old }),
                        Err(LastIn) => {
                            last_in = true;
                            None
                        }
                    }
                })
                .collect::<Vec<_>>();
            let refused = || MoveFailure::Refused {
                reason: StayReason::LastIn,
            };
            let (outcome, stays) = match shape {
                Shape::Done(outcome) => (outcome, false),
                Shape::Moved if last_in => (
                    DrainOutcome::Failed {
                        moves,
                        failure: refused(),
                    },
                    false,
                ),
                Shape::Moved => (DrainOutcome::Moved { moves }, false),
                Shape::Failed(failure) => (DrainOutcome::Failed { moves, failure }, last_in),
                Shape::Interrupted => (DrainOutcome::Interrupted { moves }, last_in),
            };
            record.push(service.clone(), outcome);
            if stays {
                record.push(
                    service,
                    DrainOutcome::Failed {
                        moves: Vec::new(),
                        failure: refused(),
                    },
                );
            }
        }
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
        self.services.push(ServiceDrain { service, outcome });
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

/// Whether `id` answered the Container listing; the detail when it didn't.
fn observed(live: &LiveServices<RpcError>, id: &MachineId) -> Result<(), String> {
    if let Some(failure) = live
        .containers
        .failures
        .iter()
        .find(|failure| failure.machine_id == *id)
    {
        return Err(crate::ui::row(&failure.error));
    }
    if live.containers.omissions.contains(id) {
        return Err("no terminal response".into());
    }
    Ok(())
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
