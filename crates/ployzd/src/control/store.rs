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

use ployz_core::intent::recovery::ControlPlaneEpoch;
use rusqlite::{Connection, OptionalExtension};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

const CURRENT_SCHEMA_VERSION: i64 = 14;

const CURRENT_SCHEMA: &str = "
    CREATE TABLE operations (
        created_order INTEGER PRIMARY KEY AUTOINCREMENT,
        operation_id  TEXT NOT NULL UNIQUE,
        status_json   TEXT NOT NULL
    );
    CREATE TABLE operation_events (
        operation_id TEXT    NOT NULL,
        sequence     INTEGER NOT NULL,
        event_json   TEXT    NOT NULL,
        subject      TEXT,
        PRIMARY KEY (operation_id, sequence)
    );
    CREATE UNIQUE INDEX operation_events_subject
        ON operation_events(operation_id, subject)
        WHERE subject IS NOT NULL;
    CREATE TABLE machine_add_idempotency (
        idempotency_key TEXT PRIMARY KEY,
        operation_id    TEXT NOT NULL UNIQUE
    );
    CREATE TABLE machine_add_claims (
        idempotency_key TEXT PRIMARY KEY,
        operation_id    TEXT NOT NULL UNIQUE,
        machine_id      TEXT NOT NULL,
        raw_join_token  TEXT NOT NULL,
        claim_json      TEXT NOT NULL
    );
    CREATE TABLE machine_add_submissions (
        idempotency_key TEXT PRIMARY KEY,
        operation_id    TEXT NOT NULL UNIQUE,
        machine_id      TEXT NOT NULL,
        raw_join_token  TEXT NOT NULL,
        submission_json TEXT NOT NULL
    );
    CREATE TABLE machine_add_join_tokens (
        fingerprint     TEXT PRIMARY KEY,
        operation_id    TEXT NOT NULL,
        idempotency_key TEXT NOT NULL UNIQUE
    );
    CREATE TABLE machine_add_secret_deliveries (
        idempotency_key      TEXT PRIMARY KEY,
        operation_id         TEXT NOT NULL,
        secret_delivery_json TEXT NOT NULL
    );
    CREATE TABLE machine_add_mint_claims (
        idempotency_key TEXT PRIMARY KEY,
        operation_id    TEXT NOT NULL,
        nkey_public     TEXT NOT NULL,
        mint_claim_json TEXT NOT NULL
    );
    CREATE TABLE route_bindings (
        hostname         TEXT PRIMARY KEY,
        route_binding_id TEXT NOT NULL UNIQUE,
        json             TEXT NOT NULL
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
    CREATE TABLE control_plane (
        id   INTEGER PRIMARY KEY CHECK (id = 0),
        json TEXT NOT NULL
    );
    CREATE TABLE nats_authorizations (
        authority_key TEXT PRIMARY KEY,
        json          TEXT NOT NULL
    );
    CREATE TABLE volume_pins (
        namespace_id TEXT NOT NULL,
        volume_name  TEXT NOT NULL,
        json         TEXT NOT NULL,
        PRIMARY KEY (namespace_id, volume_name)
    );
    CREATE TABLE deploy_claims (
        key  TEXT PRIMARY KEY,
        json TEXT NOT NULL
    );
    CREATE TABLE deploy_reservations (
        namespace_id                  TEXT PRIMARY KEY,
        last_issued                   TEXT NOT NULL,
        last_committed                TEXT,
        committed_owner_operation_id  TEXT,
        CHECK (
            (last_committed IS NULL AND committed_owner_operation_id IS NULL)
            OR (last_committed IS NOT NULL AND committed_owner_operation_id IS NOT NULL)
        )
    );
    CREATE TABLE acme_http01_challenges (
        hostname TEXT NOT NULL,
        token    TEXT NOT NULL,
        json     TEXT NOT NULL,
        PRIMARY KEY (hostname, token)
    );
    CREATE TABLE acme_accounts (
        directory_url    TEXT PRIMARY KEY,
        credentials_json TEXT NOT NULL
    );
    CREATE TABLE machine_dataplane_staging (
        id           INTEGER PRIMARY KEY CHECK (id = 1),
        operation_id TEXT NOT NULL UNIQUE,
        machine_id   TEXT NOT NULL UNIQUE,
        json         TEXT NOT NULL
    );
    CREATE TABLE automatic_hostname_intent (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT NOT NULL
    );
    CREATE TABLE ployz_dns_target_intent (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT NOT NULL
    );
    CREATE TABLE ployz_dns_target_allocation (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT NOT NULL
    );
    CREATE TABLE managed_dns_checkpoint (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT NOT NULL
    );
    CREATE TABLE ingress_endpoint_projection (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT NOT NULL
    );
    CREATE TABLE active_certificate_metadata (
        owner_key TEXT PRIMARY KEY,
        json      TEXT NOT NULL
    );
    ";

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
    /// and verify or initialize the current schema.
    pub async fn open(path: PathBuf) -> Result<Self, CoreStoreError> {
        Self::open_blocking(move || {
            let conn = Connection::open(&path).map_err(CoreStoreError::Open)?;
            restrict_core_store_permissions(&path)?;
            // WAL + NORMAL: crash-atomic commits without an fsync per statement,
            // the durability the tmpfile+rename file stores gave. journal_mode
            // must be set outside a transaction, so this runs before schema setup.
            conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL;")
                .map_err(CoreStoreError::Open)?;
            Ok(conn)
        })
        .await
    }

    /// Open a private in-memory database with the schema applied. The database
    /// lives only as long as this handle and its clones (they share the one
    /// connection), which is exactly what a test wants.
    #[cfg(test)]
    pub async fn open_in_memory() -> Result<Self, CoreStoreError> {
        Self::open_blocking(|| Connection::open_in_memory().map_err(CoreStoreError::Open)).await
    }

    /// Run `open_connection` on the blocking pool, then apply foreign-key
    /// enforcement, initialize or verify the schema, and wrap the connection.
    async fn open_blocking(
        open_connection: impl FnOnce() -> Result<Connection, CoreStoreError> + Send + 'static,
    ) -> Result<Self, CoreStoreError> {
        let conn = tokio::task::spawn_blocking(move || {
            let mut conn = open_connection()?;
            conn.execute_batch("PRAGMA foreign_keys = ON;")
                .map_err(CoreStoreError::Open)?;
            initialize_schema(&mut conn).map_err(CoreStoreError::Schema)?;
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

    /// The core's control-plane epoch, minting the initial epoch on first read.
    /// Advertised with intent so a machine can fence a stale core; promotion
    /// bumps it (ADR 0031).
    pub async fn control_plane_epoch(&self) -> Result<ControlPlaneEpoch, CoreStoreError> {
        self.call(|conn| {
            let existing: Option<String> = conn
                .query_row("SELECT json FROM control_plane WHERE id = 0", [], |row| {
                    row.get(0)
                })
                .optional()?;
            match existing {
                Some(json) => from_json(&json),
                None => {
                    let epoch = ControlPlaneEpoch::initial();
                    conn.execute(
                        "INSERT INTO control_plane (id, json) VALUES (0, ?1)",
                        [to_json(&epoch)?],
                    )?;
                    Ok(epoch)
                }
            }
        })
        .await
    }

    /// The epoch if the store already has one, **without** minting. `None` means a
    /// fresh store that has never served as a core — the only state a promotion
    /// may seed. (Distinct from `control_plane_epoch`, which mints `initial` and so
    /// cannot tell a fresh store from one promoted at the initial generation.)
    pub async fn control_plane_epoch_if_present(
        &self,
    ) -> Result<Option<ControlPlaneEpoch>, CoreStoreError> {
        self.call(|conn| {
            let existing: Option<String> = conn
                .query_row("SELECT json FROM control_plane WHERE id = 0", [], |row| {
                    row.get(0)
                })
                .optional()?;
            existing.map(|json| from_json(&json)).transpose()
        })
        .await
    }

    /// Atomically raise the control-plane epoch above both `mirror` and this
    /// store's current value, returning the new epoch. Promotion fences the core
    /// it succeeds with this — the read-max-bump-write happens in one transaction,
    /// so there is no interleaving window, and re-running only ever advances.
    pub async fn fence_control_plane_epoch_above(
        &self,
        mirror: ControlPlaneEpoch,
    ) -> Result<ControlPlaneEpoch, CoreStoreError> {
        self.call(move |conn| {
            let existing: Option<String> = conn
                .query_row("SELECT json FROM control_plane WHERE id = 0", [], |row| {
                    row.get(0)
                })
                .optional()?;
            let current = match existing {
                Some(json) => from_json(&json)?,
                None => ControlPlaneEpoch::initial(),
            };
            let bumped = mirror.max(current).next();
            conn.execute(
                "INSERT INTO control_plane (id, json) VALUES (0, ?1)
                 ON CONFLICT(id) DO UPDATE SET json = excluded.json",
                [to_json(&bumped)?],
            )?;
            Ok(bumped)
        })
        .await
    }
}

#[cfg(unix)]
fn restrict_core_store_permissions(path: &std::path::Path) -> Result<(), CoreStoreError> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|source| {
        CoreStoreError::FilePermissions {
            path: path.to_path_buf(),
            source,
        }
    })
}

#[cfg(not(unix))]
fn restrict_core_store_permissions(_path: &std::path::Path) -> Result<(), CoreStoreError> {
    Ok(())
}

fn initialize_schema(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    let applied: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if applied == CURRENT_SCHEMA_VERSION {
        return Ok(());
    }
    if applied != 0 {
        return Err(incompatible_schema(applied));
    }
    let existing_tables: u64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master
         WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    if existing_tables != 0 {
        return Err(incompatible_schema(applied));
    }
    let transaction = conn.transaction()?;
    transaction.execute_batch(CURRENT_SCHEMA)?;
    transaction.execute_batch(&format!("PRAGMA user_version = {CURRENT_SCHEMA_VERSION}"))?;
    transaction.commit()
}

fn incompatible_schema(actual: i64) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(IncompatibleSchemaVersion {
        actual,
        expected: CURRENT_SCHEMA_VERSION,
    }))
}

