//! Drain: turn a Machine's services role off, retire its chosen Globals there, run
//! Placement convergence for each chosen replicated Service with a Container on it, then
//! observe what still runs there. One bounded command.
//!
//! A preflight that may refuse comes first; nothing has moved when it does. After it the
//! Drain cannot fail: `execute` returns a [`DrainReport`], not a `Result`, so whatever moved
//! is always in the report.
//!
//! Callers: `ployz server drain` (streams a line per step through `progress`) and
//! [`crate::sdk::Session::drain_machine`] (Cloud stores the final report).

use std::fmt;
use std::time::Duration;

use ployz_core::{
    EnvironmentValues, LiveServices, Machine, MachineId, MachineName, MachineObservation,
    MachineTarget, MachineUpdate, NameMatches, Namespace, QualifiedService, RpcError, RpcErrorCode,
    ServiceMode, UpdateMachineRequest, op,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};
use crate::deploy::{Convergence, converge};
use crate::global_catch_up::retire_globals;

pub use crate::deploy::{MachineRef, Move, MoveFailure, StayReason};

/// How long the entry may take to observe the services role off before the Drain refuses.
// ponytail: fixed 30 s bound, as `server set` uses; the role replicates within seconds.
const ROLE_OBSERVED_WITHIN: Duration = Duration::from_secs(30);
const ROLE_POLL: Duration = Duration::from_millis(250);

/// Which user Namespaces a Drain acts on. Reserved Namespaces never are.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum DrainScope {
    /// Every user Namespace: a Standalone Cluster records no ownership.
    EveryNamespace,
    /// Only these Namespaces, which some Project owns. Empty selects nothing. Containers of
    /// other Namespaces stay where they are and are not reported.
    Owned { namespaces: Vec<Namespace> },
}

