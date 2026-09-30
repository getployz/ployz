//! The Postgres adapter Cloud hosts the Store on (`postgres://…`).

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use ployz_core::RpcError;
use postgres::error::SqlState;
use postgres::types::{ToSql, Type};
use postgres::{Client, Config, IsolationLevel};
use tokio_postgres_rustls::MakeRustlsConnect;

use super::{Cell, Param, Row, Tx};
use crate::error;

/// How long a connect, a statement or a wait for another writer's lock may take
/// before the command ends `unavailable`.
const TIMEOUT: Duration = Duration::from_secs(10);

/// Idle connections kept for reuse. Callers bound how many run at once.
const IDLE: usize = 8;

/// TLS follows the URL's `sslmode`: `prefer` (the default) falls back to plaintext on a
/// private network, `require` refuses to.
pub(crate) struct Postgres {
    config: Config,
    tls: MakeRustlsConnect,
    idle: Mutex<Vec<Client>>,
}

impl Postgres {
    pub(super) fn open(url: &str) -> Result<Self, RpcError> {
        let mut config: Config = url.parse().map_err(|_| {
            error::invalid(
                "Expected a postgres:// Config Store URL",
                serde_json::Value::Null,
            )
        })?;
        config.connect_timeout(TIMEOUT);
        let roots =
            rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls = rustls::ClientConfig::builder_with_provider(std::sync::Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|_| error::internal("The Config Store TLS setup failed"))?
        .with_root_certificates(roots)
        .with_no_client_auth();
        Ok(Self {
            config,
            tls: MakeRustlsConnect::new(tls),
            idle: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn transaction<T>(
        &self,
        write: bool,
        work: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let idle = self
            .idle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop();
        let mut client = match idle {
            Some(client) if !client.is_closed() => client,
            _ => self
                .config
                .connect(self.tls.clone())
                .map_err(storage_error)?,
        };
        let result = run(&mut client, write, work);
        if !client.is_closed() {
            let mut idle = self.idle.lock().unwrap_or_else(PoisonError::into_inner);
            if idle.len() < IDLE {
                idle.push(client);
            }
        }
        result
    }
}

/// Writers run read-committed and lock the rows they change (see `scope::lock`);
/// readers see one snapshot.
fn run<T>(
    client: &mut Client,
    write: bool,
    work: impl FnOnce(&mut dyn Tx) -> Result<T, RpcError>,
) -> Result<T, RpcError> {
    let isolation = if write {
        IsolationLevel::ReadCommitted
    } else {
        IsolationLevel::RepeatableRead
    };
    let mut transaction = client
        .build_transaction()
        .isolation_level(isolation)
        .read_only(!write)
        .start()
        .map_err(storage_error)?;
    // Per transaction, not as startup options: PgBouncer refuses those.
    let millis = TIMEOUT.as_millis();
    transaction
        .batch_execute(&format!(
            "SET LOCAL statement_timeout = {millis}; SET LOCAL lock_timeout = {millis}"
        ))
        .map_err(storage_error)?;
    // Dropping the transaction on an error rolls it back.
    let value = work(&mut PostgresTx(&mut transaction))?;
    transaction.commit().map_err(storage_error)?;
    Ok(value)
}

struct PostgresTx<'a, 'tx>(&'a mut postgres::Transaction<'tx>);

impl Tx for PostgresTx<'_, '_> {
    fn execute(&mut self, sql: &str, params: &[Param<'_>]) -> Result<usize, RpcError> {
        let changed = self
            .0
            .execute(numbered(sql).as_str(), &bind(params))
            .map_err(storage_error)?;
        Ok(usize::try_from(changed).unwrap_or(usize::MAX))
    }

    fn query(&mut self, sql: &str, params: &[Param<'_>]) -> Result<Vec<Row>, RpcError> {
        let rows = self
            .0
            .query(numbered(sql).as_str(), &bind(params))
            .map_err(storage_error)?;
        rows.iter().map(row).collect()
    }

    fn batch(&mut self, sql: &str) -> Result<(), RpcError> {
        self.0.batch_execute(sql).map_err(storage_error)
    }
}

/// `?N` to Postgres's `$N`. The Store's SQL never puts `?` in a literal.
fn numbered(sql: &str) -> String {
    sql.replace('?', "$")
}

fn bind<'a>(params: &'a [Param<'_>]) -> Vec<&'a (dyn ToSql + Sync)> {
    const NULL_TEXT: &Option<String> = &None;
    const NULL_INT: &Option<i64> = &None;
    params
        .iter()
        .map(|param| match param {
            Param::Text(value) => value as &(dyn ToSql + Sync),
            Param::Int(value) => value as &(dyn ToSql + Sync),
            Param::NullText => NULL_TEXT as &(dyn ToSql + Sync),
            Param::NullInt => NULL_INT as &(dyn ToSql + Sync),
        })
        .collect()
}

fn row(row: &postgres::Row) -> Result<Row, RpcError> {
    let read = |index: usize| -> Result<Cell, postgres::Error> {
        let kind = row
            .columns()
            .get(index)
            .map(|column| column.type_().clone());
        Ok(match kind {
            Some(Type::TEXT) => row
                .try_get::<_, Option<String>>(index)?
                .map_or(Cell::Null, Cell::Text),
            Some(Type::INT8) => row
                .try_get::<_, Option<i64>>(index)?
                .map_or(Cell::Null, Cell::Int),
            // The schema has no such columns; reading one reports corruption.
            _ => Cell::Null,
        })
    };
    (0..row.len())
        .map(read)
        .collect::<Result<Vec<_>, _>>()
        .map(Row)
        .map_err(storage_error)
}

/// A taken key or unique name is `conflict`, whichever check first noticed it. A
/// lost connection, a timeout, or a lock or serialization failure decided nothing
/// the caller can see, so it is `unavailable` and a retry answers properly.
fn storage_error(source: postgres::Error) -> RpcError {
    if source.code() == Some(&SqlState::UNIQUE_VIOLATION) {
        return error::conflict(
            "An ID or name in this request is already taken",
            serde_json::Value::Null,
        );
    }
    let transient = source.is_closed()
        || source.code().is_none_or(|code| {
            [
                SqlState::T_R_SERIALIZATION_FAILURE,
                SqlState::T_R_DEADLOCK_DETECTED,
                SqlState::LOCK_NOT_AVAILABLE,
                SqlState::QUERY_CANCELED,
                SqlState::ADMIN_SHUTDOWN,
                SqlState::TOO_MANY_CONNECTIONS,
            ]
            .contains(code)
        });
    if transient {
        error::unavailable("The Config Store is unavailable; retry the command")
    } else {
        // `{source}` alone prints "db error"; the server's code and message say what failed.
        let detail = source.as_db_error().map_or_else(
            || source.to_string(),
            |db| format!("{} ({})", db.message(), db.code().code()),
        );
        error::internal(format!("Config Store storage failed: {detail}"))
    }
}
