//! Per-Server Deployment outcomes.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{SavedEnvironmentIntent, SavedServiceIntent};
use ployz_core::{
    ContainerId, ContainerRuntimeObservation, DependencyHealthFailure, DeployOperation,
    ExecutionError, HealthFailure, HookFailure, LimitsRefusal, MachineAction, MachineId,
    OperationPhase, OperationRow, OperationStatus, OutOfMemory, PullPolicy, RpcError, ServiceName,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{DeploymentStatus, Stored, TargetNode, json_text, now, saved_at};
use crate::error;
use crate::id::OrganizationId;
use crate::storage::{Row, Tx};
use crate::tools::{LogText, Withheld};

/// What one Service's work on one Server is doing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RowState {
    Pending,
    /// `serving` is true once every new Container this Server starts serves,
    /// so stop and remove phases then work on old ones. A Container updated
    /// in place serves throughout.
    Running {
        phase: RowPhase,
        #[serde(default)]
        serving: bool,
    },
    Completed,
    /// `reason` is our sentence; `cause` lists what it came from, outermost first.
    /// `step` is the work that failed, when known. `log` is the failed Container's last
    /// lines, scrubbed, or why they are not shown; absent when no Container was involved.
    Failed {
        reason: String,
        cause: Vec<String>,
        #[serde(default)]
        step: Option<FailedAt>,
        #[serde(default, deserialize_with = "stored_log")]
        log: Option<LogText>,
        /// The Server didn't answer, so it may succeed once it does. False when it
        /// answered with this failure.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        unanswered: bool,
        /// Docker OOM-killed a process of the failed Container.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        out_of_memory: Option<OutOfMemory>,
        #[serde(flatten)]
        #[ts(flatten)]
        stopped_by: StoppedBy,
    },
    /// The Deployment stopped before this work finished.
    NotAttempted,
    /// Reporting stopped before this row's outcome was confirmed.
    Unknown,
}

impl RowState {
    pub(crate) const fn finished(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::NotAttempted | Self::Unknown
        )
    }
}

/// What stopped a Failed row's work that didn't fail on its own, so it reads
/// other than Failed. Stored and sent as a marker beside `failed` rather than a
/// state of its own, so a reader from before it reads Failed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct StoppedBy {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    cancelled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    interrupted: bool,
}

impl StoppedBy {
    /// It failed on its own.
    pub const NOTHING: Self = Self {
        cancelled: false,
        interrupted: false,
    };
    /// The Deployment's cancel stopped it.
    pub const CANCEL: Self = Self {
        cancelled: true,
        interrupted: false,
    };
    /// The Deployment lost its runner, to another Deployment taking the
    /// Environment over or to a lease that lapsed, so the runner stopped it.
    pub const LOST_RUNNER: Self = Self {
        cancelled: false,
        interrupted: true,
    };
}

/// The step a failed row's work was on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FailedAt {
    Build,
    Pull,
    Create,
    Start,
    Health,
    Hook,
    Stop,
    Remove,
    Volume,
}

impl FailedAt {
    fn of(error: &ExecutionError, last: Option<&RowState>) -> Option<Self> {
        let from_phase = || match last {
            Some(RowState::Running { phase, .. }) => phase.failed_at(),
            Some(RowState::Failed { step, .. }) => *step,
            Some(
                RowState::Pending
                | RowState::Completed
                | RowState::NotAttempted
                | RowState::Unknown,
            )
            | None => None,
        };
        match error {
            ExecutionError::Machine { action, .. } => match action {
                MachineAction::PrepareVolumes | MachineAction::RemoveVolume => Some(Self::Volume),
                MachineAction::CreateContainer => Some(Self::Create),
                MachineAction::StartContainer => Some(Self::Start),
                MachineAction::StopContainer => Some(Self::Stop),
                MachineAction::RemoveContainer => Some(Self::Remove),
                MachineAction::PullImage => Some(Self::Pull),
                MachineAction::InspectContainer
                | MachineAction::UpdateContainerLimits
                | MachineAction::ListContainers
                | MachineAction::ReadRoutes => from_phase(),
            },
            ExecutionError::Health { .. } | ExecutionError::DependencyHealth { .. } => {
                Some(Self::Health)
            }
            ExecutionError::Hook { .. } => Some(Self::Hook),
            ExecutionError::NewerServing { .. } => None,
            ExecutionError::Cancelled => from_phase(),
        }
    }
}

/// Rows stored before logs were scrubbed hold raw lines. They are never shown again.
fn stored_log<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<LogText>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Stored {
        Raw(Vec<String>),
        Scrubbed(Option<LogText>),
    }
    Ok(match Stored::deserialize(deserializer)? {
        Stored::Raw(lines) if lines.is_empty() => None,
        Stored::Raw(_) => Some(LogText::Withheld {
            why: Withheld::NoSecretContext,
        }),
        Stored::Scrubbed(log) => log,
    })
}

/// An [`OperationPhase`] without its clocks, which change on every tick.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RowPhase {
    Starting,
    CreatingContainer,
    StartingContainer,
    WaitingForHealth,
    WaitingForHook,
    WaitingForSwitch,
    StoppingContainer,
    RemovingContainer,
    RemovingVolume,
    UpdatingContainer,
    Compensating,
}

impl RowPhase {
    const fn failed_at(self) -> Option<FailedAt> {
        match self {
            Self::CreatingContainer => Some(FailedAt::Create),
            Self::StartingContainer => Some(FailedAt::Start),
            Self::WaitingForHealth => Some(FailedAt::Health),
            Self::WaitingForHook => Some(FailedAt::Hook),
            Self::StoppingContainer => Some(FailedAt::Stop),
            Self::RemovingContainer => Some(FailedAt::Remove),
            Self::RemovingVolume => Some(FailedAt::Volume),
            Self::Starting
            | Self::WaitingForSwitch
            | Self::UpdatingContainer
            | Self::Compensating => None,
        }
    }
}

impl From<&OperationPhase> for RowPhase {
    fn from(phase: &OperationPhase) -> Self {
        match phase {
            OperationPhase::Starting => Self::Starting,
            OperationPhase::CreatingContainer => Self::CreatingContainer,
            OperationPhase::StartingContainer => Self::StartingContainer,
            OperationPhase::WaitingForHealth { .. } => Self::WaitingForHealth,
            OperationPhase::WaitingForHook { .. } => Self::WaitingForHook,
            OperationPhase::WaitingForSwitch { .. } => Self::WaitingForSwitch,
            OperationPhase::StoppingContainer => Self::StoppingContainer,
            OperationPhase::RemovingContainer => Self::RemovingContainer,
            OperationPhase::RemovingVolume => Self::RemovingVolume,
            OperationPhase::UpdatingContainer => Self::UpdatingContainer,
            OperationPhase::Compensating => Self::Compensating,
        }
    }
}

/// One Service's work on one Server, as a Deployment's view shows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ServerRow {
    /// The Machine whose work this row records, independent of its display name.
    pub machine_id: MachineId,
    /// The Server's name, or its Machine ID when it has none.
    pub server: String,
    #[serde(flatten)]
    pub state: RowState,
    /// When its work started, in Unix seconds.
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    /// When it finished, in Unix seconds.
    #[ts(type = "number | null")]
    pub finished_at: Option<i64>,
    /// Every state the row passed through, oldest first, a phase that recurs listed
    /// again. Absent for rows recorded before history was kept and for Volume and
    /// Config rows, which merge several Services' rows. The row's current state is
    /// `state`, which can differ from the last entry when a lost run shows Unknown.
    #[serde(default)]
    pub history: Option<Vec<RowTransition>>,
}

/// One state a row entered and when, in Unix seconds. A failed state's `log` is
/// left on the row itself, not repeated here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RowTransition {
    #[ts(type = "number")]
    pub at: i64,
    #[serde(flatten)]
    pub state: RowState,
}

impl RowTransition {
    fn new(at: i64, state: &RowState) -> Self {
        let mut state = state.clone();
        if let RowState::Failed { log, .. } = &mut state {
            *log = None;
        }
        Self { at, state }
    }
}

