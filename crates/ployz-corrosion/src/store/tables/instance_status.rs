use crate::client::CorrClient;
use crate::store::shared::decode::{integer, text};
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::{
    DeployId, DrainState, InstanceId, InstancePhase, InstanceStatusRecord, MachineId, SlotId,
};
use ployz_sdk::spec::Namespace;
use std::collections::BTreeMap;
use std::net::Ipv4Addr;

pub(crate) const SQL_ALL_INSTANCE_STATUS: &str = "SELECT instance_id, namespace, service, slot_id, machine_id, revision_hash, deploy_id, docker_container_id, overlay_ip, backend_ports_json, phase, ready, drain_state, error, started_at, updated_at FROM instance_status ORDER BY namespace, service, slot_id, instance_id";

pub(crate) fn all_statement() -> Statement {
    Statement::Simple(SQL_ALL_INSTANCE_STATUS.to_string())
}

pub(crate) async fn load_all_instance_status(
    client: &CorrClient,
) -> Result<Vec<InstanceStatusRecord>> {
    let stmt = all_statement();
    query_rows(client, &stmt, "load_routing_state")
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
        "SELECT instance_id, namespace, service, slot_id, machine_id, revision_hash, deploy_id, docker_container_id, overlay_ip, backend_ports_json, phase, ready, drain_state, error, started_at, updated_at FROM instance_status WHERE namespace = ? ORDER BY service, slot_id, instance_id".to_string(),
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
    let backend_ports_json = serde_json::to_string(&record.backend_ports)
        .map_err(|e| Error::operation("upsert_instance_status", format!("serialize: {e}")))?;
    let overlay_ip = record
        .overlay_ip
        .map(|ip| ip.to_string())
        .unwrap_or_default();
    let error = record.error.clone().unwrap_or_default();
    let stmt = Statement::WithParams(
        "INSERT INTO instance_status (instance_id, namespace, service, slot_id, machine_id, revision_hash, deploy_id, docker_container_id, overlay_ip, backend_ports_json, phase, ready, drain_state, error, started_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(instance_id) DO UPDATE SET namespace=excluded.namespace, service=excluded.service, slot_id=excluded.slot_id, machine_id=excluded.machine_id, revision_hash=excluded.revision_hash, deploy_id=excluded.deploy_id, docker_container_id=excluded.docker_container_id, overlay_ip=excluded.overlay_ip, backend_ports_json=excluded.backend_ports_json, phase=excluded.phase, ready=excluded.ready, drain_state=excluded.drain_state, error=excluded.error, started_at=excluded.started_at, updated_at=excluded.updated_at"
            .to_string(),
        vec![
            record.instance_id.0.clone().into(),
            record.namespace.0.clone().into(),
            record.service.clone().into(),
            record.slot_id.0.clone().into(),
            record.machine_id.0.clone().into(),
            record.revision_hash.clone().into(),
            record.deploy_id.0.clone().into(),
            record.docker_container_id.clone().into(),
            overlay_ip.into(),
            backend_ports_json.into(),
            record.phase.to_string().into(),
            (if record.ready { 1_i64 } else { 0_i64 }).into(),
            record.drain_state.to_string().into(),
            error.into(),
            (record.started_at as i64).into(),
            (record.updated_at as i64).into(),
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
        slot_val,
        machine_val,
        revision_val,
        deploy_val,
        docker_val,
        overlay_val,
        backend_ports_val,
        phase_val,
        ready_val,
        drain_val,
        error_val,
        started_val,
        updated_val,
    ] = row
    else {
        return Err(Error::operation(
            "parse_instance_status",
            format!("expected 16 columns, got {}", row.len()),
        ));
    };

    let overlay_str = text(overlay_val, "overlay_ip")?;
    let overlay_ip = if overlay_str.is_empty() {
        None
    } else {
        Some(overlay_str.parse::<Ipv4Addr>().map_err(|e| {
            Error::operation("parse_instance_status", format!("invalid overlay ip: {e}"))
        })?)
    };
    let backend_ports: BTreeMap<String, u16> =
        serde_json::from_str(&text(backend_ports_val, "backend_ports_json")?).map_err(|e| {
            Error::operation(
                "parse_instance_status",
                format!("invalid backend ports json: {e}"),
            )
        })?;
    let phase: InstancePhase = text(phase_val, "phase")?
        .parse()
        .map_err(|e: strum::ParseError| Error::operation("parse_instance_status", e.to_string()))?;
    let drain_state: DrainState = text(drain_val, "drain_state")?
        .parse()
        .map_err(|e: strum::ParseError| Error::operation("parse_instance_status", e.to_string()))?;
    let error = text(error_val, "error")?;

    Ok(InstanceStatusRecord {
        instance_id: InstanceId(text(instance_val, "instance_id")?),
        namespace: Namespace(text(namespace_val, "namespace")?),
        service: text(service_val, "service")?,
        slot_id: SlotId(text(slot_val, "slot_id")?),
        machine_id: MachineId(text(machine_val, "machine_id")?),
        revision_hash: text(revision_val, "revision_hash")?,
        deploy_id: DeployId(text(deploy_val, "deploy_id")?),
        docker_container_id: text(docker_val, "docker_container_id")?,
        overlay_ip,
        backend_ports,
        phase,
        ready: integer(ready_val, "ready")? != 0,
        drain_state,
        error: if error.is_empty() { None } else { Some(error) },
        started_at: integer(started_val, "started_at")? as u64,
        updated_at: integer(updated_val, "updated_at")? as u64,
    })
}
