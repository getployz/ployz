use crate::error::{Error, Result};
use crate::model::{
    DeployId, DeployRecord, DeployState, DrainState, InstanceId, InstancePhase,
    InstanceStatusRecord, InviteRecord, MachineEvent, MachineId, MachineRecord, MachineStatus,
    OverlayIp, Participation, PublicKey, ServiceHeadRecord, ServiceRevisionRecord,
    ServiceSlotRecord, SlotId,
};
use crate::spec::Namespace;
use crate::store::{DeployStore, InviteStore, MachineStore, SyncProbe, SyncStatus};
use corro_api_types::{ExecResult, SqliteValue, Statement, TypedQueryEvent, sqlite::ChangeType};
use futures_util::StreamExt;
use ipnet::Ipv4Net;
use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use tokio::sync::mpsc;
use tracing::warn;

pub mod client;
pub mod config;
pub mod docker;
pub mod host;

pub use client::{CorrClient, Transport};

pub const SCHEMA_SQL: &str = include_str!("schema.sql");

#[derive(Clone)]
pub struct CorrosionStore {
    client: CorrClient,
}

impl CorrosionStore {
    pub fn new(api_addr: SocketAddr, transport: Transport) -> Self {
        let client = CorrClient::new(api_addr, transport);
        Self { client }
    }
}

impl SyncProbe for CorrosionStore {
    async fn sync_status(&self) -> Result<SyncStatus> {
        let health = self
            .client
            .health()
            .await
            .map_err(|e| Error::operation("sync_status", format!("health request: {e}")))?;

        let status = if health.members < 1 {
            SyncStatus::Disconnected
        } else if health.gaps > 0 {
            SyncStatus::Syncing {
                gaps: health.gaps as u64,
            }
        } else {
            SyncStatus::Synced
        };

        Ok(status)
    }
}

impl MachineStore for CorrosionStore {
    async fn init(&self) -> Result<()> {
        let res = self
            .client
            .schema(&[Statement::Simple(SCHEMA_SQL.to_string())])
            .await
            .map_err(|e| Error::operation("schema", e.to_string()))?;
        if let Some(ExecResult::Error { error }) = res.results.first() {
            return Err(Error::operation("schema", error.clone()));
        }
        Ok(())
    }

    async fn list_machines(&self) -> Result<Vec<MachineRecord>> {
        let stmt = Statement::Simple(
            "SELECT id, public_key, overlay_ip, subnet, bridge_ip, endpoints, status, participation, last_heartbeat, created_at, updated_at FROM machines ORDER BY id".to_string(),
        );
        query_rows(&self.client, &stmt, "list_machines")
            .await?
            .iter()
            .map(|row| parse_machine(row))
            .collect()
    }

    /// Upsert a full machine record. Callers should only upsert their own row —
    /// each machine owns its record. The one exception is initial onboarding,
    /// where the founder seeds a joiner's record.
    async fn upsert_machine(&self, record: &MachineRecord) -> Result<()> {
        let endpoints = serde_json::to_string(&record.endpoints)
            .map_err(|e| Error::operation("upsert_machine", format!("serialize: {e}")))?;
        let subnet_str = record.subnet.map(|s| s.to_string()).unwrap_or_default();
        let bridge_str = record
            .bridge_ip
            .map(|b| b.0.to_string())
            .unwrap_or_default();
        let stmt = Statement::WithParams(
            "INSERT INTO machines (id, public_key, overlay_ip, subnet, bridge_ip, endpoints, status, participation, last_heartbeat, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET public_key=excluded.public_key, \
             overlay_ip=excluded.overlay_ip, subnet=excluded.subnet, \
             bridge_ip=excluded.bridge_ip, endpoints=excluded.endpoints, \
             status=excluded.status, participation=excluded.participation, \
             last_heartbeat=excluded.last_heartbeat, \
             created_at=CASE WHEN machines.created_at > 0 THEN machines.created_at ELSE excluded.created_at END, \
             updated_at=excluded.updated_at"
                .to_string(),
            vec![
                record.id.0.clone().into(),
                record.public_key.0.to_vec().into(),
                record.overlay_ip.0.to_string().into(),
                subnet_str.into(),
                bridge_str.into(),
                endpoints.into(),
                record.status.to_string().into(),
                record.participation.to_string().into(),
                (record.last_heartbeat as i64).into(),
                (record.created_at as i64).into(),
                (record.updated_at as i64).into(),
            ],
        );
        exec_one(&self.client, &[stmt], "upsert_machine").await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        let stmt = Statement::WithParams(
            "DELETE FROM machines WHERE id = ?".to_string(),
            vec![id.0.clone().into()],
        );
        exec_one(&self.client, &[stmt], "delete_machine").await
    }

