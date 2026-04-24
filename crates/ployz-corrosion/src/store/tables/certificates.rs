use crate::client::CorrClient;
use crate::store::shared::decode::text;
use crate::store::shared::sql::{exec_one, query_rows};
use corro_api_types::{SqliteValue, Statement};
use ployz_types::error::{Error, Result};
use ployz_types::model::CertificateRecord;

pub(crate) async fn list_certificates(client: &CorrClient) -> Result<Vec<CertificateRecord>> {
    let stmt = Statement::Simple(
        "SELECT hostname, payload_json FROM certificates WHERE payload_json <> '' ORDER BY hostname"
            .to_string(),
    );
    query_rows(client, &stmt, "list_certificates")
        .await?
        .iter()
        .map(|row| parse_certificate(row))
        .collect()
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
