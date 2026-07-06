//! One SQLite database for core-local durable control state.
//!
//! `CoreStore` is the single connection owner: operation records and events,
//! namespace intent (routes and serving targets), and the machine table all
//! live here. Machine facts do not — those are live testimony, gathered at the
//! point of use. Callers reach the database only through the store types built
//! on this handle, never with `SELECT` at a call site: one owner per projection
//! keeps the storage swappable behind the read seams.
//!
//! The connection is synchronous `rusqlite` behind a mutex; every access runs
//! on the blocking pool via [`CoreStore::call`], matching how the file stores
//! this replaces already blocked inside an async mutex.

use rusqlite::{Connection, OptionalExtension};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

/// Ordered schema migrations. Each entry is applied once, in order; the applied
/// count is stored in `PRAGMA user_version`. Append new statements, never edit
/// or reorder existing ones.
const MIGRATIONS: &[&str] = &[
    // v1 — the full core schema. Event and status payloads are JSON columns:
    // the operation models stay enum-shaped in Rust and are persisted whole.
    "
    CREATE TABLE operations (
        operation_id TEXT PRIMARY KEY,
        status_json  TEXT NOT NULL
    );
    CREATE TABLE operation_events (
        operation_id TEXT    NOT NULL,
        sequence     INTEGER NOT NULL,
        event_json   TEXT    NOT NULL,
        PRIMARY KEY (operation_id, sequence)
    );
    CREATE TABLE deploy_claims (key TEXT PRIMARY KEY, json TEXT NOT NULL);
    CREATE TABLE machine_add_claims (key TEXT PRIMARY KEY, json TEXT NOT NULL);
    CREATE TABLE machine_add_submissions (key TEXT PRIMARY KEY, json TEXT NOT NULL);
    CREATE TABLE machine_add_secret_deliveries (key TEXT PRIMARY KEY, json TEXT NOT NULL);
    CREATE TABLE machine_add_mint_claims (key TEXT PRIMARY KEY, json TEXT NOT NULL);
    CREATE TABLE machine_add_join_tokens (key TEXT PRIMARY KEY, json TEXT NOT NULL);
    CREATE TABLE route_bindings (
        hostname TEXT    NOT NULL,
        port     INTEGER NOT NULL,
        json     TEXT    NOT NULL,
        PRIMARY KEY (hostname, port)
    );
    CREATE TABLE serving_targets (
        namespace_id TEXT NOT NULL,
        service_id   TEXT NOT NULL,
        json         TEXT NOT NULL,
        PRIMARY KEY (namespace_id, service_id)
    );
    CREATE TABLE machines (
        machine_id TEXT PRIMARY KEY,
        json       TEXT NOT NULL
    );
    ",
];

/// A cloneable handle to the core database. Clones share one connection and one
/// lock, so writes serialize the same way the file stores' mutex did.
#[derive(Clone)]
pub struct CoreStore {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for CoreStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CoreStore")
    }
}

impl CoreStore {
    /// Open (creating if absent) the database at `path`, enable WAL durability,
    /// and apply any pending migrations.
    pub async fn open(path: PathBuf) -> Result<Self, CoreStoreError> {
        Self::open_blocking(move || {
            let conn = Connection::open(&path).map_err(CoreStoreError::Open)?;
            // WAL + NORMAL: crash-atomic commits without an fsync per statement,
            // the durability the tmpfile+rename file stores gave. journal_mode
            // must be set outside a transaction, so this runs before migrate.
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
                .map_err(CoreStoreError::Open)?;
            Ok(conn)
        })
        .await
    }

    /// Open a private in-memory database with the schema applied. The database
    /// lives only as long as this handle and its clones (they share the one
    /// connection), which is exactly what a test wants.
    pub async fn open_in_memory() -> Result<Self, CoreStoreError> {
        Self::open_blocking(|| Connection::open_in_memory().map_err(CoreStoreError::Open)).await
    }

