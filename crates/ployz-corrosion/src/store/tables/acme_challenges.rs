use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_types::error::{Error, Result};
use ployz_types::model::AcmeChallengeRecord;

pub(crate) async fn list_acme_challenges(client: &CorrClient) -> Result<Vec<AcmeChallengeRecord>> {
    let stmt = Statement::Simple(
        "SELECT hostname, token, payload_json FROM acme_challenges WHERE payload_json <> '' ORDER BY hostname, token"
            .to_string(),
    );
    query_rows(client, &stmt, "list_acme_challenges")
        .await?
        .iter()
        .map(|row| parse_acme_challenge(row))
        .collect()
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
