//! One Postgres connection, used the way PgBouncer's transaction pooling allows: every statement
//! runs inside a short transaction, timeouts are `SET LOCAL`, and only unnamed statements
//! (`query_typed`) are sent, so a pooled backend never has to remember anything for us.

use std::sync::Arc;
use std::time::Duration;

use rustls::crypto::CryptoProvider;
use tokio::sync::Mutex;
use tokio_postgres::error::SqlState;
use tokio_postgres::{Client, Config, Row, Transaction};
use tokio_postgres_rustls::MakeRustlsConnect;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One connection, reconnected lazily by the next transaction after it breaks. Clones share it.
#[derive(Clone)]
pub struct Pg(Arc<Shared>);

struct Shared {
    config: Config,
    tls: MakeRustlsConnect,
    client: Mutex<Option<Client>>,
}

impl Pg {
    /// Parses the URL and connects once, so a bad URL or unreachable server shows at boot.
    /// TLS follows the URL's `sslmode`, with `crypto` as the only provider.
    ///
    /// # Errors
    /// The URL doesn't parse, TLS can't be set up, or the first connect fails.
    pub async fn connect(url: &str, crypto: Arc<CryptoProvider>) -> Result<Pg, PgError> {
        let mut config: Config = url
            .parse()
            .map_err(|_| PgError::Fatal("DATABASE_URL is not a postgres:// URL".into()))?;
        config.connect_timeout(CONNECT_TIMEOUT);
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(crypto)
            .with_safe_default_protocol_versions()
            .map_err(|e| PgError::Fatal(format!("Postgres TLS setup failed: {e}")))?
            .with_root_certificates(roots)
            .with_no_client_auth();
        let pg = Pg(Arc::new(Shared {
            config,
            tls: MakeRustlsConnect::new(tls),
            client: Mutex::new(None),
        }));
        *pg.0.client.lock().await = Some(pg.open().await?);
        Ok(pg)
    }

    async fn open(&self) -> Result<Client, PgError> {
        let (client, connection) = self
            .0
            .config
            .connect(self.0.tls.clone())
            .await
            .map_err(PgError::from)?;
        // The client sees the connection's end as `is_closed`, so its error needs no other home.
        tokio::spawn(async move { drop(connection.await) });
        Ok(client)
    }

    /// `BEGIN; SET LOCAL statement_timeout, lock_timeout; f(tx); COMMIT`, reconnecting first if
    /// the connection broke. Inside `f`, use only `query_typed`. A broken connection is dropped,
    /// so the next call reconnects.
    ///
    /// # Errors
    /// What `f` returns, or the connect, `BEGIN` or `COMMIT` failing, classified as [`PgError`].
    pub async fn tx<T>(
        &self,
        f: impl AsyncFnOnce(&Transaction<'_>) -> Result<T, PgError>,
    ) -> Result<T, PgError> {
        let mut slot = self.0.client.lock().await;
        let client = match slot.take() {
            Some(client) if !client.is_closed() => slot.insert(client),
            _ => slot.insert(self.open().await?),
        };
        let result = run(client, f).await;
        if client.is_closed() {
            *slot = None;
        }
        result
    }
}

async fn run<T>(
    client: &mut Client,
    f: impl AsyncFnOnce(&Transaction<'_>) -> Result<T, PgError>,
) -> Result<T, PgError> {
    let tx = client.transaction().await?;
    tx.batch_execute("SET LOCAL statement_timeout = '5s'; SET LOCAL lock_timeout = '1s'")
        .await?;
    // Dropping the transaction on an error rolls it back.
    let value = f(&tx).await?;
    tx.commit().await?;
    Ok(value)
}

/// A column that won't decode means the schema doesn't match this build, which a retry won't fix.
///
/// # Errors
/// The column is missing or holds another type.
pub fn column<'a, T: tokio_postgres::types::FromSql<'a>>(
    row: &'a Row,
    name: &str,
) -> Result<T, PgError> {
    row.try_get(name)
        .map_err(|e| PgError::Fatal(format!("Column {name} doesn't read as expected: {e}")))
}

#[derive(Debug, thiserror::Error)]
pub enum PgError {
    /// 42P01 or 42703: the migration this build reads hasn't landed yet. Keep what is held, wait.
    #[error("The Postgres schema is behind this build (SQLSTATE {sqlstate}).")]
    SchemaNotReady { sqlstate: &'static str },
    /// The connection, a timeout, a lock wait or a serialization conflict. Retry.
    #[error("Postgres is unavailable: {0}")]
    Transient(String),
    /// Anything a retry won't fix.
    #[error("Postgres refused the statement: {0}")]
    Fatal(String),
}

impl From<tokio_postgres::Error> for PgError {
    fn from(error: tokio_postgres::Error) -> PgError {
        let Some(db) = error.as_db_error() else {
            // No SQLSTATE: the connection or the protocol failed, not the statement.
            return PgError::Transient(error.to_string());
        };
        let code = db.code();
        if *code == SqlState::UNDEFINED_TABLE {
            return PgError::SchemaNotReady { sqlstate: "42P01" };
        }
        if *code == SqlState::UNDEFINED_COLUMN {
            return PgError::SchemaNotReady { sqlstate: "42703" };
        }
        let transient = code.code().starts_with("08")
            || code.code().starts_with("40")
            || code.code().starts_with("57")
            || *code == SqlState::LOCK_NOT_AVAILABLE
            || *code == SqlState::TOO_MANY_CONNECTIONS;
        let message = format!("{} (SQLSTATE {})", db.message(), code.code());
        if transient {
            PgError::Transient(message)
        } else {
            PgError::Fatal(message)
        }
    }
}
