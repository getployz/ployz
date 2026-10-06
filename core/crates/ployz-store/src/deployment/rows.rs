//! Per-Server progress: one row per target Service per Server, written by the
//! runner only when the row's state changes, so a long health wait is one write.

use std::collections::BTreeMap;

use ployz_core::{
    ContainerId, ExecutionError, MachineId, OperationPhase, OperationRow, OperationStatus,
    RpcError, ServiceName,
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
    /// Its runner stopped reporting before this work finished, so what it did is
    /// unknown, as the Deployment's own status says.
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
                vec![failure.to_string()],
            ),
            ExecutionError::Hook { failure, .. } => (
                "The hook Container failed".to_owned(),
                vec![failure.to_string()],
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
    /// The Container each row last worked on, which a failed row's log comes from.
    containers: BTreeMap<Key, ContainerId>,
}

impl RowTracker {
    /// The rows of `snapshot` whose state changed since the last snapshot.
    pub fn changes(&mut self, snapshot: &[OperationRow]) -> Vec<ServerProgress> {
        let mut grouped: BTreeMap<Key, (String, Vec<&OperationStatus>)> = BTreeMap::new();
        for row in snapshot {
            let Some(service) = row.service_name() else {
                continue;
            };
            let key = (service.clone(), row.machine_id);
            if let Some(container) = container(&row.status) {
                self.containers.insert(key.clone(), container);
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

    /// The Container `row` last worked on.
    #[must_use]
    pub fn container(&self, row: &ServerProgress) -> Option<ContainerId> {
        self.containers
            .get(&(row.service.clone(), row.machine))
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

/// One row's state from its operations: a failure wins, then running work. Between
/// two operations it keeps the phase it was in.
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

/// Store each changed row; a row keeps the time its work first started.
pub(super) fn record(
    tx: &mut dyn Tx,
    stored: &Stored,
    rows: &[ServerProgress],
) -> Result<(), RpcError> {
    let now = now();
    for row in rows {
        let started = (row.state != RowState::Pending).then_some(now);
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

/// Once execution ended, a row that never finished never will.
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

/// Each target node's rows, by node ID. A Volume shows the rows of the target
/// Services mounting it, one per Server, as its Node Outcome follows them.
pub(super) fn of_nodes(
    tx: &mut dyn Tx,
    stored: &Stored,
) -> Result<BTreeMap<String, Vec<ServerRow>>, RpcError> {
    let found = tx.query(
        "SELECT service, server, state, started, finished FROM config_deployment_row \
         WHERE deployment_id = ?1 ORDER BY service, server",
        &[stored.summary.id.as_str().into()],
    )?;
    if found.is_empty() {
        return Ok(BTreeMap::new());
    }
    let lost = stored.summary.status == DeploymentStatus::Unknown;
    let mut by_service: BTreeMap<ServiceName, Vec<ServerRow>> = BTreeMap::new();
    for row in &found {
        let state: RowState = row.json(2, "Deployment row")?;
        by_service
            .entry(row.parse(0, "Deployment row")?)
            .or_default()
            .push(ServerRow {
                server: row.text(1)?.to_owned(),
                state: if lost && !state.finished() {
                    RowState::Unknown
                } else {
                    state
                },
                started_at: row.optional_int(3)?,
                finished_at: row.optional_int(4)?,
            });
    }
    let mut nodes = BTreeMap::new();
    for node in &stored.nodes {
        if let TargetNode::Service { id, runtime, .. } = node
            && let Some(rows) = by_service.get(runtime)
        {
            nodes.insert(id.as_str().to_owned(), rows.clone());
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
        let mut rows: Vec<ServerRow> = Vec::new();
        for service in saved.services.iter().filter(|service| {
            service
                .volume_attachments
                .iter()
                .any(|mount| mount.volume_resource_id == id.as_str())
        }) {
            for row in by_service
                .get(&service.config.private_dns)
                .into_iter()
                .flatten()
            {
                if !rows.iter().any(|kept| kept.server == row.server) {
                    rows.push(row.clone());
                }
            }
        }
        if !rows.is_empty() {
            rows.sort_by(|a, b| a.server.cmp(&b.server));
            nodes.insert(id.as_str().to_owned(), rows);
        }
    }
    Ok(nodes)
}
