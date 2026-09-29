//! The SQLite adapter behind the hidden test mode (`PLOYZ_STORE=sqlite:PATH`).

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use ployz_core::RpcError;
use rusqlite::types::{ToSqlOutput, ValueRef};
use rusqlite::{Connection, ErrorCode, ToSql, TransactionBehavior, ffi};

use super::{Cell, Param, Row, Tx};
use crate::error;

/// How long a writer waits for another process's write lock before `unavailable`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) struct Sqlite(Mutex<Connection>);

impl Sqlite {
    pub(super) fn open(path: &str) -> Result<Self, RpcError> {
        let connection = Connection::open(path).map_err(storage_error)?;
        connection
            .busy_timeout(BUSY_TIMEOUT)
            .map_err(storage_error)?;
        connection
            .pragma_update(None, "foreign_keys", true)
            .map_err(storage_error)?;
        Ok(Self(Mutex::new(connection)))
    }

    pub(super) fn transaction<T>(
        &self,
        write: bool,
        work: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        // A panic mid-transaction dropped it, which rolled it back: the connection is sound.
        let mut connection = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let behavior = if write {
            // Take the database write lock before reading, so read-modify-write never races.
            TransactionBehavior::Immediate
        } else {
            TransactionBehavior::Deferred
        };
        let transaction = connection
            .transaction_with_behavior(behavior)
            .map_err(storage_error)?;
        let value = work(&mut SqliteTx(&transaction))?;
        transaction.commit().map_err(storage_error)?;
        Ok(value)
    }
}

struct SqliteTx<'tx>(&'tx rusqlite::Transaction<'tx>);

impl Tx for SqliteTx<'_> {
    fn execute(&mut self, sql: &str, params: &[Param<'_>]) -> Result<usize, RpcError> {
        self.0
            .execute(sql, rusqlite::params_from_iter(params))
            .map_err(storage_error)
    }

    fn query(&mut self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>, RpcError> {
        let mut statement = self.0.prepare(sql).map_err(storage_error)?;
        let columns = statement.column_count();
        statement
            .query_map(rusqlite::params_from_iter(params), |row| {
                (0..columns)
                    .map(|index| {
                        Ok(match row.get_ref(index)? {
                            ValueRef::Null => Cell::Null,
                            ValueRef::Integer(value) => Cell::Int(value),
                            ValueRef::Text(value) => {
                                Cell::Text(String::from_utf8_lossy(value).into_owned())
                            }
                            // The schema has no such columns; reading one reports corruption.
                            ValueRef::Real(_) | ValueRef::Blob(_) => Cell::Null,
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
                    .map(Row)
            })
            .map_err(storage_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(storage_error)
    }

    fn batch(&mut self, sql: &str) -> Result<(), RpcError> {
        self.0.execute_batch(sql).map_err(storage_error)
    }
}

impl ToSql for Param<'_> {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            Self::Text(value) => ToSqlOutput::Borrowed(ValueRef::Text(value.as_bytes())),
            Self::Int(value) => ToSqlOutput::Borrowed(ValueRef::Integer(*value)),
        })
    }
}

/// Busy or locked means another writer held the database past the timeout; nothing
/// was written, but the caller cannot tell that from outside, so it is `unavailable`.
/// A taken key or unique name is `conflict`, whichever check first noticed it.
fn storage_error(source: rusqlite::Error) -> RpcError {
    if let rusqlite::Error::SqliteFailure(failure, _) = &source
        && matches!(
            failure.extended_code,
            ffi::SQLITE_CONSTRAINT_PRIMARYKEY | ffi::SQLITE_CONSTRAINT_UNIQUE
        )
    {
        return error::conflict(
            "An ID or name in this request is already taken",
            serde_json::Value::Null,
        );
    }
    match source.sqlite_error_code() {
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => {
            error::unavailable("The Config Store is busy; retry the command")
        }
        _ => error::internal(format!("Config Store storage failed: {source}")),
    }
}