    /// Run `open_connection` on the blocking pool, then apply foreign-key
    /// enforcement, migrations, and wrap the connection in the shared handle.
    async fn open_blocking(
        open_connection: impl FnOnce() -> Result<Connection, CoreStoreError> + Send + 'static,
    ) -> Result<Self, CoreStoreError> {
        let conn = tokio::task::spawn_blocking(move || {
            let mut conn = open_connection()?;
            conn.execute_batch("PRAGMA foreign_keys = ON;")
                .map_err(CoreStoreError::Open)?;
            migrate(&mut conn).map_err(CoreStoreError::Migrate)?;
            Ok::<_, CoreStoreError>(conn)
        })
        .await
        .map_err(CoreStoreError::Join)??;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Run `work` against the connection on the blocking pool. The closure owns
    /// the connection for its duration, so a multi-statement transaction is one
    /// `call`.
    pub async fn call<T, F>(&self, work: F) -> Result<T, CoreStoreError>
    where
        F: FnOnce(&mut Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().unwrap_or_else(PoisonError::into_inner);
            work(&mut guard).map_err(CoreStoreError::Sqlite)
        })
        .await
        .map_err(CoreStoreError::Join)?
    }
}

fn migrate(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    let applied: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let applied = usize::try_from(applied).unwrap_or(0);
    let transaction = conn.transaction()?;
    for migration in MIGRATIONS.iter().skip(applied) {
        transaction.execute_batch(migration)?;
    }
    // PRAGMA user_version takes no bind parameters; the count is our own usize.
    transaction.execute_batch(&format!("PRAGMA user_version = {}", MIGRATIONS.len()))?;
    transaction.commit()
}

#[derive(Debug)]
pub enum CoreStoreError {
    Open(rusqlite::Error),
    Migrate(rusqlite::Error),
    Sqlite(rusqlite::Error),
    Join(tokio::task::JoinError),
}

impl std::fmt::Display for CoreStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(error) => write!(formatter, "open core database: {error}"),
            Self::Migrate(error) => write!(formatter, "migrate core database: {error}"),
            Self::Sqlite(error) => write!(formatter, "core database query: {error}"),
            Self::Join(error) => write!(formatter, "core database task: {error}"),
        }
    }
}

impl std::error::Error for CoreStoreError {}

/// Serialize a value for a JSON text column. A failure here is a programming
/// error (our own types), surfaced as a rusqlite conversion failure so it
/// travels the same channel as any other statement error.
pub(crate) fn to_json<V: serde::Serialize>(value: &V) -> Result<String, rusqlite::Error> {
    serde_json::to_string(value)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))
}

/// Deserialize a value from a JSON text column.
pub(crate) fn from_json<V: serde::de::DeserializeOwned>(json: &str) -> Result<V, rusqlite::Error> {
    serde_json::from_str(json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            json.len(),
            rusqlite::types::Type::Text,
            Box::new(error),
        )
    })
}

/// Run a single-row query whose first column is a JSON blob, bound to one key,
/// and decode it. `None` when no row matches.
pub(crate) fn query_json<V: serde::de::DeserializeOwned>(
    conn: &Connection,
    sql: &str,
    key: &str,
) -> Result<Option<V>, rusqlite::Error> {
    conn.query_row(sql, [key], |row| row.get::<_, String>(0))
        .optional()?
        .map(|json| from_json(&json))
        .transpose()
}

/// Run a query whose first column is a JSON blob and decode every row. `sql`
/// selects that one `json` column (with whatever ordering the caller wants).
pub(crate) fn query_json_list<V: serde::de::DeserializeOwned>(
    conn: &Connection,
    sql: &str,
) -> Result<Vec<V>, rusqlite::Error> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut values = Vec::new();
    for row in rows {
        values.push(from_json(&row?)?);
    }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn open_creates_schema_and_is_idempotent() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("ployz-core.db");

        let store = CoreStore::open(path.clone()).await.expect("first open");
        let tables: Vec<String> = store
            .call(|conn| {
                let mut statement = conn.prepare(
                    "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
                )?;
                let names = statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(names)
            })
            .await
            .expect("read tables");

        for expected in [
            "machines",
            "operation_events",
            "operations",
            "route_bindings",
            "serving_targets",
        ] {
            assert!(tables.contains(&expected.to_owned()), "missing {expected}");
        }

        // Reopening applies no migration twice and leaves user_version pinned.
        drop(store);
        let reopened = CoreStore::open(path).await.expect("reopen");
        let version: i64 = reopened
            .call(|conn| conn.query_row("PRAGMA user_version", [], |row| row.get(0)))
            .await
            .expect("read user_version");
        assert_eq!(version, MIGRATIONS.len() as i64);
    }
}
