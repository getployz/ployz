//! Drain: turn a Machine's services role off, retire its chosen Globals there, run
//! Placement convergence for each chosen replicated Service with a Container on it, then
//! observe what still runs there. One bounded command.
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

use std::collections::VecDeque;
use std::pin::pin;

use futures_util::future::BoxFuture;
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
use crate::deploy::{Converged, LastIn, Moving, can_move, converge, named};

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
    async fn converge(
        &mut self,
        service: &QualifiedService,
        cancellation: &CancellationToken,
    ) -> Converged<Moving>;
    async fn copies_on(
        &mut self,
        server: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<Copy>, RpcError>;
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
        cancellation: &CancellationToken,
    ) -> Converged<Moving> {
        converge(self, service, cancellation).await
    }

    async fn copies_on(
        &mut self,
        server: &MachineId,
        service: &QualifiedService,
    ) -> Result<Vec<Copy>, RpcError> {
        copies_on(self, server, service).await
    }

    async fn let_go(&mut self, copy: &Copy, cancellation: &CancellationToken) -> OldCopyEnd {
        let known: Vec<MachineRef> = self
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
                    behind.moved(service.clone(), Shape::Interrupted, moves);
                    behind.finish(&mut record).await;
                    record.stop(stop, after);
                    break 'work;
                }
            };
            let held = left_behind
                .iter()
                .any(|copy| matches!(copy.rotation, CopyRotation::Held(_)));
            let out = left_behind
                .iter()
                .filter(|copy| copy.rotation == CopyRotation::Out)
                .collect::<Vec<_>>();
            let only_leftovers = matches!(shape, Shape::Done(DrainOutcome::NothingToMove))
                && (held || !out.is_empty());
            if !only_leftovers {
                // Rows for the leftovers follow, so a convergence that moved only other
                // Servers' copies adds none of its own.
                let beside_leftovers = held || !out.is_empty();
                behind.moved_beside(service.clone(), shape, moves, beside_leftovers);
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
    /// The Service's leftover rows follow this one.
    beside_leftovers: bool,
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
        self.moved(service, Shape::Done(outcome), Vec::new());
    }

    fn moved(&mut self, service: QualifiedService, shape: Shape, moves: Vec<Moving>) {
        self.moved_beside(service, shape, moves, false);
    }

    fn moved_beside(
        &mut self,
        service: QualifiedService,
        shape: Shape,
        moves: Vec<Moving>,
        beside_leftovers: bool,
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
            beside_leftovers,
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
                beside_leftovers,
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
                Shape::Moved if moves.is_empty() && beside_leftovers => continue,
                Shape::Moved if moves.is_empty() => (DrainOutcome::NothingToMove, false),
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
