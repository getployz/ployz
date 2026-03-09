use crate::client::CorrClient;
use crate::store::shared::decode::{integer, text};
use crate::store::shared::sql::{exec_all, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::{DeployId, InstanceId, MachineId, ServiceSlotRecord, SlotId};
use ployz_sdk::spec::Namespace;

pub(crate) const SQL_ALL_SERVICE_SLOTS: &str = "SELECT namespace, service, slot_id, machine_id, active_instance_id, revision_hash, updated_by_deploy_id, updated_at FROM service_slots ORDER BY namespace, service, slot_id";

pub(crate) fn all_statement() -> Statement {
    Statement::Simple(SQL_ALL_SERVICE_SLOTS.to_string())
}

pub(crate) async fn load_all_service_slots(client: &CorrClient) -> Result<Vec<ServiceSlotRecord>> {
    let stmt = all_statement();
    query_rows(client, &stmt, "load_routing_state")
        .await?
        .iter()
        .map(|row| parse_service_slot(row))
        .collect()
}

pub(crate) async fn list_service_slots(
    client: &CorrClient,
    namespace: &Namespace,
) -> Result<Vec<ServiceSlotRecord>> {
    let stmt = Statement::WithParams(
        "SELECT namespace, service, slot_id, machine_id, active_instance_id, revision_hash, updated_by_deploy_id, updated_at FROM service_slots WHERE namespace = ? ORDER BY service, slot_id".to_string(),
        vec![namespace.0.clone().into()],
    );
    query_rows(client, &stmt, "list_service_slots")
        .await?
        .iter()
        .map(|row| parse_service_slot(row))
        .collect()
}

pub(crate) async fn replace_service_slots(
    client: &CorrClient,
    namespace: &Namespace,
    service: &str,
    records: &[ServiceSlotRecord],
) -> Result<()> {
    let mut statements = vec![delete_statement(namespace, service)];

    for record in records {
        statements.push(insert_statement(record));
    }

    exec_all(client, &statements, "replace_service_slots").await
}

pub(crate) fn delete_statement(namespace: &Namespace, service: &str) -> Statement {
    Statement::WithParams(
        "DELETE FROM service_slots WHERE namespace = ? AND service = ?".to_string(),
        vec![namespace.0.clone().into(), service.to_string().into()],
    )
}

pub(crate) fn insert_statement(record: &ServiceSlotRecord) -> Statement {
    Statement::WithParams(
        "INSERT INTO service_slots (namespace, service, slot_id, machine_id, active_instance_id, revision_hash, updated_by_deploy_id, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)".to_string(),
        vec![
            record.namespace.0.clone().into(),
            record.service.clone().into(),
            record.slot_id.0.clone().into(),
            record.machine_id.0.clone().into(),
            record.active_instance_id.0.clone().into(),
            record.revision_hash.clone().into(),
            record.updated_by_deploy_id.0.clone().into(),
            (record.updated_at as i64).into(),
        ],
    )
}

pub(crate) fn parse_service_slot(row: &[SqliteValue]) -> Result<ServiceSlotRecord> {
    let [
        namespace_val,
        service_val,
        slot_val,
        machine_val,
        instance_val,
        revision_val,
        deploy_val,
        updated_val,
    ] = row
    else {
        return Err(Error::operation(
            "parse_service_slot",
            format!("expected 8 columns, got {}", row.len()),
        ));
    };

    Ok(ServiceSlotRecord {
        namespace: Namespace(text(namespace_val, "namespace")?),
        service: text(service_val, "service")?,
        slot_id: SlotId(text(slot_val, "slot_id")?),
        machine_id: MachineId(text(machine_val, "machine_id")?),
        active_instance_id: InstanceId(text(instance_val, "active_instance_id")?),
        revision_hash: text(revision_val, "revision_hash")?,
        updated_by_deploy_id: DeployId(text(deploy_val, "updated_by_deploy_id")?),
        updated_at: integer(updated_val, "updated_at")? as u64,
    })
}
