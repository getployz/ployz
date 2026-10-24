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
    (
        "0009_offer_conditional_syncs",
        Step::Rust(|tx, backend| convert::step(tx, backend).map(Some)),
    ),
    (
        "0010_drop_conditional_sync",
        Step::Sql(include_str!("migrations/0010_drop_conditional_sync.sql")),
    ),
    (
        "0011_retire_offers",
        Step::Sql(include_str!("migrations/0011_retire_offers.sql")),
    ),
    (
        "0012_on_merge",
        Step::Sql(include_str!("migrations/0012_on_merge.sql")),
    ),
    (
        "0013_removal_mark",
        Step::Sql(include_str!("migrations/0013_removal_mark.sql")),
    ),
    (
        "0014_merge_removed_services",
        Step::Sql(include_str!("migrations/0014_merge_removed_services.sql")),
    ),
    (
        "0015_sealed_variable",
        Step::Rust(|tx, _| {
            tx.batch(include_str!("migrations/0015_sealed_variable.sql"))?;
            crate::review::sealed::record_working(tx).map(|()| None)
        }),
    ),
    (
        "0016_saved_accepted_losses",
        Step::Sql(include_str!("migrations/0016_saved_accepted_losses.sql")),
    ),
    (
        "0017_pull_request_standing",
        Step::Sql(include_str!("migrations/0017_pull_request_standing.sql")),
    ),
    (
        "0018_deployment_timing",
        Step::Sql(include_str!("migrations/0018_deployment_timing.sql")),
    ),
    (
        "0019_environment_protected",
        Step::Sql(include_str!("migrations/0019_environment_protected.sql")),
    ),
    (
        "0020_environment_removed",
        Step::Sql(include_str!("migrations/0020_environment_removed.sql")),
    ),
    (
        "0021_deployment_waiting_for_ci",
        Step::Rust(|tx, backend| {
            match backend {
                Backend::Postgres => tx.batch(include_str!(
                    "migrations/0021_deployment_waiting_for_ci_postgres.sql"
                ))?,
                Backend::Sqlite => {
                    tx.batch(include_str!(
                        "migrations/0021_deployment_waiting_for_ci_sqlite.sql"
                    ))?;
                    // The rebuild kept every child row's Deployment.
                    if !tx.query("PRAGMA foreign_key_check", &[])?.is_empty() {
                        return Err(error::corrupt("Deployment references"));
                    }
                }
            }
            Ok(None)
        }),
    ),
    (
        "0022_deployment_service_uploads",
        Step::Sql(include_str!(
            "migrations/0022_deployment_service_uploads.sql"
        )),
    ),
    (
        "0023_build_server",
        Step::Sql(include_str!("migrations/0023_build_server.sql")),
    ),
    (
        "0024_deployment_admitted_by_user",
        Step::Sql(include_str!(
            "migrations/0024_deployment_admitted_by_user.sql"
        )),
    ),
    (
        "0025_sealed_variables",
        Step::Sql(include_str!("migrations/0025_sealed_variables.sql")),
    ),
    (
        "0026_saved_value_changes",
        Step::Sql(include_str!("migrations/0026_saved_value_changes.sql")),
    ),
    (
        "0027_drop_sealed_variables",
        Step::Sql(include_str!("migrations/0027_drop_sealed_variables.sql")),
    ),
];

enum Step {
    /// A script of parameterless statements.
    Sql(&'static str),
    /// Rust run inside the migrating transaction, for what SQL alone cannot convert,
    /// with what converting Conditional Syncs did when it is that step.
    Rust(fn(&mut dyn Tx, Backend) -> Result<Option<Converted>, RpcError>),
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
        let sqlite = sqlite::Sqlite::open(path)?;
        // A migration may rebuild a table others reference: dropping the old one must
        // cascade into none of them, and a pragma cannot change mid-transaction.
        sqlite.foreign_keys(false)?;
        let storage = Self::Sqlite(sqlite);
        let converted = storage.write(|tx| migrate(tx, Backend::Sqlite, migrations));
        if let Self::Sqlite(sqlite) = &storage {
            sqlite.foreign_keys(true)?;
        }
        Ok((storage, converted?))
    }

    /// Run `work` in one transaction that commits only when it returns `Ok`.
    /// Writes are serialized: SQLite takes its write lock up front, and writers lock
    /// the rows they change (see `scope::lock`), which is what Postgres waits on.
    pub(crate) fn write<T>(
        &self,
        work: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        match self {
            Self::Sqlite(sqlite) => sqlite
                .transaction(true, work)
                .map_err(|failed| newer_store(sqlite, failed, "write")),
            Self::Postgres(postgres) => postgres.transaction(true, work),
        }
    }