/// `history` with `state` appended at `at`, unless it is already the latest.
fn entered(mut history: Vec<RowTransition>, at: i64, state: &RowState) -> Vec<RowTransition> {
    let next = RowTransition::new(at, state);
    if history.last().is_none_or(|last| last.state != next.state) {
        history.push(next);
    }
    history
}

/// A row whose state changed, as the runner records it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ServerProgress {
    /// The runtime Service.
    pub service: ServiceName,
    pub machine: MachineId,
    /// The Server's name, or its Machine ID when it has none.
    pub server: String,
    pub state: RowState,
    /// Only cancelled work stopped it. The Store marks it Cancelled when the
    /// Deployment's cancel asked for that, Interrupted when the Deployment lost
    /// its runner, and leaves it Failed otherwise.
    #[serde(default)]
    pub stopped: bool,
}

/// Why something failed, as users read it: our sentence, then what it came from,
/// outermost first.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub reason: String,
    #[serde(default)]
    pub cause: Vec<String>,
    /// Deploying the same thing again fails this way again: against the same
    /// Cluster, the refusal holds until something staged changes, so a retry alone
    /// never gets past it. Rows from before this field read false, as does every
    /// failure a retry may get past.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub same_again: bool,
}

impl From<String> for Failure {
    fn from(reason: String) -> Self {
        Self {
            reason,
            cause: Vec::new(),
            same_again: false,
        }
    }
}

impl From<&str> for Failure {
    fn from(reason: &str) -> Self {
        reason.to_owned().into()
    }
}

impl From<&RpcError> for Failure {
    fn from(error: &RpcError) -> Self {
        match error.refusal() {
            Some(refusal) => refusal.to_string().into(),
            None => Self {
                reason: error.message.clone(),
                cause: ployz_core::error_chain::causes(error),
                same_again: false,
            },
        }
    }
}

/// "Server <name>" for the Machine `error` names, by its name in `operations` when known.
fn server_named(error: &RpcError, operations: &[OperationRow]) -> Option<String> {
    let id = error.details.get("machine_id")?.as_str()?;
    Some(server_of(id, operations))
}

/// "Server <name>" for Machine `id`, by its name in `operations`, else its id.
fn server_of(id: &str, operations: &[OperationRow]) -> String {
    let name = operations
        .iter()
        .find(|row| row.machine_id.to_string() == id)
        .and_then(|row| row.machine_name.as_ref())
        .map_or_else(|| id.to_owned(), ToString::to_string);
    format!("Server {name}")
}

/// What the transport said when the call to `machine` itself went unanswered: the
/// client's message names that Machine by its ID or its name, and no other Machine
/// is named as the silent one.
fn unanswered_on<'e>(
    error: &'e RpcError,
    machine: &MachineId,
    operations: &[OperationRow],
) -> Option<&'e str> {
    if !error.target_unanswered() {
        return None;
    }
    let id = machine.to_string();
    if let Some(named) = error
        .details
        .get("machine_id")
        .and_then(serde_json::Value::as_str)
        && named != id
    {
        return None;
    }
    let server = server_of(&id, operations);
    [id.as_str(), server.trim_start_matches("Server ")]
        .into_iter()
        .find_map(|known| {
            error
                .message
                .strip_prefix(&format!("Machine {known} did not answer: "))
        })
}

impl Failure {
    /// `error` for users. A Server that refused for want of a capability is named
    /// from `operations`, and its sentence stands alone.
    #[must_use]
    pub fn of(error: &ExecutionError, operations: &[OperationRow]) -> Self {
        Self::on(error, operations, None)
    }

