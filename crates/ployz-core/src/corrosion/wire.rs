//! Transport-neutral shapes for Corrosion's v1 public HTTP API.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Corrosion v1.0.0's exact cold-start health error.
pub const CORROSION_NO_P99_LAG_SAMPLE: &str = "no p99 lag information available";

/// The exact body returned by Corrosion v1.0.0's `/v1/health` endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CorrosionHealthResponse {
    Response {
        gaps: i64,
        members: i64,
        p99_lag: f64,
        queue_size: u64,
    },
    Error(String),
}

/// A value accepted as a positional SQLite statement parameter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SqliteParameter {
    Null,
    Bool(bool),
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// A SQLite value returned in a query or subscription row.
///
/// SQLite has no boolean storage class, so boolean parameters return as
/// integers when selected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SqliteValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// A SQL statement in the two v1 forms used by Ployz.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Statement {
    Simple(String),
    WithParams(String, Vec<SqliteParameter>),
}

impl Statement {
    #[must_use]
    pub fn simple(query: impl Into<String>) -> Self {
        Self::Simple(query.into())
    }

    #[must_use]
    pub fn with_params(query: impl Into<String>, params: Vec<SqliteParameter>) -> Self {
        Self::WithParams(query.into(), params)
    }
}

/// The response body returned by `/v1/transactions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransactionResponse {
    pub results: Vec<TransactionResult>,
    pub time: f64,
    #[serde(default)]
    pub version: Option<u64>,
    #[serde(default)]
    pub actor_id: Option<String>,
}

/// The result of one statement in a transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TransactionResult {
    Success(TransactionSuccess),
    Error(TransactionError),
}

/// A successfully executed statement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionSuccess {
    pub rows_affected: usize,
    pub time: f64,
}

/// A statement rejected by Corrosion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransactionError {
    #[serde(rename = "error")]
    pub message: String,
}

/// The identity Corrosion assigns to a query or subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryIdentity {
    id: Uuid,
    hash: QueryHash,
}

impl QueryIdentity {
    pub fn from_parts(id: Option<&str>, hash: Option<&str>) -> Result<Self, QueryIdentityError> {
        let Some(id) = id else {
            return Err(QueryIdentityError::MissingId);
        };
        let id = Uuid::parse_str(id).map_err(|_| QueryIdentityError::InvalidId)?;
        let Some(hash) = hash else {
            return Err(QueryIdentityError::MissingHash);
        };

        Ok(Self {
            id,
            hash: QueryHash::new(hash)?,
        })
    }

    #[must_use]
    pub const fn id(&self) -> Uuid {
        self.id
    }

    #[must_use]
    pub fn hash(&self) -> &str {
        self.hash.as_str()
    }
}

/// A validated Corrosion query hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueryHash(String);

impl QueryHash {
    pub fn new(value: impl Into<String>) -> Result<Self, QueryIdentityError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(QueryIdentityError::EmptyHash);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Invalid or missing Corrosion query identity data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum QueryIdentityError {
    #[error("Corrosion response omitted the query id")]
    MissingId,
    #[error("Corrosion response carried an invalid query id")]
    InvalidId,
    #[error("Corrosion response omitted the query hash")]
    MissingHash,
    #[error("Corrosion response carried an empty query hash")]
    EmptyHash,
}

/// A row identifier scoped to one Corrosion query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RowId(u64);

impl RowId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// A monotonically increasing change identifier scoped to one subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ChangeId(u64);

impl ChangeId {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// A change to the result set of a subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Insert,
    Update,
    Delete,
}

/// Metadata marking the end of an initial query snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EndOfQuery {
    pub time: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change_id: Option<ChangeId>,
}

/// One NDJSON frame returned by `/v1/queries`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryEvent {
    Columns(Vec<String>),
    Row(RowId, Vec<SqliteValue>),
    #[serde(rename = "eoq")]
    EndOfQuery(EndOfQuery),
    Error(String),
}

/// One NDJSON frame returned by `/v1/subscriptions`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubscriptionEvent {
    Columns(Vec<String>),
    Row(RowId, Vec<SqliteValue>),
    #[serde(rename = "eoq")]
    EndOfQuery(EndOfQuery),
    Change(ChangeKind, RowId, Vec<SqliteValue>, ChangeId),
    Error(String),
}