    /// Run `work` in one read transaction, so it sees one consistent state.
    pub(crate) fn read<T>(
        &self,
        work: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        match self {
            Self::Sqlite(sqlite) => sqlite
                .transaction(false, work)
                .map_err(|failed| newer_store(sqlite, failed, "read")),
            Self::Postgres(postgres) => postgres.transaction(false, work),
        }
    }
}

/// The highest published release on either channel: Publish only moves a pointer
/// forward, and a stable Publish moves `beta` too (`core/scripts/promote-release.sh`).
/// So it reaches whichever published Ployz upgraded the Store. The stable line
/// reinstalls this same release when a beta did.
const INSTALL_NEWEST: &str = "curl -fsSL https://ployz.sh | sh -s -- beta";

/// A SQLite schema failure as `unsupported` when `config_migration` names a
/// migration this binary doesn't know: a newer Ployz changed the schema its SQL
/// expects. The failure stays the cause. Every other failure is returned as it was.
/// Not on Postgres, where versions of Cloud share one database through a rolling
/// deploy and a CLI upgrade fixes nothing.
fn newer_store(sqlite: &sqlite::Sqlite, failed: sqlite::Failed, verb: &str) -> RpcError {
    let failed = match failed {
        sqlite::Failed::Schema(error) => error,
        sqlite::Failed::Other(error) => return error,
    };
    let Ok(applied) = sqlite.transaction(false, |tx| {
        tx.query("SELECT name FROM config_migration ORDER BY name", &[])
    }) else {
        return failed;
    };
    let newer = applied
        .iter()
        .filter_map(|row| row.text(0).ok())
        .filter(|name| MIGRATIONS.iter().all(|(known, _)| known != name))
        .collect::<Vec<_>>();
    if newer.is_empty() {
        return failed;
    }
    let mut refused = error::unsupported(
        format!(
            "A newer Ployz upgraded this Config Store, and ployz {} could not {verb} it.",
            env!("CARGO_PKG_VERSION")
        ),
        serde_json::json!({ "next": INSTALL_NEWEST, "migrations": newer }),
    );
    refused.cause = std::iter::once(failed.message)
        .chain(failed.cause)
        .collect();
    refused
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
    /// The evidence of the caller this transaction acts for. The Store's own work acts
    /// for nobody. Read only by [`admit_protect`].
    fn acting_for(&self) -> Option<&crate::Trusted> {
        None
    }
}

/// The one Protected Environment fence: a caller without
/// [`EnvironmentsProtect`](crate::Permission::EnvironmentsProtect) is refused when
/// `touches` says this write reaches into a Protected Environment, and the refusal's
/// details name what it reaches. It runs in the write's own transaction, so a protect
/// that lands first still fences the write.
pub(crate) fn admit_protect(
    tx: &mut dyn Tx,
    touches: impl FnOnce(&mut dyn Tx) -> Result<Option<serde_json::Value>, RpcError>,
) -> Result<(), RpcError> {
    let Some(Err(mut refused)) = tx
        .acting_for()
        .map(|trusted| trusted.admit(crate::Permission::EnvironmentsProtect))
    else {
        return Ok(());
    };
    let Some(reached) = touches(tx)? else {
        return Ok(());
    };
    if let (serde_json::Value::Object(details), serde_json::Value::Object(reached)) =
        (&mut refused.details, reached)
    {
        details.extend(reached);
    }
    Err(refused)
}

/// A transaction acting for the caller `trusted` describes: see [`admit_protect`].
pub(crate) struct Acting<'tx, 'trusted> {
    pub(crate) tx: &'tx mut dyn Tx,
    pub(crate) trusted: &'trusted crate::Trusted,
}

