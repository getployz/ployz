use crate::client::CorrClient;
use corro_api_types::{ExecResult, SqliteValue, Statement, TypedQueryEvent};
use futures_util::StreamExt;
use ployz_sdk::error::{Error, Result};
use std::time::Duration;
use tokio::time::timeout;

const QUERY_REQUEST_TIMEOUT_SECS: u64 = 5;
const QUERY_EVENT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) async fn query_rows(
    client: &CorrClient,
    stmt: &Statement,
    op: &'static str,
) -> Result<Vec<Vec<SqliteValue>>> {
    let mut stream = timeout(
        QUERY_EVENT_TIMEOUT,
        client.query(stmt, Some(QUERY_REQUEST_TIMEOUT_SECS)),
    )
    .await
    .map_err(|_| Error::operation(op, "timed out starting query stream"))?
    .map_err(|e| Error::operation(op, e.to_string()))?;
    let mut rows = Vec::new();
    while let Some(event) = timeout(QUERY_EVENT_TIMEOUT, stream.next())
        .await
        .map_err(|_| Error::operation(op, "timed out waiting for query event"))?
    {
        match event.map_err(|e| Error::operation(op, e.to_string()))? {
            TypedQueryEvent::Row(_, cells) => rows.push(cells),
            TypedQueryEvent::EndOfQuery { .. } => break,
            TypedQueryEvent::Error(e) => return Err(Error::operation(op, e.to_string())),
            TypedQueryEvent::Columns(_) | TypedQueryEvent::Change(..) => {}
        }
    }
    Ok(rows)
}

pub(crate) async fn exec_one(
    client: &CorrClient,
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

pub(crate) async fn exec_all(
    client: &CorrClient,
    stmts: &[Statement],
    op: &'static str,
) -> Result<()> {
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
