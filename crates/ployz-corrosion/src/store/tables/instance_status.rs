use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_types::error::{Error, Result};
use ployz_types::model::{InstanceId, InstanceStatusRecord};
use ployz_types::spec::Namespace;

const SQL_ALL_INSTANCE_STATUS: &str = "SELECT instance_id, namespace, service, machine_id, payload_json FROM instance_status WHERE payload_json <> '' ORDER BY namespace, service, machine_id, instance_id";

pub(crate) fn all_statement() -> Statement {
    Statement::Simple(SQL_ALL_INSTANCE_STATUS.to_string())
}

pub(crate) async fn load_all_instance_status(
    client: &CorrClient,
) -> Result<Vec<InstanceStatusRecord>> {
    let stmt = all_statement();
    query_rows(client, &stmt, "load_all_instance_status")
        .await?
        .iter()
        .map(|row| parse_instance_status(row))
        .collect()
}

pub(crate) async fn list_instance_status(
    client: &CorrClient,
    namespace: &Namespace,
) -> Result<Vec<InstanceStatusRecord>> {
    let stmt = Statement::WithParams(
        "SELECT instance_id, namespace, service, machine_id, payload_json FROM instance_status WHERE namespace = ? AND payload_json <> '' ORDER BY service, machine_id, instance_id".to_string(),
        vec![namespace.0.clone().into()],
    );
    query_rows(client, &stmt, "list_instance_status")
        .await?
        .iter()
        .map(|row| parse_instance_status(row))
        .collect()
}

pub(crate) async fn upsert_instance_status(
    client: &CorrClient,
    record: &InstanceStatusRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)
        .map_err(|e| Error::operation("upsert_instance_status", format!("serialize: {e}")))?;
    let stmt = Statement::WithParams(
        "INSERT INTO instance_status (instance_id, namespace, service, machine_id, payload_json) VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(instance_id) DO UPDATE SET namespace=excluded.namespace, service=excluded.service, machine_id=excluded.machine_id, payload_json=excluded.payload_json"
            .to_string(),
        vec![
            record.instance_id.0.clone().into(),
            record.namespace.0.clone().into(),
            record.service.clone().into(),
            record.machine_id.0.clone().into(),
            payload_json.into(),
        ],
    );
    exec_one(client, &[stmt], "upsert_instance_status").await
}

pub(crate) async fn delete_instance_status(
    client: &CorrClient,
    instance_id: &InstanceId,
) -> Result<()> {
    let stmt = Statement::WithParams(
        "DELETE FROM instance_status WHERE instance_id = ?".to_string(),
        vec![instance_id.0.clone().into()],
    );
    exec_one(client, &[stmt], "delete_instance_status").await
}

pub(crate) fn parse_instance_status(row: &[SqliteValue]) -> Result<InstanceStatusRecord> {
    let [
        instance_val,
        namespace_val,
        service_val,
        machine_val,
        payload_val,
    ] = row
    else {
        return Err(Error::operation(
            "parse_instance_status",
            format!("expected 5 columns, got {}", row.len()),
        ));
    };

    let instance_id = text(instance_val, "instance_id")?;
    let namespace = text(namespace_val, "namespace")?;
    let service = text(service_val, "service")?;
    let machine_id = text(machine_val, "machine_id")?;
    let payload_json = text(payload_val, "payload_json")?;

    let record: InstanceStatusRecord = serde_json::from_str(&payload_json)
        .map_err(|e| Error::operation("parse_instance_status", format!("decode payload: {e}")))?;
    if record.instance_id.0 != instance_id
        || record.namespace.0 != namespace
        || record.service != service
        || record.machine_id.0 != machine_id
    {
        return Err(Error::operation(
            "parse_instance_status",
            "instance status key mismatch between row and payload",
        ));
    }
    Ok(record)
}
