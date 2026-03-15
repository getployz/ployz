use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_types::error::{Error, Result};
use ployz_types::model::{DeployId, DeployRecord};

pub(crate) async fn upsert_deploy(client: &CorrClient, record: &DeployRecord) -> Result<()> {
    let stmt = upsert_statement(record)?;
    exec_one(client, &[stmt], "upsert_deploy").await
}

pub(crate) async fn get_deploy(
    client: &CorrClient,
    deploy_id: &DeployId,
) -> Result<Option<DeployRecord>> {
    let stmt = Statement::WithParams(
        "SELECT deploy_id, namespace, payload_json FROM deploys WHERE deploy_id = ? AND payload_json <> '' LIMIT 1".to_string(),
        vec![deploy_id.0.clone().into()],
    );
    let rows = query_rows(client, &stmt, "get_deploy").await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(parse_deploy(row)?))
}

pub(crate) fn upsert_statement(record: &DeployRecord) -> Result<Statement> {
    let payload_json = serde_json::to_string(record)
        .map_err(|e| Error::operation("upsert_deploy", format!("serialize: {e}")))?;
    Ok(Statement::WithParams(
        "INSERT INTO deploys (deploy_id, namespace, payload_json) VALUES (?, ?, ?) \
         ON CONFLICT(deploy_id) DO UPDATE SET namespace=excluded.namespace, payload_json=excluded.payload_json"
            .to_string(),
        vec![
            record.deploy_id.0.clone().into(),
            record.namespace.0.clone().into(),
            payload_json.into(),
        ],
    ))
}

pub(crate) fn parse_deploy(row: &[SqliteValue]) -> Result<DeployRecord> {
    let [deploy_val, namespace_val, payload_val] = row else {
        return Err(Error::operation(
            "parse_deploy",
            format!("expected 3 columns, got {}", row.len()),
        ));
    };

    let deploy_id = text(deploy_val, "deploy_id")?;
    let namespace = text(namespace_val, "namespace")?;
    let payload_json = text(payload_val, "payload_json")?;
    let record: DeployRecord = serde_json::from_str(&payload_json)
        .map_err(|e| Error::operation("parse_deploy", format!("decode payload: {e}")))?;
    if record.deploy_id.0 != deploy_id || record.namespace.0 != namespace {
        return Err(Error::operation(
            "parse_deploy",
            "deploy key mismatch between row and payload",
        ));
    }
    Ok(record)
}
