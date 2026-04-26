use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::query_rows;
use corro_api_types::{SqliteValue, Statement};
use ployz_types::error::{Error, Result};
use ployz_types::model::VolumeRecord;
use ployz_types::spec::Namespace;

pub(crate) async fn list_volumes(
    client: &CorrClient,
    namespace: &Namespace,
) -> Result<Vec<VolumeRecord>> {
    let stmt = Statement::WithParams(
        "SELECT namespace, volume_name, payload_json FROM volumes WHERE namespace = ? AND payload_json <> '' ORDER BY volume_name".to_string(),
        vec![namespace.0.clone().into()],
    );
    query_rows(client, &stmt, "list_volumes")
        .await?
        .iter()
        .map(|row| parse_volume(row))
        .collect()
}

pub(crate) async fn get_volume(
    client: &CorrClient,
    namespace: &Namespace,
    volume_name: &str,
) -> Result<Option<VolumeRecord>> {
    let stmt = Statement::WithParams(
        "SELECT namespace, volume_name, payload_json FROM volumes WHERE namespace = ? AND volume_name = ? AND payload_json <> ''".to_string(),
        vec![namespace.0.clone().into(), volume_name.to_string().into()],
    );
    let rows = query_rows(client, &stmt, "get_volume").await?;
    match rows.as_slice() {
        [] => Ok(None),
        [row] => parse_volume(row).map(Some),
        more => Err(Error::operation(
            "get_volume",
            format!(
                "expected at most one row for volume '{volume_name}' in namespace '{}', got {}",
                namespace.0,
                more.len()
            ),
        )),
    }
}

pub(crate) fn upsert_statement(record: &VolumeRecord) -> Result<Statement> {
    let payload_json = serde_json::to_string(record)
        .map_err(|e| Error::operation("upsert_volume", format!("serialize: {e}")))?;
    Ok(Statement::WithParams(
        "INSERT INTO volumes (namespace, volume_name, payload_json) VALUES (?, ?, ?) \
         ON CONFLICT(namespace, volume_name) DO UPDATE SET payload_json=excluded.payload_json"
            .to_string(),
        vec![
            record.namespace.0.clone().into(),
            record.volume_name.clone().into(),
            payload_json.into(),
        ],
    ))
}

fn parse_volume(row: &[SqliteValue]) -> Result<VolumeRecord> {
    let [namespace_val, volume_name_val, payload_val] = row else {
        return Err(Error::operation(
            "parse_volume",
            format!("expected 3 columns, got {}", row.len()),
        ));
    };
    let namespace = text(namespace_val, "namespace")?;
    let volume_name = text(volume_name_val, "volume_name")?;
    let payload_json = text(payload_val, "payload_json")?;
    let record: VolumeRecord = serde_json::from_str(&payload_json)
        .map_err(|e| Error::operation("parse_volume", format!("decode payload: {e}")))?;
    if record.namespace.0 != namespace || record.volume_name != volume_name {
        return Err(Error::operation(
            "parse_volume",
            "volume key mismatch between row and payload",
        ));
    }
    Ok(record)
}
