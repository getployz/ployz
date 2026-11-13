//! A Deployment's per-Server progress: the rows a runner's Deploy Progress snapshots become,
//! and why each failed, as Cloud and the CLI both render them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::memory_size::memory_size;
use crate::scrub::excerpt::{LogText, Withheld};
use crate::{
    ContainerId, ContainerRuntimeObservation, DependencyHealthFailure, DeployOperation,
    ExecutionError, HealthFailure, HookFailure, LimitsRefusal, MachineAction, MachineId,
    OperationPhase, OperationRow, OperationStatus, OutOfMemory, PullPolicy, RpcError, ServiceName,
};

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
    #[must_use]
    pub const fn finished(&self) -> bool {
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
#[must_use]
pub fn entered(mut history: Vec<RowTransition>, at: i64, state: &RowState) -> Vec<RowTransition> {
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
                cause: crate::error_chain::causes(error),
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
                cause.extend(crate::error_chain::causes(error));
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
                crate::error_chain::causes(error),
            ),
            ExecutionError::Machine {
                action: MachineAction::PullImage,
                error,
            } if let Some(full) = error.disk_full() => (
                match machine {
                    Some(id) => full.on(&server_of(&id.to_string(), operations)),
                    None => full.to_string(),
                },
                crate::error_chain::causes(error),
            ),
            ExecutionError::Machine {
                action: MachineAction::PullImage,
                error,
            } if let Some(host) = error.unresolved_host() => (
                format!(
                    "Host {host} does not resolve, so the image could not be pulled. Check the image's registry, or this Server's DNS"
                ),
                crate::error_chain::causes(error),
            ),
            ExecutionError::Machine {
                action: MachineAction::UpdateContainerLimits,
                error,
            } => {
                let server = server_named(error, operations);
                let mut cause = vec![error.message.clone()];
                cause.extend(crate::error_chain::causes(error));
                // A refusal comes before any Docker change, so the old limits hold.
                // Anything else, such as a Docker error, an unapplied read-back or a
                // lost answer, leaves the live limits unknown.
                let outcome = match error.evidence::<LimitsRefusal>() {
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
                cause.extend(crate::error_chain::causes(error));
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
                cause.extend(crate::error_chain::causes(error));
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
                        memory_size(bytes.get() as f64 / 1e9)
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
                        cause.extend(crate::error_chain::causes(error));
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
                        cause.extend(crate::error_chain::causes(error));
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
            statuses.push((&row.status, row.operation.serves_new_spec()));
            if row.operation.serves_new_spec() {
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
    // A new copy completes only once its switch lands, and an in-place update once it
    // changed the serving copy, so a Server whose old copy's removal a later failure
    // left unexecuted still switched (#3684).
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

#[cfg(test)]
#[path = "deploy_progress_tests.rs"]
mod tests;
