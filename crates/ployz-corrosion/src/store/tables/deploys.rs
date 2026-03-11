use crate::client::CorrClient;
use crate::store::shared::decode::{integer, text};
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::{DeployId, DeployRecord, DeployState, MachineId};
use ployz_sdk::spec::Namespace;

pub(crate) async fn upsert_deploy(client: &CorrClient, record: &DeployRecord) -> Result<()> {
    let stmt = upsert_statement(record);
    exec_one(client, &[stmt], "upsert_deploy").await
}

pub(crate) async fn get_deploy(
    client: &CorrClient,
    deploy_id: &DeployId,
) -> Result<Option<DeployRecord>> {
    let stmt = Statement::WithParams(
        "SELECT deploy_id, namespace, coordinator_machine_id, manifest_hash, state, started_at, committed_at, finished_at, summary_json FROM deploys WHERE deploy_id = ? LIMIT 1".to_string(),
        vec![deploy_id.0.clone().into()],
    );
    let rows = query_rows(client, &stmt, "get_deploy").await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(parse_deploy(row)?))
}

pub(crate) fn upsert_statement(record: &DeployRecord) -> Statement {
    let committed_at = record.committed_at.unwrap_or(0);
    let finished_at = record.finished_at.unwrap_or(0);
    Statement::WithParams(
        "INSERT INTO deploys (deploy_id, namespace, coordinator_machine_id, manifest_hash, state, started_at, committed_at, finished_at, summary_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(deploy_id) DO UPDATE SET namespace=excluded.namespace, coordinator_machine_id=excluded.coordinator_machine_id, manifest_hash=excluded.manifest_hash, state=excluded.state, started_at=excluded.started_at, committed_at=excluded.committed_at, finished_at=excluded.finished_at, summary_json=excluded.summary_json"
            .to_string(),
        vec![
            record.deploy_id.0.clone().into(),
            record.namespace.0.clone().into(),
            record.coordinator_machine_id.0.clone().into(),
            record.manifest_hash.clone().into(),
            record.state.to_string().into(),
            (record.started_at as i64).into(),
            (committed_at as i64).into(),
            (finished_at as i64).into(),
            record.summary_json.clone().into(),
        ],
    )
}

pub(crate) fn parse_deploy(row: &[SqliteValue]) -> Result<DeployRecord> {
    let [
        deploy_val,
        namespace_val,
        coordinator_val,
        manifest_val,
        state_val,
        started_val,
        committed_val,
        finished_val,
        summary_val,
    ] = row
    else {
        return Err(Error::operation(
            "parse_deploy",
            format!("expected 9 columns, got {}", row.len()),
        ));
    };

    let state: DeployState = text(state_val, "state")?
        .parse()
        .map_err(|e: strum::ParseError| Error::operation("parse_deploy", e.to_string()))?;
    let committed_at = integer(committed_val, "committed_at")? as u64;
    let finished_at = integer(finished_val, "finished_at")? as u64;

    Ok(DeployRecord {
        deploy_id: DeployId(text(deploy_val, "deploy_id")?),
        namespace: Namespace(text(namespace_val, "namespace")?),
        coordinator_machine_id: MachineId(text(coordinator_val, "coordinator_machine_id")?),
        manifest_hash: text(manifest_val, "manifest_hash")?,
        state,
        started_at: integer(started_val, "started_at")? as u64,
        committed_at: if committed_at == 0 {
            None
        } else {
            Some(committed_at)
        },
        finished_at: if finished_at == 0 {
            None
        } else {
            Some(finished_at)
        },
        summary_json: text(summary_val, "summary_json")?,
    })
}