    async fn subscribe_machines(
        &self,
    ) -> Result<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        let stmt = Statement::Simple(
            "SELECT id, public_key, overlay_ip, subnet, bridge_ip, endpoints, status, participation, last_heartbeat, created_at, updated_at FROM machines ORDER BY id".to_string(),
        );
        let mut stream = self
            .client
            .subscribe(&stmt, false, None)
            .await
            .map_err(|e| Error::operation("subscribe_machines", e.to_string()))?;

        // Collect initial snapshot from Row events until EndOfQuery
        let mut machines: HashMap<MachineId, MachineRecord> = HashMap::new();
        let mut row_index: HashMap<u64, MachineId> = HashMap::new();

        loop {
            let event = stream
                .next()
                .await
                .ok_or_else(|| {
                    Error::operation("subscribe_machines", "stream ended during snapshot")
                })?
                .map_err(|e| Error::operation("subscribe_machines", e.to_string()))?;

            match event {
                TypedQueryEvent::Columns(_) => {}
                TypedQueryEvent::EndOfQuery { .. } => break,
                TypedQueryEvent::Error(e) => {
                    return Err(Error::operation("subscribe_machines", e.to_string()));
                }
                TypedQueryEvent::Row(rowid, cells) => {
                    let record = parse_machine(&cells)?;
                    row_index.insert(rowid.0, record.id.clone());
                    machines.insert(record.id.clone(), record);
                }
                TypedQueryEvent::Change(ct, rowid, cells, _) => {
                    apply_change(ct, rowid.0, &cells, &mut machines, &mut row_index)?;
                }
            }
        }

        let mut snapshot: Vec<MachineRecord> = machines.values().cloned().collect();
        snapshot.sort_by(|a, b| a.id.0.cmp(&b.id.0));

        let (tx, rx) = mpsc::channel(64);

        // SubscriptionStream handles reconnection with backoff internally.
        // We race stream.next() against tx.closed() so that when the peer
        // sync task drops the receiver we stop immediately instead of
        // waiting through the full reconnect backoff cycle.
        tokio::spawn(async move {
            loop {
                let result = tokio::select! {
                    r = stream.next() => match r {
                        Some(r) => r,
                        None => {
                            warn!("machine subscription ended");
                            return;
                        }
                    },
                    _ = tx.closed() => return,
                };
                match result {
                    Ok(event) => match into_machine_event(event, &mut machines, &mut row_index) {
                        Ok(Some(ev)) => {
                            if tx.send(ev).await.is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => warn!(?e, "bad subscription event"),
                    },
                    Err(e) => {
                        warn!(%e, "subscription error");
                    }
                }
            }
        });

        Ok((snapshot, rx))
    }
}

impl InviteStore for CorrosionStore {
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        let stmt = Statement::WithParams(
            "INSERT INTO invites (id, expires_at) VALUES (?, ?)".to_string(),
            vec![invite.id.clone().into(), (invite.expires_at as i64).into()],
        );
        let res = self
            .client
            .execute(&[stmt], None)
            .await
            .map_err(|e| Error::operation("create_invite", e.to_string()))?;
        match res.results.first() {
            Some(ExecResult::Error { error }) if error.contains("UNIQUE") => {
                Err(Error::operation("invite_exists", error.clone()))
            }
            Some(ExecResult::Error { error }) => {
                Err(Error::operation("create_invite", error.clone()))
            }
            Some(ExecResult::Execute { .. }) => Ok(()),
            None => Err(Error::operation("create_invite", "no result")),
        }
    }

    async fn consume_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<()> {
        let stmt = Statement::WithParams(
            "DELETE FROM invites WHERE id = ? AND expires_at >= ?".to_string(),
            vec![invite_id.to_string().into(), (now_unix_secs as i64).into()],
        );
        let res = self
            .client
            .execute(&[stmt], None)
            .await
            .map_err(|e| Error::operation("consume_invite", e.to_string()))?;

        match res.results.first() {
            Some(ExecResult::Execute { rows_affected, .. }) if *rows_affected == 1 => Ok(()),
            Some(ExecResult::Error { error }) => {
                Err(Error::operation("consume_invite", error.clone()))
            }
            _ => {
                // Distinguish not-found from expired
                let lookup = Statement::WithParams(
                    "SELECT id, expires_at FROM invites WHERE id = ? LIMIT 1".to_string(),
                    vec![invite_id.to_string().into()],
                );
                if query_rows(&self.client, &lookup, "consume_invite")
                    .await?
                    .is_empty()
                {
                    Err(Error::operation(
                        "invite_not_found",
                        format!("invite '{invite_id}' not found"),
                    ))
                } else {
                    Err(Error::operation(
                        "invite_expired",
                        format!("invite '{invite_id}' is expired"),
                    ))
                }
            }
        }
    }
}

