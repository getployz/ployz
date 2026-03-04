use crate::dataplane::traits::{InviteStore, MachineStore, PortError, PortResult};
use crate::domain::model::{
    InviteRecord, MachineEvent, MachineId, MachineRecord, OverlayIp, PublicKey,
};
use corro_api_types::{QueryEvent, RqliteResult, SqliteValue, Statement, sqlite::ChangeType};
use futures_util::{StreamExt, stream::BoxStream};
use std::collections::HashMap;
use std::io;
use std::net::Ipv6Addr;
use std::net::SocketAddr;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time;
use tracing::warn;
use uuid::Uuid;

const WATCH_RETRY_BASE: Duration = Duration::from_millis(500);
const WATCH_RETRY_MAX: Duration = Duration::from_secs(5);
const SCHEMA_SQL: &str = include_str!("corrosion_schema.sql");

#[derive(Clone)]
pub struct CorrosionStore {
    client: corro_client::CorrosionApiClient,
}

impl CorrosionStore {
    pub async fn connect(endpoint: &str) -> Result<Self, String> {
        let addr: SocketAddr = endpoint
            .parse()
            .map_err(|e| format!("invalid corrosion endpoint '{endpoint}': {e}"))?;
        let client = corro_client::CorrosionApiClient::new(addr);

        let schema_stmt = Statement::Simple(SCHEMA_SQL.to_string());
        let schema_res = client
            .schema(&[schema_stmt])
            .await
            .map_err(|e| format!("apply corrosion schema: {e}"))?;
        if let Some(RqliteResult::Error { error }) = schema_res.results.first() {
            return Err(format!("apply corrosion schema: {error}"));
        }

        Ok(Self { client })
    }

    async fn execute(
        &self,
        statements: &[Statement],
        op: &'static str,
    ) -> PortResult<Vec<RqliteResult>> {
        let response = self
            .client
            .execute(statements)
            .await
            .map_err(|e| PortError::operation(op, e.to_string()))?;
        Ok(response.results)
    }

    async fn list_machines_inner(&self) -> PortResult<Vec<MachineRecord>> {
        let statement = Self::machines_watch_statement();
        let results = self.execute(&[statement], "list_machines").await?;
        let rows = query_rows(
            results
                .first()
                .ok_or_else(|| PortError::operation("list_machines", "missing query result"))?,
            "list_machines",
        )?;

        rows.into_iter()
            .map(|row| parse_machine_row(&row))
            .collect()
    }

    fn machines_watch_statement() -> Statement {
        Statement::Simple(
            "SELECT id, public_key, overlay_ip, endpoints FROM machines ORDER BY id".to_string(),
        )
    }

    async fn open_machine_watch(
        &self,
    ) -> PortResult<(Uuid, BoxStream<'static, io::Result<QueryEvent>>)> {
        let statement = Self::machines_watch_statement();
        let (watch_id, stream) = self
            .client
            .watch(&statement)
            .await
            .map_err(|e| PortError::operation("watch_machines", e.to_string()))?;
        Ok((watch_id, Box::pin(stream)))
    }

