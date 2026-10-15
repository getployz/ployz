//! Per-Server Deployment outcomes.

use std::collections::BTreeMap;

use ployz_core::config::SavedServiceIntent;
use ployz_core::{
    ContainerId, ContainerRuntimeObservation, DependencyHealthFailure, DeployOperation,
    ExecutionError, HealthFailure, HookFailure, MachineAction, MachineId, OperationPhase,
    OperationRow, OperationStatus, RpcError, ServiceName,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{DeploymentStatus, Stored, TargetNode, json_text, now, saved_at};
use crate::error;
use crate::storage::{Row, Tx};
use crate::tools::{LogText, Withheld};

/// What one Service's work on one Server is doing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RowState {
    Pending,
    /// `serving` is true once every new Container this Server starts serves,
    /// so stop and remove phases then work on old ones.
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

/// The step a failed row's work was on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum FailedAt {
    Build,
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
                MachineAction::PullImage => Some(Self::Create),
                MachineAction::InspectContainer
                | MachineAction::ListContainers
                | MachineAction::ReadServingPointer
                | MachineAction::PublishServingPointer => from_phase(),
            },
            ExecutionError::Health { .. } | ExecutionError::DependencyHealth { .. } => {
                Some(Self::Health)
            }
            ExecutionError::Hook { .. } => Some(Self::Hook),
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
            Self::Starting | Self::WaitingForSwitch | Self::Compensating => None,
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
    /// Config rows, which merge several Services' rows.
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
        let state = match state {
            RowState::Failed {
                reason,
                cause,
                step,
                ..
            } => RowState::Failed {
                reason: reason.clone(),
                cause: cause.clone(),
                step: *step,
                log: None,
            },
            state => state.clone(),
        };
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
}

/// Why something failed, as users read it: our sentence, then what it came from,
/// outermost first.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    pub reason: String,
    #[serde(default)]
    pub cause: Vec<String>,
}

impl From<String> for Failure {
    fn from(reason: String) -> Self {
        Self {
            reason,
            cause: Vec::new(),
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
            },
        }
    }
}

impl Failure {
    /// `error` for users. A Server that refused for want of a capability is named
    /// from `operations`, and its sentence stands alone.
    pub fn of(error: &ExecutionError, operations: &[OperationRow]) -> Self {
        let (reason, cause) = match error {
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
            ExecutionError::Machine { action, error } => {
                let mut cause = vec![error.message.clone()];
                cause.extend(ployz_core::error_chain::causes(error));
                (format!("{action} failed"), cause)
            }
            ExecutionError::Health { failure, .. } => (
                match failure {
                    HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Restarting,
                        ..
                    } => "The Container is restarting",
                    HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Exited { .. },
                        ..
                    } => "The Container exited",
                    HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Dead,
                        ..
                    } => "The Container stopped",
                    HealthFailure::Cancelled
                    | HealthFailure::TimedOut { .. }
                    | HealthFailure::Runtime { .. } => "The Container failed its health check",
                }
                .to_owned(),
                vec![failure.to_string()],
            ),
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
                "The hook Container failed".to_owned(),
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
            ExecutionError::Cancelled => ("Cancelled".to_owned(), Vec::new()),
        };
        Self { reason, cause }
    }
}

type Key = (ServiceName, MachineId);

/// Whether `operation` starts a new Service Container.
#[must_use]
pub const fn starts_container(operation: &DeployOperation) -> bool {
    matches!(
        operation,
        DeployOperation::RunContainer { .. } | DeployOperation::ReplaceContainer(_)
    )
}

/// Turns a runner's Deploy Progress snapshots into the rows that changed, so it
/// records only those.
#[derive(Debug, Default)]
pub struct RowTracker {
    last: BTreeMap<Key, RowState>,
    containers: BTreeMap<u32, ContainerId>,
    failed: BTreeMap<Key, u32>,
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
            if matches!(row.status, OperationStatus::Failed { .. }) {
                self.failed.entry(key.clone()).or_insert(row.index);
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
                let done = matches!(row.status, OperationStatus::Completed);
                *serving = Some(serving.unwrap_or(true) && done);
            }
        }
        let mut changes = Vec::new();
        for (key, (server, statuses, serving)) in grouped {
            let container = self
                .failed
                .get(&key)
                .is_some_and(|index| self.containers.contains_key(index));
            let state = state(
                snapshot,
                &statuses,
                self.last.get(&key),
                container,
                serving.unwrap_or(false),
            );
            if self.last.get(&key) == Some(&state) {
                continue;
            }
            self.last.insert(key.clone(), state.clone());
            changes.push(ServerProgress {
                service: key.0,
                machine: key.1,
                server,
                state,
            });
        }
        changes
    }

    /// The failed operation's Container, when its progress identified one.
    #[must_use]
    pub fn container(&self, row: &ServerProgress) -> Option<ContainerId> {
        self.failed
            .get(&(row.service.clone(), row.machine))
            .and_then(|index| self.containers.get(index))
            .copied()
    }
}

