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

use ployz_core::state::ControlPlaneEpoch;
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
    "
    ALTER TABLE operation_events ADD COLUMN subject TEXT;
    CREATE UNIQUE INDEX operation_events_subject
        ON operation_events(operation_id, subject)
        WHERE subject IS NOT NULL;
    ",
    "
    CREATE TABLE control_plane (
        id   INTEGER PRIMARY KEY CHECK (id = 0),
        json TEXT NOT NULL
    );
    ",
    // Authorized-users grants: the durable source of truth that
    // `authorized-users.conf` is now a rendered projection of. One row per grant,
    // keyed by its `authority_record_key` so operator and Cloud Operator grants coexist.
    "
    CREATE TABLE nats_authorizations (
        authority_key TEXT PRIMARY KEY,
        json          TEXT NOT NULL
    );
    ",
    "
    CREATE TABLE volume_pins (
        namespace_id TEXT NOT NULL,
        volume_name  TEXT NOT NULL,
        json         TEXT NOT NULL,
        PRIMARY KEY (namespace_id, volume_name)
    );
    ",
    "
    CREATE TABLE managed_lease_intent (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT NOT NULL
    );
    ",
    "
    DELETE FROM deploy_claims;
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
    ",
    "
    CREATE TABLE custom_certificate_intent (
        hostname TEXT PRIMARY KEY,
        json     TEXT NOT NULL
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
    ",
    // Version 8 existed with either the certificate tables or the managed-lease
    // address table, so this entry reconciles both lineages before advancing.
    "
    CREATE TABLE IF NOT EXISTS custom_certificate_intent (
        hostname TEXT PRIMARY KEY,
        json     TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS acme_http01_challenges (
        hostname TEXT NOT NULL,
        token    TEXT NOT NULL,
        json     TEXT NOT NULL,
        PRIMARY KEY (hostname, token)
    );
    CREATE TABLE IF NOT EXISTS acme_accounts (
        directory_url    TEXT PRIMARY KEY,
        credentials_json TEXT NOT NULL
    );
    CREATE TABLE IF NOT EXISTS managed_lease_applied_addresses (
        id   INTEGER PRIMARY KEY CHECK (id = 1),
        json TEXT NOT NULL
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
            restrict_core_store_permissions(&path)?;
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

#[derive(Debug, thiserror::Error)]
pub enum CoreStoreError {
    #[error("open core database: {0}")]
    Open(rusqlite::Error),
    #[error("migrate core database: {0}")]
    Migrate(rusqlite::Error),
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
                let mut statement = conn
                    .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")?;
                let names = statement
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(names)
            })
            .await
            .expect("read tables");

        for expected in [
            "control_plane",
            "custom_certificate_intent",
            "acme_accounts",
            "acme_http01_challenges",
            "machines",
            "managed_lease_intent",
            "managed_lease_applied_addresses",
            "operation_events",
            "operations",
            "deploy_reservations",
            "route_bindings",
            "serving_targets",
            "volume_pins",
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

    #[test]
    fn version_eight_lineages_reconcile_to_the_current_schema() {
        let Some(certificate_schema) = MIGRATIONS.get(7) else {
            panic!("missing version eight certificate migration");
        };
        for schema in [
            "
            CREATE TABLE managed_lease_applied_addresses (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                json TEXT NOT NULL
            );
            ",
            *certificate_schema,
        ] {
            let mut conn = Connection::open_in_memory().expect("open version eight database");
            for migration in MIGRATIONS.iter().take(7) {
                conn.execute_batch(migration).expect("seed shared schema");
            }
            conn.execute_batch(schema)
                .expect("seed version eight schema");
            conn.pragma_update(None, "user_version", 8)
                .expect("stamp version eight schema");
            migrate(&mut conn).expect("migrate lineage");

            let table_count: usize = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN (
                        'custom_certificate_intent',
                        'acme_http01_challenges',
                        'acme_accounts',
                        'managed_lease_applied_addresses',
                        'operations',
                        'machines',
                        'managed_lease_intent'
                    )",
                    [],
                    |row| row.get(0),
                )
                .expect("read reconciled schema");
            assert_eq!(table_count, 7);
        }
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
