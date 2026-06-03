//! ACME certificate attempt rows over Polis store primitives.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::acme::{
    Hostname,
    attempt::{
        CertificateAttemptReplay, CertificateAttemptRequest, CertificateAttemptStart,
        CertificateAttemptStore, CertificateAttemptTerminal,
    },
};
use crate::error::{CertificateFailure, PrimitiveFailure};
use crate::operation::{IdempotencyKey, OperationId, ScopeId};

use super::failure_codecs;
use super::scalars;

const ATTEMPTS_TABLE: &str = "ployz_acme_certificate_attempts";
const ATTEMPT_COLUMNS: &[polis::StoreTableColumn] = &[
    polis::StoreTableColumn::new(
        "scope",
        "TEXT",
        polis::StorePrimaryKey::order(1),
        Some("length(trim(scope)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "operation",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(operation)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "idempotency",
        "TEXT",
        polis::StorePrimaryKey::order(2),
        Some("length(trim(idempotency)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "hostname",
        "TEXT",
        polis::StorePrimaryKey::order(3),
        Some("length(trim(hostname)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "certificate_deadline_secs",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("certificate_deadline_secs >= 0"),
    ),
    polis::StoreTableColumn::new(
        "certificate_deadline_nanos",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("certificate_deadline_nanos >= 0 AND certificate_deadline_nanos < 1000000000"),
    ),
    polis::StoreTableColumn::new(
        "status",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("status IN ('open', 'succeeded', 'failed', 'interrupted')"),
    ),
    polis::StoreTableColumn::new(
        "state_payload",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(state_payload)) > 0"),
    ),
];

#[derive(Clone)]
pub(crate) struct CorrosionCertificateAttempts {
    store: polis::CorrosionStore,
}

impl CorrosionCertificateAttempts {
    #[must_use]
    pub(crate) fn new(store: polis::CorrosionStore) -> Self {
        Self { store }
    }
}

pub(crate) fn start_corrosion_certificate_attempts(
    store: polis::CorrosionStore,
) -> impl CertificateAttemptStore {
    CorrosionCertificateAttempts::new(store)
}

#[cfg(test)]
fn certificate_attempt_schema_statements() -> Result<Vec<polis::StoreStatement>, polis::StoreError>
{
    polis::schema_statements(&schema_sql())
}

#[cfg(test)]
fn schema_sql() -> String {
    polis::create_table_sql(ATTEMPTS_TABLE, ATTEMPT_COLUMNS)
}

impl CertificateAttemptStore for CorrosionCertificateAttempts {
    async fn begin(
        &self,
        request: &CertificateAttemptRequest,
    ) -> Result<CertificateAttemptStart, PrimitiveFailure> {
        match self
            .begin_attempt(request)
            .await
            .map_err(map_attempt_store_error)?
        {
            CertificateAttemptBegin::Inserted => Ok(CertificateAttemptStart::Started),
            CertificateAttemptBegin::Existing(row) => {
                if !row.matches(request) {
                    Err(PrimitiveFailure::Conflict)
                } else {
                    Ok(CertificateAttemptStart::Replayed(row.into_replay()))
                }
            }
            CertificateAttemptBegin::MissingAfterConflict => Err(PrimitiveFailure::Conflict),
        }
    }

    async fn finish(
        &self,
        request: &CertificateAttemptRequest,
        terminal: CertificateAttemptTerminal,
    ) -> Result<(), PrimitiveFailure> {
        let changed = self
            .terminalize(request, &terminal)
            .await
            .map_err(map_attempt_store_error)?;
        if changed == 1 {
            Ok(())
        } else {
            Err(PrimitiveFailure::OperationStateConflict)
        }
    }
}

impl CorrosionCertificateAttempts {
    async fn begin_attempt(
        &self,
        request: &CertificateAttemptRequest,
    ) -> Result<CertificateAttemptBegin, polis::StoreError> {
        let storage_key = CertificateAttemptStorageKey::from_request(request);
        let attributes = CertificateAttemptAttributes::from_request(request);
        let row = CertificateAttemptRow::open(storage_key.clone(), attributes, request);
        let receipt = self
            .store
            .execute_transaction(
                &[insert_open_statement(&row)?],
                polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
            )
            .await?;
        if receipt.rows_affected() == 1 {
            return Ok(CertificateAttemptBegin::Inserted);
        }
        let statement = find_attempt_statement(&storage_key)?;
        let rows = self
            .store
            .query(&statement, polis::StoreTimeout::CONTROL_PLANE_DEFAULT)
            .await?;
        match rows.rows() {
            [row] => decode_attempt_row(row).map(CertificateAttemptBegin::Existing),
            [] => Ok(CertificateAttemptBegin::MissingAfterConflict),
            [_, _, ..] => Ok(CertificateAttemptBegin::MissingAfterConflict),
        }
    }

    async fn terminalize(
        &self,
        request: &CertificateAttemptRequest,
        terminal: &CertificateAttemptTerminal,
    ) -> Result<usize, polis::StoreError> {
        let receipt = self
            .store
            .execute_transaction(
                &[terminalize_statement(request, terminal)?],
                polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
            )
            .await?;
        Ok(receipt.rows_affected())
    }
}

fn find_attempt_statement(
    key: &CertificateAttemptStorageKey,
) -> Result<polis::StoreStatement, polis::StoreError> {
    polis::StoreStatement::with_params(
        format!(
            "SELECT {}
        FROM {ATTEMPTS_TABLE}
        WHERE scope = ?1
          AND idempotency = ?2
          AND hostname = ?3",
            polis::select_columns(ATTEMPT_COLUMNS)
        ),
        vec![
            polis::StoreParam::Text(key.scope.as_str().to_string()),
            polis::StoreParam::Text(key.idempotency.as_str().to_string()),
            polis::StoreParam::Text(key.hostname.as_str().to_string()),
        ],
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CertificateAttemptBegin {
    Inserted,
    Existing(CertificateAttemptRow),
    MissingAfterConflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CertificateAttemptStorageKey {
    scope: ScopeId,
    idempotency: IdempotencyKey,
    hostname: Hostname,
}

impl CertificateAttemptStorageKey {
    fn from_request(request: &CertificateAttemptRequest) -> Self {
        Self {
            scope: request.scope().clone(),
            idempotency: request.idempotency().clone(),
            hostname: request.hostname().clone(),
        }
    }

    fn matches_request(&self, request: &CertificateAttemptRequest) -> bool {
        &self.scope == request.scope()
            && &self.idempotency == request.idempotency()
            && &self.hostname == request.hostname()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CertificateAttemptAttributes {
    operation: OperationId,
    certificate_deadline: SystemTime,
}

impl CertificateAttemptAttributes {
    fn from_request(request: &CertificateAttemptRequest) -> Self {
        Self {
            operation: request.operation().clone(),
            certificate_deadline: request.certificate_deadline(),
        }
    }

    fn certificate_deadline_parts(&self) -> Result<(i64, i64), polis::StoreError> {
        scalars::unix_time_parts(self.certificate_deadline)
    }

    fn matches_request(&self, request: &CertificateAttemptRequest) -> bool {
        &self.operation == request.operation()
            && self.certificate_deadline == request.certificate_deadline()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CertificateAttemptRow {
    storage_key: CertificateAttemptStorageKey,
    attributes: CertificateAttemptAttributes,
    state: CertificateAttemptState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CertificateAttemptState {
    Open { owner_deadline: SystemTime },
    Succeeded,
    Failed(CertificateFailure),
    Interrupted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CertificateAttemptStatus {
    Open,
    Succeeded,
    Failed,
    Interrupted,
}

impl CertificateAttemptStatus {
    fn parse(value: &str) -> Result<Self, polis::StoreError> {
        match value {
            "open" => Ok(Self::Open),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "interrupted" => Ok(Self::Interrupted),
            _ => Err(polis::StoreError::MalformedPayload),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

impl CertificateAttemptRow {
    fn open(
        storage_key: CertificateAttemptStorageKey,
        attributes: CertificateAttemptAttributes,
        request: &CertificateAttemptRequest,
    ) -> Self {
        Self {
            storage_key,
            attributes,
            state: CertificateAttemptState::Open {
                owner_deadline: request.owner_deadline(),
            },
        }
    }

    fn matches(&self, request: &CertificateAttemptRequest) -> bool {
        self.storage_key.matches_request(request) && self.attributes.matches_request(request)
    }

    fn into_replay(self) -> CertificateAttemptReplay {
        match self.state {
            CertificateAttemptState::Succeeded => CertificateAttemptReplay::Succeeded {
                operation: self.attributes.operation,
            },
            CertificateAttemptState::Failed(failure) => CertificateAttemptReplay::Failed {
                operation: self.attributes.operation,
                failure,
            },
            CertificateAttemptState::Interrupted => CertificateAttemptReplay::Interrupted {
                operation: self.attributes.operation,
            },
            CertificateAttemptState::Open { owner_deadline } => CertificateAttemptReplay::Open {
                operation: self.attributes.operation,
                owner_deadline,
            },
        }
    }
}

impl CertificateAttemptState {
    fn from_terminal(terminal: &CertificateAttemptTerminal) -> Self {
        match terminal {
            CertificateAttemptTerminal::Succeeded => Self::Succeeded,
            CertificateAttemptTerminal::Failed(failure) => Self::Failed(failure.clone()),
            CertificateAttemptTerminal::Interrupted => Self::Interrupted,
        }
    }

    fn status(&self) -> CertificateAttemptStatus {
        match self {
            Self::Open { .. } => CertificateAttemptStatus::Open,
            Self::Succeeded => CertificateAttemptStatus::Succeeded,
            Self::Failed(_) => CertificateAttemptStatus::Failed,
            Self::Interrupted => CertificateAttemptStatus::Interrupted,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedOpenAttempt {
    owner_deadline_secs: i64,
    owner_deadline_nanos: i64,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedFailedAttempt {
    failure_payload: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedEmptyAttempt {}

fn state_payload(state: &CertificateAttemptState) -> Result<String, polis::StoreError> {
    match state {
        CertificateAttemptState::Open { owner_deadline } => {
            let (owner_deadline_secs, owner_deadline_nanos) =
                scalars::unix_time_parts(*owner_deadline)?;
            serde_json::to_string(&PersistedOpenAttempt {
                owner_deadline_secs,
                owner_deadline_nanos,
            })
            .map_err(|_| polis::StoreError::MalformedPayload)
        }
        CertificateAttemptState::Succeeded | CertificateAttemptState::Interrupted => {
            serde_json::to_string(&PersistedEmptyAttempt {})
                .map_err(|_| polis::StoreError::MalformedPayload)
        }
        CertificateAttemptState::Failed(failure) => {
            serde_json::to_string(&PersistedFailedAttempt {
                failure_payload: failure_codecs::certificate_failure_payload(failure)?,
            })
            .map_err(|_| polis::StoreError::MalformedPayload)
        }
    }
}

fn state_from_payload(
    status: CertificateAttemptStatus,
    payload: &str,
) -> Result<CertificateAttemptState, polis::StoreError> {
    match status {
        CertificateAttemptStatus::Open => {
            let payload = serde_json::from_str::<PersistedOpenAttempt>(payload)
                .map_err(|_| polis::StoreError::MalformedPayload)?;
            Ok(CertificateAttemptState::Open {
                owner_deadline: scalars::system_time_from_unix_parts(
                    payload.owner_deadline_secs,
                    payload.owner_deadline_nanos,
                )?,
            })
        }
        CertificateAttemptStatus::Succeeded => {
            serde_json::from_str::<PersistedEmptyAttempt>(payload)
                .map_err(|_| polis::StoreError::MalformedPayload)?;
            Ok(CertificateAttemptState::Succeeded)
        }
        CertificateAttemptStatus::Failed => {
            let payload = serde_json::from_str::<PersistedFailedAttempt>(payload)
                .map_err(|_| polis::StoreError::MalformedPayload)?;
            Ok(CertificateAttemptState::Failed(
                failure_codecs::certificate_failure_from_payload(&payload.failure_payload)?,
            ))
        }
        CertificateAttemptStatus::Interrupted => {
            serde_json::from_str::<PersistedEmptyAttempt>(payload)
                .map_err(|_| polis::StoreError::MalformedPayload)?;
            Ok(CertificateAttemptState::Interrupted)
        }
    }
}

fn insert_open_statement(
    row: &CertificateAttemptRow,
) -> Result<polis::StoreStatement, polis::StoreError> {
    let (certificate_secs, certificate_nanos) = row.attributes.certificate_deadline_parts()?;
    let status = row.state.status();
    let payload = state_payload(&row.state)?;
    polis::StoreStatement::with_params(
        format!(
            "INSERT OR IGNORE INTO {ATTEMPTS_TABLE} (
            scope,
            operation,
            idempotency,
            hostname,
            certificate_deadline_secs,
            certificate_deadline_nanos,
            status,
            state_payload
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ),
        vec![
            polis::StoreParam::Text(row.storage_key.scope.as_str().to_string()),
            polis::StoreParam::Text(row.attributes.operation.as_str().to_string()),
            polis::StoreParam::Text(row.storage_key.idempotency.as_str().to_string()),
            polis::StoreParam::Text(row.storage_key.hostname.as_str().to_string()),
            polis::StoreParam::Integer(certificate_secs),
            polis::StoreParam::Integer(certificate_nanos),
            polis::StoreParam::Text(status.as_str().to_string()),
            polis::StoreParam::Text(payload),
        ],
    )
}

fn terminalize_statement(
    request: &CertificateAttemptRequest,
    terminal: &CertificateAttemptTerminal,
) -> Result<polis::StoreStatement, polis::StoreError> {
    let (sql, params) = terminalize_statement_parts(request, terminal)?;
    polis::StoreStatement::with_params(sql, params)
}

fn terminalize_statement_parts(
    request: &CertificateAttemptRequest,
    terminal: &CertificateAttemptTerminal,
) -> Result<(String, Vec<polis::StoreParam>), polis::StoreError> {
    let storage_key = CertificateAttemptStorageKey::from_request(request);
    let attributes = CertificateAttemptAttributes::from_request(request);
    let (certificate_secs, certificate_nanos) = attributes.certificate_deadline_parts()?;
    let state = CertificateAttemptState::from_terminal(terminal);
    let status = state.status();
    let payload = state_payload(&state)?;
    Ok((
        format!(
            "UPDATE {ATTEMPTS_TABLE}
        SET status = ?7,
            state_payload = ?8
        WHERE scope = ?1
          AND idempotency = ?2
          AND hostname = ?3
          AND operation = ?4
          AND certificate_deadline_secs = ?5
          AND certificate_deadline_nanos = ?6
          AND status = 'open'"
        ),
        vec![
            polis::StoreParam::Text(storage_key.scope.as_str().to_string()),
            polis::StoreParam::Text(storage_key.idempotency.as_str().to_string()),
            polis::StoreParam::Text(storage_key.hostname.as_str().to_string()),
            polis::StoreParam::Text(attributes.operation.as_str().to_string()),
            polis::StoreParam::Integer(certificate_secs),
            polis::StoreParam::Integer(certificate_nanos),
            polis::StoreParam::Text(status.as_str().to_string()),
            polis::StoreParam::Text(payload),
        ],
    ))
}

fn decode_attempt_row(row: &polis::StoreRow) -> Result<CertificateAttemptRow, polis::StoreError> {
    let scope =
        ScopeId::parse(row.text("scope")?).map_err(|_| polis::StoreError::MalformedPayload)?;
    let operation = OperationId::parse(row.text("operation")?)
        .map_err(|_| polis::StoreError::MalformedPayload)?;
    let status = CertificateAttemptStatus::parse(row.text("status")?)?;
    let state = state_from_payload(status, row.text("state_payload")?)?;
    Ok(CertificateAttemptRow {
        storage_key: CertificateAttemptStorageKey {
            scope,
            idempotency: IdempotencyKey::parse(row.text("idempotency")?)
                .map_err(|_| polis::StoreError::MalformedPayload)?,
            hostname: Hostname::parse(row.text("hostname")?)
                .map_err(|_| polis::StoreError::MalformedPayload)?,
        },
        attributes: CertificateAttemptAttributes {
            operation,
            certificate_deadline: scalars::system_time_from_unix_parts(
                row.integer("certificate_deadline_secs")?,
                row.integer("certificate_deadline_nanos")?,
            )?,
        },
        state,
    })
}

fn map_attempt_store_error(error: polis::StoreError) -> PrimitiveFailure {
    match error {
        polis::StoreError::MalformedPayload => PrimitiveFailure::MalformedPayload,
        polis::StoreError::Timeout => PrimitiveFailure::Timeout,
        polis::StoreError::MissedChange { .. }
        | polis::StoreError::Stream { .. }
        | polis::StoreError::Client { .. }
        | polis::StoreError::Response { .. }
        | polis::StoreError::QueryChangedBeforeEndOfQuery
        | polis::StoreError::QueryEndedBeforeEndOfQuery => PrimitiveFailure::NoResponder,
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use rusqlite::params_from_iter;
    use rusqlite::types::Value;

    use super::*;
    use crate::acme::{CertificateDeadline, CertificateIssueRequest, HttpsBinding};
    use crate::operation::{
        AuthorityContext, AuthorityEpoch, IdempotencyKey, OperationId, PrincipalId, ScopeId,
    };

    #[test]
    fn schema_creates_scoped_attempt_table() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        for statement in certificate_attempt_schema_statements().expect("schema") {
            conn.execute(statement.sql(), []).expect("schema statement");
        }

        let primary_keys = conn
            .prepare(
                "SELECT name, pk
                FROM pragma_table_info('ployz_acme_certificate_attempts')
                WHERE name IN ('scope', 'operation', 'idempotency', 'hostname', 'certificate_deadline_secs')
                ORDER BY name",
            )
            .expect("prepare primary keys")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("query primary keys")
            .collect::<Result<Vec<_>, _>>()
            .expect("primary keys");
        let product_indexes = conn
            .prepare(
                "SELECT name FROM sqlite_schema
                WHERE type = 'index'
                  AND name NOT LIKE 'sqlite_%'
                ORDER BY name",
            )
            .expect("prepare indexes")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query indexes")
            .collect::<Result<Vec<_>, _>>()
            .expect("indexes");

        assert_eq!(
            primary_keys,
            vec![
                ("certificate_deadline_secs".to_string(), 0),
                ("hostname".to_string(), 3),
                ("idempotency".to_string(), 2),
                ("operation".to_string(), 0),
                ("scope".to_string(), 1),
            ]
        );
        assert!(product_indexes.is_empty());
    }

    #[test]
    fn partial_attempt_rows_are_rejected() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        for statement in certificate_attempt_schema_statements().expect("schema") {
            conn.execute(statement.sql(), []).expect("schema statement");
        }

        let result = conn.execute(
            "INSERT INTO ployz_acme_certificate_attempts (scope, operation)
            VALUES ('cluster', 'operation-1')",
            [],
        );

        assert!(result.is_err());
    }

    #[test]
    fn terminalize_requires_full_attempt_identity() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        for statement in certificate_attempt_schema_statements().expect("schema") {
            conn.execute(statement.sql(), []).expect("schema statement");
        }
        let original = attempt_request("deploy-1", deadline(120), owner_deadline(60));
        let open_payload = state_payload(&CertificateAttemptState::Open {
            owner_deadline: owner_deadline(60),
        })
        .expect("open state payload");
        conn.execute(
            "INSERT INTO ployz_acme_certificate_attempts (
                scope,
                operation,
                idempotency,
                hostname,
                certificate_deadline_secs,
                certificate_deadline_nanos,
                status,
                state_payload
            ) VALUES ('cluster', 'deploy-1', 'idem-1', 'app.example.com', 120, 0, 'open', ?1)",
            [&open_payload],
        )
        .expect("insert original attempt");

        let changed_operation = attempt_request("deploy-2", deadline(120), owner_deadline(60));
        let changed_deadline = attempt_request("deploy-1", deadline(240), owner_deadline(60));

        assert_eq!(
            execute_terminalize(&conn, &changed_operation),
            0,
            "changed operation must not finish the stored attempt"
        );
        assert_eq!(
            execute_terminalize(&conn, &changed_deadline),
            0,
            "changed certificate deadline must not finish the stored attempt"
        );
        assert_eq!(attempt_status(&conn), "open");

        assert_eq!(execute_terminalize(&conn, &original), 1);
        assert_eq!(attempt_status(&conn), "succeeded");
    }

    #[test]
    fn decode_rejects_status_and_state_payload_mismatch() {
        let failed_payload = state_payload(&CertificateAttemptState::Failed(
            CertificateFailure::IssuanceFailed,
        ))
        .expect("failed payload");
        let row = attempt_store_row("succeeded", failed_payload);

        assert_eq!(
            decode_attempt_row(&row),
            Err(polis::StoreError::MalformedPayload)
        );
    }

    fn attempt_store_row(status: &str, state_payload: String) -> polis::StoreRow {
        polis::StoreRow::from_fields([
            ("scope", polis::StoreValue::Text("cluster".to_string())),
            ("operation", polis::StoreValue::Text("deploy-1".to_string())),
            ("idempotency", polis::StoreValue::Text("idem-1".to_string())),
            (
                "hostname",
                polis::StoreValue::Text("app.example.com".to_string()),
            ),
            ("certificate_deadline_secs", polis::StoreValue::Integer(120)),
            ("certificate_deadline_nanos", polis::StoreValue::Integer(0)),
            ("status", polis::StoreValue::Text(status.to_string())),
            ("state_payload", polis::StoreValue::Text(state_payload)),
        ])
        .expect("store row")
    }

    fn execute_terminalize(
        conn: &rusqlite::Connection,
        request: &CertificateAttemptRequest,
    ) -> usize {
        let (sql, params) =
            terminalize_statement_parts(request, &CertificateAttemptTerminal::Succeeded)
                .expect("terminalize");
        conn.execute(&sql, params_from_iter(params.iter().map(sqlite_value)))
            .expect("execute terminalize")
    }

    fn sqlite_value(param: &polis::StoreParam) -> Value {
        match param {
            polis::StoreParam::Null => Value::Null,
            polis::StoreParam::Integer(value) => Value::Integer(*value),
            polis::StoreParam::Real(value) => Value::Real(*value),
            polis::StoreParam::Text(value) => Value::Text(value.clone()),
            polis::StoreParam::Blob(value) => Value::Blob(value.clone()),
        }
    }

    fn attempt_status(conn: &rusqlite::Connection) -> String {
        conn.query_row(
            "SELECT status FROM ployz_acme_certificate_attempts",
            [],
            |row| row.get(0),
        )
        .expect("attempt status")
    }

    fn attempt_request(
        operation: &str,
        certificate_deadline: SystemTime,
        owner_deadline: SystemTime,
    ) -> CertificateAttemptRequest {
        CertificateAttemptRequest::for_issue(
            &context(operation, owner_deadline),
            &issue_request(certificate_deadline),
        )
    }

    fn context(operation: &str, deadline: SystemTime) -> crate::operation::MutationContext {
        crate::operation::MutationContext::new(
            OperationId::parse(operation).expect("operation"),
            IdempotencyKey::parse("idem-1").expect("idempotency"),
            AuthorityContext::new(
                PrincipalId::parse("node-a").expect("principal"),
                ScopeId::parse("cluster").expect("scope"),
                AuthorityEpoch::new(7),
            ),
            None,
            deadline,
        )
    }

    fn issue_request(deadline: SystemTime) -> CertificateIssueRequest {
        CertificateIssueRequest::new(
            HttpsBinding::new(Hostname::parse("app.example.com").expect("hostname")),
            CertificateDeadline {
                expires_at: deadline,
            },
        )
    }

    fn deadline(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn owner_deadline(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }
}
