//! The `config_applied` table: Applied State, one row per node an Environment's
//! Deployments confirmed. A node is replaced only by the Deployment that confirmed it,
//! and forgotten when that Deployment found it gone.

use ployz_core::RpcError;

use crate::id::EnvironmentId;
use crate::storage::{Row, Tx};

/// Every node of `environment`, ordered by node ID, with its node type.
pub(crate) fn rows(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<Vec<Row>, RpcError> {
    tx.query(
        "SELECT node, node_type FROM config_applied WHERE environment_id = ?1 ORDER BY node_id",
        &[environment.as_str().into()],
    )
}

/// The ID of each node `environment` holds.
pub(crate) fn node_ids(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Vec<String>, RpcError> {
    tx.query(
        "SELECT node_id FROM config_applied WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?
    .iter()
    .map(|row| row.text(0).map(str::to_owned))
    .collect()
}

/// The document of node `node_id`, if a Deploy put it there.
pub(crate) fn node(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    node_id: &str,
) -> Result<Option<Row>, RpcError> {
    Ok(tx
        .query(
            "SELECT node FROM config_applied WHERE environment_id = ?1 AND node_id = ?2",
            &[environment.as_str().into(), node_id.into()],
        )?
        .into_iter()
        .next())
}

/// Deployment `deployment` confirmed node `node_id` as `document`.
pub(crate) fn confirm(
    tx: &mut dyn Tx,
    deployment: &str,
    node_id: &str,
    document: &str,
    node_type: &str,
) -> Result<(), RpcError> {
    tx.execute(
        "INSERT INTO config_applied \
         (environment_id, node_id, organization_id, deployment_id, node, node_type) \
         SELECT environment_id, ?2, organization_id, id, ?3, ?4 \
         FROM config_deployment WHERE id = ?1 \
         ON CONFLICT (environment_id, node_id) \
         DO UPDATE SET deployment_id = excluded.deployment_id, node = excluded.node",
        &[
            deployment.into(),
            node_id.into(),
            document.into(),
            node_type.into(),
        ],
    )?;
    Ok(())
}

/// A Deployment found node `node_id` gone.
pub(crate) fn remove_node(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    node_id: &str,
) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_applied WHERE environment_id = ?1 AND node_id = ?2",
        &[environment.as_str().into(), node_id.into()],
    )?;
    Ok(())
}

/// Let go of every node of `environment`.
pub(crate) fn forget(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
    tx.execute(
        "DELETE FROM config_applied WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?;
    Ok(())
}

/// Node `node_id` is `document` now, with no Deployment confirming it.
pub(crate) fn resize(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    node_id: &str,
    document: &str,
) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_applied SET node = ?3 WHERE environment_id = ?1 AND node_id = ?2",
        &[environment.as_str().into(), node_id.into(), document.into()],
    )?;
    Ok(())
}