    async fn resume_machine_watch(
        &self,
        watch_id: Uuid,
    ) -> PortResult<BoxStream<'static, io::Result<QueryEvent>>> {
        let stream = self
            .client
            .watched_query(watch_id)
            .await
            .map_err(|e| PortError::operation("watch_machines", e.to_string()))?;
        Ok(Box::pin(stream))
    }

    async fn collect_initial_snapshot(
        &self,
        stream: &mut BoxStream<'static, io::Result<QueryEvent>>,
    ) -> PortResult<(Vec<MachineRecord>, HashMap<u64, MachineId>)> {
        let mut machines_by_id: HashMap<MachineId, MachineRecord> = HashMap::new();
        let mut row_index: HashMap<u64, MachineId> = HashMap::new();

        loop {
            let event = stream.next().await.ok_or_else(|| {
                PortError::operation(
                    "watch_machines",
                    "watch stream ended before initial snapshot completed",
                )
            })?;

            let event = event.map_err(|e| {
                PortError::operation("watch_machines", format!("watch io error: {e}"))
            })?;

            match event {
                QueryEvent::Columns(_) => {}
                QueryEvent::EndOfQuery => break,
                QueryEvent::Error(err) => {
                    return Err(PortError::operation(
                        "watch_machines",
                        format!("watch query error: {err}"),
                    ));
                }
                QueryEvent::Row {
                    rowid,
                    change_type,
                    cells,
                } => {
                    let rowid = as_u64_rowid(rowid)?;
                    match change_type {
                        ChangeType::Upsert => {
                            let record = parse_machine_row(&cells)?;
                            row_index.insert(rowid, record.id.clone());
                            machines_by_id.insert(record.id.clone(), record);
                        }
                        ChangeType::Delete => {
                            if let Some(id) = row_index.remove(&rowid) {
                                machines_by_id.remove(&id);
                            }
                        }
                    }
                }
            }
        }

        let mut snapshot: Vec<MachineRecord> = machines_by_id.into_values().collect();
        snapshot.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        Ok((snapshot, row_index))
    }

    async fn fetch_invite(
        &self,
        invite_id: &str,
    ) -> PortResult<Option<InviteRecord>> {
        let statement = Statement::WithParams(
            "SELECT id, expires_at FROM invites WHERE id = ? LIMIT 1".to_string(),
            vec![invite_id.to_string().into()],
        );
        let results = self.execute(&[statement], "fetch_invite").await?;
        let rows = query_rows(
            results
                .first()
                .ok_or_else(|| PortError::operation("fetch_invite", "missing query result"))?,
            "fetch_invite",
        )?;
        if rows.is_empty() {
            return Ok(None);
        }

        parse_invite_row(&rows[0]).map(Some)
    }
}

impl MachineStore for CorrosionStore {
    async fn list_machines(&self) -> PortResult<Vec<MachineRecord>> {
        self.list_machines_inner().await
    }

    async fn upsert_machine(
        &self,
        record: &MachineRecord,
    ) -> PortResult<()> {
        let endpoints = serde_json::to_string(&record.endpoints).map_err(|e| {
            PortError::operation("upsert_machine", format!("serialize endpoints: {e}"))
        })?;
        let statement = Statement::WithParams(
            "INSERT INTO machines (id, public_key, overlay_ip, endpoints) VALUES (?, ?, ?, ?) ON CONFLICT(id) DO UPDATE SET public_key = excluded.public_key, overlay_ip = excluded.overlay_ip, endpoints = excluded.endpoints".to_string(),
            vec![
                record.id.0.clone().into(),
                record.public_key.0.to_vec().into(),
                record.overlay_ip.0.to_string().into(),
                endpoints.into(),
            ],
        );

        let results = self.execute(&[statement], "upsert_machine").await?;
        expect_execute(&results, "upsert_machine")
    }

    async fn delete_machine(&self, id: &MachineId) -> PortResult<()> {
        let statement = Statement::WithParams(
            "DELETE FROM machines WHERE id = ?".to_string(),
            vec![id.0.clone().into()],
        );

        let results = self.execute(&[statement], "delete_machine").await?;
        expect_execute(&results, "delete_machine")
    }

    async fn subscribe_machines(
        &self,
    ) -> PortResult<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
        let (watch_id, mut stream) = self.open_machine_watch().await?;
        let (snapshot, mut row_index) = self.collect_initial_snapshot(&mut stream).await?;
        let (tx, rx) = mpsc::channel(64);

        let store = self.clone();
        let mut known: HashMap<MachineId, MachineRecord> = snapshot
            .iter()
            .cloned()
            .map(|machine| (machine.id.clone(), machine))
            .collect();

