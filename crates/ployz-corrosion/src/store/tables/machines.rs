use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement, TypedQueryEvent, sqlite::ChangeType};
use futures_util::StreamExt;
use ployz_types::error::{Error, Result};
use ployz_types::model::{MachineEvent, MachineId, MachineRecord};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::warn;

const SQL_LIST_MACHINES: &str =
    "SELECT machine_id, payload_json FROM machines WHERE payload_json <> '' ORDER BY machine_id";

pub(crate) async fn list_machines(client: &CorrClient) -> Result<Vec<MachineRecord>> {
    let stmt = Statement::Simple(SQL_LIST_MACHINES.to_string());
    query_rows(client, &stmt, "list_machines")
        .await?
        .iter()
        .map(|row| parse_machine_row(row))
        .collect()
}

pub(crate) async fn upsert_self_machine(client: &CorrClient, record: &MachineRecord) -> Result<()> {
    let payload_json = serde_json::to_string(record)
        .map_err(|e| Error::operation("upsert_self_machine", format!("serialize: {e}")))?;
    let stmt = Statement::WithParams(
        "INSERT INTO machines (machine_id, payload_json) VALUES (?, ?) \
         ON CONFLICT(machine_id) DO UPDATE SET payload_json=excluded.payload_json"
            .to_string(),
        vec![record.id.0.clone().into(), payload_json.into()],
    );
    exec_one(client, &[stmt], "upsert_self_machine").await
}

pub(crate) async fn delete_machine(client: &CorrClient, id: &MachineId) -> Result<()> {
    let stmt = Statement::WithParams(
        "DELETE FROM machines WHERE machine_id = ?".to_string(),
        vec![id.0.clone().into()],
    );
    exec_one(client, &[stmt], "delete_machine").await
}

pub(crate) async fn subscribe_machines(
    client: &CorrClient,
) -> Result<(Vec<MachineRecord>, mpsc::Receiver<MachineEvent>)> {
    let stmt = Statement::Simple(SQL_LIST_MACHINES.to_string());
    let mut stream = client
        .subscribe(&stmt, false, None)
        .await
        .map_err(|e| Error::operation("subscribe_machines", e.to_string()))?;

    let mut machines: HashMap<MachineId, MachineRecord> = HashMap::new();
    let mut row_index: HashMap<u64, MachineId> = HashMap::new();

    loop {
        let event = stream
            .next()
            .await
            .ok_or_else(|| Error::operation("subscribe_machines", "stream ended during snapshot"))?
            .map_err(|e| Error::operation("subscribe_machines", e.to_string()))?;

        match event {
            TypedQueryEvent::Columns(_) => {}
            TypedQueryEvent::EndOfQuery { .. } => break,
            TypedQueryEvent::Error(e) => {
                return Err(Error::operation("subscribe_machines", e.to_string()));
            }
            TypedQueryEvent::Row(rowid, cells) => {
                let record = parse_machine_row(&cells)?;
                row_index.insert(rowid.0, record.id.clone());
                machines.insert(record.id.clone(), record);
            }
            TypedQueryEvent::Change(change_type, rowid, cells, _) => {
                apply_change(change_type, rowid.0, &cells, &mut machines, &mut row_index)?;
            }
        }
    }

    let mut snapshot: Vec<MachineRecord> = machines.values().cloned().collect();
    snapshot.sort_by(|left, right| left.id.0.cmp(&right.id.0));

    let (tx, rx) = mpsc::channel(64);

    tokio::spawn(async move {
        loop {
            let result = tokio::select! {
                next_event = stream.next() => match next_event {
                    Some(event) => event,
                    None => {
                        warn!("machine subscription ended");
                        return;
                    }
                },
                _ = tx.closed() => return,
            };
            match result {
                Ok(event) => match into_machine_event(event, &mut machines, &mut row_index) {
                    Ok(Some(machine_event)) => {
                        if tx.send(machine_event).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(?e, "machine subscription failed");
                        return;
                    }
                },
                Err(e) => {
                    warn!(%e, "subscription error");
                    return;
                }
            }
        }
    });

    Ok((snapshot, rx))
}

fn parse_machine_row(row: &[SqliteValue]) -> Result<MachineRecord> {
    let [machine_id_val, payload_val] = row else {
        return Err(Error::operation(
            "parse_machine_row",
            format!("expected 2 columns, got {}", row.len()),
        ));
    };

    let machine_id = text(machine_id_val, "machine_id")?;
    let payload_json = text(payload_val, "payload_json")?;
    let record: MachineRecord = serde_json::from_str(&payload_json)
        .map_err(|e| Error::operation("parse_machine_row", format!("decode payload: {e}")))?;
    if record.id.0 != machine_id {
        return Err(Error::operation(
            "parse_machine_row",
            format!(
                "machine_id mismatch between key '{}' and payload '{}'",
                machine_id, record.id
            ),
        ));
    }
    Ok(record)
}

