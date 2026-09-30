//! Reading what ran: Head, Applied State, in-flight and past Deployments.

use super::*;

/// `environment`'s latest `limit` Deployments that weren't superseded, newest first.
pub(crate) fn history(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    limit: i64,
) -> Result<Vec<DeploymentSummary>, RpcError> {
    tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_deployment \
             WHERE environment_id = ?1 AND status <> 'superseded' ORDER BY number DESC LIMIT ?2"
        ),
        &[environment.as_str().into(), limit.into()],
    )?
    .iter()
    .map(|row| stored(row).map(|stored| stored.summary))
    .collect()
}

/// Applied State: each node as its latest confirmed Deployment applied it, as one
/// document shaped like `like`.
pub(crate) fn applied_state(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    like: &SavedEnvironmentIntent,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let rows = tx.query(
        "SELECT node, node_type FROM config_applied WHERE environment_id = ?1 ORDER BY node_id",
        &[environment.as_str().into()],
    )?;
    scope::nodes(&rows, like, "Applied State")
}

/// What reviews compare Working State against: Applied State, overlaid with the
/// target nodes of the Deployment in flight, if any. Its token changes whenever a
/// Deployment is admitted or ends.
pub(crate) fn head(tx: &mut dyn Tx, environment: &Environment) -> Result<Head, RpcError> {
    let id = &environment.summary.id;
    let applied = applied_state(tx, id, &environment.working)?;
    let in_flight = in_flight_sql();
    let ended = tx
        .query(
            &format!(
                "SELECT COUNT(*) FROM config_deployment \
                 WHERE environment_id = ?1 AND status <> 'superseded' AND NOT {in_flight}"
            ),
            &[id.as_str().into()],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Deployment count"))?
        .int(0)?;
    let in_flight = tx.query(
        &format!(
            "SELECT number, saved_revision, nodes FROM config_deployment \
             WHERE environment_id = ?1 AND {in_flight} ORDER BY number DESC LIMIT 1"
        ),
        &[id.as_str().into()],
    )?;
    let Some(row) = in_flight.first() else {
        return Ok(Head {
            token: format!("0.{ended}"),
            intent: applied.clone(),
            applied,
        });
    };
    let saved = saved_at(tx, id, revision(row.int(1)?)?)?;
    let nodes: Vec<TargetNode> = row.json(2, "Deployment")?;
    let mut intent = applied.clone();
    for node in nodes {
        intent.services.retain(|service| service.id != node.id());
        intent
            .volumes
            .retain(|volume| volume.resource_id != node.id());
        match node {
            TargetNode::Service { .. } => intent.services.extend(
                saved
                    .services
                    .iter()
                    .filter(|service| service.id == node.id())
                    .cloned(),
            ),
            // A Volume the Deployment removes isn't in Head.
            TargetNode::Volume {
                deletes: Some(_), ..
            } => {}
            TargetNode::Volume { deletes: None, .. } => intent.volumes.extend(
                saved
                    .volumes
                    .iter()
                    .filter(|volume| volume.resource_id == node.id())
                    .cloned(),
            ),
        }
    }
    Ok(Head {
        token: format!("{}.{ended}", row.int(0)?),
        intent,
        applied,
    })
}

/// `environment`'s Deployment that may still run, if any: queued, or claimed by a
/// runner whose lease holds.
pub(crate) fn in_flight(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    latest_where(tx, environment, &in_flight_sql())
}

/// `environment`'s latest Deployment that started: claimed by a runner, or a removal
/// that applied without one. What may run on the Servers.
pub(crate) fn last_ran(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    latest_where(tx, environment, "started IS NOT NULL")
}

pub(super) fn latest_where(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    condition: &str,
) -> Result<Option<DeploymentSummary>, RpcError> {
    tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_deployment \
             WHERE environment_id = ?1 AND {condition} ORDER BY number DESC LIMIT 1"
        ),
        &[environment.as_str().into()],
    )?
    .first()
    .map(|row| stored(row).map(|stored| stored.summary))
    .transpose()
}

/// Every queued Deployment no runner claimed that was admitted before `before`
/// (Unix seconds), oldest first: those whose dispatch may have been lost.
pub(crate) fn unclaimed(tx: &mut dyn Tx, before: i64) -> Result<Vec<Unclaimed>, RpcError> {
    tx.query(
        "SELECT organization_id, environment_id, id, admitted FROM config_deployment \
         WHERE status = 'queued' AND runner IS NULL AND admitted < ?1 ORDER BY admitted, id",
        &[before.into()],
    )?
    .iter()
    .map(|row| {
        Ok(Unclaimed {
            organization: row.parse(0, "identity")?,
            environment: row.parse(1, "identity")?,
            deployment: row.parse(2, "identity")?,
            admitted_at: row.int(3)?,
        })
    })
    .collect()
}

/// One Deployment in `who`'s Organization, with its Node Outcomes.
pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentView, RpcError> {
    let stored = owned(tx, who, id)?;
    let environment = scope::load_by_id(tx, &stored.summary.environment_id)?;
    let nodes = stored
        .nodes
        .iter()
        .map(|node| NodeOutcome {
            node: node.shown(),
            outcome: match (stored.run.nodes.get(node.id()), stored.summary.status) {
                (Some(status), _) => *status,
                (None, DeploymentStatus::Unknown) => NodeStatus::Unknown,
                (
                    None,
                    DeploymentStatus::Queued
                    | DeploymentStatus::Running
                    | DeploymentStatus::Cancelling,
                ) => NodeStatus::Pending,
                (
                    None,
                    DeploymentStatus::Superseded
                    | DeploymentStatus::Applied
                    | DeploymentStatus::Failed
                    | DeploymentStatus::Cancelled,
                ) => NodeStatus::NotAttempted,
            },
        })
        .collect();
    let runtime_names = stored
        .nodes
        .iter()
        .filter_map(|node| match node {
            TargetNode::Service { name, runtime, .. } => Some((name.clone(), runtime.clone())),
            TargetNode::Volume { .. } => None,
        })
        .collect();
    let builds = build::views(tx, &stored)?;
    Ok(DeploymentView {
        builds,
        runtime_names,
        deployment: stored.summary,
        environment: environment.summary,
        namespace: stored.namespace,
        nodes,
        preview: stored.run.preview,
        outcome: stored.run.outcome,
    })
}

/// One Git build of a Deployment in `who`'s Organization, with its log.
pub(crate) fn build_log(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &build::BuildLogQuery,
) -> Result<build::BuildLogView, RpcError> {
    let stored = owned(tx, who, &query.deployment)?;
    build::log(tx, &stored, &query.service)
}

/// One page of `environment`'s Deployments, newest first, before `cursor`.
pub(crate) fn page(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    limit: usize,
    before: Option<u64>,
) -> Result<(Vec<DeploymentSummary>, Option<u64>), RpcError> {
    let before = i64::try_from(before.unwrap_or(u64::MAX >> 1)).unwrap_or(i64::MAX);
    let rows = tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_deployment WHERE environment_id = ?1 AND number < ?2 \
             ORDER BY number DESC LIMIT ?3"
        ),
        &[
            environment.as_str().into(),
            before.into(),
            i64::try_from(limit + 1).unwrap_or(i64::MAX).into(),
        ],
    )?;
    let mut page = rows
        .iter()
        .map(|row| stored(row).map(|stored| stored.summary))
        .collect::<Result<Vec<_>, _>>()?;
    let next = (page.len() > limit).then(|| {
        page.truncate(limit);
        page.last().map_or(0, |last| last.number)
    });
    Ok((page, next))
}