impl Tx for Acting<'_, '_> {
    fn execute(&mut self, sql: &str, params: &[Param<'_>]) -> Result<usize, RpcError> {
        self.tx.execute(sql, params)
    }
    fn query(&mut self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>, RpcError> {
        self.tx.query(sql, params)
    }
    fn batch(&mut self, sql: &str) -> Result<(), RpcError> {
        self.tx.batch(sql)
    }
    fn acting_for(&self) -> Option<&crate::Trusted> {
        Some(self.trusted)
    }
}

/// A bound parameter. Only the column types the schema uses exist.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Param<'text> {
    Text(&'text str),
    Int(i64),
    NullText,
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

    pub(crate) fn optional_text(&self, index: usize) -> Result<Option<&str>, RpcError> {
        match self.0.get(index) {
            Some(Cell::Null) => Ok(None),
            Some(Cell::Text(value)) => Ok(Some(value)),
            Some(Cell::Int(_)) | None => Err(error::corrupt("text column")),
        }
    }

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
            Step::Rust(step) => converted = step(tx, backend)?.or(converted),
        }
        tx.execute(
            "INSERT INTO config_migration (name) VALUES (?1)",
            &[(*name).into()],
        )?;
    }
    Ok(converted)
}

#[cfg(test)]
mod mixed_version_tests {
    use ployz_core::{RpcError, RpcErrorCode};

    use super::{Step, Storage};
    use crate::error;

    /// A newer Ployz's migration that older SQL breaks on, as 0021's `listed` broke
    /// 0.3.0-beta.6's: every insert must now fill `future`.
    const REQUIRED_COLUMN: &[(&str, Step)] = &[(
        "9999_required_column",
        Step::Sql(
            "CREATE TABLE new_config_create (id TEXT PRIMARY KEY, organization_id TEXT NOT NULL, \
             command TEXT NOT NULL, written TEXT NOT NULL, future TEXT NOT NULL); \
             INSERT INTO new_config_create SELECT id, organization_id, command, written, '' \
             FROM config_create; \
             DROP TABLE config_create; \
             ALTER TABLE new_config_create RENAME TO config_create;",
        ),
    )];

    /// A newer Ployz's migration that drops a column older SQL still reads.
    const DROPPED_COLUMN: &[(&str, Step)] = &[(
        "9999_dropped_column",
        Step::Sql("ALTER TABLE config_create DROP COLUMN written;"),
    )];

    /// A newer Ployz's migration that drops a table older SQL still reads.
    const DROPPED_TABLE: &[(&str, Step)] =
        &[("9999_dropped_table", Step::Sql("DROP TABLE config_create;"))];

