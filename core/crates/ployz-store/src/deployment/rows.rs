//! Per-Server Deployment outcomes.

use std::collections::BTreeMap;

use ployz_core::{
    ContainerId, DependencyHealthFailure, ExecutionError, HookFailure, MachineId, OperationPhase,
    OperationRow, OperationStatus, RpcError, ServiceName,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{DeploymentStatus, Stored, TargetNode, json_text, now, saved_at};
use crate::storage::Tx;

/// The most log lines a failed row keeps.
pub const LOG_TAIL: usize = 10;

/// What one Service's work on one Server is doing.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RowState {
    Pending,
    Running {
        phase: RowPhase,
    },
    Completed,
    /// `reason` is our sentence; `cause` lists what it came from, outermost first.
    /// `log` is the failed Container's last lines, when they could be read.
    Failed {
        reason: String,
        cause: Vec<String>,
        log: Vec<String>,
    },
    /// The Deployment stopped before this work finished.
    NotAttempted,
    /// Reporting stopped before this row's outcome was confirmed.
    Unknown,
}

impl RowState {
    const fn finished(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::NotAttempted | Self::Unknown
        )
    }
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
    StoppingContainer,
    RemovingContainer,
    RemovingVolume,
    Compensating,
}

impl From<&OperationPhase> for RowPhase {
    fn from(phase: &OperationPhase) -> Self {
        match phase {
            OperationPhase::Starting => Self::Starting,
            OperationPhase::CreatingContainer => Self::CreatingContainer,
            OperationPhase::StartingContainer => Self::StartingContainer,
            OperationPhase::WaitingForHealth { .. } => Self::WaitingForHealth,
            OperationPhase::WaitingForHook { .. } => Self::WaitingForHook,
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
        Self {
            reason: error.message.clone(),
            cause: ployz_core::error_chain::causes(error),
        }
    }
}

impl From<&ExecutionError> for Failure {
    fn from(error: &ExecutionError) -> Self {
        let (reason, cause) = match error {
            ExecutionError::Machine { action, error } => {
                let mut cause = vec![error.message.clone()];
                cause.extend(ployz_core::error_chain::causes(error));
                (format!("{action} failed"), cause)
            }
            ExecutionError::Health { failure, .. } => (
                "The Container failed its health check".to_owned(),
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
                    _ => vec![failure.to_string()],
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
                    _ => vec![failure.to_string()],
                },
            ),
            ExecutionError::Cancelled => ("Cancelled".to_owned(), Vec::new()),
        };
        Self { reason, cause }
    }
}

