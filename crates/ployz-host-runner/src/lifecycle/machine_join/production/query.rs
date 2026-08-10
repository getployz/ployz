//! Bounded local Corrosion roster convergence checks.

use std::fs;
use std::path::Path;

use ployz_core::corrosion::{
    MachineDocument, QueryEvent, SqliteParameter, SqliteValue, Statement, StoredRow,
    read_named_roster_rows,
};
use ployz_core::join::ValidatedMachineJoinAccepted;
use ployz_core::operation::FailureMessage;

use super::{CORROSION_API_PORT, CORROSION_QUERY_BODY_LIMIT, failure};

#[derive(Clone, PartialEq, Eq)]
pub(super) struct CorrosionBearerToken(String);

impl std::fmt::Debug for CorrosionBearerToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CorrosionBearerToken([REDACTED])")
    }
}

impl CorrosionBearerToken {
    pub(super) fn from_file(path: &Path) -> Result<Self, FailureMessage> {
        let value = match fs::read_to_string(path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(failure("durable Corrosion bearer token is absent"));
            }
            Err(error) => {
                return Err(failure(format!(
                    "could not read durable Corrosion bearer token: {error}"
                )));
            }
        };
        let value = value.trim();
        if value.is_empty() {
            return Err(failure("durable Corrosion bearer token is empty"));
        }
        Ok(Self(value.to_owned()))
    }

    fn authorization_header(&self) -> String {
        format!("Bearer {}", self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CorrosionRosterQuery {
    pub(super) url: String,
    pub(super) body: String,
}

pub(super) fn corrosion_roster_query(
    accepted: &ValidatedMachineJoinAccepted,
) -> Result<CorrosionRosterQuery, FailureMessage> {
    let accepted = accepted.accepted();
    let statement = Statement::with_params(
        "SELECT id, document FROM machines WHERE id = ?",
        vec![SqliteParameter::Text(
            accepted.machine.name.as_str().to_owned(),
        )],
    );
    Ok(CorrosionRosterQuery {
        url: format!("http://127.0.0.1:{CORROSION_API_PORT}/v1/queries"),
        body: serde_json::to_string(&statement).map_err(failure)?,
    })
}

pub(super) fn query_corrosion_roster(
    agent: &ureq::Agent,
    query: &CorrosionRosterQuery,
    token: &CorrosionBearerToken,
) -> Result<Vec<StoredRow>, FailureMessage> {
    let authorization = token.authorization_header();
    let mut response = agent
        .post(&query.url)
        .header("Authorization", authorization)
        .header("Accept", "application/x-ndjson")
        .content_type("application/json")
        .send(query.body.as_bytes())
        .map_err(|error| failure(format!("local Corrosion query failed: {error}")))?;
    let body = response
        .body_mut()
        .with_config()
        .limit(CORROSION_QUERY_BODY_LIMIT)
        .read_to_string()
        .map_err(|error| failure(format!("local Corrosion response failed: {error}")))?;
    decode_corrosion_rows(&body)
}

pub(super) fn decode_corrosion_rows(body: &str) -> Result<Vec<StoredRow>, FailureMessage> {
    if body.len() as u64 > CORROSION_QUERY_BODY_LIMIT {
        return Err(failure("local Corrosion response exceeded 65536 bytes"));
    }
    let mut saw_columns = false;
    let mut saw_end = false;
    let mut rows = Vec::new();
    for line in body.lines().filter(|line| !line.trim().is_empty()) {
        if saw_end {
            return Err(failure(
                "local Corrosion response continued after end-of-query",
            ));
        }
        let event: QueryEvent = serde_json::from_str(line)
            .map_err(|error| failure(format!("invalid local Corrosion frame: {error}")))?;
        match event {
            QueryEvent::Columns(columns) if !saw_columns && columns == ["id", "document"] => {
                saw_columns = true;
            }
            QueryEvent::Columns(_) if saw_columns => {
                return Err(failure("local Corrosion response repeated columns"));
            }
            QueryEvent::Columns(_) => {
                return Err(failure(
                    "local Corrosion response columns were not id and document",
                ));
            }
            QueryEvent::Row(_, _values) if !saw_columns => {
                return Err(failure("local Corrosion row preceded columns"));
            }
            QueryEvent::Row(_, values) => {
                let [SqliteValue::Text(key), SqliteValue::Text(document)] = values.as_slice()
                else {
                    return Err(failure(
                        "local Corrosion row was not a text id and document pair",
                    ));
                };
                rows.push(StoredRow::new(key.clone(), document.clone()));
            }
            QueryEvent::EndOfQuery(_end) if !saw_columns => {
                return Err(failure("local Corrosion end-of-query preceded columns"));
            }
            QueryEvent::EndOfQuery(end) if end.change_id.is_some() => {
                return Err(failure(
                    "local Corrosion query end unexpectedly carried a change id",
                ));
            }
            QueryEvent::EndOfQuery(_) => saw_end = true,
            QueryEvent::Error(message) => {
                return Err(failure(format!("local Corrosion query error: {message}")));
            }
        }
    }
    if !saw_end {
        return Err(failure("local Corrosion response omitted end-of-query"));
    }
    Ok(rows)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RosterConvergenceDisposition {
    Converged,
    Missing,
    Skipped,
    Divergent,
}

impl std::fmt::Display for RosterConvergenceDisposition {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Converged => formatter.write_str("converged"),
            Self::Missing => formatter.write_str("accepted machine row is missing"),
            Self::Skipped => formatter.write_str("accepted machine row is invalid locally"),
            Self::Divergent => formatter.write_str("accepted machine row differs locally"),
        }
    }
}

pub(super) fn roster_convergence_disposition(
    accepted: &ValidatedMachineJoinAccepted,
    rows: Vec<StoredRow>,
) -> RosterConvergenceDisposition {
    let accepted = accepted.accepted();
    let report = read_named_roster_rows::<MachineDocument>(&accepted.cluster, rows);
    let winner = report
        .accepted
        .iter()
        .find(|row| row.value.name == accepted.machine.name);
    if let Some(winner) = winner {
        if winner.source.key != accepted.machine.name.as_str() {
            return RosterConvergenceDisposition::Skipped;
        }
        if winner.value == accepted.machine {
            return RosterConvergenceDisposition::Converged;
        }
        return RosterConvergenceDisposition::Divergent;
    }
    if report
        .skipped
        .iter()
        .any(|row| row.source.key == accepted.machine.name.as_str())
    {
        RosterConvergenceDisposition::Skipped
    } else {
        RosterConvergenceDisposition::Missing
    }
}