    /// A Store opened by this Ployz on a database `newer` migrated past it.
    fn older_store(newer: &[(&str, Step)]) -> (tempfile::TempDir, Storage) {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}", dir.path().join("store.db").display());
        drop(Storage::open(&url).unwrap());
        drop(Storage::open_with(&url, newer).unwrap());
        let (older, _) = Storage::open(&url).unwrap();
        (dir, older)
    }

    fn create(storage: &Storage, command: Option<&str>) -> Result<usize, RpcError> {
        storage.write(|tx| {
            tx.execute(
                "INSERT INTO config_create (id, organization_id, command, written) \
                 VALUES (?1, ?2, ?3, ?4)",
                &["c1".into(), "o1".into(), command.into(), "{}".into()],
            )
        })
    }

    const NEXT: &str = "curl -fsSL https://ployz.sh | sh -s -- beta";

    #[test]
    fn a_write_a_newer_migration_broke_says_a_newer_ployz_upgraded_the_store() {
        let (_dir, older) = older_store(REQUIRED_COLUMN);
        let refused = create(&older, Some("project.new")).unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Unsupported);
        assert_eq!(
            refused.message,
            format!(
                "A newer Ployz upgraded this Config Store, and ployz {} could not write it.",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(
            refused.details,
            serde_json::json!({ "migrations": ["9999_required_column"], "next": NEXT })
        );
        assert_eq!(
            refused.cause,
            ["Config Store storage failed: NOT NULL constraint failed: config_create.future"]
        );
    }

    #[test]
    fn a_read_a_newer_migration_broke_says_so_too() {
        let (_dir, older) = older_store(DROPPED_COLUMN);
        let refused = older
            .read(|tx| tx.query("SELECT written FROM config_create", &[]))
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Unsupported);
        assert_eq!(
            refused.message,
            format!(
                "A newer Ployz upgraded this Config Store, and ployz {} could not read it.",
                env!("CARGO_PKG_VERSION")
            )
        );
        assert_eq!(
            refused.details,
            serde_json::json!({ "migrations": ["9999_dropped_column"], "next": NEXT })
        );
        let cause = "Config Store storage failed: no such column: written in \
                     SELECT written FROM config_create at offset 7";
        assert_eq!(refused.cause, [cause]);
    }

    #[test]
    fn a_read_of_a_table_a_newer_migration_dropped_says_so_too() {
        let (_dir, older) = older_store(DROPPED_TABLE);
        let refused = older
            .read(|tx| tx.query("SELECT id FROM config_create", &[]))
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::Unsupported);
        assert_eq!(
            refused.cause,
            ["Config Store storage failed: no such table: config_create"]
        );
    }

    /// A domain refusal is the Store's answer, whatever migrated the database.
    #[test]
    fn a_refusal_on_a_newer_store_stays_the_refusal() {
        let (_dir, older) = older_store(REQUIRED_COLUMN);
        let refused = older
            .write(|_| {
                Err::<(), _>(error::not_found(
                    "No Project named shop",
                    serde_json::Value::Null,
                ))
            })
            .unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::NotFound);
        assert_eq!(refused.message, "No Project named shop");
    }

    /// Failures no migration causes: unreadable rows, a sealed value under another
    /// key, and SQLite failing a statement it accepted.
    #[test]
    fn an_internal_failure_the_schema_did_not_cause_stays_as_it_was_on_a_newer_store() {
        let (_dir, older) = older_store(REQUIRED_COLUMN);
        for made in [
            error::corrupt("Deployment references"),
            error::internal("A sealed secret could not be opened with this Store's key"),
        ] {
            let failed = older.write(|_| Err::<(), _>(made.clone())).unwrap_err();
            assert_eq!(failed, made);
        }
        let failed = older
            .read(|tx| tx.query("SELECT json_extract('not json', '$.a')", &[]))
            .unwrap_err();
        assert_eq!(failed.code, RpcErrorCode::Internal);
        assert_eq!(
            failed.message,
            "Config Store storage failed: malformed JSON"
        );
        assert!(failed.cause.is_empty());
    }

    /// Work that met a schema failure, got past it, and then failed otherwise failed
    /// otherwise.
    #[test]
    fn a_later_failure_after_a_skipped_schema_failure_stays_as_it_was() {
        let (_dir, older) = older_store(REQUIRED_COLUMN);
        let failed = older
            .write(|tx| {
                super::attempt(tx, |tx| {
                    tx.execute(
                        "INSERT INTO config_create (id, organization_id, command, written) \
                         VALUES ('c1', 'o1', 'project.new', '{}')",
                        &[],
                    )
                })
                .unwrap_err();
                Err::<(), _>(error::corrupt("Deployment references"))
            })
            .unwrap_err();
        assert_eq!(failed, error::corrupt("Deployment references"));
    }

    /// Without a migration table there is nothing to say a newer Ployz was here.
    #[test]
    fn a_schema_failure_on_a_store_without_a_migration_table_stays_as_it_was() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}", dir.path().join("store.db").display());
        let (store, _) = Storage::open(&url).unwrap();
        store
            .write(|tx| tx.batch("DROP TABLE config_migration"))
            .unwrap();
        let failed = create(&store, None).unwrap_err();
        assert_eq!(failed.code, RpcErrorCode::Internal);
        assert_eq!(
            failed.message,
            "Config Store storage failed: NOT NULL constraint failed: config_create.command"
        );
        assert!(failed.cause.is_empty());
    }

    #[test]
    fn a_store_no_newer_ployz_migrated_reports_a_constraint_failure_as_before() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}", dir.path().join("store.db").display());
        let (current, _) = Storage::open(&url).unwrap();
        let failed = create(&current, None).unwrap_err();
        assert_eq!(failed.code, RpcErrorCode::Internal);
        assert_eq!(
            failed.message,
            "Config Store storage failed: NOT NULL constraint failed: config_create.command"
        );
        assert!(failed.cause.is_empty());
    }

    /// A Cloud from before 0024 still running beside one that migrated their shared
    /// database: its Store opens it, skipping the migration it doesn't know, and every
    /// Deployment it admits leaves `admitted_by_user` NULL, which reads by name.
    #[test]
    fn a_store_from_before_admitted_by_user_opens_a_database_migrated_past_it() {
        let dir = tempfile::tempdir().unwrap();
        let url = format!("sqlite:{}", dir.path().join("store.db").display());
        drop(Storage::open(&url).unwrap());
        let older = Storage::open_through(&url, "0023_build_server").unwrap();
        let migrated = older
            .read(|tx| {
                tx.query(
                    "SELECT name FROM config_migration WHERE name = ?1",
                    &["0024_deployment_admitted_by_user".into()],
                )
            })
            .unwrap();
        assert_eq!(migrated.len(), 1);
    }
}