impl DeployStore for CorrosionStore {
    async fn list_service_heads(&self, namespace: &Namespace) -> Result<Vec<ServiceHeadRecord>> {
        let stmt = Statement::WithParams(
            "SELECT namespace, service, current_revision_hash, updated_by_deploy_id, updated_at FROM service_heads WHERE namespace = ? ORDER BY service".to_string(),
            vec![namespace.0.clone().into()],
        );
        query_rows(&self.client, &stmt, "list_service_heads")
            .await?
            .iter()
            .map(|row| parse_service_head(row))
            .collect()
    }

    async fn list_service_slots(&self, namespace: &Namespace) -> Result<Vec<ServiceSlotRecord>> {
        let stmt = Statement::WithParams(
            "SELECT namespace, service, slot_id, machine_id, active_instance_id, revision_hash, updated_by_deploy_id, updated_at FROM service_slots WHERE namespace = ? ORDER BY service, slot_id".to_string(),
            vec![namespace.0.clone().into()],
        );
        query_rows(&self.client, &stmt, "list_service_slots")
            .await?
            .iter()
            .map(|row| parse_service_slot(row))
            .collect()
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        let stmt = Statement::WithParams(
            "SELECT instance_id, namespace, service, slot_id, machine_id, revision_hash, deploy_id, docker_container_id, overlay_ip, backend_ports_json, phase, ready, drain_state, error, started_at, updated_at FROM instance_status WHERE namespace = ? ORDER BY service, slot_id, instance_id".to_string(),
            vec![namespace.0.clone().into()],
        );
        query_rows(&self.client, &stmt, "list_instance_status")
            .await?
            .iter()
            .map(|row| parse_instance_status(row))
            .collect()
    }

    async fn upsert_service_revision(&self, record: &ServiceRevisionRecord) -> Result<()> {
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
        exec_one(&self.client, &[stmt], "upsert_service_revision").await
    }

    async fn upsert_service_head(&self, record: &ServiceHeadRecord) -> Result<()> {
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
        exec_one(&self.client, &[stmt], "upsert_service_head").await
    }

    async fn delete_service_head(&self, namespace: &Namespace, service: &str) -> Result<()> {
        let stmt = Statement::WithParams(
            "DELETE FROM service_heads WHERE namespace = ? AND service = ?".to_string(),
            vec![namespace.0.clone().into(), service.to_string().into()],
        );
        exec_one(&self.client, &[stmt], "delete_service_head").await
    }

    async fn replace_service_slots(
        &self,
        namespace: &Namespace,
        service: &str,
        records: &[ServiceSlotRecord],
    ) -> Result<()> {
        let mut statements = vec![Statement::WithParams(
            "DELETE FROM service_slots WHERE namespace = ? AND service = ?".to_string(),
            vec![namespace.0.clone().into(), service.to_string().into()],
        )];

        for record in records {
            statements.push(Statement::WithParams(
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
            ));
        }

        exec_all(&self.client, &statements, "replace_service_slots").await
    }

