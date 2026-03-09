use crate::client::CorrClient;
use crate::store::shared::decode::{integer, text};
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::{DeployId, ServiceHeadRecord};
use ployz_sdk::spec::Namespace;

pub(crate) const SQL_ALL_SERVICE_HEADS: &str = "SELECT namespace, service, current_revision_hash, updated_by_deploy_id, updated_at FROM service_heads ORDER BY namespace, service";

pub(crate) fn all_statement() -> Statement {
    Statement::Simple(SQL_ALL_SERVICE_HEADS.to_string())
}

pub(crate) async fn load_all_service_heads(client: &CorrClient) -> Result<Vec<ServiceHeadRecord>> {
    let stmt = all_statement();
    query_rows(client, &stmt, "load_routing_state")
        .await?
        .iter()
        .map(|row| parse_service_head(row))
        .collect()
}

pub(crate) async fn list_service_heads(
    client: &CorrClient,
    namespace: &Namespace,
) -> Result<Vec<ServiceHeadRecord>> {
    let stmt = Statement::WithParams(
        "SELECT namespace, service, current_revision_hash, updated_by_deploy_id, updated_at FROM service_heads WHERE namespace = ? ORDER BY service".to_string(),
        vec![namespace.0.clone().into()],
    );
    query_rows(client, &stmt, "list_service_heads")
        .await?
        .iter()
        .map(|row| parse_service_head(row))
        .collect()
}

pub(crate) async fn upsert_service_head(
    client: &CorrClient,
    record: &ServiceHeadRecord,
) -> Result<()> {
    let stmt = Statement::WithParams(
        "INSERT INTO service_heads (namespace, service, current_revision_hash, updated_by_deploy_id, updated_at) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(namespace, service) DO UPDATE SET current_revision_hash=excluded.current_revision_hash, updated_by_deploy_id=excluded.updated_by_deploy_id, updated_at=excluded.updated_at"
            .to_string(),
        vec![
            record.namespace.0.clone().into(),
            record.service.clone().into(),
            record.current_revision_hash.clone().into(),
            record.updated_by_deploy_id.0.clone().into(),
            (record.updated_at as i64).into(),
        ],
    );
    exec_one(client, &[stmt], "upsert_service_head").await
}

pub(crate) async fn delete_service_head(
    client: &CorrClient,
    namespace: &Namespace,
    service: &str,
) -> Result<()> {
    let stmt = delete_statement(namespace, service);
    exec_one(client, &[stmt], "delete_service_head").await
}

pub(crate) fn delete_statement(namespace: &Namespace, service: &str) -> Statement {
    Statement::WithParams(
        "DELETE FROM service_heads WHERE namespace = ? AND service = ?".to_string(),
        vec![namespace.0.clone().into(), service.to_string().into()],
    )
}

pub(crate) fn insert_statement(record: &ServiceHeadRecord) -> Statement {
    Statement::WithParams(
        "INSERT INTO service_heads (namespace, service, current_revision_hash, updated_by_deploy_id, updated_at) VALUES (?, ?, ?, ?, ?)".to_string(),
        vec![
            record.namespace.0.clone().into(),
            record.service.clone().into(),
            record.current_revision_hash.clone().into(),
            record.updated_by_deploy_id.0.clone().into(),
            (record.updated_at as i64).into(),
        ],
    )
}

pub(crate) fn parse_service_head(row: &[SqliteValue]) -> Result<ServiceHeadRecord> {
    let [
        namespace_val,
        service_val,
        revision_val,
        deploy_val,
        updated_val,
    ] = row
    else {
        return Err(Error::operation(
            "parse_service_head",
            format!("expected 5 columns, got {}", row.len()),
        ));
    };

    Ok(ServiceHeadRecord {
        namespace: Namespace(text(namespace_val, "namespace")?),
        service: text(service_val, "service")?,
        current_revision_hash: text(revision_val, "current_revision_hash")?,
        updated_by_deploy_id: DeployId(text(deploy_val, "updated_by_deploy_id")?),
        updated_at: integer(updated_val, "updated_at")? as u64,
    })
}
