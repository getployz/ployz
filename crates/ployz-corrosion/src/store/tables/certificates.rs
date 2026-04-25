use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement, TypedQueryEvent, sqlite::ChangeType};
use futures_util::StreamExt;
use ployz_types::error::{Error, Result};
use ployz_types::model::{CertificateEvent, CertificateRecord};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::warn;

const SQL_LIST_CERTIFICATES: &str =
    "SELECT hostname, payload_json FROM certificates WHERE payload_json <> '' ORDER BY hostname";

pub(crate) async fn list_certificates(client: &CorrClient) -> Result<Vec<CertificateRecord>> {
    let stmt = Statement::Simple(SQL_LIST_CERTIFICATES.to_string());
    query_rows(client, &stmt, "list_certificates")
        .await?
        .iter()
        .map(|row| parse_certificate(row))
        .collect()
}

pub(crate) async fn subscribe_certificates(
    client: &CorrClient,
) -> Result<(Vec<CertificateRecord>, mpsc::Receiver<CertificateEvent>)> {
    let stmt = Statement::Simple(SQL_LIST_CERTIFICATES.to_string());
    let mut stream = client
        .subscribe(&stmt, false, None)
        .await
        .map_err(|e| Error::operation("subscribe_certificates", e.to_string()))?;

    let mut known: HashMap<String, CertificateRecord> = HashMap::new();
    let mut row_index: HashMap<u64, String> = HashMap::new();

    loop {
        let event = stream
            .next()
            .await
            .ok_or_else(|| {
                Error::operation("subscribe_certificates", "stream ended during snapshot")
            })?
            .map_err(|e| Error::operation("subscribe_certificates", e.to_string()))?;

        match event {
            TypedQueryEvent::Columns(_) => {}
            TypedQueryEvent::EndOfQuery { .. } => break,
            TypedQueryEvent::Error(e) => {
                return Err(Error::operation("subscribe_certificates", e.to_string()));
            }
            TypedQueryEvent::Row(rowid, cells) => {
                let record = parse_certificate(&cells)?;
                row_index.insert(rowid.0, record.hostname.clone());
                known.insert(record.hostname.clone(), record);
            }
            TypedQueryEvent::Change(change_type, rowid, cells, _) => {
                apply_certificate_change(change_type, rowid.0, &cells, &mut known, &mut row_index)?;
            }
        }
    }

    let snapshot: Vec<CertificateRecord> = known.values().cloned().collect();

    let (tx, rx) = mpsc::channel(64);

    tokio::spawn(async move {
        loop {
            let result = tokio::select! {
                next_event = stream.next() => match next_event {
                    Some(event) => event,
                    None => {
                        warn!("certificate subscription ended");
                        return;
                    }
                },
                _ = tx.closed() => return,
            };
            match result {
                Ok(event) => match into_certificate_event(event, &mut known, &mut row_index) {
                    Ok(Some(cert_event)) => {
                        if tx.send(cert_event).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(?e, "certificate subscription failed");
                        return;
                    }
                },
                Err(e) => {
                    warn!(%e, "certificate subscription stream error");
                    return;
                }
            }
        }
    });

    Ok((snapshot, rx))
}

fn apply_certificate_change(
    change_type: ChangeType,
    rowid: u64,
    cells: &[SqliteValue],
    known: &mut HashMap<String, CertificateRecord>,
    row_index: &mut HashMap<u64, String>,
) -> Result<()> {
    match change_type {
        ChangeType::Insert | ChangeType::Update => {
            let record = parse_certificate(cells)?;
            row_index.insert(rowid, record.hostname.clone());
            known.insert(record.hostname.clone(), record);
        }
        ChangeType::Delete => {
            if let Some(hostname) = row_index.remove(&rowid) {
                known.remove(&hostname);
            }
        }
    }
    Ok(())
}

fn into_certificate_event(
    event: TypedQueryEvent<Vec<SqliteValue>>,
    known: &mut HashMap<String, CertificateRecord>,
    row_index: &mut HashMap<u64, String>,
) -> Result<Option<CertificateEvent>> {
    match event {
        TypedQueryEvent::Columns(_) | TypedQueryEvent::EndOfQuery { .. } => Ok(None),
        TypedQueryEvent::Error(e) => Err(Error::operation("subscribe_certificates", e.to_string())),
        TypedQueryEvent::Row(rowid, cells) => {
            let record = parse_certificate(&cells)?;
            let is_update = known.contains_key(&record.hostname);
            row_index.insert(rowid.0, record.hostname.clone());
            known.insert(record.hostname.clone(), record.clone());
            Ok(Some(if is_update {
                CertificateEvent::Updated(record)
            } else {
                CertificateEvent::Added(record)
            }))
        }
        TypedQueryEvent::Change(change_type, rowid, cells, _) => match change_type {
            ChangeType::Insert | ChangeType::Update => {
                let record = parse_certificate(&cells)?;
                let is_update = known.contains_key(&record.hostname);
                row_index.insert(rowid.0, record.hostname.clone());
                known.insert(record.hostname.clone(), record.clone());
                Ok(Some(if is_update {
                    CertificateEvent::Updated(record)
                } else {
                    CertificateEvent::Added(record)
                }))
            }
            ChangeType::Delete => {
                if let Some(hostname) = row_index.remove(&rowid.0) {
                    if let Some(record) = known.remove(&hostname) {
                        return Ok(Some(CertificateEvent::Removed(record)));
                    }
                }
                Ok(None)
            }
        },
    }
}

pub(crate) async fn get_certificate(
    client: &CorrClient,
    hostname: &str,
) -> Result<Option<CertificateRecord>> {
    let stmt = Statement::WithParams(
        "SELECT hostname, payload_json FROM certificates WHERE hostname = ? AND payload_json <> '' LIMIT 1"
            .to_string(),
        vec![hostname.to_string().into()],
    );
    let rows = query_rows(client, &stmt, "get_certificate").await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(parse_certificate(row)?))
}

pub(crate) async fn upsert_certificate(
    client: &CorrClient,
    record: &CertificateRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)
        .map_err(|e| Error::operation("upsert_certificate", format!("serialize: {e}")))?;
    let stmt = Statement::WithParams(
        "INSERT INTO certificates (hostname, payload_json) VALUES (?, ?) \
         ON CONFLICT(hostname) DO UPDATE SET payload_json=excluded.payload_json"
            .to_string(),
        vec![record.hostname.clone().into(), payload_json.into()],
    );
    exec_one(client, &[stmt], "upsert_certificate").await
}

fn parse_certificate(row: &[SqliteValue]) -> Result<CertificateRecord> {
    let [hostname_val, payload_val] = row else {
        return Err(Error::operation(
            "parse_certificate",
            format!("expected 2 columns, got {}", row.len()),
        ));
    };
    let hostname = text(hostname_val, "hostname")?;
    let payload_json = text(payload_val, "payload_json")?;
    let record: CertificateRecord = serde_json::from_str(&payload_json)
        .map_err(|e| Error::operation("parse_certificate", format!("decode payload: {e}")))?;
    if record.hostname != hostname {
        return Err(Error::operation(
            "parse_certificate",
            "certificate key mismatch between row and payload",
        ));
    }
    Ok(record)
}