    async fn upsert_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
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
        exec_one(&self.client, &[stmt], "upsert_instance_status").await
    }

    async fn delete_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        let stmt = Statement::WithParams(
            "DELETE FROM instance_status WHERE instance_id = ?".to_string(),
            vec![instance_id.0.clone().into()],
        );
        exec_one(&self.client, &[stmt], "delete_instance_status").await
    }

    async fn upsert_deploy(&self, record: &DeployRecord) -> Result<()> {
        let committed_at = record.committed_at.unwrap_or(0);
        let finished_at = record.finished_at.unwrap_or(0);
        let stmt = Statement::WithParams(
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
        );
        exec_one(&self.client, &[stmt], "upsert_deploy").await
    }

    async fn commit_deploy(
        &self,
        namespace: &Namespace,
        removed_services: &[String],
        heads: &[ServiceHeadRecord],
        slots: &[ServiceSlotRecord],
        deploy: &DeployRecord,
    ) -> Result<()> {
        let mut touched_services: Vec<String> = removed_services.to_vec();
        for head in heads {
            if !touched_services.contains(&head.service) {
                touched_services.push(head.service.clone());
            }
        }

        let mut statements = Vec::new();
        for service in &touched_services {
            statements.push(Statement::WithParams(
                "DELETE FROM service_heads WHERE namespace = ? AND service = ?".to_string(),
                vec![namespace.0.clone().into(), service.clone().into()],
            ));
            statements.push(Statement::WithParams(
                "DELETE FROM service_slots WHERE namespace = ? AND service = ?".to_string(),
                vec![namespace.0.clone().into(), service.clone().into()],
            ));
        }

        for head in heads {
            statements.push(Statement::WithParams(
                "INSERT INTO service_heads (namespace, service, current_revision_hash, updated_by_deploy_id, updated_at) VALUES (?, ?, ?, ?, ?)".to_string(),
                vec![
                    head.namespace.0.clone().into(),
                    head.service.clone().into(),
                    head.current_revision_hash.clone().into(),
                    head.updated_by_deploy_id.0.clone().into(),
                    (head.updated_at as i64).into(),
                ],
            ));
        }

        for slot in slots {
            statements.push(Statement::WithParams(
                "INSERT INTO service_slots (namespace, service, slot_id, machine_id, active_instance_id, revision_hash, updated_by_deploy_id, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?)".to_string(),
                vec![
                    slot.namespace.0.clone().into(),
                    slot.service.clone().into(),
                    slot.slot_id.0.clone().into(),
                    slot.machine_id.0.clone().into(),
                    slot.active_instance_id.0.clone().into(),
                    slot.revision_hash.clone().into(),
                    slot.updated_by_deploy_id.0.clone().into(),
                    (slot.updated_at as i64).into(),
                ],
            ));
        }

        let committed_at = deploy.committed_at.unwrap_or(0);
        let finished_at = deploy.finished_at.unwrap_or(0);
        statements.push(Statement::WithParams(
            "INSERT INTO deploys (deploy_id, namespace, coordinator_machine_id, manifest_hash, state, started_at, committed_at, finished_at, summary_json) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(deploy_id) DO UPDATE SET namespace=excluded.namespace, coordinator_machine_id=excluded.coordinator_machine_id, manifest_hash=excluded.manifest_hash, state=excluded.state, started_at=excluded.started_at, committed_at=excluded.committed_at, finished_at=excluded.finished_at, summary_json=excluded.summary_json"
                .to_string(),
            vec![
                deploy.deploy_id.0.clone().into(),
                deploy.namespace.0.clone().into(),
                deploy.coordinator_machine_id.0.clone().into(),
                deploy.manifest_hash.clone().into(),
                deploy.state.to_string().into(),
                (deploy.started_at as i64).into(),
                (committed_at as i64).into(),
                (finished_at as i64).into(),
                deploy.summary_json.clone().into(),
            ],
        ));

        exec_all(&self.client, &statements, "commit_deploy").await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        let stmt = Statement::WithParams(
            "SELECT deploy_id, namespace, coordinator_machine_id, manifest_hash, state, started_at, committed_at, finished_at, summary_json FROM deploys WHERE deploy_id = ? LIMIT 1".to_string(),
            vec![deploy_id.0.clone().into()],
        );
        let rows = query_rows(&self.client, &stmt, "get_deploy").await?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        Ok(Some(parse_deploy(row)?))
    }
}

// --- query/exec helpers ---