type Key = (ServiceName, MachineId);

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
        let mut grouped: BTreeMap<Key, (String, Vec<&OperationStatus>)> = BTreeMap::new();
        self.failed.clear();
        for row in snapshot {
            let Some(service) = row.service_name() else {
                continue;
            };
            let key = (service.clone(), row.machine_id);
            if let Some(container) = container(&row.status) {
                self.containers.insert(row.index, container);
            }
            if matches!(row.status, OperationStatus::Failed { .. }) {
                self.failed.entry(key.clone()).or_insert(row.index);
            }
            let server = row
                .machine_name
                .as_ref()
                .map_or_else(|| row.machine_id.to_string(), ToString::to_string);
            grouped
                .entry(key)
                .or_insert_with(|| (server, Vec::new()))
                .1
                .push(&row.status);
        }
        let mut changes = Vec::new();
        for (key, (server, statuses)) in grouped {
            let state = state(&statuses, self.last.get(&key));
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

fn container(status: &OperationStatus) -> Option<ContainerId> {
    if let OperationStatus::Running {
        phase:
            OperationPhase::WaitingForHealth { container_id, .. }
            | OperationPhase::WaitingForHook { container_id, .. },
    }
    | OperationStatus::Failed {
        error:
            ExecutionError::Health { container_id, .. } | ExecutionError::Hook { container_id, .. },
    } = status
    {
        Some(*container_id)
    } else {
        None
    }
}

fn state(statuses: &[&OperationStatus], last: Option<&RowState>) -> RowState {
    let (mut failed, mut running, mut pending, mut completed) = (None, None, 0, 0);
    for status in statuses {
        match status {
            OperationStatus::Failed { error } => failed = failed.or(Some(error)),
            OperationStatus::Running { phase } => running = running.or(Some(phase)),
            OperationStatus::Pending => pending += 1,
            OperationStatus::Completed => completed += 1,
            OperationStatus::Unexecuted => {}
        }
    }
    if let Some(error) = failed {
        let Failure { reason, cause } = error.into();
        return RowState::Failed {
            reason,
            cause,
            log: Vec::new(),
        };
    }
    if let Some(phase) = running {
        return RowState::Running {
            phase: phase.into(),
        };
    }
    if completed == statuses.len() {
        RowState::Completed
    } else if pending == statuses.len() {
        RowState::Pending
    } else if pending == 0 {
        RowState::NotAttempted
    } else if let Some(running @ RowState::Running { .. }) = last {
        running.clone()
    } else {
        RowState::Running {
            phase: RowPhase::Starting,
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
        tx.execute(
            "INSERT INTO config_deployment_row \
             (deployment_id, service, machine, organization_id, server, state, started, finished) \
             SELECT id, ?2, ?3, organization_id, ?4, ?5, ?6, ?7 \
             FROM config_deployment WHERE id = ?1 \
             ON CONFLICT (deployment_id, service, machine) DO UPDATE SET \
             server = excluded.server, state = excluded.state, \
             started = COALESCE(config_deployment_row.started, excluded.started), \
             finished = excluded.finished",
            &[
                stored.summary.id.as_str().into(),
                row.service.as_str().into(),
                machine.as_str().into(),
                row.server.as_str().into(),
                json_text(&row.state).as_str().into(),
                started.into(),
                finished.into(),
            ],
        )?;
    }
    Ok(())
}

pub(super) fn settle(tx: &mut dyn Tx, stored: &Stored) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_deployment_row SET state = ?2, finished = ?3 \
         WHERE deployment_id = ?1 AND finished IS NULL",
        &[
            stored.summary.id.as_str().into(),
            json_text(&RowState::NotAttempted).as_str().into(),
            now().into(),
        ],
    )?;
    Ok(())
}

pub(super) fn of_nodes(
    tx: &mut dyn Tx,
    stored: &Stored,
) -> Result<BTreeMap<String, Vec<ServerRow>>, RpcError> {
    let found = tx.query(
        "SELECT service, server, state, started, finished, machine FROM config_deployment_row \
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
        by_service
            .entry(row.parse(0, "Deployment row")?)
            .or_default()
            .insert(
                row.parse(5, "Deployment row Machine")?,
                ServerRow {
                    server: row.text(1)?.to_owned(),
                    state: if lost && !state.finished() {
                        RowState::Unknown
                    } else {
                        state
                    },
                    started_at: row.optional_int(3)?,
                    finished_at: row.optional_int(4)?,
                },
            );
    }
    let mut nodes = BTreeMap::new();
    for node in &stored.nodes {
        if let TargetNode::Service { id, runtime, .. } = node
            && let Some(rows) = by_service.get(runtime)
        {
            nodes.insert(id.as_str().to_owned(), rows.values().cloned().collect());
        }
    }
    if !stored
        .nodes
        .iter()
        .any(|node| matches!(node, TargetNode::Volume { .. }))
    {
        return Ok(nodes);
    }
    let saved = saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    for node in &stored.nodes {
        let TargetNode::Volume { id, .. } = node else {
            continue;
        };
        let mut rows: BTreeMap<MachineId, ServerRow> = BTreeMap::new();
        for service in saved.services.iter().filter(|service| {
            service
                .volume_attachments
                .iter()
                .any(|mount| mount.volume_resource_id == id.as_str())
        }) {
            for (machine, row) in by_service
                .get(&service.config.private_dns)
                .into_iter()
                .flatten()
            {
                rows.entry(*machine)
                    .and_modify(|kept| merge_volume_row(kept, row))
                    .or_insert_with(|| row.clone());
            }
        }
        if !rows.is_empty() {
            let mut rows: Vec<_> = rows.into_values().collect();
            rows.sort_by(|a, b| a.server.cmp(&b.server));
            nodes.insert(id.as_str().to_owned(), rows);
        }
    }
    Ok(nodes)
}

fn merge_volume_row(kept: &mut ServerRow, row: &ServerRow) {
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
