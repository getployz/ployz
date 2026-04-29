use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_types::error::{Error, Result};
use ployz_types::model::AcmeAccountRecord;

pub(crate) async fn get_acme_account(
    client: &CorrClient,
    issuer_url: &str,
) -> Result<Option<AcmeAccountRecord>> {
    let stmt = Statement::WithParams(
        "SELECT issuer_url, payload_json FROM acme_accounts WHERE issuer_url = ? AND payload_json <> '' LIMIT 1"
            .to_string(),
        vec![issuer_url.to_string().into()],
    );
    let rows = query_rows(client, &stmt, "get_acme_account").await?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(parse_acme_account(row)?))
}

pub(crate) async fn upsert_acme_account(
    client: &CorrClient,
    record: &AcmeAccountRecord,
) -> Result<()> {
    let payload_json = serde_json::to_string(record)
        .map_err(|e| Error::operation("upsert_acme_account", format!("serialize: {e}")))?;
    let stmt = Statement::WithParams(
        "INSERT INTO acme_accounts (issuer_url, payload_json) VALUES (?, ?) \
         ON CONFLICT(issuer_url) DO UPDATE SET payload_json=excluded.payload_json"
            .to_string(),
        vec![record.issuer_url.clone().into(), payload_json.into()],
    );
    exec_one(client, &[stmt], "upsert_acme_account").await
}

fn parse_acme_account(row: &[SqliteValue]) -> Result<AcmeAccountRecord> {
    let [issuer_val, payload_val] = row else {
        return Err(Error::operation(
            "parse_acme_account",
            format!("expected 2 columns, got {}", row.len()),
        ));
    };
    let issuer_url = text(issuer_val, "issuer_url")?;
    let payload_json = text(payload_val, "payload_json")?;
    let record: AcmeAccountRecord = serde_json::from_str(&payload_json)
        .map_err(|e| Error::operation("parse_acme_account", format!("decode payload: {e}")))?;
    if record.issuer_url != issuer_url {
        return Err(Error::operation(
            "parse_acme_account",
            "account key mismatch between row and payload",
        ));
    }
    Ok(record)
}
