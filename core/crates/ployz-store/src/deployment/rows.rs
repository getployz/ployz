//! Per-Server Deployment outcomes.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{SavedEnvironmentIntent, SavedServiceIntent};
use ployz_core::deploy_progress::{
    Failure, RowState, RowTransition, ServerProgress, ServerRow, StoppedBy, entered,
};
use ployz_core::{MachineId, RpcError, ServiceName};

use super::{DeploymentStatus, Stored, TargetNode, json_text, now, saved_at};
use crate::error;
use crate::id::OrganizationId;
use crate::storage::{Row, Tx};

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

/// The name each of `machines` (every Server, when `None`) last ran a Deployment row
/// under in `organization`, as the newest row that named it knew it. A row carrying
/// only the Machine's id names nothing, and a Machine no row named is left out. Only
/// the rows of the Machines asked for are read, so a stored Machine ID that is not
/// one of them is never decoded.
pub(crate) fn server_names(
    tx: &mut dyn Tx,
    organization: &OrganizationId,
    machines: Option<&BTreeSet<MachineId>>,
) -> Result<BTreeMap<MachineId, String>, RpcError> {
    const NAMED: &str = "SELECT r.machine, r.server FROM config_deployment_row r \
         JOIN config_deployment d ON d.id = r.deployment_id \
         WHERE r.organization_id = ?1 AND r.server <> r.machine";
    const NEWEST_LAST: &str = "ORDER BY COALESCE(r.finished, r.started, 0), d.admitted";
    let mut names = BTreeMap::new();
    let Some(machines) = machines else {
        let found = tx.query(
            &format!("{NAMED} {NEWEST_LAST}"),
            &[organization.as_str().into()],
        )?;
        for row in &found {
            let machine: MachineId = row.parse(0, "Deployment row Machine")?;
            names.insert(machine, row.text(1)?.to_owned());
        }
        return Ok(names);
    };
    for machine in machines {
        let id = machine.to_string();
        let found = tx.query(
            &format!("{NAMED} AND r.machine = ?2 {NEWEST_LAST}"),
            &[organization.as_str().into(), id.as_str().into()],
        )?;
        if let Some(row) = found.last() {
            names.insert(*machine, row.text(1)?.to_owned());
        }
    }
    Ok(names)
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