        tokio::spawn(async move {
            let current_watch_id = watch_id;
            let mut current_stream = stream;
            let mut backoff = WATCH_RETRY_BASE;

            loop {
                match current_stream.next().await {
                    Some(Ok(event)) => {
                        backoff = WATCH_RETRY_BASE;

                        match machine_event_from_watch(event, &mut known, &mut row_index) {
                            Ok(Some(machine_event)) => {
                                if tx.send(machine_event).await.is_err() {
                                    return;
                                }
                            }
                            Ok(None) => {}
                            Err(err) => {
                                warn!(?err, "failed to parse watch event");
                            }
                        }
                    }
                    Some(Err(err)) => {
                        warn!(?err, "machine watch io error; reconnecting");
                        match store.resume_machine_watch(current_watch_id).await {
                            Ok(stream) => {
                                current_stream = stream;
                                backoff = WATCH_RETRY_BASE;
                            }
                            Err(reconnect_err) => {
                                warn!(?reconnect_err, "machine watch reconnect failed");
                                time::sleep(backoff).await;
                                backoff = std::cmp::min(backoff + backoff, WATCH_RETRY_MAX);
                            }
                        }
                    }
                    None => {
                        warn!("machine watch stream ended; reconnecting");
                        match store.resume_machine_watch(current_watch_id).await {
                            Ok(stream) => {
                                current_stream = stream;
                                backoff = WATCH_RETRY_BASE;
                            }
                            Err(reconnect_err) => {
                                warn!(?reconnect_err, "machine watch reconnect failed");
                                time::sleep(backoff).await;
                                backoff = std::cmp::min(backoff + backoff, WATCH_RETRY_MAX);
                            }
                        }
                    }
                }
            }
        });

        Ok((snapshot, rx))
    }
}

impl InviteStore for CorrosionStore {
    async fn create_invite(&self, invite: &InviteRecord) -> PortResult<()> {
        let statement = Statement::WithParams(
            "INSERT INTO invites (id, expires_at) VALUES (?, ?)".to_string(),
            vec![
                invite.id.clone().into(),
                (invite.expires_at as i64).into(),
            ],
        );

        let results = self.execute(&[statement], "create_invite").await?;
        match results.first() {
            Some(RqliteResult::Error { error }) => {
                if error.contains("UNIQUE") {
                    Err(PortError::operation("invite_exists", error.clone()))
                } else {
                    Err(PortError::operation("create_invite", error.clone()))
                }
            }
            _ => expect_execute(&results, "create_invite"),
        }
    }

    async fn consume_invite(
        &self,
        invite_id: &str,
        now_unix_secs: u64,
    ) -> PortResult<()> {
        let delete = Statement::WithParams(
            "DELETE FROM invites WHERE id = ? AND expires_at >= ?".to_string(),
            vec![
                invite_id.to_string().into(),
                (now_unix_secs as i64).into(),
            ],
        );
        let results = self.execute(&[delete], "consume_invite").await?;

        let affected = rows_affected(
            results
                .first()
                .ok_or_else(|| PortError::operation("consume_invite", "missing execute result"))?,
            "consume_invite",
        )?;

        if affected == 1 {
            return Ok(());
        }

        // No row deleted — figure out why.
        let invite = self.fetch_invite(invite_id).await?;
        match invite {
            None => Err(PortError::operation(
                "invite_not_found",
                format!("invite '{invite_id}' not found"),
            )),
            Some(_) => Err(PortError::operation(
                "invite_expired",
                format!("invite '{invite_id}' is expired"),
            )),
        }
    }
}

fn query_rows(result: &RqliteResult, op: &'static str) -> PortResult<Vec<Vec<SqliteValue>>> {
    match result {
        RqliteResult::Query { values, .. } => Ok(values.clone()),
        RqliteResult::Error { error } => Err(PortError::operation(op, error.clone())),
        _ => Err(PortError::operation(op, "expected query result")),
    }
}

fn expect_execute(results: &[RqliteResult], op: &'static str) -> PortResult<()> {
    match results.first() {
        Some(RqliteResult::Execute { .. }) => Ok(()),
        Some(RqliteResult::Error { error }) => Err(PortError::operation(op, error.clone())),
        Some(_) => Err(PortError::operation(op, "unexpected response kind")),
        None => Err(PortError::operation(op, "missing execute result")),
    }
}

fn rows_affected(result: &RqliteResult, op: &'static str) -> PortResult<usize> {
    match result {
        RqliteResult::Execute { rows_affected, .. } => Ok(*rows_affected),
        RqliteResult::Error { error } => Err(PortError::operation(op, error.clone())),
        _ => Err(PortError::operation(op, "expected execute result")),
    }
}

