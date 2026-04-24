use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement, TypedQueryEvent, sqlite::ChangeType};
use futures_util::StreamExt;
use ployz_types::error::{Error, Result};
use ployz_types::model::{AcmeChallengeEvent, AcmeChallengeRecord};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::warn;

const SQL_LIST_ACME_CHALLENGES: &str = "SELECT hostname, token, payload_json FROM acme_challenges WHERE payload_json <> '' ORDER BY hostname, token";

pub(crate) async fn list_acme_challenges(client: &CorrClient) -> Result<Vec<AcmeChallengeRecord>> {
    let stmt = Statement::Simple(SQL_LIST_ACME_CHALLENGES.to_string());
    query_rows(client, &stmt, "list_acme_challenges")
        .await?
        .iter()
        .map(|row| parse_acme_challenge(row))
        .collect()
}

pub(crate) async fn subscribe_acme_challenges(
    client: &CorrClient,
) -> Result<(Vec<AcmeChallengeRecord>, mpsc::Receiver<AcmeChallengeEvent>)> {
    let stmt = Statement::Simple(SQL_LIST_ACME_CHALLENGES.to_string());
    let mut stream = client
        .subscribe(&stmt, false, None)
        .await
        .map_err(|e| Error::operation("subscribe_acme_challenges", e.to_string()))?;

    let mut known: HashMap<(String, String), AcmeChallengeRecord> = HashMap::new();
    let mut row_index: HashMap<u64, (String, String)> = HashMap::new();

    loop {
        let event = stream
            .next()
            .await
            .ok_or_else(|| {
                Error::operation("subscribe_acme_challenges", "stream ended during snapshot")
            })?
            .map_err(|e| Error::operation("subscribe_acme_challenges", e.to_string()))?;

        match event {
            TypedQueryEvent::Columns(_) => {}
            TypedQueryEvent::EndOfQuery { .. } => break,
            TypedQueryEvent::Error(e) => {
                return Err(Error::operation("subscribe_acme_challenges", e.to_string()));
            }
            TypedQueryEvent::Row(rowid, cells) => {
                let record = parse_acme_challenge(&cells)?;
                let key = (record.hostname.clone(), record.token.clone());
                row_index.insert(rowid.0, key.clone());
                known.insert(key, record);
            }
            TypedQueryEvent::Change(change_type, rowid, cells, _) => {
                apply_challenge_change(change_type, rowid.0, &cells, &mut known, &mut row_index)?;
            }
        }
    }

    let snapshot: Vec<AcmeChallengeRecord> = known.values().cloned().collect();

    let (tx, rx) = mpsc::channel(64);

    tokio::spawn(async move {
        loop {
            let result = tokio::select! {
                next_event = stream.next() => match next_event {
                    Some(event) => event,
                    None => {
                        warn!("acme challenge subscription ended");
                        return;
                    }
                },
                _ = tx.closed() => return,
            };
            match result {
                Ok(event) => match into_challenge_event(event, &mut known, &mut row_index) {
                    Ok(Some(ev)) => {
                        if tx.send(ev).await.is_err() {
                            return;
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        warn!(?e, "acme challenge subscription failed");
                        return;
                    }
                },
                Err(e) => {
                    warn!(%e, "acme challenge subscription stream error");
                    return;
                }
            }
        }
    });

    Ok((snapshot, rx))
}

fn apply_challenge_change(
    change_type: ChangeType,
    rowid: u64,
    cells: &[SqliteValue],
    known: &mut HashMap<(String, String), AcmeChallengeRecord>,
    row_index: &mut HashMap<u64, (String, String)>,
) -> Result<()> {
    match change_type {
        ChangeType::Insert | ChangeType::Update => {
            let record = parse_acme_challenge(cells)?;
            let key = (record.hostname.clone(), record.token.clone());
            row_index.insert(rowid, key.clone());
            known.insert(key, record);
        }
        ChangeType::Delete => {
            if let Some(key) = row_index.remove(&rowid) {
                known.remove(&key);
            }
        }
    }
    Ok(())
}

fn into_challenge_event(
    event: TypedQueryEvent<Vec<SqliteValue>>,
    known: &mut HashMap<(String, String), AcmeChallengeRecord>,
    row_index: &mut HashMap<u64, (String, String)>,
) -> Result<Option<AcmeChallengeEvent>> {
    match event {
        TypedQueryEvent::Columns(_) | TypedQueryEvent::EndOfQuery { .. } => Ok(None),
        TypedQueryEvent::Error(e) => {
            Err(Error::operation("subscribe_acme_challenges", e.to_string()))
        }
        TypedQueryEvent::Row(rowid, cells) => {
            let record = parse_acme_challenge(&cells)?;
            let key = (record.hostname.clone(), record.token.clone());
            let is_update = known.contains_key(&key);
            row_index.insert(rowid.0, key.clone());
            known.insert(key, record.clone());
            Ok(Some(if is_update {
                AcmeChallengeEvent::Updated(record)
            } else {
                AcmeChallengeEvent::Added(record)
            }))
        }
        TypedQueryEvent::Change(change_type, rowid, cells, _) => match change_type {
            ChangeType::Insert | ChangeType::Update => {
                let record = parse_acme_challenge(&cells)?;
                let key = (record.hostname.clone(), record.token.clone());
                let is_update = known.contains_key(&key);
                row_index.insert(rowid.0, key.clone());
                known.insert(key, record.clone());
                Ok(Some(if is_update {
                    AcmeChallengeEvent::Updated(record)
                } else {
                    AcmeChallengeEvent::Added(record)
                }))
            }
            ChangeType::Delete => {
                if let Some(key) = row_index.remove(&rowid.0) {
                    if let Some(record) = known.remove(&key) {
                        return Ok(Some(AcmeChallengeEvent::Removed(record)));
                    }
                }
                Ok(None)
            }
        },
    }
}

pub(crate) async fn upsert_acme_challenge(
    client: &CorrClient,
    record: &AcmeChallengeRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)
        .map_err(|e| Error::operation("upsert_acme_challenge", format!("serialize: {e}")))?;
    let stmt = Statement::WithParams(
        "INSERT INTO acme_challenges (hostname, token, payload_json) VALUES (?, ?, ?) \
         ON CONFLICT(hostname, token) DO UPDATE SET payload_json=excluded.payload_json"
            .to_string(),
        vec![
            record.hostname.clone().into(),
            record.token.clone().into(),
            payload_json.into(),
        ],
    );
    exec_one(client, &[stmt], "upsert_acme_challenge").await
}

pub(crate) async fn delete_acme_challenge(
    client: &CorrClient,
    hostname: &str,
    token: &str,
) -> Result<()> {
    let stmt = Statement::WithParams(
        "DELETE FROM acme_challenges WHERE hostname = ? AND token = ?".to_string(),
        vec![hostname.to_string().into(), token.to_string().into()],
    );
    exec_one(client, &[stmt], "delete_acme_challenge").await
}

fn parse_acme_challenge(row: &[SqliteValue]) -> Result<AcmeChallengeRecord> {
    let [hostname_val, token_val, payload_val] = row else {
        return Err(Error::operation(
            "parse_acme_challenge",
            format!("expected 3 columns, got {}", row.len()),
        ));
    };
    let hostname = text(hostname_val, "hostname")?;
    let token = text(token_val, "token")?;
    let payload_json = text(payload_val, "payload_json")?;
    let record: AcmeChallengeRecord = serde_json::from_str(&payload_json)
        .map_err(|e| Error::operation("parse_acme_challenge", format!("decode payload: {e}")))?;
    if record.hostname != hostname || record.token != token {
        return Err(Error::operation(
            "parse_acme_challenge",
            "challenge key mismatch between row and payload",
        ));
    }
    Ok(record)
}