async fn query_rows(
    client: &CorrClient,
    stmt: &Statement,
    op: &'static str,
) -> Result<Vec<Vec<SqliteValue>>> {
    let mut stream = client
        .query(stmt, None)
        .await
        .map_err(|e| Error::operation(op, e.to_string()))?;
    let mut rows = Vec::new();
    while let Some(event) = stream.next().await {
        match event.map_err(|e| Error::operation(op, e.to_string()))? {
            TypedQueryEvent::Row(_, cells) => rows.push(cells),
            TypedQueryEvent::EndOfQuery { .. } => break,
            TypedQueryEvent::Error(e) => return Err(Error::operation(op, e.to_string())),
            _ => {}
        }
    }
    Ok(rows)
}

async fn exec_one(client: &CorrClient, stmts: &[Statement], op: &'static str) -> Result<()> {
    let res = client
        .execute(stmts, None)
        .await
        .map_err(|e| Error::operation(op, e.to_string()))?;
    match res.results.first() {
        Some(ExecResult::Execute { .. }) => Ok(()),
        Some(ExecResult::Error { error }) => Err(Error::operation(op, error.clone())),
        None => Err(Error::operation(op, "no result")),
    }
}

async fn exec_all(client: &CorrClient, stmts: &[Statement], op: &'static str) -> Result<()> {
    let res = client
        .execute(stmts, None)
        .await
        .map_err(|e| Error::operation(op, e.to_string()))?;
    for result in &res.results {
        if let ExecResult::Error { error } = result {
            return Err(Error::operation(op, error.clone()));
        }
    }
    Ok(())
}

// --- subscription helpers ---

fn apply_change(
    ct: ChangeType,
    rowid: u64,
    cells: &[SqliteValue],
    machines: &mut HashMap<MachineId, MachineRecord>,
    row_index: &mut HashMap<u64, MachineId>,
) -> Result<()> {
    match ct {
        ChangeType::Insert | ChangeType::Update => {
            let record = parse_machine(cells)?;
            row_index.insert(rowid, record.id.clone());
            machines.insert(record.id.clone(), record);
        }
        ChangeType::Delete => {
            if let Some(id) = row_index.remove(&rowid) {
                machines.remove(&id);
            }
        }
    }
    Ok(())
}

fn upsert_event(
    rowid: u64,
    cells: &[SqliteValue],
    known: &mut HashMap<MachineId, MachineRecord>,
    row_index: &mut HashMap<u64, MachineId>,
) -> Result<Option<MachineEvent>> {
    let record = parse_machine(cells)?;
    let is_update = known.contains_key(&record.id);
    row_index.insert(rowid, record.id.clone());
    known.insert(record.id.clone(), record.clone());
    Ok(Some(if is_update {
        MachineEvent::Updated(record)
    } else {
        MachineEvent::Added(record)
    }))
}

fn into_machine_event(
    event: TypedQueryEvent<Vec<SqliteValue>>,
    known: &mut HashMap<MachineId, MachineRecord>,
    row_index: &mut HashMap<u64, MachineId>,
) -> Result<Option<MachineEvent>> {
    match event {
        TypedQueryEvent::Columns(_) | TypedQueryEvent::EndOfQuery { .. } => Ok(None),
        TypedQueryEvent::Error(e) => Err(Error::operation("subscribe_machines", e.to_string())),
        TypedQueryEvent::Row(rowid, cells) => upsert_event(rowid.0, &cells, known, row_index),
        TypedQueryEvent::Change(ct, rowid, cells, _) => match ct {
            ChangeType::Insert | ChangeType::Update => {
                upsert_event(rowid.0, &cells, known, row_index)
            }
            ChangeType::Delete => {
                if let Some(id) = row_index.remove(&rowid.0) {
                    if let Some(record) = known.remove(&id) {
                        Ok(Some(MachineEvent::Removed(record)))
                    } else {
                        Ok(None)
                    }
                } else {
                    Ok(None)
                }
            }
        },
    }
}

// --- row parsing ---

