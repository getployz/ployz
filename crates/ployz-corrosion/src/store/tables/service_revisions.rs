use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_types::error::{Error, Result};
use ployz_types::model::ServiceRevisionRecord;
use ployz_types::spec::Namespace;

pub(crate) const SQL_ALL_SERVICE_REVISIONS: &str = "SELECT namespace, service, revision_hash, payload_json FROM service_revisions WHERE payload_json <> '' ORDER BY namespace, service, revision_hash";

pub(crate) fn all_statement() -> Statement {
    Statement::Simple(SQL_ALL_SERVICE_REVISIONS.to_string())
}

pub(crate) async fn load_all_service_revisions(
    client: &CorrClient,
) -> Result<Vec<ServiceRevisionRecord>> {
    let stmt = all_statement();
    query_rows(client, &stmt, "load_routing_state")
        .await?
        .iter()
        .map(|row| parse_service_revision(row))
        .collect()
}

pub(crate) async fn list_service_revisions(
    client: &CorrClient,
    namespace: &Namespace,
) -> Result<Vec<ServiceRevisionRecord>> {
    let stmt = Statement::WithParams(
        "SELECT namespace, service, revision_hash, payload_json FROM service_revisions WHERE namespace = ? AND payload_json <> '' ORDER BY service, revision_hash".to_string(),
        vec![namespace.0.clone().into()],
    );
    query_rows(client, &stmt, "list_service_revisions")
        .await?
        .iter()
        .map(|row| parse_service_revision(row))
        .collect()
}

pub(crate) async fn upsert_service_revision(
    client: &CorrClient,
    record: &ServiceRevisionRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)
        .map_err(|e| Error::operation("upsert_service_revision", format!("serialize: {e}")))?;
    let stmt = Statement::WithParams(
        "INSERT INTO service_revisions (namespace, service, revision_hash, payload_json) VALUES (?, ?, ?, ?) \
         ON CONFLICT(namespace, service, revision_hash) DO UPDATE SET payload_json = CASE WHEN service_revisions.payload_json = '' THEN excluded.payload_json ELSE service_revisions.payload_json END"
            .to_string(),
        vec![
            record.namespace.0.clone().into(),
            record.service.clone().into(),
            record.revision_hash.clone().into(),
            payload_json.into(),
        ],
    );
    exec_one(client, &[stmt], "upsert_service_revision").await
}

pub(crate) fn parse_service_revision(row: &[SqliteValue]) -> Result<ServiceRevisionRecord> {
    let [namespace_val, service_val, revision_val, payload_val] = row else {
        return Err(Error::operation(
            "parse_service_revision",
            format!("expected 4 columns, got {}", row.len()),
        ));
    };

    let namespace = text(namespace_val, "namespace")?;
    let service = text(service_val, "service")?;
    let revision_hash = text(revision_val, "revision_hash")?;
    let payload_json = text(payload_val, "payload_json")?;

    let record: ServiceRevisionRecord = serde_json::from_str(&payload_json)
        .map_err(|e| Error::operation("parse_service_revision", format!("decode payload: {e}")))?;
    if record.namespace.0 != namespace
        || record.service != service
        || record.revision_hash != revision_hash
    {
        return Err(Error::operation(
            "parse_service_revision",
            "revision key mismatch between row and payload",
        ));
    }
    Ok(record)
}