fn apply_change(
    change_type: ChangeType,
    rowid: u64,
    cells: &[SqliteValue],
    machines: &mut HashMap<MachineId, MachineRecord>,
    row_index: &mut HashMap<u64, MachineId>,
) -> Result<()> {
    match change_type {
        ChangeType::Insert | ChangeType::Update => {
            let record = parse_machine_row(cells)?;
            row_index.insert(rowid, record.id.clone());
            machines.insert(record.id.clone(), record);
        }
        ChangeType::Delete => {
            if let Some(machine_id) = row_index.remove(&rowid) {
                machines.remove(&machine_id);
            }
        }
    }
    Ok(())
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
        TypedQueryEvent::Change(change_type, rowid, cells, _) => match change_type {
            ChangeType::Insert | ChangeType::Update => {
                upsert_event(rowid.0, &cells, known, row_index)
            }
            ChangeType::Delete => {
                if let Some(machine_id) = row_index.remove(&rowid.0) {
                    if let Some(record) = known.remove(&machine_id) {
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

fn upsert_event(
    rowid: u64,
    cells: &[SqliteValue],
    known: &mut HashMap<MachineId, MachineRecord>,
    row_index: &mut HashMap<u64, MachineId>,
) -> Result<Option<MachineEvent>> {
    let record = parse_machine_row(cells)?;
    let is_update = known.contains_key(&record.id);
    row_index.insert(rowid, record.id.clone());
    known.insert(record.id.clone(), record.clone());
    Ok(Some(if is_update {
        MachineEvent::Updated(record)
    } else {
        MachineEvent::Added(record)
    }))
}

#[cfg(test)]
mod tests {
    use super::{into_machine_event, parse_machine_row};
    use corro_api_types::{ChangeId, RowId, TypedQueryEvent, sqlite::ChangeType};
    use ployz_types::model::{
        MachineEvent, MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
    };
    use std::collections::{BTreeMap, HashMap};
    use std::net::Ipv6Addr;

    fn record(id: &str, role: &str) -> MachineRecord {
        let mut labels = BTreeMap::new();
        labels.insert(String::from("role"), role.to_string());
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([7_u8; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::LOCALHOST),
            subnet: None,
            bridge_ip: None,
            endpoints: vec![String::from("127.0.0.1:51820")],
            status: MachineStatus::Up,
            participation: Participation::Enabled,
            last_heartbeat: 123,
            created_at: 100,
            updated_at: 200,
            labels,
        }
    }

    fn machine_row(id: &str, role: &str) -> Vec<corro_api_types::SqliteValue> {
        vec![
            id.into(),
            serde_json::to_string(&record(id, role))
                .expect("serialize record")
                .into(),
        ]
    }

    #[test]
    fn parse_machine_reads_payload() {
        let record =
            parse_machine_row(&machine_row("machine-1", "db")).expect("machine row should parse");

        assert_eq!(record.id.0, "machine-1");
        assert_eq!(record.labels.get("role"), Some(&String::from("db")));
    }

    #[test]
    fn into_machine_event_emits_add_update_and_remove() {
        let mut known = HashMap::new();
        let mut row_index = HashMap::new();

        let added = into_machine_event(
            TypedQueryEvent::Row(RowId(1), machine_row("machine-1", "db")),
            &mut known,
            &mut row_index,
        )
        .expect("row event should succeed");

        match added {
            Some(MachineEvent::Added(record)) => {
                assert_eq!(record.id.0, "machine-1");
                assert_eq!(record.labels.get("role"), Some(&String::from("db")));
            }
            Some(MachineEvent::Updated(_)) => panic!("expected add event"),
            Some(MachineEvent::Removed(_)) => panic!("expected add event"),
            None => panic!("expected add event"),
        }

        let updated = into_machine_event(
            TypedQueryEvent::Change(
                ChangeType::Update,
                RowId(1),
                machine_row("machine-1", "api"),
                ChangeId(9),
            ),
            &mut known,
            &mut row_index,
        )
        .expect("update event should succeed");

        match updated {
            Some(MachineEvent::Updated(record)) => {
                assert_eq!(record.labels.get("role"), Some(&String::from("api")));
            }
            Some(MachineEvent::Added(_)) => panic!("expected update event"),
            Some(MachineEvent::Removed(_)) => panic!("expected update event"),
            None => panic!("expected update event"),
        }

        let removed = into_machine_event(
            TypedQueryEvent::Change(ChangeType::Delete, RowId(1), Vec::new(), ChangeId(10)),
            &mut known,
            &mut row_index,
        )
        .expect("delete event should succeed");

        match removed {
            Some(MachineEvent::Removed(record)) => {
                assert_eq!(record.id, MachineId(String::from("machine-1")));
            }
            Some(MachineEvent::Added(_)) => panic!("expected remove event"),
            Some(MachineEvent::Updated(_)) => panic!("expected remove event"),
            None => panic!("expected remove event"),
        }
    }
}