fn parse_machine(row: &[SqliteValue]) -> Result<MachineRecord> {
    let [
        id_val,
        key_val,
        overlay_val,
        subnet_val,
        bridge_val,
        endpoints_val,
        status_val,
        participation_val,
        heartbeat_val,
        created_val,
        updated_val,
    ] = row
    else {
        return Err(Error::operation(
            "parse_machine",
            format!("expected 11 columns, got {}", row.len()),
        ));
    };

    let id = text(id_val, "id")?;
    let key_blob = blob(key_val, "public_key")?;
    let overlay = text(overlay_val, "overlay_ip")?;
    let subnet_str = text(subnet_val, "subnet")?;
    let bridge_str = text(bridge_val, "bridge_ip")?;
    let endpoints_json = text(endpoints_val, "endpoints")?;
    let status_str = text(status_val, "status")?;
    let participation_str = text(participation_val, "participation")?;
    let last_heartbeat = integer(heartbeat_val, "last_heartbeat")? as u64;
    let created_at = integer(created_val, "created_at")? as u64;
    let updated_at = integer(updated_val, "updated_at")? as u64;

    let public_key: [u8; 32] = key_blob.as_slice().try_into().map_err(|_| {
        Error::operation(
            "parse_machine",
            format!("public key must be 32 bytes, got {}", key_blob.len()),
        )
    })?;
    let overlay_ip: Ipv6Addr = overlay
        .parse()
        .map_err(|e| Error::operation("parse_machine", format!("invalid overlay ip: {e}")))?;

    let subnet: Option<Ipv4Net> = if subnet_str.is_empty() {
        None
    } else {
        Some(
            subnet_str
                .parse()
                .map_err(|e| Error::operation("parse_machine", format!("invalid subnet: {e}")))?,
        )
    };

    let bridge_ip: Option<OverlayIp> = if bridge_str.is_empty() {
        None
    } else {
        let addr: Ipv6Addr = bridge_str
            .parse()
            .map_err(|e| Error::operation("parse_machine", format!("invalid bridge ip: {e}")))?;
        Some(OverlayIp(addr))
    };

    let endpoints: Vec<String> = serde_json::from_str(&endpoints_json)
        .map_err(|e| Error::operation("parse_machine", format!("invalid endpoints json: {e}")))?;

    let status: MachineStatus = status_str
        .parse()
        .map_err(|e| Error::operation("parse_machine", format!("invalid status: {e}")))?;
    let participation: Participation = participation_str
        .parse()
        .map_err(|e| Error::operation("parse_machine", format!("invalid participation: {e}")))?;

    Ok(MachineRecord {
        id: MachineId(id),
        public_key: PublicKey(public_key),
        overlay_ip: OverlayIp(overlay_ip),
        subnet,
        bridge_ip,
        endpoints,
        status,
        participation,
        last_heartbeat,
        created_at,
        updated_at,
    })
}

fn parse_service_head(row: &[SqliteValue]) -> Result<ServiceHeadRecord> {
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

fn parse_service_slot(row: &[SqliteValue]) -> Result<ServiceSlotRecord> {
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

fn parse_instance_status(row: &[SqliteValue]) -> Result<InstanceStatusRecord> {
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
        .map_err(|e| Error::operation("parse_instance_status", e))?;
    let drain_state: DrainState = text(drain_val, "drain_state")?
        .parse()
        .map_err(|e| Error::operation("parse_instance_status", e))?;
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

fn parse_deploy(row: &[SqliteValue]) -> Result<DeployRecord> {
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
        .map_err(|e| Error::operation("parse_deploy", e))?;
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

fn integer(val: &SqliteValue, field: &'static str) -> Result<i64> {
    if let Some(&v) = val.as_integer() {
        return Ok(v);
    }
    // Corrosion may deliver integers as text after schema migrations
    if let Some(s) = val.as_text() {
        if s.is_empty() {
            return Ok(0);
        }
        return s.parse::<i64>().map_err(|e| {
            Error::operation("decode", format!("invalid integer for '{field}': {e}"))
        });
    }
    Err(Error::operation(
        "decode",
        format!("expected integer for '{field}', got {:?}", val),
    ))
}

fn text(val: &SqliteValue, field: &'static str) -> Result<String> {
    val.as_text()
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::operation("decode", format!("expected text for '{field}'")))
}

fn blob(val: &SqliteValue, field: &'static str) -> Result<Vec<u8>> {
    val.as_blob()
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::operation("decode", format!("expected blob for '{field}'")))
}
