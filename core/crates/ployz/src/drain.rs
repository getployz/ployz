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

use ployz_core::{
    EnvironmentValues, LiveServices, Machine, MachineId, MachineName, MachineTarget, MachineUpdate,
    Namespace, QualifiedService, RpcError, RpcErrorCode, ServiceMode, UpdateMachineRequest, op,
};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use ts_rs::TS;

use crate::cluster::{RoleSetting, RoleWaitError, visible_machine, wait_for_role};
use crate::connect::{Client, ConnectError, TARGET_RPC_TIMEOUT};
use crate::deploy::{Converged, converge};

mod retirement;

use retirement::{Retirement, retire_globals};

pub use crate::deploy::{DrainStop, MachineRef, Move, MoveFailure, StayReason};

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
/// looked: Globals first, then replicated Services, in the order handled. When `stopped` is
/// set, the Service it stopped at reads `interrupted` or `not_attempted`, and every one
/// after it reads `not_attempted`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
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
    /// Every chosen Service left the Server, the Drain ran to its end, and what remains
    /// was observed.
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
    /// Everything this Service had on the Server is gone from it.
    fn complete(&self) -> bool {
        match self {
            Self::Moved { .. } | Self::NothingToMove | Self::Retired => true,
            Self::Failed { .. }
            | Self::Stays { .. }
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
        /// Those of `services` in a user Namespace the Drain's scope left alone.
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
    #[error("{0}")]
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
        .filter(|service| scope.includes(&service.namespace))
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
    ) -> Converged;
}

impl DrainClient for Client {
    async fn still_on(&mut self, id: &MachineId) -> Result<Vec<QualifiedService>, Lost> {
        let machines = self
            .machines()
            .await
            .map_err(|error| Lost::Entry(error.to_string()))?;
        let live = self
            .live_services_from(&machines, EnvironmentValues::Redacted)
            .await
            .map_err(|error| Lost::Entry(error.to_string()))?;
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
    ) -> Converged {
        converge(self, service, cancellation).await
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
                record.stop(DrainStop::Cancelled, rest);
                break 'work;
            }
            let outcome = match client.converge(service, cancellation).await {
                Converged::Moved { moves } => DrainOutcome::Moved { moves },
                Converged::NothingToMove => DrainOutcome::NothingToMove,
                Converged::Failed { moves, failure } => DrainOutcome::Failed { moves, failure },
                Converged::Stays { reason } => DrainOutcome::Stays { reason },
                Converged::Stopped { moves, stop } => {
                    record.push(service.clone(), DrainOutcome::Interrupted { moves });
                    record.stop(stop, after);
                    break 'work;
                }
            };
            record.push(service.clone(), outcome);
            rest = after;
        }
    }
    let remaining = match client.still_on(&server.id).await {
        Ok(services) => Remaining::Observed {
            unchosen: services
                .iter()
                .filter(|service| {
                    !service.namespace.is_reserved() && !scope.includes(&service.namespace)
                })
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
