use crate::error::{Error, Result};
use crate::store::{InviteStore, MachineStore, SyncProbe, SyncStatus};
use crate::store::model::{
    InviteRecord, MachineEvent, MachineId, MachineRecord, OverlayIp, PublicKey,
};
use corro_api_types::{ExecResult, SqliteValue, Statement, TypedQueryEvent, sqlite::ChangeType};
use corro_client::CorrosionClient;
use futures_util::StreamExt;
use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddr};
use tokio::sync::mpsc;
use tracing::warn;

pub mod config;
pub mod docker;

pub const SCHEMA_SQL: &str = include_str!("schema.sql");

#[derive(Clone)]
pub struct CorrosionStore {
    client: CorrosionClient,
    addr: SocketAddr,
    http: reqwest::Client,
}

impl CorrosionStore {
    pub fn new(endpoint: &str, db_path: &std::path::Path) -> std::result::Result<Self, String> {
        let addr: SocketAddr = endpoint
            .parse()
            .map_err(|e| format!("invalid corrosion endpoint '{endpoint}': {e}"))?;
        let client = CorrosionClient::new(addr, db_path)
            .map_err(|e| format!("corrosion client: {e}"))?;
        let http = reqwest::Client::new();
        Ok(Self { client, addr, http })
    }
}

impl SyncProbe for CorrosionStore {
    async fn sync_status(&self) -> Result<SyncStatus> {
        let resp = self
            .http
            .get(format!("http://{}/v1/health?gaps=0", self.addr))
            .send()
            .await
            .map_err(|e| Error::operation("sync_status", format!("health request: {e}")))?;

        if resp.status() == reqwest::StatusCode::OK {
            return Ok(SyncStatus::Synced);
        }

        #[derive(serde::Deserialize)]
        struct Envelope {
            response: Health,
        }
        #[derive(serde::Deserialize)]
        struct Health {
            gaps: i64,
            members: i64,
        }

        let health = resp
            .json::<Envelope>()
            .await
            .map_err(|e| Error::operation("sync_status", format!("decode: {e}")))?
            .response;

        if health.members <= 1 {
            Ok(SyncStatus::Disconnected)
        } else if health.gaps > 0 {
            Ok(SyncStatus::Syncing {
                gaps: health.gaps as u64,
            })
        } else {
            Ok(SyncStatus::Synced)
        }
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
            "SELECT id, public_key, overlay_ip, endpoints FROM machines ORDER BY id".to_string(),
        );
        query_rows(&self.client, &stmt, "list_machines")
            .await?
            .iter()
            .map(|row| parse_machine(row))
            .collect()
    }

    async fn upsert_machine(&self, record: &MachineRecord) -> Result<()> {
        let endpoints = serde_json::to_string(&record.endpoints)
            .map_err(|e| Error::operation("upsert_machine", format!("serialize: {e}")))?;
        let stmt = Statement::WithParams(
            "INSERT INTO machines (id, public_key, overlay_ip, endpoints) VALUES (?, ?, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET public_key=excluded.public_key, \
             overlay_ip=excluded.overlay_ip, endpoints=excluded.endpoints"
                .to_string(),
            vec![
                record.id.0.clone().into(),
                record.public_key.0.to_vec().into(),
                record.overlay_ip.0.to_string().into(),
                endpoints.into(),
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
            "SELECT id, public_key, overlay_ip, endpoints FROM machines ORDER BY id".to_string(),
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

        // SubscriptionStream handles reconnection with backoff internally
        tokio::spawn(async move {
            while let Some(result) = stream.next().await {
                match result {
                    Ok(event) => {
                        match into_machine_event(event, &mut machines, &mut row_index) {
                            Ok(Some(ev)) => {
                                if tx.send(ev).await.is_err() {
                                    return;
                                }
                            }
                            Ok(None) => {}
                            Err(e) => warn!(?e, "bad subscription event"),
                        }
                    }
                    Err(e) => warn!(%e, "subscription error"),
                }
            }
            warn!("machine subscription ended");
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

// --- query/exec helpers ---

async fn query_rows(
    client: &CorrosionClient,
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

async fn exec_one(
    client: &CorrosionClient,
    stmts: &[Statement],
    op: &'static str,
) -> Result<()> {
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
        TypedQueryEvent::Error(e) => {
            Err(Error::operation("subscribe_machines", e.to_string()))
        }
        TypedQueryEvent::Row(rowid, cells) => {
            upsert_event(rowid.0, &cells, known, row_index)
        }
        TypedQueryEvent::Change(ct, rowid, cells, _) => match ct {
            ChangeType::Insert | ChangeType::Update => {
                upsert_event(rowid.0, &cells, known, row_index)
            }
            ChangeType::Delete => {
                if let Some(id) = row_index.remove(&rowid.0) {
                    known.remove(&id);
                    Ok(Some(MachineEvent::Removed { id }))
                } else {
                    Ok(None)
                }
            }
        },
    }
}

// --- row parsing ---

fn parse_machine(row: &[SqliteValue]) -> Result<MachineRecord> {
    if row.len() != 4 {
        return Err(Error::operation(
            "parse_machine",
            format!("expected 4 columns, got {}", row.len()),
        ));
    }

    let id = text(&row[0], "id")?;
    let key_blob = blob(&row[1], "public_key")?;
    let overlay = text(&row[2], "overlay_ip")?;
    let endpoints_json = text(&row[3], "endpoints")?;

    let public_key: [u8; 32] = key_blob.as_slice().try_into().map_err(|_| {
        Error::operation(
            "parse_machine",
            format!("public key must be 32 bytes, got {}", key_blob.len()),
        )
    })?;
    let overlay_ip: Ipv6Addr = overlay.parse().map_err(|e| {
        Error::operation("parse_machine", format!("invalid overlay ip: {e}"))
    })?;
    let endpoints: Vec<String> = serde_json::from_str(&endpoints_json).map_err(|e| {
        Error::operation("parse_machine", format!("invalid endpoints json: {e}"))
    })?;

    Ok(MachineRecord {
        id: MachineId(id),
        public_key: PublicKey(public_key),
        overlay_ip: OverlayIp(overlay_ip),
        endpoints,
    })
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