fn container(row: &OperationRow, last: Option<ContainerId>) -> Option<ContainerId> {
    match &row.status {
        OperationStatus::Running { phase } => match phase {
            OperationPhase::WaitingForHealth { container_id, .. }
            | OperationPhase::WaitingForHook { container_id, .. }
            | OperationPhase::WaitingForSwitch { container_id } => Some(*container_id),
            OperationPhase::StoppingContainer | OperationPhase::RemovingContainer => {
                row.operation.container_id()
            }
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
                MachineAction::StopContainer | MachineAction::RemoveContainer => {
                    row.operation.container_id()
                }
                MachineAction::InspectContainer => last,
                MachineAction::CreateContainer
                | MachineAction::StartContainer
                | MachineAction::PrepareVolumes
                | MachineAction::RemoveVolume
                | MachineAction::PullImage
                | MachineAction::ListContainers
                | MachineAction::ReadServingPointer
                | MachineAction::PublishServingPointer => None,
            },
            ExecutionError::DependencyHealth { .. } | ExecutionError::Cancelled => None,
        },
        OperationStatus::Completed => last,
        OperationStatus::Pending | OperationStatus::Unexecuted => None,
    }
}

/// A failed row's `log` waits as unanswered until the runner reads its Container's tail.
fn state(
    operations: &[OperationRow],
    statuses: &[(&OperationStatus, bool)],
    last: Option<&RowState>,
    container: bool,
    serving: bool,
) -> RowState {
    let (mut failed, mut running, mut switching, mut pending, mut unstarted, mut completed) =
        (None, None, None, 0, 0, 0);
    for (status, starts) in statuses {
        match status {
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
    if let Some(error) = failed {
        let Failure { reason, cause } = Failure::of(error, operations);
        return RowState::Failed {
            reason,
            cause,
            step: FailedAt::of(error, last),
            log: container.then_some(LogText::Withheld {
                why: Withheld::Unanswered,
            }),
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
    if completed == statuses.len() {
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
    for row in rows {
        let started = matches!(
            row.state,
            RowState::Running { .. } | RowState::Completed | RowState::Failed { .. }
        )
        .then_some(now);
        let finished = row.state.finished().then_some(now);
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
        .map(|history| json_text(&entered(history, now, &row.state)));
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
                json_text(&row.state).as_str().into(),
                started.into(),
                finished.into(),
                history.as_deref().into(),
            ],
        )?;
    }
    Ok(())
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
        .map(|text| serde_json::from_str(text).map_err(|_| error::corrupt("Deployment row history")))
        .transpose()
}

pub(super) fn of_nodes(
    tx: &mut dyn Tx,
    stored: &Stored,
) -> Result<BTreeMap<String, Vec<ServerRow>>, RpcError> {
    let found = tx.query(
        "SELECT service, server, state, started, finished, machine, history \
         FROM config_deployment_row \
         WHERE deployment_id = ?1 ORDER BY service, server, machine",
        &[stored.summary.id.as_str().into()],
    )?;
    if found.is_empty() {
        return Ok(BTreeMap::new());
    }
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
    let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
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
        RowState::Failed { .. } => 6,
        RowState::Unknown => 5,
        RowState::Running { .. } => 4,
        RowState::Pending => 3,
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
        ContainerId, ContainerRuntimeObservation, ExecutionError, HealthFailure, MachineAction,
        OperationPhase, OperationRow, OperationStatus, RpcError, RpcErrorCode,
    };

    use super::{Failure, RowPhase, RowState};

    fn server(statuses: &[OperationStatus]) -> RowState {
        let statuses: Vec<_> = statuses.iter().map(|status| (status, true)).collect();
        super::state(&[], &statuses, None, true, false)
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
                &[(&switch, true), (&OperationStatus::Pending, false)],
                None,
                true,
                false
            ),
            running(RowPhase::WaitingForSwitch),
            "the old copy's removal waits for the switch, not the Server"
        );
    }

    fn reason(observation: ContainerRuntimeObservation) -> String {
        Failure::of(
            &ExecutionError::Health {
                container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                failure: HealthFailure::Runtime {
                    observation,
                    last_check: None,
                },
            },
            &[],
        )
        .reason
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
                details: serde_json::json!({"capability": "ployz.serving.read.v1", "target": machine_id}),
                cause: vec!["unimplemented".into()],
            },
        };
        let named = Failure::of(&error, &operations);
        assert_eq!(
            named.reason,
            "Server prod-1 needs a newer Ployz. It cannot serve ployz.serving.read.v1."
        );
        assert!(named.cause.is_empty());
        assert_eq!(
            Failure::of(&error, &[]).reason,
            format!(
                "Server {machine_id} needs a newer Ployz. It cannot serve ployz.serving.read.v1."
            )
        );
    }

    #[test]
    fn a_container_that_stops_running_is_not_called_unhealthy() {
        assert_eq!(
            reason(ContainerRuntimeObservation::Restarting),
            "The Container is restarting"
        );
        assert_eq!(
            reason(ContainerRuntimeObservation::Exited { code: 1 }),
            "The Container exited"
        );
        assert_eq!(
            reason(ContainerRuntimeObservation::Dead),
            "The Container stopped"
        );
    }
}
