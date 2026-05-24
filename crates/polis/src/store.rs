//! Minimal Corrosion-specific store primitives.
//!
//! This module intentionally exposes distributed store mechanics, not Ployz
//! product services. Ployz adapters own row decoding and product semantics.

use std::{fmt, net::SocketAddr, time::Duration};

use corro_api_types::{
    ChangeId, ExecResult, SqliteParam, SqliteValue, Statement, TypedNotifyEvent, TypedQueryEvent,
    sqlite::ChangeType,
};
use corro_client::{CorrosionApiClient, sub};
use futures_util::StreamExt;
use thiserror::Error;

pub type StoreResult<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum StoreError {
    #[error("payload is malformed")]
    MalformedPayload,

    #[error("corrosion client error: {message}")]
    Client { message: String },

    #[error("corrosion response error: {message}")]
    Response { message: String },

    #[error("query stream ended before end-of-query")]
    QueryEndedBeforeEndOfQuery,

    #[error("store operation timed out")]
    Timeout,

    #[error("subscription missed a change (expected: {expected}, got: {got})")]
    MissedChange {
        expected: StoreChangeId,
        got: StoreChangeId,
    },

    #[error("store stream error: {message}")]
    Stream { message: String },
}

#[derive(Debug, Clone)]
pub struct StoreStatement(Statement);

impl StoreStatement {
    pub fn new(sql: impl Into<String>) -> StoreResult<Self> {
        let sql = sql.into();
        if sql.trim().is_empty() {
            return Err(StoreError::MalformedPayload);
        }
        Ok(Self(Statement::Simple(sql)))
    }

    pub fn with_params(sql: impl Into<String>, params: Vec<StoreParam>) -> StoreResult<Self> {
        let sql = sql.into();
        if sql.trim().is_empty() {
            return Err(StoreError::MalformedPayload);
        }
        Ok(Self(Statement::WithParams(
            sql,
            params.into_iter().map(StoreParam::into_corrosion).collect(),
        )))
    }

    #[must_use]
    pub fn sql(&self) -> &str {
        self.0.query()
    }