impl DrainScope {
    fn includes(&self, namespace: &Namespace) -> bool {
        !namespace.is_reserved()
            && match self {
                Self::EveryNamespace => true,
                Self::Owned { namespaces } => namespaces.contains(namespace),
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
/// looked: Globals first, then replicated Services, in the order handled. Services read
/// `not_attempted` only when `stopped` is set, and only after the one that stopped it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DrainReport {
    /// The drained Server's record as selected, before this Drain changed its role.
    pub server: Machine,
    pub services_role: ServicesRole,
    pub services: Vec<ServiceDrain>,
    /// Why the Drain ended before handling every Service.
    pub stopped: Option<DrainStop>,
    /// What runs on the Server after the Drain, reserved and unchosen Namespaces included.
    pub remaining: Remaining,
}

impl DrainReport {
    /// Every chosen Service left the Server, the Drain ran to its end, and what remains
    /// was observed.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.stopped.is_none()
            && matches!(self.remaining, Remaining::Observed { .. })
            && self
                .services
                .iter()
                .all(|service| service.outcome.complete())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ServicesRole {
    TurnedOff,
    AlreadyOff,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ServiceDrain {
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
    /// Global: its Container on the Server is gone.
    Retired,
    /// Global: it still runs on the Server.
    NotRetired { error: String },
    /// The Drain stopped before reaching it.
    NotAttempted,
}

impl DrainOutcome {
    /// Everything this Service had on the Server is gone from it.
    #[must_use]
    pub fn complete(&self) -> bool {
        match self {
            Self::Moved { .. } | Self::NothingToMove | Self::Retired => true,
            Self::Failed { .. }
            | Self::Stays { .. }
            | Self::NotRetired { .. }
            | Self::NotAttempted => false,
        }
    }
}

impl From<Convergence> for DrainOutcome {
    fn from(convergence: Convergence) -> Self {
        match convergence {
            Convergence::Moved { moves } => Self::Moved { moves },
            Convergence::NothingToMove => Self::NothingToMove,
            Convergence::Failed { moves, failure } => Self::Failed { moves, failure },
            Convergence::Stays { reason } => Self::Stays { reason },
        }
    }
}

/// Why a Drain ended early. Every other problem is one Service's evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DrainStop {
    /// The caller cancelled, or the SDK session closed.
    Cancelled,
    /// The entry Server stopped answering.
    EntryUnreachable { detail: String },
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

/// What runs on the Server once the Drain finished.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Remaining {
    Observed { services: Vec<QualifiedService> },
    Unobserved { error: String },
}

/// Why a Drain did nothing past, at most, turning the services role off. The role is
/// idempotent; a rerun finds it off.
#[derive(Debug, thiserror::Error)]
pub enum DrainError {
    #[error("Server {0} was not found")]
    NotFound(String),
    #[error("Server name {name} is ambiguous: {ids}")]
    Ambiguous { name: String, ids: String },
    /// Turning the services role off failed.
    #[error("{0}")]
    Cordon(RpcError),
    #[error("this entry Server has not yet observed the new services role")]
    RoleNotObserved,
    #[error("Cannot observe Services on Server {server}: {detail}")]
    Unobservable { server: MachineName, detail: String },
    #[error("cancelled before the Drain began")]
    Cancelled,
    #[error(transparent)]
    Connect(#[from] ConnectError),
}

impl From<DrainError> for RpcError {
    fn from(error: DrainError) -> Self {
        let coded = |code, error: &DrainError| Self {
            code,
            message: error.to_string(),
            details: serde_json::Value::Null,
        };
        match error {
            DrainError::Cordon(error) => error,
            DrainError::Connect(error) => error.into(),
            error @ DrainError::NotFound(_) => coded(RpcErrorCode::NotFound, &error),
            error @ DrainError::Ambiguous { .. } => coded(RpcErrorCode::Ambiguous, &error),
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
    let server = select(&machines, target)?.clone();
    if cancellation.is_cancelled() {
        return Err(DrainError::Cancelled);
    }
    let services_role = if server.accepts_services {
        cordon(client, &server.id, cancellation).await?;
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
        .filter(|service| scope.includes(&service.namespace))
        .partition(|service| replicated.contains(service));
    Ok(Ready {
        server,
        services_role,
        globals,
        replicated,
    })
}

fn select<'list>(
    machines: &'list [MachineObservation],
    target: &MachineTarget,
) -> Result<&'list Machine, DrainError> {
    match target.resolve(machines.iter().map(|entry| &entry.machine)) {
        NameMatches::None => Err(DrainError::NotFound(
            target.as_str().escape_debug().to_string(),
        )),
        NameMatches::One(machine) => Ok(machine),
        matches @ NameMatches::Ambiguous { .. } => Err(DrainError::Ambiguous {
            name: target.as_str().escape_debug().to_string(),
            ids: matches
                .iter()
                .map(|machine| machine.id.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        }),
    }
}

/// Turn the services role off on `id`, then wait until the entry observes it off.
async fn cordon(
    client: &mut Client,
    id: &MachineId,
    cancellation: &CancellationToken,
) -> Result<(), DrainError> {
    client
        .invoke::<op::UpdateMachine>(
            UpdateMachineRequest {
                update: MachineUpdate {
                    accepts_services: Some(false),
                    ..MachineUpdate::default()
                },
            },
            &MachineTarget::from(id),
            Some(TARGET_RPC_TIMEOUT),
        )
        .await
        .map_err(DrainError::Cordon)?;
    let deadline = tokio::time::Instant::now() + ROLE_OBSERVED_WITHIN;
    loop {
        let machines = client.machines().await?;
        if machines
            .iter()
            .any(|entry| entry.machine.id == *id && !entry.machine.accepts_services)
        {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(DrainError::RoleNotObserved);
        }
        tokio::select! {
            () = cancellation.cancelled() => return Err(DrainError::Cancelled),
            () = tokio::time::sleep(ROLE_POLL) => {}
        }
    }
}

/// Globals, then each replicated Service, then what remains. Infallible: every problem
/// here is recorded, never propagated.
async fn execute(
    client: &mut Client,
    ready: Ready,
    cancellation: &CancellationToken,
    progress: &mut (dyn FnMut(DrainStep<'_>) + Send),
) -> DrainReport {
    let Ready {
        server,
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
    'work: {
        if !globals.is_empty() {
            if cancellation.is_cancelled() {
                record.stop(DrainStop::Cancelled, globals.iter().chain(&replicated));
                break 'work;
            }
            let unretired = retire_globals(client, &server, &globals).await;
            let still = still_on(client, &server.id).await;
            for global in &globals {
                let outcome = match &still {
                    Ok(still) if !still.contains(global) => DrainOutcome::Retired,
                    Ok(_) => DrainOutcome::NotRetired {
                        error: unretired
                            .iter()
                            .find(|(identity, _)| identity == global)
                            .map_or_else(
                                || format!("still running on {}", server.name),
                                |(_, error)| error.clone(),
                            ),
                    },
                    Err(Lost::Server(detail) | Lost::Entry(detail)) => DrainOutcome::NotRetired {
                        error: detail.clone(),
                    },
                };
                record.push(global.clone(), outcome);
            }
            if let Err(Lost::Entry(detail)) = still {
                record.stop(DrainStop::EntryUnreachable { detail }, &replicated);
                break 'work;
            }
        }
        let mut rest = replicated.as_slice();
        while let Some((service, after)) = rest.split_first() {
            if cancellation.is_cancelled() {
                record.stop(DrainStop::Cancelled, rest);
                break 'work;
            }
            let convergence = converge(client, service, cancellation).await;
            let lost = convergence.lost_entry().map(str::to_owned);
            record.push(service.clone(), convergence.into());
            if let Some(detail) = lost {
                record.stop(DrainStop::EntryUnreachable { detail }, after);
                break 'work;
            }
            rest = after;
        }
    }
    let remaining = match still_on(client, &server.id).await {
        Ok(services) => Remaining::Observed { services },
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

/// The Services with a Container on `id`, from a fresh observation.
async fn still_on(client: &mut Client, id: &MachineId) -> Result<Vec<QualifiedService>, Lost> {
    let machines = client
        .machines()
        .await
        .map_err(|error| Lost::Entry(error.to_string()))?;
    let live = client
        .live_services_from(&machines, EnvironmentValues::Redacted)
        .await
        .map_err(|error| Lost::Entry(error.to_string()))?;
    observed(&live, id).map_err(Lost::Server)?;
    Ok(services_on(id, &live))
}

/// Whether `id` answered the Container listing; the detail when it didn't.
fn observed(live: &LiveServices<RpcError>, id: &MachineId) -> Result<(), String> {
    if let Some(failure) = live
        .containers
        .failures
        .iter()
        .find(|failure| failure.machine_id == *id)
    {
        return Err(failure.error.message.clone());
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