    /// [`Self::of`] for an operation that ran on `machine`. A full disk then names
    /// its Server: on a multi-Server deploy only one of them filled.
    #[must_use]
    pub fn on(
        error: &ExecutionError,
        operations: &[OperationRow],
        machine: Option<MachineId>,
    ) -> Self {
        let (reason, cause) = match error {
            // The Leave explains whatever the copy did, so nothing of it is a cause.
            ExecutionError::Machine { error, .. } if let Some(departure) = error.departure() => (
                format!(
                    "Server {} was removed while the deploy ran",
                    departure.server
                ),
                Vec::new(),
            ),
            ExecutionError::Health {
                departing: Some(departure),
                ..
            } => (
                format!(
                    "Server {} was removed while the deploy ran",
                    departure.server
                ),
                Vec::new(),
            ),
            ExecutionError::Machine { error, .. } if let Some(refusal) = error.refusal() => {
                let name = operations
                    .iter()
                    .find(|row| row.machine_id.to_string() == refusal.target)
                    .and_then(|row| row.machine_name.as_ref())
                    .map(ToString::to_string);
                let refusal = match name {
                    Some(name) => refusal.naming(name),
                    None => refusal,
                };
                (refusal.to_string(), Vec::new())
            }
            // The entry Machine stopped listing the Server the call went to, as after
            // `ployz server rm`, so the plan's name for it says what happened.
            ExecutionError::Machine { error, .. }
                if let Some(id) = machine
                    && error.server_unlisted(&id) =>
            {
                (
                    format!(
                        "{} was removed while the deploy ran",
                        server_of(&id.to_string(), operations)
                    ),
                    Vec::new(),
                )
            }
            // The call to the row's Server went unanswered, by the router's or this
            // client's deadline, and the client's message leads with that Machine as
            // it knew it. The row knows its name, so the reason names it and the cause
            // keeps what the transport said. Another Machine's silence, as a listing
            // across the Cluster meets it, names that Machine and keeps its words; a
            // limits update keeps its own outcome below.
            ExecutionError::Machine { action, error }
                if *action != MachineAction::UpdateContainerLimits
                    && let Some(id) = machine
                    && let Some(said) = unanswered_on(error, &id, operations) =>
            {
                let mut cause = vec![said.to_owned()];
                cause.extend(ployz_core::error_chain::causes(error));
                (
                    format!(
                        "{} did not answer during {action}",
                        server_of(&id.to_string(), operations)
                    ),
                    cause,
                )
            }
            ExecutionError::Machine {
                action: MachineAction::PullImage,
                error,
            } if let Some(missing) = error.missing_image() => (
                match missing.policy {
                    Some(PullPolicy::Never) => format!(
                        "Image {} is not on this Server, and the pull policy is Never",
                        missing.image
                    ),
                    _ => format!(
                        "Image {} not found, or its registry needs credentials",
                        missing.image
                    ),
                },
                ployz_core::error_chain::causes(error),
            ),
            ExecutionError::Machine {
                action: MachineAction::PullImage,
                error,
            } if let Some(full) = error.disk_full() => (
                match machine {
                    Some(id) => full.on(&server_of(&id.to_string(), operations)),
                    None => full.to_string(),
                },
                ployz_core::error_chain::causes(error),
            ),
            ExecutionError::Machine {
                action: MachineAction::PullImage,
                error,
            } if let Some(host) = error.unresolved_host() => (
                format!(
                    "Host {host} does not resolve, so the image could not be pulled. Check the image's registry, or this Server's DNS"
                ),
                ployz_core::error_chain::causes(error),
            ),
            ExecutionError::Machine {
                action: MachineAction::UpdateContainerLimits,
                error,
            } => {
                let server = server_named(error, operations);
                let mut cause = vec![error.message.clone()];
                cause.extend(ployz_core::error_chain::causes(error));
                // A refusal comes before any Docker change, so the old limits hold.
                // Anything else, such as a Docker error, an unapplied read-back or a
                // lost answer, leaves the live limits unknown.
                let outcome = match LimitsRefusal::from_details(&error.details) {
                    Some(LimitsRefusal::NotRunning) => {
                        "found the Container not running, so the new limits do not apply"
                    }
                    Some(_) => {
                        "did not apply the new limits. The Container keeps its previous limits"
                    }
                    None => "could not confirm the new limits",
                };
                (
                    format!(
                        "{} {outcome}",
                        server.unwrap_or_else(|| "The Server".to_owned())
                    ),
                    cause,
                )
            }
            // The Server may have done the work: only its answer was lost.
            ExecutionError::Machine { action, error } if error.reply_lost() => {
                let mut cause = vec![error.message.clone()];
                cause.extend(ployz_core::error_chain::causes(error));
                let server = server_named(error, operations).unwrap_or_else(|| "the Server".into());
                (
                    format!(
                        "The connection to {server} dropped during {action}, so it may have happened anyway"
                    ),
                    cause,
                )
            }
            ExecutionError::Machine { action, error } => {
                let mut cause = vec![error.message.clone()];
                cause.extend(ployz_core::error_chain::causes(error));
                (format!("{action} failed"), cause)
            }
            ExecutionError::Health {
                out_of_memory: Some(OutOfMemory { memory_bytes }),
                failure,
                ..
            } => (
                match memory_bytes {
                    Some(bytes) => format!(
                        "The Container ran out of memory under its {} memLimit",
                        crate::memory_size(bytes.get() as f64 / 1e9)
                    ),
                    None => "The Container ran out of memory".to_owned(),
                },
                vec![failure.to_string()],
            ),
            ExecutionError::Health {
                failure, last_run, ..
            } => {
                let exit_code = last_run.as_ref().and_then(|run| run.exit_code);
                let reason = match failure {
                    HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Restarting,
                        ..
                    } => exit_code.map_or_else(
                        || "The Container is restarting".to_owned(),
                        |code| format!("The Container exited with code {code} and is restarting"),
                    ),
                    HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Exited { code },
                        ..
                    } => format!("The Container exited with code {code}"),
                    HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Dead,
                        ..
                    } => "The Container stopped".to_owned(),
                    HealthFailure::Cancelled
                    | HealthFailure::TimedOut { .. }
                    | HealthFailure::Runtime { .. } => {
                        "The Container failed its health check".to_owned()
                    }
                };
                let mut cause = match failure {
                    // The reason already says it.
                    HealthFailure::Runtime {
                        observation:
                            ContainerRuntimeObservation::Restarting
                            | ContainerRuntimeObservation::Exited { .. }
                            | ContainerRuntimeObservation::Dead,
                        last_check: None,
                    } => Vec::new(),
                    HealthFailure::Cancelled
                    | HealthFailure::TimedOut { .. }
                    | HealthFailure::Runtime { .. } => vec![failure.to_string()],
                };
                // Only a Container that stopped on its own ends with its last line. A
                // running one's check result is the cause; what it printed while Ployz
                // stopped it is not.
                if let HealthFailure::Runtime {
                    observation:
                        ContainerRuntimeObservation::Restarting
                        | ContainerRuntimeObservation::Exited { .. }
                        | ContainerRuntimeObservation::Dead,
                    ..
                } = failure
                {
                    cause.extend(last_run.as_ref().and_then(|run| run.last_line.clone()));
                }
                (reason, cause)
            }
            ExecutionError::DependencyHealth {
                dependency,
                failure,
            } => (
                format!("Dependency {dependency} failed its health gate"),
                match failure {
                    DependencyHealthFailure::Observation { error } => {
                        let mut cause = vec![
                            "Container observation failed".to_owned(),
                            error.message.clone(),
                        ];
                        cause.extend(ployz_core::error_chain::causes(error));
                        cause
                    }
                    DependencyHealthFailure::Cancelled
                    | DependencyHealthFailure::NoContainers
                    | DependencyHealthFailure::NotRouted { .. }
                    | DependencyHealthFailure::Container { .. } => vec![failure.to_string()],
                },
            ),
            ExecutionError::Hook { failure, .. } => (
                "The setup or pre-deploy command failed".to_owned(),
                match failure {
                    HookFailure::Cancelled {
                        stop_error: Some(error),
                    }
                    | HookFailure::TimedOut {
                        stop_error: Some(error),
                    } => {
                        let stage = if matches!(failure, HookFailure::Cancelled { .. }) {
                            "cancelled"
                        } else {
                            "timed out"
                        };
                        let mut cause = vec![
                            stage.to_owned(),
                            "Stopping the hook failed".to_owned(),
                            error.message.clone(),
                        ];
                        cause.extend(ployz_core::error_chain::causes(error));
                        cause
                    }
                    HookFailure::Cancelled { stop_error: None }
                    | HookFailure::TimedOut { stop_error: None }
                    | HookFailure::Exit { .. } => vec![failure.to_string()],
                },
            ),
            ExecutionError::NewerServing { service } => (
                format!("A newer deploy is already serving {service}"),
                Vec::new(),
            ),
            ExecutionError::Cancelled => ("Cancelled".to_owned(), Vec::new()),
        };
        Self {
            reason,
            cause,
            same_again: false,
        }
    }
}

type Key = (ServiceName, MachineId);

/// Whether `operation` leaves a Service Container serving the new spec: a fresh
/// start, or a running Container updated in place.
#[must_use]
pub const fn starts_container(operation: &DeployOperation) -> bool {
    matches!(
        operation,
        DeployOperation::RunContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::UpdateContainer(_)
    )
}

/// Turns a runner's Deploy Progress snapshots into the rows that changed, so it
/// records only those.
#[derive(Debug, Default)]
pub struct RowTracker {
    last: BTreeMap<Key, RowState>,
    containers: BTreeMap<u32, ContainerId>,
    /// The operation each Server's row failed with, and whether it really failed.
    failed: BTreeMap<Key, (u32, bool)>,
}

impl RowTracker {
    /// The rows of `snapshot` whose state changed since the last snapshot.
    pub fn changes(&mut self, snapshot: &[OperationRow]) -> Vec<ServerProgress> {
        let mut grouped = BTreeMap::<Key, (String, Vec<_>, Option<bool>)>::new();
        self.failed.clear();
        for row in snapshot {
            let Some(service) = row.service_name() else {
                continue;
            };
            let key = (service.clone(), row.machine_id);
            if let Some(container) = container(row, self.containers.get(&row.index).copied()) {
                self.containers.insert(row.index, container);
            } else {
                self.containers.remove(&row.index);
            }
            if let OperationStatus::Failed { error } = &row.status {
                let real = !error.cancelled();
                let kept = self.failed.entry(key.clone()).or_insert((row.index, real));
                if real && !kept.1 {
                    *kept = (row.index, real);
                }
            }
            let server = row
                .machine_name
                .as_ref()
                .map_or_else(|| row.machine_id.to_string(), ToString::to_string);
            let (_, statuses, serving) = grouped
                .entry(key)
                .or_insert_with(|| (server, Vec::new(), None));
            statuses.push((&row.status, starts_container(&row.operation)));
            if starts_container(&row.operation) {
                let serves = matches!(row.operation, DeployOperation::UpdateContainer(_))
                    || matches!(row.status, OperationStatus::Completed);
                *serving = Some(serving.unwrap_or(true) && serves);
            }
        }
        let mut changes = Vec::new();
        for (key, (server, statuses, serving)) in grouped {
            let container = self
                .failed
                .get(&key)
                .is_some_and(|(index, _)| self.containers.contains_key(index));
            let state = state(
                snapshot,
                key.1,
                &statuses,
                self.last.get(&key),
                container,
                serving.unwrap_or(false),
            );
            if self.last.get(&key) == Some(&state) {
                continue;
            }
            self.last.insert(key.clone(), state.clone());
            let stopped = self.failed.get(&key).is_some_and(|(_, real)| !real);
            changes.push(ServerProgress {
                service: key.0,
                machine: key.1,
                server,
                state,
                stopped,
            });
        }
        changes
    }

    /// The failed operation's Container, when its progress identified one.
    #[must_use]
    pub fn container(&self, row: &ServerProgress) -> Option<ContainerId> {
        self.failed
            .get(&(row.service.clone(), row.machine))
            .and_then(|(index, _)| self.containers.get(index))
            .copied()
    }
}

