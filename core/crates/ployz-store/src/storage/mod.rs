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

/// Migrations, applied once, in order, each as one batch.
const MIGRATIONS: &[(&str, &str)] = &[
    (
        "0001_config_store",
        include_str!("migrations/0001_config_store.sql"),
    ),
    ("0002_sync", include_str!("migrations/0002_sync.sql")),
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

/// Run `work` inside a savepoint: when it fails, everything it wrote is undone and
/// the rest of the transaction goes on. An attempt that may be skipped runs here, so
/// skipping it leaves no rows behind.
pub(crate) fn attempt<T>(
    tx: &mut dyn Tx,
    work: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
) -> Result<T, RpcError> {
    tx.execute("SAVEPOINT attempt", &[])?;
    let result = work(tx);
    if result.is_err() {
        tx.execute("ROLLBACK TO SAVEPOINT attempt", &[])?;
    }
    tx.execute("RELEASE SAVEPOINT attempt", &[])?;
    result
}

/// A unit enum variant as the Store stores it: its serde name, so SQL and JSON
/// always agree.
pub(crate) fn name_of(variant: impl serde::Serialize) -> String {
    serde_json::to_value(variant)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .expect("a unit variant serializes as its name")
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
    /// NULL in a text column.
    NullText,
    /// NULL in an integer column.
    NullInt,
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

impl<'text> From<Option<&'text str>> for Param<'text> {
    fn from(value: Option<&'text str>) -> Self {
        value.map_or(Self::NullText, Self::Text)
    }
}

impl From<Option<i64>> for Param<'_> {
    fn from(value: Option<i64>) -> Self {
        value.map_or(Self::NullInt, Self::Int)
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

    /// A text column as `T`; one it can't be is `what`, corrupt.
    pub(crate) fn parse<T: TryFrom<String>>(
        &self,
        index: usize,
        what: &str,
    ) -> Result<T, RpcError> {
        T::try_from(self.text(index)?.to_owned()).map_err(|_| error::corrupt(what))
    }

    /// A text column that may be NULL, as `T`; text it can't be is `what`, corrupt.
    pub(crate) fn parse_optional<T: TryFrom<String>>(
        &self,
        index: usize,
        what: &str,
    ) -> Result<Option<T>, RpcError> {
        self.optional_text(index)?
            .map(|text| T::try_from(text.to_owned()).map_err(|_| error::corrupt(what)))
            .transpose()
    }

    /// An integer column as `T`; one it can't be is `what`, corrupt.
    pub(crate) fn number<T: TryFrom<u64>>(&self, index: usize, what: &str) -> Result<T, RpcError> {
        u64::try_from(self.int(index)?)
            .ok()
            .and_then(|value| T::try_from(value).ok())
            .ok_or_else(|| error::corrupt(what))
    }

    /// A text column as the unit enum variant of that serde name; anything else is
    /// `what`, corrupt.
    pub(crate) fn variant<T: serde::de::DeserializeOwned>(
        &self,
        index: usize,
        what: &str,
    ) -> Result<T, RpcError> {
        serde_json::from_value(serde_json::Value::String(self.text(index)?.to_owned()))
            .map_err(|_| error::corrupt(what))
    }

    /// A JSON document column as `T`; one it can't be is `what`, corrupt.
    pub(crate) fn json<T: serde::de::DeserializeOwned>(
        &self,
        index: usize,
        what: &str,
    ) -> Result<T, RpcError> {
        serde_json::from_str(self.text(index)?).map_err(|_| error::corrupt(what))
    }

    /// An Environment document column, validated whole; one that isn't is `what`,
    /// corrupt.
    pub(crate) fn intent(
        &self,
        index: usize,
        what: &str,
    ) -> Result<ployz_core::config::SavedEnvironmentIntent, RpcError> {
        ployz_core::config::parse_environment_intent(self.json(index, what)?)
            .map_err(|_| error::corrupt(what))
    }

    /// A text column that may be NULL.
    pub(crate) fn optional_text(&self, index: usize) -> Result<Option<&str>, RpcError> {
        match self.0.get(index) {
            Some(Cell::Null) => Ok(None),
            Some(Cell::Text(value)) => Ok(Some(value)),
            Some(Cell::Int(_)) | None => Err(error::corrupt("text column")),
        }
    }

    /// An integer column that may be NULL.
    pub(crate) fn optional_int(&self, index: usize) -> Result<Option<i64>, RpcError> {
        match self.0.get(index) {
            Some(Cell::Null) => Ok(None),
            Some(Cell::Int(value)) => Ok(Some(*value)),
            Some(Cell::Text(_)) | None => Err(error::corrupt("integer column")),
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
