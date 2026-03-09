use crate::client::CorrClient;
use crate::store::shared::decode::{integer, text};
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::{MachineId, ServiceRevisionRecord};
use ployz_sdk::spec::Namespace;

pub(crate) const SQL_ALL_SERVICE_REVISIONS: &str = "SELECT namespace, service, revision_hash, spec_json, created_by, created_at FROM service_revisions ORDER BY namespace, service, revision_hash";

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

pub(crate) async fn upsert_service_revision(
    client: &CorrClient,
    record: &ServiceRevisionRecord,
) -> Result<()> {
    let stmt = Statement::WithParams(
        "INSERT OR IGNORE INTO service_revisions (namespace, service, revision_hash, spec_json, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?)".to_string(),
        vec![
            record.namespace.0.clone().into(),
            record.service.clone().into(),
            record.revision_hash.clone().into(),
            record.spec_json.clone().into(),
            record.created_by.0.clone().into(),
            (record.created_at as i64).into(),
        ],
    );
    exec_one(client, &[stmt], "upsert_service_revision").await
}

pub(crate) fn parse_service_revision(row: &[SqliteValue]) -> Result<ServiceRevisionRecord> {
    let [
        namespace_val,
        service_val,
        revision_val,
        spec_val,
        created_by_val,
        created_at_val,
    ] = row
    else {
        return Err(Error::operation(
            "parse_service_revision",
            format!("expected 6 columns, got {}", row.len()),
        ));
    };

    Ok(ServiceRevisionRecord {
        namespace: Namespace(text(namespace_val, "namespace")?),
        service: text(service_val, "service")?,
        revision_hash: text(revision_val, "revision_hash")?,
        spec_json: text(spec_val, "spec_json")?,
        created_by: MachineId(text(created_by_val, "created_by")?),
        created_at: integer(created_at_val, "created_at")? as u64,
    })
}