fn machine_event_from_watch(
    event: QueryEvent,
    known: &mut HashMap<MachineId, MachineRecord>,
    row_index: &mut HashMap<u64, MachineId>,
) -> PortResult<Option<MachineEvent>> {
    match event {
        QueryEvent::Columns(_) | QueryEvent::EndOfQuery => Ok(None),
        QueryEvent::Error(err) => Err(PortError::operation(
            "watch_machines",
            format!("watch query error: {err}"),
        )),
        QueryEvent::Row {
            rowid,
            change_type,
            cells,
        } => {
            let rowid = as_u64_rowid(rowid)?;

            match change_type {
                ChangeType::Upsert => {
                    let record = parse_machine_row(&cells)?;
                    let is_update = known.contains_key(&record.id);
                    row_index.insert(rowid, record.id.clone());
                    known.insert(record.id.clone(), record.clone());
                    let event = if is_update {
                        MachineEvent::Updated(record)
                    } else {
                        MachineEvent::Added(record)
                    };
                    Ok(Some(event))
                }
                ChangeType::Delete => {
                    if let Some(id) = row_index.remove(&rowid) {
                        known.remove(&id);
                        Ok(Some(MachineEvent::Removed { id }))
                    } else {
                        Ok(None)
                    }
                }
            }
        }
    }
}

fn as_u64_rowid(rowid: i64) -> PortResult<u64> {
    u64::try_from(rowid)
        .map_err(|_| PortError::operation("watch_machines", format!("negative rowid: {rowid}")))
}

fn parse_machine_row(values: &[SqliteValue]) -> PortResult<MachineRecord> {
    if values.len() != 4 {
        return Err(PortError::operation(
            "parse_machine_row",
            format!("expected 4 columns, got {}", values.len()),
        ));
    }

    let id = expect_text(values, 0, "id")?;
    let key_blob = expect_blob(values, 1, "public_key")?;
    let overlay = expect_text(values, 2, "overlay_ip")?;
    let endpoints_json = expect_text(values, 3, "endpoints")?;

    let public_key: [u8; 32] = key_blob.as_slice().try_into().map_err(|_| {
        PortError::operation(
            "parse_machine_row",
            format!("public key length must be 32, got {}", key_blob.len()),
        )
    })?;
    let overlay_ip: Ipv6Addr = overlay.parse().map_err(|e| {
        PortError::operation("parse_machine_row", format!("invalid overlay ip: {e}"))
    })?;
    let endpoints: Vec<String> = serde_json::from_str(&endpoints_json).map_err(|e| {
        PortError::operation("parse_machine_row", format!("invalid endpoints json: {e}"))
    })?;

    Ok(MachineRecord {
        id: MachineId(id),
        public_key: PublicKey(public_key),
        overlay_ip: OverlayIp(overlay_ip),
        endpoints,
    })
}

fn parse_invite_row(values: &[SqliteValue]) -> PortResult<InviteRecord> {
    if values.len() != 2 {
        return Err(PortError::operation(
            "parse_invite_row",
            format!("expected 2 columns, got {}", values.len()),
        ));
    }

    let id = expect_text(values, 0, "id")?;
    let expires_at = expect_i64(values, 1, "expires_at")?;

    Ok(InviteRecord {
        id,
        expires_at: to_u64(expires_at, "expires_at")?,
    })
}

fn expect_text(values: &[SqliteValue], idx: usize, field: &'static str) -> PortResult<String> {
    values
        .get(idx)
        .and_then(SqliteValue::as_text)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            PortError::operation("corrosion_decode", format!("missing text field '{field}'"))
        })
}

fn expect_blob(values: &[SqliteValue], idx: usize, field: &'static str) -> PortResult<Vec<u8>> {
    values
        .get(idx)
        .and_then(SqliteValue::as_blob)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            PortError::operation("corrosion_decode", format!("missing blob field '{field}'"))
        })
}

fn expect_i64(values: &[SqliteValue], idx: usize, field: &'static str) -> PortResult<i64> {
    values
        .get(idx)
        .and_then(SqliteValue::as_integer)
        .copied()
        .ok_or_else(|| {
            PortError::operation(
                "corrosion_decode",
                format!("missing integer field '{field}'"),
            )
        })
}

fn to_u64(value: i64, field: &'static str) -> PortResult<u64> {
    if value < 0 {
        return Err(PortError::operation(
            "corrosion_decode",
            format!("negative value for '{field}': {value}"),
        ));
    }
    Ok(value as u64)
}
