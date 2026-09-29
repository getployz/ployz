//! The Store's only I/O: plain SQL run inside one transaction per `read` or `write`.
//!
//! Statements are written once, in SQL both adapters run: `?1`-numbered parameters,
//! `TEXT` and `BIGINT` columns, and documents stored as JSON text. SQLite runs them
//! as written; the Postgres adapter rewrites `?N` to `$N`. Adding an adapter means a
//! [`Storage`] variant and a [`Tx`] implementation; the Store's logic never changes.

mod pg;
mod sqlite;

use ployz_core::RpcError;

use crate::error;

/// One migration per ticket, applied once, in order, each as one batch.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_config_store",
        include_str!("migrations/0001_config_store.sql"),
    ),
    (
        "0002_saved_state",
        include_str!("migrations/0002_saved_state.sql"),
    ),
    (
        "0003_deployments",
        include_str!("migrations/0003_deployments.sql"),
    ),
    (
        "0004_registry_credentials",
        include_str!("migrations/0004_registry_credentials.sql"),
    ),
    (
        "0005_uploaded_source",
        include_str!("migrations/0005_uploaded_source.sql"),
    ),
    (
        "0006_cluster_domain",
        include_str!("migrations/0006_cluster_domain.sql"),
    ),
    (
        "0007_git_builds",
        include_str!("migrations/0007_git_builds.sql"),
    ),
    (
        "0008_applied_volumes",
        include_str!("migrations/0008_applied_volumes.sql"),
    ),
    (
        "0009_git_automation",
        include_str!("migrations/0009_git_automation.sql"),
    ),
    (
        "0010_branches",
        include_str!("migrations/0010_branches.sql"),
    ),
];

pub(crate) enum Storage {
    Sqlite(sqlite::Sqlite),
    Postgres(Box<pg::Postgres>),
}

impl Storage {
    /// Open `url` (`postgres://…`, `sqlite:PATH` or `sqlite::memory:`) and apply
    /// pending migrations.
    pub(crate) fn open(url: &str) -> Result<Self, RpcError> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            let storage = Self::Postgres(Box::new(pg::Postgres::open(url)?));
            storage.write(|tx| {
                // Processes opening one database at once migrate it one at a time.
                tx.execute("SELECT pg_advisory_xact_lock(1225)", &[])?;
                migrate(tx)
            })?;
            return Ok(storage);
        }
        let Some(path) = url.strip_prefix("sqlite:") else {
            return Err(error::invalid(
                "Expected a Config Store URL starting with postgres:// or sqlite:",
                serde_json::Value::Null,
            ));
        };
        let storage = Self::Sqlite(sqlite::Sqlite::open(path)?);
        storage.write(migrate)?;
        Ok(storage)
    }

    /// Run `work` in one transaction that commits only when it returns `Ok`.
    /// Writes are serialized: SQLite takes its write lock up front, and writers lock
    /// the rows they change (see `scope::lock`), which is what Postgres waits on.
    pub(crate) fn write<T>(
        &self,
        work: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        match self {
            Self::Sqlite(sqlite) => sqlite.transaction(true, work),
            Self::Postgres(postgres) => postgres.transaction(true, work),
        }
    }

    /// Run `work` in one read transaction, so it sees one consistent state.
    pub(crate) fn read<T>(
        &self,
        work: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        match self {
            Self::Sqlite(sqlite) => sqlite.transaction(false, work),
            Self::Postgres(postgres) => postgres.transaction(false, work),
        }
    }
}

/// A statement runner inside one open transaction.
pub(crate) trait Tx {
    /// Run a statement and return how many rows it changed.
    fn execute(&mut self, sql: &str, params: &[Param<'_>]) -> Result<usize, RpcError>;
    /// Run a query and return every row.
    fn query(&mut self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>, RpcError>;
    /// Run a script of parameterless statements, as the driver splits it.
    fn batch(&mut self, sql: &str) -> Result<(), RpcError>;
}

/// A bound parameter. Only the column types the schema uses exist.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Param<'text> {
    Text(&'text str),
    Int(i64),
}

impl<'text> From<&'text str> for Param<'text> {
    fn from(value: &'text str) -> Self {
        Self::Text(value)
    }
}

impl From<i64> for Param<'_> {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

/// One result cell.
#[derive(Clone, Debug)]
pub(crate) enum Cell {
    Null,
    Int(i64),
    Text(String),
}

/// One result row, read by column index in `SELECT` order.
#[derive(Clone, Debug)]
pub(crate) struct Row(pub(crate) Vec<Cell>);

impl Row {
    pub(crate) fn text(&self, index: usize) -> Result<&str, RpcError> {
        match self.0.get(index) {
            Some(Cell::Text(value)) => Ok(value),
            Some(Cell::Null | Cell::Int(_)) | None => Err(error::corrupt("text column")),
        }
    }

    pub(crate) fn int(&self, index: usize) -> Result<i64, RpcError> {
        match self.0.get(index) {
            Some(Cell::Int(value)) => Ok(*value),
            Some(Cell::Null | Cell::Text(_)) | None => Err(error::corrupt("integer column")),
        }
    }
}

fn migrate(tx: &mut dyn Tx) -> Result<(), RpcError> {
    tx.execute(
        "CREATE TABLE IF NOT EXISTS config_migration (name TEXT PRIMARY KEY)",
        &[],
    )?;
    for (name, sql) in MIGRATIONS {
        let applied = tx.query(
            "SELECT name FROM config_migration WHERE name = ?1",
            &[(*name).into()],
        )?;
        if !applied.is_empty() {
            continue;
        }
        tx.batch(sql)?;
        tx.execute(
            "INSERT INTO config_migration (name) VALUES (?1)",
            &[(*name).into()],
        )?;
    }
    Ok(())
}