fn container(row: &OperationRow, last: Option<ContainerId>) -> Option<ContainerId> {
    match &row.status {
        OperationStatus::Running { phase } => match phase {
            OperationPhase::WaitingForHealth { container_id, .. }
            | OperationPhase::WaitingForHook { container_id, .. }
            | OperationPhase::WaitingForSwitch { container_id } => Some(*container_id),
            OperationPhase::StoppingContainer
            | OperationPhase::RemovingContainer
            | OperationPhase::UpdatingContainer => row.operation.container_id(),
            OperationPhase::Starting
            | OperationPhase::CreatingContainer
            | OperationPhase::StartingContainer
            | OperationPhase::RemovingVolume => None,
            OperationPhase::Compensating => last,
        },
        OperationStatus::Failed { error } => match error {
            ExecutionError::Health { container_id, .. }
            | ExecutionError::Hook { container_id, .. } => Some(*container_id),
            ExecutionError::Machine { action, .. } => match action {
                MachineAction::StopContainer
                | MachineAction::RemoveContainer
                | MachineAction::UpdateContainerLimits => row.operation.container_id(),
                MachineAction::InspectContainer => last,
                MachineAction::CreateContainer
                | MachineAction::StartContainer
                | MachineAction::PrepareVolumes
                | MachineAction::RemoveVolume
                | MachineAction::PullImage
                | MachineAction::ListContainers
                | MachineAction::ReadRoutes => None,
            },
            ExecutionError::DependencyHealth { .. }
            | ExecutionError::NewerServing { .. }
            | ExecutionError::Cancelled => None,
        },
        OperationStatus::Completed => last,
        OperationStatus::Pending | OperationStatus::Unexecuted => None,
    }
}

/// The state of one Service's rows on `machine`. A failed row's `log` waits as
/// unanswered until the runner reads its Container's tail.
fn state(
    operations: &[OperationRow],
    machine: MachineId,
    statuses: &[(&OperationStatus, bool)],
    last: Option<&RowState>,
    container: bool,
    serving: bool,
) -> RowState {
    let (mut failed, mut cancelled, mut running, mut switching) = (None, None, None, None);
    let (mut pending, mut unstarted, mut completed) = (0, 0, 0);
    for (status, starts) in statuses {
        match status {
            OperationStatus::Failed { error } if error.cancelled() => {
                cancelled = cancelled.or(Some(error));
            }
            OperationStatus::Failed { error } => failed = failed.or(Some(error)),
            OperationStatus::Running {
                phase: phase @ OperationPhase::WaitingForSwitch { .. },
            } => switching = switching.or(Some(phase)),
            OperationStatus::Running { phase } => running = running.or(Some(phase)),
            OperationStatus::Pending => {
                pending += 1;
                unstarted += usize::from(*starts);
            }
            OperationStatus::Completed => completed += 1,
            OperationStatus::Unexecuted => {}
        }
    }
    if let Some(error) = failed.or(cancelled) {
        let Failure { reason, cause, .. } = Failure::on(error, operations, Some(machine));
        return RowState::Failed {
            reason,
            cause,
            step: FailedAt::of(error, last),
            log: container.then_some(LogText::Withheld {
                why: Withheld::Unanswered,
            }),
            unanswered: matches!(error, ExecutionError::Machine { error, .. }
                if error.target_unanswered() || error.reply_lost()),
            out_of_memory: match error {
                ExecutionError::Health { out_of_memory, .. } => *out_of_memory,
                ExecutionError::Machine { .. }
                | ExecutionError::DependencyHealth { .. }
                | ExecutionError::Hook { .. }
                | ExecutionError::NewerServing { .. }
                | ExecutionError::Cancelled => None,
            },
            stopped_by: StoppedBy::NOTHING,
        };
    }
    if let Some(phase) = running {
        return RowState::Running {
            phase: phase.into(),
            serving,
        };
    }
    if let Some(phase) = switching {
        // A Server is ready for the switch once each of its new Containers is.
        return RowState::Running {
            phase: if unstarted == 0 {
                phase.into()
            } else {
                RowPhase::Starting
            },
            serving,
        };
    }
    // A new copy completes only once its switch lands, so a Server whose old copy's
    // removal a later failure left unexecuted still switched (#3684).
    let switched = statuses.iter().any(|(_, starts)| *starts)
        && statuses.iter().all(|(status, starts)| match status {
            OperationStatus::Completed => true,
            OperationStatus::Unexecuted => !starts,
            OperationStatus::Pending
            | OperationStatus::Running { .. }
            | OperationStatus::Failed { .. } => false,
        });
    if completed == statuses.len() || switched {
        RowState::Completed
    } else if pending == statuses.len() {
        RowState::Pending
    } else if pending == 0 {
        RowState::NotAttempted
    } else if let Some(RowState::Running { phase, .. }) = last {
        RowState::Running {
            phase: *phase,
            serving,
        }
    } else {
        RowState::Running {
            phase: RowPhase::Starting,
            serving,
        }
    }
}

pub(super) fn record(
    tx: &mut dyn Tx,
    stored: &Stored,
    rows: &[ServerProgress],
) -> Result<(), RpcError> {
    let now = now();
    // Only the Deployment's own cancel makes the work it stopped read Cancelled,
    // and only losing its runner makes it read Interrupted.
    let by = match stored.summary.status {
        DeploymentStatus::Cancelling => StoppedBy::CANCEL,
        DeploymentStatus::Unknown => StoppedBy::LOST_RUNNER,
        DeploymentStatus::WaitingForCi
        | DeploymentStatus::Queued
        | DeploymentStatus::Running
        | DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Cancelled
        | DeploymentStatus::Superseded => StoppedBy::NOTHING,
    };
    for row in rows {
        let mut state = row.state.clone();
        if let RowState::Failed { stopped_by, .. } = &mut state {
            *stopped_by = if row.stopped { by } else { StoppedBy::NOTHING };
        }
        let state = &state;
        let started = matches!(
            state,
            RowState::Running { .. } | RowState::Completed | RowState::Failed { .. }
        )
        .then_some(now);
        let finished = state.finished().then_some(now);
        let machine = row.machine.to_string();
        let kept = tx.query(
            "SELECT history FROM config_deployment_row \
             WHERE deployment_id = ?1 AND service = ?2 AND machine = ?3",
            &[
                stored.summary.id.as_str().into(),
                row.service.as_str().into(),
                machine.as_str().into(),
            ],
        )?;
        let history = match kept.first() {
            None => Some(Vec::new()),
            Some(kept) => history(kept, 0)?,
        }
        .map(|history| json_text(&entered(history, now, state)));
        tx.execute(
            "INSERT INTO config_deployment_row \
             (deployment_id, service, machine, organization_id, server, state, started, finished, history) \
             SELECT id, ?2, ?3, organization_id, ?4, ?5, ?6, ?7, ?8 \
             FROM config_deployment WHERE id = ?1 \
             ON CONFLICT (deployment_id, service, machine) DO UPDATE SET \
             server = excluded.server, state = excluded.state, \
             started = COALESCE(config_deployment_row.started, excluded.started), \
             finished = COALESCE(config_deployment_row.finished, excluded.finished), \
             history = excluded.history",
            &[
                stored.summary.id.as_str().into(),
                row.service.as_str().into(),
                machine.as_str().into(),
                row.server.as_str().into(),
                json_text(state).as_str().into(),
                started.into(),
                finished.into(),
                history.as_deref().into(),
            ],
        )?;
    }
    Ok(())
}

type FailedAndCancelled = (Vec<(ServiceName, Failure)>, BTreeSet<ServiceName>);

