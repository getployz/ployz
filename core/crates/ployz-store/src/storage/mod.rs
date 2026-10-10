//! The Store's only I/O: plain SQL run inside one transaction per `read` or `write`.
//!
//! Statements are written once, in SQL both adapters run: `?1`-numbered parameters,
//! `TEXT` and `BIGINT` columns, and documents stored as JSON text. SQLite runs them
//! as written; the Postgres adapter rewrites `?N` to `$N`. Adding an adapter means a
//! [`Storage`] variant and a [`Tx`] implementation; the Store's logic never changes.

mod convert;
mod pg;
mod sqlite;

pub use convert::Converted;
use ployz_core::RpcError;

use crate::error;

/// Migrations, applied once, in order: each a batch of SQL or a step in Rust.
const MIGRATIONS: &[(&str, Step)] = &[
    (
        "0001_config_store",
        Step::Sql(include_str!("migrations/0001_config_store.sql")),
    ),
    (
        "0002_sync",
        Step::Sql(include_str!("migrations/0002_sync.sql")),
    ),
    (
        "0003_deployment_rows",
        Step::Sql(include_str!("migrations/0003_deployment_rows.sql")),
    ),
    (
        "0004_saved_history",
        Step::Sql(include_str!("migrations/0004_saved_history.sql")),
    ),
    (
        "0005_proposal",
        Step::Sql(include_str!("migrations/0005_proposal.sql")),
    ),
    (
        "0006_sync_receipt",
        Step::Sql(include_str!("migrations/0006_sync_receipt.sql")),
    ),
    (
        "0007_proposal_carried",
        Step::Sql(include_str!("migrations/0007_proposal_carried.sql")),
    ),
    (
        "0008_proposal_offer",
        Step::Sql(include_str!("migrations/0008_proposal_offer.sql")),
    ),
    ("0009_offer_conditional_syncs", Step::Rust(convert::step)),
    (
        "0010_drop_conditional_sync",
        Step::Sql(include_str!("migrations/0010_drop_conditional_sync.sql")),
    ),
];

/// One migration.
enum Step {
    /// A script of parameterless statements.
    Sql(&'static str),
    /// Rust run inside the migrating transaction, for what SQL alone cannot convert.
    Rust(fn(&mut dyn Tx, Backend) -> Result<Converted, RpcError>),
}

/// Which database a migration runs on.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    Sqlite,
    Postgres,
}

pub(crate) enum Storage {
    Sqlite(sqlite::Sqlite),
    Postgres(Box<pg::Postgres>),
}

impl Storage {
    /// Open `url` (`postgres://…`, `sqlite:PATH` or `sqlite::memory:`) and apply
    /// pending migrations, with what converting Conditional Syncs did when this open
    /// was the one that converted them.
    pub(crate) fn open(url: &str) -> Result<(Self, Option<Converted>), RpcError> {
        Self::open_with(url, MIGRATIONS)
    }

    /// Open `url` with only the migrations up to and including `last` applied, as a
    /// Store from before the later ones was.
    #[cfg(test)]
    pub(crate) fn open_through(url: &str, last: &str) -> Result<Self, RpcError> {
        let end = MIGRATIONS
            .iter()
            .position(|(name, _)| *name == last)
            .expect("a known migration");
        Self::open_with(url, MIGRATIONS.get(..=end).expect("a known migration"))
            .map(|(storage, _)| storage)
    }

    fn open_with(
        url: &str,
        migrations: &[(&str, Step)],
    ) -> Result<(Self, Option<Converted>), RpcError> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            let storage = Self::Postgres(Box::new(pg::Postgres::open(url)?));
            let converted = storage.write(|tx| {
                // Processes opening one database at once migrate it one at a time.
                tx.execute("SELECT pg_advisory_xact_lock(1225)", &[])?;
                migrate(tx, Backend::Postgres, migrations)
            })?;
            return Ok((storage, converted));
        }
        let Some(path) = url.strip_prefix("sqlite:") else {
            return Err(error::invalid(
                "Expected a Config Store URL starting with postgres:// or sqlite:",
                serde_json::Value::Null,
            ));
        };
        let storage = Self::Sqlite(sqlite::Sqlite::open(path)?);
        let converted = storage.write(|tx| migrate(tx, Backend::Sqlite, migrations))?;
        Ok((storage, converted))
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

fn migrate(
    tx: &mut dyn Tx,
    backend: Backend,
    migrations: &[(&str, Step)],
) -> Result<Option<Converted>, RpcError> {
    let mut converted = None;
    tx.execute(
        "CREATE TABLE IF NOT EXISTS config_migration (name TEXT PRIMARY KEY)",
        &[],
    )?;
    for (name, step) in migrations {
        let applied = tx.query(
            "SELECT name FROM config_migration WHERE name = ?1",
            &[(*name).into()],
        )?;
        if !applied.is_empty() {
            continue;
        }
        match step {
            Step::Sql(sql) => tx.batch(sql)?,
            Step::Rust(step) => converted = Some(step(tx, backend)?),
        }
        tx.execute(
            "INSERT INTO config_migration (name) VALUES (?1)",
            &[(*name).into()],
        )?;
    }
    Ok(converted)
}
