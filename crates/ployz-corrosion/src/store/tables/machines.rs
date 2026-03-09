use crate::client::CorrClient;
use crate::store::shared::decode::{blob, integer, text};
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement, TypedQueryEvent, sqlite::ChangeType};
use futures_util::StreamExt;
use ipnet::Ipv4Net;
use ployz_sdk::error::{Error, Result};
use ployz_sdk::model::{
    MachineEvent, MachineId, MachineRecord, MachineStatus, OverlayIp, Participation, PublicKey,
};
use std::collections::{BTreeMap, HashMap};
use std::net::Ipv6Addr;
use tokio::sync::mpsc;
use tracing::warn;

const SQL_LIST_MACHINES: &str = "SELECT id, public_key, overlay_ip, subnet, bridge_ip, endpoints, status, participation, last_heartbeat, labels, created_at, updated_at FROM machines ORDER BY id";

pub(crate) async fn list_machines(client: &CorrClient) -> Result<Vec<MachineRecord>> {
    let stmt = Statement::Simple(SQL_LIST_MACHINES.to_string());
    query_rows(client, &stmt, "list_machines")
        .await?
        .iter()
        .map(|row| parse_machine(row))
        .collect()
}

pub(crate) async fn upsert_machine(client: &CorrClient, record: &MachineRecord) -> Result<()> {
    let endpoints = serde_json::to_string(&record.endpoints)
        .map_err(|e| Error::operation("upsert_machine", format!("serialize: {e}")))?;
    let labels = serde_json::to_string(&record.labels)
        .map_err(|e| Error::operation("upsert_machine", format!("serialize labels: {e}")))?;
    let subnet_str = record
        .subnet
        .map(|subnet| subnet.to_string())
        .unwrap_or_default();
    let bridge_str = record
        .bridge_ip
        .map(|bridge_ip| bridge_ip.0.to_string())
        .unwrap_or_default();
    let stmt = Statement::WithParams(
        "INSERT INTO machines (id, public_key, overlay_ip, subnet, bridge_ip, endpoints, status, participation, last_heartbeat, labels, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET public_key=excluded.public_key, \
         overlay_ip=excluded.overlay_ip, subnet=excluded.subnet, \
         bridge_ip=excluded.bridge_ip, endpoints=excluded.endpoints, \
         status=excluded.status, participation=excluded.participation, \
         last_heartbeat=excluded.last_heartbeat, labels=excluded.labels, \
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
            labels.into(),
            (record.created_at as i64).into(),
            (record.updated_at as i64).into(),
        ],
    );
    exec_one(client, &[stmt], "upsert_machine").await
}

pub(crate) async fn delete_machine(client: &CorrClient, id: &MachineId) -> Result<()> {
    let stmt = Statement::WithParams(
        "DELETE FROM machines WHERE id = ?".to_string(),
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
                let record = parse_machine(&cells)?;
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

fn apply_change(
    change_type: ChangeType,
    rowid: u64,
    cells: &[SqliteValue],
    machines: &mut HashMap<MachineId, MachineRecord>,
    row_index: &mut HashMap<u64, MachineId>,
) -> Result<()> {
    match change_type {
        ChangeType::Insert | ChangeType::Update => {
            let record = parse_machine(cells)?;
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
        labels_val,
        created_val,
        updated_val,
    ] = row
    else {
        return Err(Error::operation(
            "parse_machine",
            format!("expected 12 columns, got {}", row.len()),
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
    let labels_json = text(labels_val, "labels")?;
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

    let subnet = if subnet_str.is_empty() {
        None
    } else {
        Some(
            subnet_str
                .parse::<Ipv4Net>()
                .map_err(|e| Error::operation("parse_machine", format!("invalid subnet: {e}")))?,
        )
    };

    let bridge_ip = if bridge_str.is_empty() {
        None
    } else {
        let address: Ipv6Addr = bridge_str
            .parse()
            .map_err(|e| Error::operation("parse_machine", format!("invalid bridge ip: {e}")))?;
        Some(OverlayIp(address))
    };

    let endpoints: Vec<String> = serde_json::from_str(&endpoints_json)
        .map_err(|e| Error::operation("parse_machine", format!("invalid endpoints json: {e}")))?;

    let status: MachineStatus = status_str
        .parse()
        .map_err(|e| Error::operation("parse_machine", format!("invalid status: {e}")))?;
    let participation: Participation = participation_str
        .parse()
        .map_err(|e| Error::operation("parse_machine", format!("invalid participation: {e}")))?;

    let labels: BTreeMap<String, String> = serde_json::from_str(&labels_json).unwrap_or_default();

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
        labels,
    })
}

#[cfg(test)]
mod tests {
    use super::{into_machine_event, parse_machine};
    use corro_api_types::{ChangeId, RowId, SqliteValue, TypedQueryEvent, sqlite::ChangeType};
    use ployz_sdk::model::{MachineEvent, MachineId};
    use std::collections::HashMap;

    fn machine_row(id: &str, labels_json: &str) -> Vec<SqliteValue> {
        vec![
            id.into(),
            vec![7_u8; 32].into(),
            "fd00::1".into(),
            "10.0.0.0/24".into(),
            "fd00::2".into(),
            "[\"127.0.0.1:51820\"]".into(),
            "up".into(),
            "enabled".into(),
            123_i64.into(),
            labels_json.into(),
            100_i64.into(),
            200_i64.into(),
        ]
    }

    #[test]
    fn parse_machine_reads_labels() {
        let record = parse_machine(&machine_row("machine-1", "{\"role\":\"db\"}"))
            .expect("machine row should parse");

        assert_eq!(record.id.0, "machine-1");
        assert_eq!(record.labels.get("role"), Some(&String::from("db")));
    }

    #[test]
    fn into_machine_event_emits_add_update_and_remove() {
        let mut known = HashMap::new();
        let mut row_index = HashMap::new();

        let added = into_machine_event(
            TypedQueryEvent::Row(RowId(1), machine_row("machine-1", "{\"role\":\"db\"}")),
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
                machine_row("machine-1", "{\"role\":\"api\"}"),
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