/// Each row that failed on its own, by runtime Service and first to fail on its own
/// first, then the runtime Services one of whose rows the Deployment's cancel stopped.
pub(super) fn failed_or_cancelled(
    tx: &mut dyn Tx,
    stored: &Stored,
) -> Result<FailedAndCancelled, RpcError> {
    let found = tx.query(
        "SELECT service, machine, state, finished, history FROM config_deployment_row \
         WHERE deployment_id = ?1",
        &[stored.summary.id.as_str().into()],
    )?;
    let (mut failed, mut cancelled) = (Vec::new(), BTreeSet::new());
    for row in &found {
        let RowState::Failed {
            reason,
            cause,
            stopped_by,
            ..
        } = row.json(2, "Deployment row")?
        else {
            continue;
        };
        let service = row.parse(0, "Deployment row")?;
        match stopped_by {
            StoppedBy::NOTHING => {
                // A row the cancel stopped before it failed on its own keeps the
                // cancel's finish time, so its own failure dates from its history.
                let at = history(row, 4)?
                    .as_deref()
                    .and_then(failed_on_its_own)
                    .or(row.optional_int(3)?);
                let machine = row.text(1)?.to_owned();
                failed.push((
                    at,
                    service,
                    machine,
                    Failure {
                        reason,
                        cause,
                        same_again: false,
                    },
                ));
            }
            StoppedBy::CANCEL => {
                cancelled.insert(service);
            }
            _ => {}
        }
    }
    failed.sort_by(|a, b| (a.0, &a.1, &a.2).cmp(&(b.0, &b.1, &b.2)));
    let failed = failed
        .into_iter()
        .map(|(_, service, _, failure)| (service, failure))
        .collect();
    Ok((failed, cancelled))
}

/// When the row last began failing on its own.
fn failed_on_its_own(history: &[RowTransition]) -> Option<i64> {
    history
        .iter()
        .rev()
        .take_while(|entry| {
            matches!(
                entry.state,
                RowState::Failed {
                    stopped_by: StoppedBy::NOTHING,
                    ..
                }
            )
        })
        .last()
        .map(|entry| entry.at)
}

pub(super) fn settle(tx: &mut dyn Tx, stored: &Stored) -> Result<(), RpcError> {
    let now = now();
    let open = tx.query(
        "SELECT service, machine, history FROM config_deployment_row \
         WHERE deployment_id = ?1 AND finished IS NULL",
        &[stored.summary.id.as_str().into()],
    )?;
    for row in &open {
        let history = history(row, 2)?
            .map(|history| json_text(&entered(history, now, &RowState::NotAttempted)));
        tx.execute(
            "UPDATE config_deployment_row SET state = ?4, finished = ?5, history = ?6 \
             WHERE deployment_id = ?1 AND service = ?2 AND machine = ?3",
            &[
                stored.summary.id.as_str().into(),
                row.text(0)?.into(),
                row.text(1)?.into(),
                json_text(&RowState::NotAttempted).as_str().into(),
                now.into(),
                history.as_deref().into(),
            ],
        )?;
    }
    Ok(())
}

fn history(row: &Row, index: usize) -> Result<Option<Vec<RowTransition>>, RpcError> {
    row.optional_text(index)?
        .map(|text| {
            serde_json::from_str(text).map_err(|_| error::corrupt("Deployment row history"))
        })
        .transpose()
}

/// The name each Server `organization`'s Deployments ran on went by, as the newest
/// of them knew it.
pub(crate) fn servers(
    tx: &mut dyn Tx,
    organization: &OrganizationId,
) -> Result<BTreeMap<MachineId, String>, RpcError> {
    let found = tx.query(
        "SELECT r.machine, r.server FROM config_deployment_row r \
         JOIN config_deployment d ON d.id = r.deployment_id \
         WHERE r.organization_id = ?1 ORDER BY d.admitted",
        &[organization.as_str().into()],
    )?;
    found
        .iter()
        .map(|row| {
            let machine: MachineId = row.parse(0, "Deployment row Machine")?;
            Ok((machine, row.text(1)?.to_owned()))
        })
        .collect()
}

/// Each runtime Service's rows, by the Machine they ran on.
pub(super) fn by_service(
    tx: &mut dyn Tx,
    stored: &Stored,
) -> Result<BTreeMap<ServiceName, BTreeMap<MachineId, ServerRow>>, RpcError> {
    let found = tx.query(
        "SELECT service, server, state, started, finished, machine, history \
         FROM config_deployment_row \
         WHERE deployment_id = ?1 ORDER BY service, server, machine",
        &[stored.summary.id.as_str().into()],
    )?;
    let lost = matches!(
        stored.summary.status,
        DeploymentStatus::Unknown | DeploymentStatus::Cancelled
    );
    let mut by_service: BTreeMap<ServiceName, BTreeMap<MachineId, ServerRow>> = BTreeMap::new();
    for row in &found {
        let state: RowState = row.json(2, "Deployment row")?;
        let machine_id: MachineId = row.parse(5, "Deployment row Machine")?;
        by_service
            .entry(row.parse(0, "Deployment row")?)
            .or_default()
            .insert(
                machine_id,
                ServerRow {
                    machine_id,
                    server: row.text(1)?.to_owned(),
                    state: if lost && !state.finished() {
                        RowState::Unknown
                    } else {
                        state
                    },
                    started_at: row.optional_int(3)?,
                    finished_at: row.optional_int(4)?,
                    history: history(row, 6)?,
                },
            );
    }
    Ok(by_service)
}

/// The rows of the runtime Services the Deployment's preview plans to remove as
/// obsolete and no node targets: Services this Store doesn't hold, which a full
/// reconciliation removes. Empty without a preview.
pub(super) fn untargeted(
    stored: &Stored,
    by_service: &BTreeMap<ServiceName, BTreeMap<MachineId, ServerRow>>,
) -> BTreeMap<ServiceName, Vec<ServerRow>> {
    let Some(preview) = &stored.run.preview else {
        return BTreeMap::new();
    };
    let targeted: BTreeSet<&ServiceName> =
        stored.nodes.iter().flat_map(TargetNode::runtimes).collect();
    preview
        .would_remove
        .iter()
        .map(|obsolete| &obsolete.name)
        .filter(|service| !targeted.contains(service))
        .filter_map(|service| {
            let rows = by_service.get(service)?;
            Some((service.clone(), rows.values().cloned().collect()))
        })
        .collect()
}

/// Each node's rows. A mounted Volume's or Config's rows read the Deployment's Saved
/// State, which is left in `saved` for the caller to reuse.
pub(super) fn of_nodes(
    tx: &mut dyn Tx,
    stored: &Stored,
    by_service: &BTreeMap<ServiceName, BTreeMap<MachineId, ServerRow>>,
    saved: &mut Option<SavedEnvironmentIntent>,
) -> Result<BTreeMap<String, Vec<ServerRow>>, RpcError> {
    if by_service.is_empty() {
        return Ok(BTreeMap::new());
    }
    let mut nodes = BTreeMap::new();
    for node in &stored.nodes {
        let rows: Vec<_> = node
            .runtimes()
            .into_iter()
            .filter_map(|runtime| by_service.get(runtime))
            .flat_map(|rows| rows.values().cloned())
            .collect();
        if !rows.is_empty() {
            nodes.insert(node.id().to_owned(), rows);
        }
    }
    if !stored
        .nodes
        .iter()
        .any(|node| matches!(node, TargetNode::Volume { .. } | TargetNode::Config { .. }))
    {
        return Ok(nodes);
    }
    let saved = saved.insert(saved_at(
        tx,
        &stored.summary.environment_id,
        stored.summary.saved,
    )?);
    for node in &stored.nodes {
        let id = match node {
            TargetNode::Service { .. } => continue,
            TargetNode::Volume { id, .. } => id.as_str(),
            TargetNode::Config { id, .. } => id.as_str(),
        };
        let mounts = |service: &&SavedServiceIntent| match node {
            TargetNode::Volume { .. } => service
                .volume_attachments
                .iter()
                .any(|mount| mount.volume_resource_id == id),
            TargetNode::Config { .. } => service
                .config_attachments
                .iter()
                .any(|mount| mount.config_resource_id == id),
            TargetNode::Service { .. } => false,
        };
        let mut rows: BTreeMap<MachineId, ServerRow> = BTreeMap::new();
        for service in saved.services.iter().filter(mounts) {
            for (machine, row) in by_service
                .get(&service.config.private_dns)
                .into_iter()
                .flatten()
            {
                rows.entry(*machine)
                    .and_modify(|kept| merge_mounting_row(kept, row))
                    .or_insert_with(|| ServerRow {
                        history: None,
                        ..row.clone()
                    });
            }
        }
        if !rows.is_empty() {
            let mut rows: Vec<_> = rows.into_values().collect();
            rows.sort_by(|a, b| a.server.cmp(&b.server));
            nodes.insert(id.to_owned(), rows);
        }
    }
    Ok(nodes)
}