    fn as_corrosion(&self) -> &Statement {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoreParam {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl StoreParam {
    fn into_corrosion(self) -> SqliteParam {
        match self {
            Self::Null => SqliteParam::Null,
            Self::Integer(value) => SqliteParam::Integer(value),
            Self::Real(value) => SqliteParam::Real(value),
            Self::Text(value) => SqliteParam::Text(value.into()),
            Self::Blob(value) => SqliteParam::Blob(value.into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreTimeout(u64);

impl StoreTimeout {
    pub const CONTROL_PLANE_DEFAULT: Self = Self(2);

    pub fn seconds(value: u64) -> StoreResult<Self> {
        if value == 0 {
            return Err(StoreError::MalformedPayload);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn seconds_value(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StoreChangeId(u64);

impl StoreChangeId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn value(self) -> u64 {
        self.0
    }
}

impl From<ChangeId> for StoreChangeId {
    fn from(value: ChangeId) -> Self {
        Self(value.0)
    }
}

impl From<StoreChangeId> for ChangeId {
    fn from(value: StoreChangeId) -> Self {
        Self(value.0)
    }
}

impl fmt::Display for StoreChangeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoreValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<SqliteValue> for StoreValue {
    fn from(value: SqliteValue) -> Self {
        match value {
            SqliteValue::Null => Self::Null,
            SqliteValue::Integer(value) => Self::Integer(value),
            SqliteValue::Real(value) => Self::Real(value.0),
            SqliteValue::Text(value) => Self::Text(value.to_string()),
            SqliteValue::Blob(value) => Self::Blob(value.to_vec()),
        }
    }
}

pub type StoreRow = Vec<StoreValue>;

#[derive(Debug, Clone, PartialEq)]
pub struct StoreQueryRows {
    rows: Vec<StoreRow>,
    change_id: Option<StoreChangeId>,
}

impl StoreQueryRows {
    #[must_use]
    pub fn rows(&self) -> &[StoreRow] {
        &self.rows
    }

    #[must_use]
    pub fn change_id(&self) -> Option<StoreChangeId> {
        self.change_id
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoreQueryEvent {
    Columns(Vec<String>),
    Row(StoreRow),
    EndOfQuery {
        change_id: Option<StoreChangeId>,
    },
    Change {
        change_type: StoreChangeType,
        row: StoreRow,
        change_id: StoreChangeId,
    },
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreChangeType {
    Insert,
    Update,
    Delete,
}

impl From<ChangeType> for StoreChangeType {
    fn from(value: ChangeType) -> Self {
        match value {
            ChangeType::Insert => Self::Insert,
            ChangeType::Update => Self::Update,
            ChangeType::Delete => Self::Delete,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionReceipt {
    rows_affected: usize,
}

impl TransactionReceipt {
    #[must_use]
    pub fn rows_affected(&self) -> usize {
        self.rows_affected
    }
}

#[derive(Clone)]
pub struct CorrosionStore {
    client: CorrosionApiClient,
}

impl CorrosionStore {
    pub fn new(api_addr: SocketAddr) -> StoreResult<Self> {
        let client = CorrosionApiClient::new(api_addr).map_err(|error| StoreError::Client {
            message: error.to_string(),
        })?;
        Ok(Self { client })
    }

    #[must_use]
    pub fn from_client(client: CorrosionApiClient) -> Self {
        Self { client }
    }

    pub async fn apply_schema(
        &self,
        statements: &[StoreStatement],
        timeout: StoreTimeout,
    ) -> StoreResult<TransactionReceipt> {
        self.execute_transaction(statements, timeout).await
    }

    pub async fn execute_transaction(
        &self,
        statements: &[StoreStatement],
        timeout: StoreTimeout,
    ) -> StoreResult<TransactionReceipt> {
        if statements.is_empty() {
            return Err(StoreError::MalformedPayload);
        }

        let statements = statements
            .iter()
            .map(|statement| statement.as_corrosion().clone())
            .collect::<Vec<_>>();
        let response = self
            .client
            .execute(&statements, Some(timeout.seconds_value()))
            .await
            .map_err(map_client_error)?;
        transaction_receipt(response.results)
    }

    pub async fn query(
        &self,
        statement: &StoreStatement,
        timeout: StoreTimeout,
    ) -> StoreResult<StoreQueryRows> {
        let mut stream = tokio::time::timeout(
            timeout.duration(),
            self.client
                .query(statement.as_corrosion(), Some(timeout.seconds_value())),
        )
        .await
        .map_err(|_| StoreError::Timeout)?
        .map_err(map_client_error)?;
        tokio::time::timeout(timeout.duration(), collect_query_rows(&mut stream))
            .await
            .map_err(|_| StoreError::Timeout)?
    }

    pub async fn subscribe(
        &self,
        statement: &StoreStatement,
        skip_rows: bool,
        from: Option<StoreChangeId>,
        timeout: StoreTimeout,
    ) -> StoreResult<StoreSubscription> {
        let inner = tokio::time::timeout(
            timeout.duration(),
            self.client.subscribe(
                statement.as_corrosion(),
                skip_rows,
                from.map(ChangeId::from),
            ),
        )
        .await
        .map_err(|_| StoreError::Timeout)?
        .map_err(map_client_error)?;
        Ok(StoreSubscription { inner, timeout })
    }

    pub async fn updates(&self, table: &str, timeout: StoreTimeout) -> StoreResult<StoreUpdate> {
        if table.trim().is_empty() {
            return Err(StoreError::MalformedPayload);
        }
        let inner = tokio::time::timeout(timeout.duration(), self.client.updates(table))
            .await
            .map_err(|_| StoreError::Timeout)?
            .map_err(map_client_error)?;
        Ok(StoreUpdate { inner, timeout })
    }
}

pub struct StoreSubscription {
    inner: sub::SubscriptionStream<Vec<SqliteValue>>,
    timeout: StoreTimeout,
}

impl StoreSubscription {
    #[must_use]
    pub fn id(&self) -> String {
        self.inner.id().to_string()
    }

    pub async fn next(&mut self) -> StoreResult<Option<StoreSubscriptionEvent>> {
        tokio::time::timeout(self.timeout.duration(), self.inner.next())
            .await
            .map_err(|_| StoreError::Timeout)?
            .transpose()
            .map_err(map_subscription_error)
            .map(|event| event.map(StoreSubscriptionEvent::from))
    }
}

pub type StoreSubscriptionEvent = StoreQueryEvent;

pub struct StoreUpdate {
    inner: sub::UpdatesStream<Vec<SqliteValue>>,
    timeout: StoreTimeout,
}

impl StoreUpdate {
    #[must_use]
    pub fn id(&self) -> String {
        self.inner.id().to_string()
    }

    pub async fn next(&mut self) -> StoreResult<Option<StoreUpdateEvent>> {
        tokio::time::timeout(self.timeout.duration(), self.inner.next())
            .await
            .map_err(|_| StoreError::Timeout)?
            .transpose()
            .map_err(map_updates_error)
            .map(|event| event.map(StoreUpdateEvent::from))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StoreUpdateEvent {
    Notify {
        change_type: StoreChangeType,
        row: StoreRow,
    },
    Error(String),
}

impl From<TypedNotifyEvent<Vec<SqliteValue>>> for StoreUpdateEvent {
    fn from(value: TypedNotifyEvent<Vec<SqliteValue>>) -> Self {
        match value {
            TypedNotifyEvent::Notify(change_type, row) => Self::Notify {
                change_type: change_type.into(),
                row: convert_row(row),
            },
            TypedNotifyEvent::Error(error) => Self::Error(error.to_string()),
        }
    }
}

async fn collect_query_rows(
    stream: &mut sub::QueryStream<Vec<SqliteValue>>,
) -> StoreResult<StoreQueryRows> {
    let mut rows = Vec::new();
    while let Some(event) = stream.next().await {
        match event.map_err(map_query_error)? {
            TypedQueryEvent::Columns(_) => {}
            TypedQueryEvent::Row(_, row) => rows.push(convert_row(row)),
            TypedQueryEvent::EndOfQuery { change_id, .. } => {
                return Ok(StoreQueryRows {
                    rows,
                    change_id: change_id.map(StoreChangeId::from),
                });
            }
            TypedQueryEvent::Change(_, _, row, change_id) => {
                rows.push(convert_row(row));
                return Ok(StoreQueryRows {
                    rows,
                    change_id: Some(StoreChangeId::from(change_id)),
                });
            }
            TypedQueryEvent::Error(error) => {
                return Err(StoreError::Response {
                    message: error.to_string(),
                });
            }
        }
    }
    Err(StoreError::QueryEndedBeforeEndOfQuery)
}

impl From<TypedQueryEvent<Vec<SqliteValue>>> for StoreQueryEvent {
    fn from(value: TypedQueryEvent<Vec<SqliteValue>>) -> Self {
        match value {
            TypedQueryEvent::Columns(columns) => Self::Columns(
                columns
                    .into_iter()
                    .map(|column| column.to_string())
                    .collect(),
            ),
            TypedQueryEvent::Row(_, row) => Self::Row(convert_row(row)),
            TypedQueryEvent::EndOfQuery { change_id, .. } => Self::EndOfQuery {
                change_id: change_id.map(StoreChangeId::from),
            },
            TypedQueryEvent::Change(change_type, _, row, change_id) => Self::Change {
                change_type: change_type.into(),
                row: convert_row(row),
                change_id: StoreChangeId::from(change_id),
            },
            TypedQueryEvent::Error(error) => Self::Error(error.to_string()),
        }
    }
}

fn transaction_receipt(results: Vec<ExecResult>) -> StoreResult<TransactionReceipt> {
    let mut rows_affected = 0;
    for result in results {
        match result {
            ExecResult::Execute {
                rows_affected: rows,
                ..
            } => rows_affected += rows,
            ExecResult::Error { error } => {
                return Err(StoreError::Response { message: error });
            }
        }
    }
    Ok(TransactionReceipt { rows_affected })
}

fn convert_row(row: Vec<SqliteValue>) -> StoreRow {
    row.into_iter().map(StoreValue::from).collect()
}

impl StoreTimeout {
    fn duration(self) -> Duration {
        Duration::from_secs(self.seconds_value())
    }
}

fn map_client_error(error: corro_client::Error) -> StoreError {
    match error {
        corro_client::Error::ResponseError(message) => StoreError::Response { message },
        other @ (corro_client::Error::Dns(_)
        | corro_client::Error::Reqwest(_)
        | corro_client::Error::InvalidUri(_)
        | corro_client::Error::Http(_)
        | corro_client::Error::Serde(_)
        | corro_client::Error::UnexpectedStatusCode(_)
        | corro_client::Error::UnexpectedResult(_)
        | corro_client::Error::ExpectedQueryId) => StoreError::Client {
            message: other.to_string(),
        },
    }
}

fn map_query_error(error: sub::QueryError) -> StoreError {
    StoreError::Stream {
        message: error.to_string(),
    }
}

fn map_subscription_error(error: sub::SubscriptionError) -> StoreError {
    match error {
        sub::SubscriptionError::MissedChange { expected, got } => StoreError::MissedChange {
            expected: StoreChangeId::from(expected),
            got: StoreChangeId::from(got),
        },
        other @ (sub::SubscriptionError::Io(_)
        | sub::SubscriptionError::Http(_)
        | sub::SubscriptionError::Deserialize(_)
        | sub::SubscriptionError::MaxLineLengthExceeded
        | sub::SubscriptionError::UnfinishedQuery
        | sub::SubscriptionError::MaxRetryAttempts) => StoreError::Stream {
            message: other.to_string(),
        },
    }
}

fn map_updates_error(error: sub::UpdatesError) -> StoreError {
    StoreError::Stream {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statement_rejects_empty_sql() {
        assert!(matches!(
            StoreStatement::new(""),
            Err(StoreError::MalformedPayload)
        ));
        assert!(matches!(
            StoreStatement::new("   "),
            Err(StoreError::MalformedPayload)
        ));
    }

    #[test]
    fn timeout_rejects_zero_seconds() {
        assert_eq!(StoreTimeout::seconds(0), Err(StoreError::MalformedPayload));
        assert_eq!(
            StoreTimeout::seconds(3).expect("timeout").seconds_value(),
            3
        );
    }

    #[test]
    fn transaction_receipt_rejects_statement_errors() {
        let result = super::transaction_receipt(vec![ExecResult::Error {
            error: "no table".to_string(),
        }]);

        assert_eq!(
            result,
            Err(StoreError::Response {
                message: "no table".to_string()
            })
        );
    }

    #[test]
    fn transaction_receipt_sums_rows_affected() {
        let receipt = super::transaction_receipt(vec![
            ExecResult::Execute {
                rows_affected: 1,
                time: 0.1,
            },
            ExecResult::Execute {
                rows_affected: 2,
                time: 0.2,
            },
        ])
        .expect("receipt");

        assert_eq!(receipt.rows_affected(), 3);
    }

    #[test]
    fn sqlite_values_are_hidden_behind_store_values() {
        let row = super::convert_row(vec![
            SqliteValue::Null,
            SqliteValue::Integer(7),
            SqliteValue::Text("hello".into()),
        ]);

        assert_eq!(
            row,
            vec![
                StoreValue::Null,
                StoreValue::Integer(7),
                StoreValue::Text("hello".to_string())
            ]
        );
    }
}
