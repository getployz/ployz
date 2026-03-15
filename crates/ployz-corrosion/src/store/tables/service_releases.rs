use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::ServiceReleaseRecord;
use ployz_sdk::spec::Namespace;

pub(crate) const SQL_ALL_SERVICE_RELEASES: &str = "SELECT namespace, service, payload_json FROM service_releases WHERE payload_json <> '' ORDER BY namespace, service";

pub(crate) fn all_statement() -> Statement {
    Statement::Simple(SQL_ALL_SERVICE_RELEASES.to_string())
}

pub(crate) async fn load_all_service_releases(
    client: &CorrClient,
) -> Result<Vec<ServiceReleaseRecord>> {
    let stmt = all_statement();
    query_rows(client, &stmt, "load_routing_state")
        .await?
        .iter()
        .map(|row| parse_service_release(row))
        .collect()
}

pub(crate) async fn list_service_releases(
    client: &CorrClient,
    namespace: &Namespace,
) -> Result<Vec<ServiceReleaseRecord>> {
    let stmt = Statement::WithParams(
        "SELECT namespace, service, payload_json FROM service_releases WHERE namespace = ? AND payload_json <> '' ORDER BY service".to_string(),
        vec![namespace.0.clone().into()],
    );
    query_rows(client, &stmt, "list_service_releases")
        .await?
        .iter()
        .map(|row| parse_service_release(row))
        .collect()
}

pub(crate) async fn upsert_service_release(
    client: &CorrClient,
    record: &ServiceReleaseRecord,
) -> Result<()> {
    let stmt = upsert_statement(record)?;
    exec_one(client, &[stmt], "upsert_service_release").await
}

pub(crate) async fn delete_service_release(
    client: &CorrClient,
    namespace: &Namespace,
    service: &str,
) -> Result<()> {
    let stmt = delete_statement(namespace, service);
    exec_one(client, &[stmt], "delete_service_release").await
}

pub(crate) fn delete_statement(namespace: &Namespace, service: &str) -> Statement {
    Statement::WithParams(
        "DELETE FROM service_releases WHERE namespace = ? AND service = ?".to_string(),
        vec![namespace.0.clone().into(), service.to_string().into()],
    )
}

pub(crate) fn upsert_statement(record: &ServiceReleaseRecord) -> Result<Statement> {
    let payload_json = serde_json::to_string(record)
        .map_err(|e| Error::operation("upsert_service_release", format!("serialize: {e}")))?;
    Ok(Statement::WithParams(
        "INSERT INTO service_releases (namespace, service, payload_json) VALUES (?, ?, ?) \
         ON CONFLICT(namespace, service) DO UPDATE SET payload_json=excluded.payload_json"
            .to_string(),
        vec![
            record.namespace.0.clone().into(),
            record.service.clone().into(),
            payload_json.into(),
        ],
    ))
}

pub(crate) fn parse_service_release(row: &[SqliteValue]) -> Result<ServiceReleaseRecord> {
    let [namespace_val, service_val, payload_val] = row else {
        return Err(Error::operation(
            "parse_service_release",
            format!("expected 3 columns, got {}", row.len()),
        ));
    };

    let namespace = text(namespace_val, "namespace")?;
    let service = text(service_val, "service")?;
    let payload_json = text(payload_val, "payload_json")?;

    let record: ServiceReleaseRecord = serde_json::from_str(&payload_json).map_err(|e| {
        Error::operation("parse_service_release", format!("decode payload: {e}"))
    })?;
    if record.namespace.0 != namespace || record.service != service {
        return Err(Error::operation(
            "parse_service_release",
            "service release key mismatch between row and payload",
        ));
    }
    Ok(record)
}