fn merge_mounting_row(kept: &mut ServerRow, row: &ServerRow) {
    let priority = |state: &RowState| match state {
        RowState::Failed {
            stopped_by: StoppedBy::NOTHING,
            ..
        } => 7,
        RowState::Unknown => 6,
        RowState::Running { .. } => 5,
        RowState::Pending => 4,
        RowState::Failed { .. } => 3,
        RowState::NotAttempted => 2,
        RowState::Completed => 1,
    };
    if priority(&row.state) > priority(&kept.state) {
        kept.state = row.state.clone();
    }
    kept.started_at = match (kept.started_at, row.started_at) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    };
    kept.finished_at = kept.finished_at.zip(row.finished_at).map(|(a, b)| a.max(b));
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerId, ContainerRuntimeObservation, ExecutionError, HealthFailure, HealthObservation,
        HookFailure, LastHealthCheck, LastRun, MachineAction, OperationPhase, OperationRow,
        OperationStatus, RpcError, RpcErrorCode,
    };

    use super::{Failure, RowPhase, RowState};

    fn server(statuses: &[OperationStatus]) -> RowState {
        let statuses: Vec<_> = statuses.iter().map(|status| (status, true)).collect();
        super::state(&[], any_machine(), &statuses, None, true, false)
    }

    #[test]
    fn a_server_waits_for_the_switch_only_once_each_new_container_is_ready() {
        let switch = OperationStatus::Running {
            phase: OperationPhase::WaitingForSwitch {
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
            },
        };
        let creating = OperationStatus::Running {
            phase: OperationPhase::CreatingContainer,
        };
        let running = |phase| RowState::Running {
            phase,
            serving: false,
        };
        assert_eq!(
            server(&[switch.clone(), creating]),
            running(RowPhase::CreatingContainer)
        );
        assert_eq!(
            server(&[switch.clone(), OperationStatus::Pending]),
            running(RowPhase::Starting)
        );
        assert_eq!(
            server(&[switch.clone(), switch.clone()]),
            running(RowPhase::WaitingForSwitch)
        );
        assert_eq!(
            super::state(
                &[],
                any_machine(),
                &[(&switch, true), (&OperationStatus::Pending, false)],
                None,
                true,
                false
            ),
            running(RowPhase::WaitingForSwitch),
            "the old copy's removal waits for the switch, not the Server"
        );
    }

    /// A later failure that aborts the run leaves the old copy's removal unexecuted
    /// after the new copy switched, which still reads as switched (#3684).
    #[test]
    fn a_server_whose_new_copy_switched_reads_completed_when_only_its_removal_did_not_run() {
        let read = |start: &OperationStatus| {
            super::state(
                &[],
                any_machine(),
                &[(start, true), (&OperationStatus::Unexecuted, false)],
                None,
                true,
                false,
            )
        };
        assert_eq!(read(&OperationStatus::Completed), RowState::Completed);
        assert_eq!(
            read(&OperationStatus::Unexecuted),
            RowState::NotAttempted,
            "a copy whose switch never landed did not start"
        );
    }

    #[test]
    fn only_cancelled_work_is_stopped_and_a_real_failure_names_the_row() {
        let container_id = ContainerId::parse("a".repeat(64)).unwrap();
        let failed = |error| OperationStatus::Failed { error };
        let create = || {
            failed(ExecutionError::Machine {
                action: MachineAction::CreateContainer,
                error: RpcError {
                    code: RpcErrorCode::Internal,
                    message: "no space left on device".into(),
                    details: serde_json::Value::Null,
                    cause: Vec::new(),
                },
            })
        };
        let hook = |stop_error| {
            failed(ExecutionError::Hook {
                container_id,
                failure: HookFailure::Cancelled { stop_error },
            })
        };
        let tracked = |statuses: &[OperationStatus]| {
            let machine_id = "a".repeat(32);
            let rows: Vec<OperationRow> = serde_json::from_value(serde_json::Value::Array(
                statuses
                    .iter()
                    .enumerate()
                    .map(|(index, status)| serde_json::json!({
                        "index": index, "machine_id": machine_id, "service_name": "web",
                        "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
                        "status": status
                    }))
                    .collect(),
            ))
            .unwrap();
            let row = super::RowTracker::default().changes(&rows).remove(0);
            let RowState::Failed { reason, .. } = row.state else {
                panic!("expected a failed row, got {:?}", row.state);
            };
            (row.stopped, reason)
        };
        let cancelled = || failed(ExecutionError::Cancelled);
        assert_eq!(tracked(&[cancelled()]), (true, "Cancelled".to_owned()));
        assert!(tracked(&[hook(None)]).0);
        assert!(tracked(&[cancelled(), OperationStatus::Completed]).0);
        assert!(!tracked(&[create()]).0);
        let (stopped, reason) = tracked(&[cancelled(), create()]);
        assert!(!stopped, "a real failure beside a cancel's is not stopped");
        assert_ne!(reason, "Cancelled", "the row names its real failure");
        assert!(
            !tracked(&[hook(Some(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "stop timed out".into(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            }))])
            .0,
            "a hook the cancel could not stop may still run"
        );
    }

    fn any_machine() -> ployz_core::MachineId {
        ployz_core::MachineId::parse("1".repeat(32)).unwrap()
    }

    /// A failure marked with the Leave that overlapped it names the removed Server
    /// and nothing of what the copy did, whether the Server answered an error or an
    /// unhealthy copy (#2924).
    #[test]
    fn a_failure_a_leave_overlapped_names_the_removed_server() {
        let server = ployz_core::MachineName::parse("web-2").unwrap();
        let failed = Failure::of(
            &ExecutionError::Machine {
                action: MachineAction::CreateContainer,
                error: RpcError {
                    code: RpcErrorCode::Internal,
                    message: "detail temporarily unavailable".into(),
                    details: serde_json::Value::Null,
                    cause: Vec::new(),
                }
                .overlapping_leave(&server),
            },
            &[],
        );
        assert_eq!(
            failed.reason,
            "Server web-2 was removed while the deploy ran"
        );
        assert!(failed.cause.is_empty(), "{failed:?}");

        let exited = Failure::of(
            &ExecutionError::Health {
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                failure: HealthFailure::Runtime {
                    observation: ContainerRuntimeObservation::Exited { code: 0 },
                    last_check: None,
                },
                out_of_memory: None,
                last_run: None,
                departing: Some(ployz_core::Departure { server }),
            },
            &[],
        );
        assert_eq!(
            exited.reason,
            "Server web-2 was removed while the deploy ran"
        );
        assert!(exited.cause.is_empty(), "{exited:?}");
    }

    fn reason(observation: ContainerRuntimeObservation) -> String {
        failure(observation).reason
    }

    fn failure(observation: ContainerRuntimeObservation) -> Failure {
        Failure::of(
            &ExecutionError::Health {
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                failure: HealthFailure::Runtime {
                    observation,
                    last_check: None,
                },
                out_of_memory: None,
                last_run: None,
                departing: None,
            },
            &[],
        )
    }

    #[test]
    fn a_server_without_a_capability_is_named_and_told_to_upgrade() {
        let machine_id = "a".repeat(32);
        let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
            "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
            "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
            "status": {"type": "pending"}
        }]))
        .unwrap();
        let error = ExecutionError::Machine {
            action: MachineAction::PullImage,
            error: RpcError {
                code: RpcErrorCode::Unsupported,
                message: "unimplemented".into(),
                details: serde_json::json!({"capability": "ployz.serving.routes.v1", "target": machine_id}),
                cause: vec!["unimplemented".into()],
            },
        };
        let named = Failure::of(&error, &operations);
        assert_eq!(
            named.reason,
            "Server prod-1 needs a newer Ployz. It cannot serve ployz.serving.routes.v1."
        );
        assert!(named.cause.is_empty());
        assert_eq!(
            Failure::of(&error, &[]).reason,
            format!(
                "Server {machine_id} needs a newer Ployz. It cannot serve ployz.serving.routes.v1."
            )
        );
    }

    /// A Server removed while the deploy ran is gone from the entry Machine's routing
    /// by its turn, which refuses with the ID it looked up. The row the call went to
    /// names the Server (#3151).
    #[test]
    fn a_server_removed_while_the_deploy_ran_is_named_from_its_row() {
        let machine_id = "a".repeat(32);
        let machine = ployz_core::MachineId::parse(&machine_id).unwrap();
        let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
            "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
            "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
            "status": {"type": "pending"}
        }]))
        .unwrap();
        let not_found = |details| ExecutionError::Machine {
            action: MachineAction::CreateContainer,
            error: RpcError {
                code: RpcErrorCode::NotFound,
                message: format!("No Server named {machine_id:?}"),
                details,
                cause: Vec::new(),
            },
        };
        let removed = not_found(serde_json::json!({
            "reason": "no_such_server",
            "targets": [machine_id],
        }));
        let named = Failure::on(&removed, &operations, Some(machine));
        assert_eq!(
            named.reason,
            "Server prod-1 was removed while the deploy ran"
        );
        assert!(named.cause.is_empty());
        assert!(!named.same_again);
        // The row a reader tracks names it too, as Cloud's per-Server row shows it.
        let failed: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
            "index": 0, "machine_id": machine_id, "machine_name": "prod-1", "service_name": "web",
            "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
            "status": OperationStatus::Failed { error: removed.clone() },
        }]))
        .unwrap();
        let tracked = super::RowTracker::default().changes(&failed);
        let Some(RowState::Failed { reason, .. }) = tracked.first().map(|row| &row.state) else {
            panic!("expected a failed row, got {tracked:?}");
        };
        assert_eq!(reason, "Server prod-1 was removed while the deploy ran");
        assert_eq!(
            Failure::on(&removed, &[], Some(machine)).reason,
            format!("Server {machine_id} was removed while the deploy ran")
        );
        // A reader that doesn't know which Machine the call went to keeps the answer.
        assert_eq!(
            Failure::of(&removed, &operations).reason,
            "create Container failed"
        );
        // A daemon from before the tag, or a Server's own not-found, reads as before.
        let untagged = not_found(serde_json::json!({}));
        let kept = Failure::on(&untagged, &operations, Some(machine));
        assert_eq!(kept.reason, "create Container failed");
        assert_eq!(kept.cause, [format!("No Server named {machine_id:?}")]);
    }

    /// A Server that left a call unanswered is named from its row, not by the id the
    /// client's deadline put in the message (#3277).
    #[test]
    fn a_server_that_did_not_answer_is_named_from_its_row() {
        let machine_id = "a".repeat(32);
        let machine = ployz_core::MachineId::parse(&machine_id).unwrap();
        let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
            "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
            "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
            "status": {"type": "pending"}
        }]))
        .unwrap();
        let unanswered = |said: String| ExecutionError::Machine {
            action: MachineAction::CreateContainer,
            error: RpcError::unanswered_target(RpcErrorCode::Unavailable, said),
        };
        let by_id = unanswered(format!(
            "Machine {machine_id} did not answer: target Machine RPC timed out"
        ));
        let named = Failure::on(&by_id, &operations, Some(machine));
        assert_eq!(
            named.reason,
            "Server prod-1 did not answer during create Container"
        );
        assert_eq!(named.cause, ["target Machine RPC timed out"]);
        // The deploy names a Machine it already knows silent by name.
        let by_name = unanswered("Machine prod-1 did not answer: deadline has elapsed".into());
        let named = Failure::on(&by_name, &operations, Some(machine));
        assert_eq!(
            named.reason,
            "Server prod-1 did not answer during create Container"
        );
        assert_eq!(named.cause, ["deadline has elapsed"]);
        // Another Machine's silence, met listing across the Cluster, is not this row's.
        let other = unanswered("Machine prod-2 did not answer: deadline has elapsed".into());
        let kept = Failure::on(&other, &operations, Some(machine));
        assert_eq!(kept.reason, "create Container failed");
        assert_eq!(
            kept.cause,
            ["Machine prod-2 did not answer: deadline has elapsed"]
        );
        let mut tagged = RpcError::unanswered_target(
            RpcErrorCode::Unavailable,
            format!("Machine {machine_id} did not answer: deadline has elapsed"),
        );
        if let Some(details) = tagged.details.as_object_mut() {
            details.insert("machine_id".into(), serde_json::json!("b".repeat(32)));
        }
        let elsewhere = ExecutionError::Machine {
            action: MachineAction::CreateContainer,
            error: tagged,
        };
        assert_eq!(
            Failure::on(&elsewhere, &operations, Some(machine)).reason,
            "create Container failed"
        );
        // A limits update keeps saying its live limits are unknown.
        let limits = ExecutionError::Machine {
            action: MachineAction::UpdateContainerLimits,
            error: RpcError::unanswered_target(
                RpcErrorCode::Unavailable,
                format!("Machine {machine_id} did not answer: deadline has elapsed"),
            ),
        };
        assert!(
            Failure::on(&limits, &operations, Some(machine))
                .reason
                .ends_with("could not confirm the new limits")
        );
        // A reader that doesn't know which Machine the call went to keeps the client's words.
        let kept = Failure::of(&by_id, &operations);
        assert_eq!(kept.reason, "create Container failed");
        assert_eq!(
            kept.cause,
            [format!(
                "Machine {machine_id} did not answer: target Machine RPC timed out"
            )]
        );
    }

    #[test]
    fn a_container_updated_in_place_serves_throughout() {
        let machine_id = "a".repeat(32);
        let tracked = |status: serde_json::Value| {
            let rows: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
                "index": 0, "machine_id": machine_id, "service_name": "web",
                "operation": {
                    "type": "update_container", "machine_id": machine_id, "container_id": "c".repeat(64),
                    "spec": {
                        "service_id": "b".repeat(32), "name": "web",
                        "mode": {"mode": "replicated", "replicas": 1},
                        "container": {"image": "web:1", "pull_policy": "missing", "resources": {"memory_bytes": 64}}
                    }
                },
                "status": status
            }]))
            .unwrap();
            super::RowTracker::default().changes(&rows).remove(0).state
        };
        assert_eq!(
            tracked(
                serde_json::json!({"type": "running", "phase": {"type": "updating_container"}})
            ),
            RowState::Running {
                phase: RowPhase::UpdatingContainer,
                serving: true,
            }
        );
        assert_eq!(
            tracked(serde_json::json!({"type": "completed"})),
            RowState::Completed
        );
    }

    #[test]
    fn a_failed_limits_update_names_the_server_and_keeps_dockers_words() {
        let machine_id = "a".repeat(32);
        let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
            "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
            "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
            "status": {"type": "pending"}
        }]))
        .unwrap();
        let docker = "Docker responded with status code 400: Conflicting options: Nano CPUs and CPU Period cannot both be set";
        let error = ExecutionError::Machine {
            action: MachineAction::UpdateContainerLimits,
            error: RpcError {
                code: RpcErrorCode::Internal,
                message: "Docker operation failed".into(),
                details: serde_json::json!({"machine_id": machine_id}),
                cause: vec![docker.into()],
            },
        };
        let named = Failure::of(&error, &operations);
        assert_eq!(
            named.reason,
            "Server prod-1 could not confirm the new limits"
        );
        assert_eq!(named.cause, ["Docker operation failed", docker]);
        assert_eq!(
            Failure::of(&error, &[]).reason,
            format!("Server {machine_id} could not confirm the new limits")
        );
    }

    #[test]
    fn a_step_whose_answer_was_lost_says_so_not_that_the_step_failed() {
        let machine_id = "a".repeat(32);
        let operations: Vec<OperationRow> = serde_json::from_value(serde_json::json!([{
            "index": 0, "machine_id": machine_id, "machine_name": "prod-1",
            "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
            "status": {"type": "pending"}
        }]))
        .unwrap();
        let lost = |action| ExecutionError::Machine {
            action,
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "connection lost".into(),
                details: serde_json::Value::Null,
                cause: Vec::new(),
            }
            .reply_lost_from(&machine_id.parse().unwrap()),
        };
        let pull = Failure::of(&lost(MachineAction::PullImage), &operations);
        assert_eq!(
            pull.reason,
            "The connection to Server prod-1 dropped during pull image, so it may have happened anyway"
        );
        assert_eq!(pull.cause, ["connection lost"]);
        let create = lost(MachineAction::CreateContainer);
        assert_eq!(
            Failure::of(&create, &[]).reason,
            format!(
                "The connection to Server {machine_id} dropped during create Container, so it may have happened anyway"
            )
        );
        let failed = OperationStatus::Failed { error: create };
        assert!(
            matches!(
                server(&[failed]),
                RowState::Failed {
                    unanswered: true,
                    ..
                }
            ),
            "a lost answer may still have done the work"
        );
    }

    #[test]
    fn a_refused_limits_update_says_which_limits_hold() {
        let machine_id = "a".repeat(32);
        let refused = |reason: &str, message: &str| ExecutionError::Machine {
            action: MachineAction::UpdateContainerLimits,
            error: RpcError {
                code: RpcErrorCode::Conflict,
                message: message.into(),
                details: serde_json::json!({"machine_id": machine_id, "reason": reason}),
                cause: Vec::new(),
            },
        };
        let message = "container c cannot take these limits in place: the Container runs with a higher memory limit";
        let before_any_change = Failure::of(&refused("would_lower_memory", message), &[]);
        assert_eq!(
            before_any_change.reason,
            format!(
                "Server {machine_id} did not apply the new limits. The Container keeps its previous limits"
            )
        );
        assert_eq!(before_any_change.cause, [message]);
        assert_eq!(
            Failure::of(&refused("not_running", "stopped"), &[]).reason,
            format!(
                "Server {machine_id} found the Container not running, so the new limits do not apply"
            )
        );
        assert_eq!(
            Failure::of(&refused("some_future_reason", "later"), &[]).reason,
            format!("Server {machine_id} could not confirm the new limits")
        );
    }

    #[test]
    fn a_failed_hook_names_both_commands_it_can_run() {
        let failed = Failure::of(
            &ExecutionError::Hook {
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                failure: HookFailure::Exit { code: 3 },
            },
            &[],
        );
        assert_eq!(failed.reason, "The setup or pre-deploy command failed");
        assert_eq!(failed.cause, ["exited with code 3"]);
    }

    #[test]
    fn a_container_that_stops_running_is_not_called_unhealthy() {
        assert_eq!(
            reason(ContainerRuntimeObservation::Restarting),
            "The Container is restarting"
        );
        assert_eq!(
            reason(ContainerRuntimeObservation::Exited { code: 1 }),
            "The Container exited with code 1"
        );
        assert_eq!(
            reason(ContainerRuntimeObservation::Dead),
            "The Container stopped"
        );
    }

    fn checked(observation: ContainerRuntimeObservation, last_line: &str) -> Failure {
        Failure::of(
            &ExecutionError::Health {
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                failure: HealthFailure::Runtime {
                    observation,
                    last_check: Some(LastHealthCheck::exited(127, "sh: pg_isready: not found")),
                },
                out_of_memory: None,
                last_run: Some(LastRun {
                    exit_code: None,
                    last_line: Some(last_line.to_owned()),
                }),
                departing: None,
            },
            &[],
        )
    }

    #[test]
    fn a_running_container_s_failed_check_is_the_deepest_cause() {
        // What the app printed while Ployz stopped it is not why its check failed (#2841).
        let failed = checked(
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Unhealthy,
            },
            "[notice] 1#1: signal 29 (SIGIO) received",
        );
        assert_eq!(failed.reason, "The Container failed its health check");
        assert_eq!(
            failed.cause,
            ["running (health: unhealthy); last check exited 127: sh: pg_isready: not found"]
        );
    }

    #[test]
    fn a_container_that_stopped_ends_with_its_last_line() {
        let failed = checked(
            ContainerRuntimeObservation::Restarting,
            "panic: no DATABASE_URL",
        );
        assert_eq!(
            failed.cause.last().map(String::as_str),
            Some("panic: no DATABASE_URL")
        );
    }

    #[test]
    fn a_failure_row_from_before_same_again_reads_back_as_one_a_retry_may_get_past() {
        let old: Failure =
            serde_json::from_str(r#"{"reason":"capacity unknown","cause":["fsn-1"]}"#).unwrap();
        assert!(!old.same_again);
        assert_eq!(
            serde_json::to_value(&old).unwrap(),
            serde_json::json!({"reason": "capacity unknown", "cause": ["fsn-1"]})
        );
        let refused = Failure {
            same_again: true,
            ..Failure::from("hostname taken".to_owned())
        };
        let json = serde_json::to_value(&refused).unwrap();
        assert_eq!(json.get("same_again"), Some(&serde_json::Value::Bool(true)));
        assert!(serde_json::from_value::<Failure>(json).unwrap().same_again);
    }

    #[test]
    fn a_cause_that_repeats_the_reason_is_left_out() {
        assert!(
            failure(ContainerRuntimeObservation::Restarting)
                .cause
                .is_empty()
        );
        assert!(
            failure(ContainerRuntimeObservation::Exited { code: 1 })
                .cause
                .is_empty()
        );
    }

    fn pull_failed(cause: &str) -> Failure {
        let error = RpcError {
            code: RpcErrorCode::Internal,
            message: "api: pull image failed.".into(),
            details: serde_json::Value::Null,
            cause: vec![cause.to_owned()],
        }
        .pulling("ghcr.io/example/private:1", ployz_core::PullPolicy::Always);
        Failure::of(
            &ExecutionError::Machine {
                action: MachineAction::PullImage,
                error,
            },
            &[],
        )
    }

    #[test]
    fn a_registry_that_denies_the_pull_names_the_image_as_missing_or_private() {
        for cause in [
            "Docker responded with status code 500: error from registry: denied; denied",
            "Docker responded with status code 500: Head \"https://registry.example.com/v2/app/manifests/1\": unauthorized: authentication required",
        ] {
            let failure = pull_failed(cause);
            assert_eq!(
                failure.reason,
                "Image ghcr.io/example/private:1 not found, or its registry needs credentials",
                "{cause}"
            );
            assert_eq!(failure.cause, [cause]);
        }
    }

    #[test]
    fn a_registry_host_that_does_not_resolve_is_named() {
        let cause = "Docker responded with status code 500: Get \"https://registry.example.invalid/v2/\": dial tcp: lookup registry.example.invalid on 127.0.0.53:53: no such host";
        let failure = pull_failed(cause);
        assert_eq!(
            failure.reason,
            "Host registry.example.invalid does not resolve, so the image could not be pulled. Check the image's registry, or this Server's DNS"
        );
        assert_eq!(failure.cause, [cause]);
    }

    #[test]
    fn a_lost_reply_that_names_an_unresolved_host_still_says_it_may_have_happened() {
        let error = RpcError {
            code: RpcErrorCode::Unavailable,
            message: "transport error".into(),
            details: serde_json::json!({"reply_lost": true}),
            cause: vec![
                "dial tcp: lookup peer.example.invalid on 127.0.0.53:53: no such host".into(),
            ],
        };
        let failure = Failure::of(
            &ExecutionError::Machine {
                action: MachineAction::PullImage,
                error,
            },
            &[],
        );
        assert!(
            failure.reason.starts_with("The connection to"),
            "{}",
            failure.reason
        );
    }

    #[test]
    fn a_local_permission_error_is_not_a_registry_refusal() {
        let cause =
            "Docker responded with status code 500: open /var/lib/docker/tmp/x: permission denied";
        assert_eq!(pull_failed(cause).reason, "pull image failed");
    }
}