#[derive(Debug, thiserror::Error)]
#[error("incompatible core schema version {actual}; expected {expected} or an empty database")]
struct IncompatibleSchemaVersion {
    actual: i64,
    expected: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum CoreStoreError {
    #[error("open core database: {0}")]
    Open(rusqlite::Error),
    #[error("initialize core database schema: {0}")]
    Schema(rusqlite::Error),
    #[error("core database query: {0}")]
    Sqlite(rusqlite::Error),
    #[error("core database task: {0}")]
    Join(tokio::task::JoinError),
    #[error("restrict core database permissions at {}: {source}", path.display())]
    FilePermissions {
        path: PathBuf,
        source: std::io::Error,
    },
}

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
                    "SELECT name FROM sqlite_master
                     WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
                     ORDER BY name",
                )?;
                let names = statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(names)
            })
            .await
            .expect("read tables");

        assert_eq!(
            tables,
            [
                "acme_accounts",
                "acme_http01_challenges",
                "active_certificate_metadata",
                "automatic_hostname_intent",
                "control_plane",
                "deploy_claims",
                "deploy_reservations",
                "ingress_endpoint_projection",
                "machine_add_claims",
                "machine_add_idempotency",
                "machine_add_join_tokens",
                "machine_add_mint_claims",
                "machine_add_secret_deliveries",
                "machine_add_submissions",
                "machine_dataplane_staging",
                "machines",
                "managed_dns_checkpoint",
                "nats_authorizations",
                "operation_events",
                "operations",
                "ployz_dns_target_allocation",
                "ployz_dns_target_intent",
                "route_bindings",
                "serving_targets",
                "volume_pins",
            ]
            .map(str::to_owned)
        );

        // Reopening accepts the current schema without rewriting it.
        drop(store);
        let reopened = CoreStore::open(path).await.expect("reopen");
        let version: i64 = reopened
            .call(|conn| conn.query_row("PRAGMA user_version", [], |row| row.get(0)))
            .await
            .expect("read user_version");
        assert_eq!(version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn previous_schema_is_rejected_without_mutation() {
        let mut conn = Connection::open_in_memory().expect("open previous database");
        conn.execute_batch(
            "CREATE TABLE previous_schema (
                id INTEGER PRIMARY KEY,
                value TEXT NOT NULL
            );
            INSERT INTO previous_schema (id, value) VALUES (1, 'untouched');
            PRAGMA user_version = 12;",
        )
        .expect("seed previous schema");

        let error = initialize_schema(&mut conn).expect_err("previous schema is incompatible");
        let stored: String = conn
            .query_row(
                "SELECT value FROM previous_schema WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .expect("previous row remains untouched");

        assert!(
            error
                .to_string()
                .contains("incompatible core schema version 12")
                && stored == "untouched"
        );
    }

    #[test]
    fn unversioned_nonempty_database_is_rejected() {
        let mut conn = Connection::open_in_memory().expect("open unversioned database");
        conn.execute("CREATE TABLE unknown_state (json TEXT NOT NULL)", [])
            .expect("seed unknown schema");

        let error = initialize_schema(&mut conn).expect_err("nonempty database is incompatible");

        assert!(
            error
                .to_string()
                .contains("incompatible core schema version 0")
        );
    }

    #[tokio::test]
    async fn control_plane_epoch_mints_initial_and_persists() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("ployz-core.db");

        let store = CoreStore::open(path.clone()).await.expect("first open");
        assert_eq!(
            store.control_plane_epoch().await.expect("mint epoch"),
            ControlPlaneEpoch::initial()
        );
        // Reading again reads the existing row rather than re-minting.
        assert_eq!(
            store.control_plane_epoch().await.expect("read epoch"),
            ControlPlaneEpoch::initial()
        );

        // The epoch survives a core restart — this is what lets it fence a
        // stale core after promotion.
        drop(store);
        let reopened = CoreStore::open(path).await.expect("reopen");
        assert_eq!(
            reopened.control_plane_epoch().await.expect("read epoch"),
            ControlPlaneEpoch::initial()
        );
    }

    #[tokio::test]
    async fn deploy_reservation_commit_requires_an_operation_owner() {
        let store = CoreStore::open_in_memory().await.expect("open store");

        let error = store
            .call(|conn| {
                conn.execute(
                    "INSERT INTO deploy_reservations
                     (namespace_id, last_issued, last_committed)
                     VALUES ('default', '1', '1')",
                    [],
                )
            })
            .await
            .expect_err("committed reservation without owner is rejected");

        assert!(matches!(error, CoreStoreError::Sqlite(_)));
    }
}
