//! The Corrosion stand-in tests run against: an in-memory SQLite database that
//! holds the real `schema.sql`, served over Corrosion's HTTP API.

use std::{
    collections::{BTreeSet, HashMap},
    convert::Infallible,
    hash::{BuildHasher, RandomState},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode, header},
    routing::post,
};
use futures_util::{StreamExt, stream};
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::broadcast};

use super::{
    ApiClient, ReplicatedStore,
    store::{SEALING_AWAITS_RECOVERY_CLUSTER_KEY, SEALING_PUBLIC_KEY_CLUSTER_KEY},
};

const CONTAINERS: &str = "containers";
const CLUSTER: &str = "cluster";
const NETWORK_KEY: &str = "network";
const DEFAULT_NETWORK: &str = "10.210.0.0/16";

/// Corrosion re-renders schema.sql before creating its tables, which declares
/// the SQLite keyword column `key` as `KEY`; an unaliased `SELECT key` then
/// reports `KEY`.
pub(crate) fn corrosion_db() -> rusqlite::Connection {
    let schema = include_str!("schema.sql").replacen("    key  ", "    KEY  ", 1);
    assert_ne!(schema, include_str!("schema.sql"));
    let db = rusqlite::Connection::open_in_memory().unwrap();
    db.execute_batch(&schema).unwrap();
    crsql_stand_ins(&db);
    // cr-sqlite keeps a Machine's key after its row is deleted: the deletion evidence.
    db.execute_batch(
        "CREATE TRIGGER machines_crsql_pks AFTER INSERT ON machines BEGIN
             INSERT OR IGNORE INTO machines__crsql_pks (id) VALUES (NEW.id);
         END;",
    )
    .unwrap();
    db
}

/// The cr-sqlite and Corrosion tables [`super::store::crsql`] reads, as the test double
/// has no extension to create them. Their shapes are the ones the pinned image's
/// `corrosion::integration_tests` exercise.
pub(crate) fn crsql_stand_ins(db: &rusqlite::Connection) {
    db.execute_batch(
        "CREATE TABLE machines__crsql_pks (__crsql_key INTEGER PRIMARY KEY, id UNIQUE);
         CREATE TABLE crsql_db_versions (site_id BLOB PRIMARY KEY, db_version INTEGER);
         CREATE TABLE __corro_bookkeeping_gaps (actor_id BLOB, start INTEGER, end INTEGER);
         CREATE TABLE __corro_seq_bookkeeping (site_id BLOB, db_version INTEGER);",
    )
    .unwrap();
}

/// What an open Container subscription sends when a Container row changes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContainerFeed {
    Changes,
    /// Nothing, as a subscription that went quiet on a live connection.
    Silent,
    /// The start of a re-snapshot that never finishes.
    StalledSnapshot,
}

/// Holds one answer of the fake, as `FakeCluster::hold_query` does.
#[derive(Clone, Default)]
pub(crate) struct QueryHold(Arc<Mutex<Option<HeldQuery>>>);

struct HeldQuery {
    query: String,
    nth: usize,
    gate: Arc<tokio::sync::Barrier>,
}

impl QueryHold {
    /// Hold the answer to the `nth` run of `query` from now, read before the hold, until the
    /// returned gate is waited on twice: once to see it held, once to release it.
    pub(crate) fn hold(&self, query: &str, nth: usize) -> Arc<tokio::sync::Barrier> {
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        *self.0.lock().unwrap() = Some(HeldQuery {
            query: query.to_owned(),
            nth,
            gate: Arc::clone(&gate),
        });
        gate
    }

    fn due(&self, query: &str) -> Option<Arc<tokio::sync::Barrier>> {
        let mut held = self.0.lock().unwrap();
        let due = held.as_mut().is_some_and(|held| {
            if held.query == query {
                held.nth -= 1;
            }
            held.nth == 0
        });
        if due {
            held.take().map(|held| held.gate)
        } else {
            None
        }
    }
}

struct Cluster {
    db: rusqlite::Connection,
    /// The `(table, rowid)` of each row a write changed, drained after each write.
    touched: Arc<Mutex<Vec<(String, i64)>>>,
    changes: HashMap<String, broadcast::Sender<()>>,
    /// The awaited Sealing public key as it stood when the network was written,
    /// so a test awaits the write instead of polling.
    network_written: tokio::sync::watch::Sender<Option<Option<String>>>,
    sealing_public_key: tokio::sync::watch::Sender<Option<String>>,
    container_feed: ContainerFeed,
    subscriptions: bool,
    snapshots_fail: bool,
    tokens: Vec<String>,
    /// Counts subscription requests, so a test awaits a subscribe instead of polling.
    subscribed: tokio::sync::watch::Sender<usize>,
    hold: QueryHold,
}

type Shared = Arc<Mutex<Cluster>>;

impl Cluster {
    fn new(db: rusqlite::Connection, subscriptions: bool) -> Self {
        let touched = Arc::new(Mutex::new(Vec::new()));
        let hook = Arc::clone(&touched);
        db.update_hook(Some(move |_, _: &str, table: &str, rowid| {
            hook.lock().unwrap().push((table.to_owned(), rowid));
        }))
        .unwrap();
        Self {
            db,
            touched,
            changes: HashMap::new(),
            network_written: tokio::sync::watch::Sender::new(None),
            sealing_public_key: tokio::sync::watch::Sender::new(None),
            container_feed: ContainerFeed::Changes,
            subscriptions,
            snapshots_fail: false,
            tokens: Vec::new(),
            subscribed: tokio::sync::watch::Sender::new(0),
            hold: QueryHold::default(),
        }
    }

    fn with_network(self) -> Self {
        self.db
            .execute(
                "INSERT INTO cluster (key, value) VALUES (?, ?)",
                [NETWORK_KEY, DEFAULT_NETWORK],
            )
            .unwrap();
        self.touched.lock().unwrap().clear();
        self
    }

    fn changes(&mut self, table: &str) -> &broadcast::Sender<()> {
        self.changes
            .entry(table.to_owned())
            .or_insert_with(|| broadcast::channel(16).0)
    }

    fn cluster_value(&self, key: &str) -> Option<String> {
        self.db
            .query_row("SELECT value FROM cluster WHERE key = ?", [key], |row| {
                row.get(0)
            })
            .ok()
    }

    /// Tells the subscriptions and watches what the last write changed.
    fn notify(&mut self) {
        let touched = std::mem::take(&mut *self.touched.lock().unwrap());
        let tables: BTreeSet<&str> = touched.iter().map(|(table, _)| table.as_str()).collect();
        for table in tables {
            let _ = self.changes(table).send(());
        }
        let network_written = touched.iter().any(|(table, rowid)| {
            table == CLUSTER
                && self
                    .db
                    .query_row("SELECT key FROM cluster WHERE rowid = ?", [rowid], |row| {
                        row.get::<_, String>(0)
                    })
                    .is_ok_and(|key| key == NETWORK_KEY)
        });
        if network_written {
            let awaited = self.cluster_value(SEALING_AWAITS_RECOVERY_CLUSTER_KEY);
            self.network_written.send_replace(Some(awaited));
        }
        let published = self.cluster_value(SEALING_PUBLIC_KEY_CLUSTER_KEY);
        self.sealing_public_key.send_if_modified(|key| {
            let changed = *key != published;
            key.clone_from(&published);
            changed
        });
    }

    fn write(&mut self, sql: &str, params: impl rusqlite::Params) {
        self.db.execute(sql, params).unwrap();
        self.notify();
    }

    /// The tables `query` reads.
    fn reads(&self, query: &str) -> BTreeSet<String> {
        let tables = Arc::new(Mutex::new(BTreeSet::new()));
        let seen = Arc::clone(&tables);
        self.db
            .authorizer(Some(move |context: AuthContext<'_>| {
                if let AuthAction::Read { table_name, .. } = context.action {
                    seen.lock().unwrap().insert(table_name.to_owned());
                }
                Authorization::Allow
            }))
            .unwrap();
        let prepared = self.db.prepare(query).map(drop);
        self.db
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)
            .unwrap();
        prepared.unwrap_or_else(|error| panic!("unexpected query {query}: {error}"));
        std::mem::take(&mut *tables.lock().unwrap())
    }
}

const TEST_TOKEN_LEN: usize = 64;

fn test_token() -> String {
    "a".repeat(TEST_TOKEN_LEN)
}

#[derive(Deserialize)]
struct Statement {
    query: String,
    params: Vec<Value>,
}

pub(crate) async fn store() -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    serve(Cluster::new(corrosion_db(), false).with_network()).await
}

/// A store whose Machine and Container subscriptions fire on every publish.
pub(crate) async fn store_with_subscriptions() -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    serve(Cluster::new(corrosion_db(), true).with_network()).await
}

/// A store the founder's publisher has not yet written the Cluster network to.
pub(crate) async fn store_without_network() -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    serve(Cluster::new(corrosion_db(), false)).await
}

/// Exercise the store API against `db`'s rows, including corrupt imported documents.
pub(crate) async fn identity_store(
    db: rusqlite::Connection,
) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    serve(Cluster::new(db, false)).await
}

/// `identity_store` with a hold on the answers to its queries.
pub(crate) async fn held_identity_store(
    db: rusqlite::Connection,
) -> (ReplicatedStore, QueryHold, tokio::task::JoinHandle<()>) {
    let cluster = Cluster::new(db, false);
    let hold = cluster.hold.clone();
    let (store, task) = serve(cluster).await;
    (store, hold, task)
}

async fn serve(cluster: Cluster) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let cluster = Arc::new(Mutex::new(cluster));
    let server = tokio::spawn(async move {
        axum::serve(listener, router(cluster)).await.unwrap();
    });
    (
        ReplicatedStore::new(ApiClient::http1(address, &test_token()).unwrap()),
        server,
    )
}

fn router(cluster: Shared) -> Router {
    Router::new()
        .route("/v1/queries", post(queries))
        .route("/v1/transactions", post(transactions))
        .route("/v1/subscriptions", post(subscriptions))
        .with_state(cluster)
}

/// A loopback address that stays free after a test releases it. The kernel
/// autobinds ports only from the ephemeral range, which starts at 32768 on
/// Linux, and the random 127/8 address keeps concurrent tests apart. Ports from
/// 10000 up belong to the `ployz` probe tests' `free_port`, which binds every
/// address.
pub(crate) fn unclaimed_loopback() -> SocketAddrV4 {
    let [a, b, c, low, high, ..] = RandomState::new().hash_one(()).to_le_bytes();
    let port = 1024 + u16::from_le_bytes([low, high]) % (10_000 - 1024);
    SocketAddrV4::new(
        Ipv4Addr::new(127, a.clamp(1, 254), b, c.clamp(1, 254)),
        port,
    )
}

/// A Corrosion stand-in on its own runtime, so stopping it severs every open
/// connection including subscription streams, and restarting it reuses the
/// address.
pub(crate) struct FakeCluster {
    address: SocketAddr,
    cluster: Shared,
    runtime: Option<tokio::runtime::Runtime>,
    /// Disconnects once the serving task, and with it the listener, is dropped.
    serving: Option<std::sync::mpsc::Receiver<()>>,
}

impl FakeCluster {
    pub(crate) fn start() -> Self {
        let listener = std::net::TcpListener::bind(unclaimed_loopback()).unwrap();
        let mut fake = Self {
            address: listener.local_addr().unwrap(),
            cluster: Arc::new(Mutex::new(
                Cluster::new(corrosion_db(), true).with_network(),
            )),
            runtime: None,
            serving: None,
        };
        fake.serve(listener);
        fake
    }

    pub(crate) fn address(&self) -> SocketAddr {
        self.address
    }

    pub(crate) fn token(&self) -> String {
        test_token()
    }

    pub(crate) fn store(&self) -> ReplicatedStore {
        ReplicatedStore::new(ApiClient::http1(self.address, &self.token()).unwrap())
    }

    /// Returns once the address is free to bind again.
    pub(crate) fn stop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
        if let Some(serving) = self.serving.take() {
            let _ = serving.recv();
        }
    }

    pub(crate) fn restart(&mut self) {
        self.stop();
        self.serve(std::net::TcpListener::bind(self.address).unwrap());
    }

    pub(crate) fn feed_container_changes(&self, feed: ContainerFeed) {
        self.cluster.lock().unwrap().container_feed = feed;
    }

    /// Fail every read of the whole Container table, as a snapshot does.
    pub(crate) fn fail_snapshots(&self, fail: bool) {
        self.cluster.lock().unwrap().snapshots_fail = fail;
    }

    /// Hold the answer to the `nth` run of `query`, read before the hold, until the
    /// returned gate is waited on twice: once to see it held, once to release it.
    pub(crate) fn hold_query(&self, query: &str, nth: usize) -> Arc<tokio::sync::Barrier> {
        self.cluster.lock().unwrap().hold.hold(query, nth)
    }

    /// Write a route row whose containers are not a list of Container IDs.
    pub(crate) fn write_undecodable_route(&self, machine: &str, service: &str) {
        self.cluster.lock().unwrap().write(
            "INSERT INTO ingress_routes (machine_id, service_id, containers) \
             VALUES (?, ?, '{\"not\":\"routes\"}')",
            [machine, service],
        );
    }

    /// Write a Machine row whose document has not fully replicated.
    pub(crate) fn write_incomplete_machine(&self, machine: &str) {
        self.cluster.lock().unwrap().write(
            "INSERT INTO machines (id, info) VALUES (?, '{}')",
            [machine],
        );
    }

    pub(crate) async fn sealing_public_key(&self) -> String {
        let mut published = self.cluster.lock().unwrap().sealing_public_key.subscribe();
        let key = published.wait_for(Option::is_some).await.unwrap();
        key.clone().unwrap()
    }

    pub(crate) fn write_sealing_awaits_recovery(&self, key: &str) {
        self.cluster.lock().unwrap().write(
            "INSERT INTO cluster (key, value) VALUES (?, ?)",
            [SEALING_AWAITS_RECOVERY_CLUSTER_KEY, key],
        );
    }

    /// The awaited Sealing public key as it stood when the Cluster network was written.
    pub(crate) async fn awaited_key_at_network(&self) -> Option<String> {
        let mut written = self.cluster.lock().unwrap().network_written.subscribe();
        let awaited = written.wait_for(Option::is_some).await.unwrap();
        awaited.clone().unwrap()
    }

    pub(crate) fn clear_network(&self) {
        self.cluster
            .lock()
            .unwrap()
            .write("DELETE FROM cluster WHERE key = ?", [NETWORK_KEY]);
    }

    pub(crate) fn subscription_tokens(&self) -> Vec<String> {
        self.cluster.lock().unwrap().tokens.clone()
    }

    pub(crate) async fn subscriptions(&self, count: usize) {
        let mut subscribed = self.cluster.lock().unwrap().subscribed.subscribe();
        subscribed.wait_for(|seen| *seen >= count).await.unwrap();
    }

    fn serve(&mut self, listener: std::net::TcpListener) {
        listener.set_nonblocking(true).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let cluster = self.cluster.clone();
        let (serving, stopped) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let _serving = serving;
            let listener = TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router(cluster)).await.unwrap();
        });
        self.runtime = Some(runtime);
        self.serving = Some(stopped);
    }
}

impl Drop for FakeCluster {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn queries(State(cluster): State<Shared>, body: Bytes) -> Result<Vec<u8>, StatusCode> {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    let (gate, answer) = {
        let cluster = cluster.lock().unwrap();
        let gate = cluster.hold.due(&statement.query);
        let answer = if cluster.snapshots_fail
            && statement.params.is_empty()
            && cluster.reads(&statement.query).contains(CONTAINERS)
        {
            Err(StatusCode::INTERNAL_SERVER_ERROR)
        } else {
            Ok(query(&cluster.db, &statement))
        };
        (gate, answer)
    };
    if let Some(gate) = gate {
        gate.wait().await;
        gate.wait().await;
    }
    answer
}

fn query(db: &rusqlite::Connection, request: &Statement) -> Vec<u8> {
    let mut statement = db
        .prepare(&request.query)
        .unwrap_or_else(|error| panic!("unexpected query {}: {error}", request.query));
    let columns = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<Vec<_>>();
    let mut rows = statement
        .query(rusqlite::params_from_iter(
            request.params.iter().map(sql_param),
        ))
        .unwrap();
    let mut response = serde_json::to_vec(&json!({"columns": columns})).unwrap();
    let mut index = 0;
    while let Some(row) = rows.next().unwrap() {
        index += 1;
        let values = (0..columns.len())
            .map(|i| match row.get_ref(i).unwrap() {
                rusqlite::types::ValueRef::Null => Value::Null,
                rusqlite::types::ValueRef::Integer(value) => json!(value),
                rusqlite::types::ValueRef::Text(value) => {
                    json!(std::str::from_utf8(value).unwrap())
                }
                rusqlite::types::ValueRef::Blob(value) => json!(value),
                value @ rusqlite::types::ValueRef::Real(_) => {
                    panic!("unsupported test row value: {value:?}")
                }
            })
            .collect::<Vec<_>>();
        response.extend(serde_json::to_vec(&json!({"row": [index, values]})).unwrap());
    }
    response.extend(br#"{"eoq":{"time":0.0}}"#);
    response
}

/// Runs the statements as one transaction, as Corrosion does: a failed statement
/// rolls back the ones before it.
async fn transactions(State(cluster): State<Shared>, body: Bytes) -> Vec<u8> {
    let statements: Vec<Statement> = serde_json::from_slice(&body).unwrap();
    let mut cluster = cluster.lock().unwrap();
    cluster.db.execute_batch("BEGIN").unwrap();
    let mut results = Vec::new();
    let mut failed = false;
    for statement in &statements {
        let params = statement.params.iter().map(sql_param);
        match cluster
            .db
            .execute(&statement.query, rusqlite::params_from_iter(params))
        {
            Ok(affected) => results.push(json!({"rows_affected": affected, "time": 0.0})),
            Err(error) => {
                results.push(json!({"error": error.to_string()}));
                failed = true;
                break;
            }
        }
    }
    if failed {
        cluster.db.execute_batch("ROLLBACK").unwrap();
        cluster.touched.lock().unwrap().clear();
    } else {
        cluster.db.execute_batch("COMMIT").unwrap();
        cluster.notify();
    }
    serde_json::to_vec(&json!({"results": results, "time": 0.0})).unwrap()
}

async fn subscriptions(
    State(shared): State<Shared>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Body, StatusCode> {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    let mut cluster = shared.lock().unwrap();
    cluster.subscribed.send_modify(|seen| *seen += 1);
    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        cluster.tokens.push(token.to_owned());
    }
    if !cluster.subscriptions {
        return Err(StatusCode::NOT_FOUND);
    }
    let tables = cluster.reads(&statement.query);
    let containers = tables.contains(CONTAINERS);
    let feeds: Vec<_> = tables
        .iter()
        .map(|table| {
            stream::unfold(
                cluster.changes(table).subscribe(),
                |mut receiver| async move {
                    loop {
                        match receiver.recv().await {
                            Ok(()) => return Some(((), receiver)),
                            Err(broadcast::error::RecvError::Lagged(_)) => {}
                            Err(broadcast::error::RecvError::Closed) => return None,
                        }
                    }
                },
            )
            .boxed()
        })
        .collect();
    let columns = Bytes::from(query_columns(&cluster.db, &statement.query));
    drop(cluster);
    let mut header = columns.to_vec();
    header.extend(b"{\"eoq\":{\"time\":0.0}}\n");
    let snapshot = stream::once(async move { Ok::<_, Infallible>(Bytes::from(header)) });
    let changes = stream::select_all(feeds).filter_map(move |()| {
        let feed = if containers {
            shared.lock().unwrap().container_feed
        } else {
            ContainerFeed::Changes
        };
        let event = match feed {
            ContainerFeed::Changes => Some(Bytes::from_static(b"{\"change\":{}}\n")),
            ContainerFeed::Silent => None,
            ContainerFeed::StalledSnapshot => Some(columns.clone()),
        };
        std::future::ready(event.map(Ok::<_, Infallible>))
    });
    Ok(Body::from_stream(snapshot.chain(changes)))
}

/// The `columns` event that opens an answer to `query`.
fn query_columns(db: &rusqlite::Connection, query: &str) -> Vec<u8> {
    let statement = db.prepare(query).unwrap();
    let mut columns = serde_json::to_vec(&json!({"columns": statement.column_names()})).unwrap();
    columns.push(b'\n');
    columns
}

fn sql_param(value: &Value) -> rusqlite::types::Value {
    match value {
        Value::String(value) => value.clone().into(),
        Value::Number(value) => value.as_i64().unwrap().into(),
        Value::Array(bytes) => bytes
            .iter()
            .map(|byte| u8::try_from(byte.as_u64().unwrap()).unwrap())
            .collect::<Vec<_>>()
            .into(),
        Value::Null => rusqlite::types::Value::Null,
        value @ (Value::Bool(_) | Value::Object(_)) => {
            panic!("unsupported test parameter: {value}")
        }
    }
}

pub(crate) fn events(columns: &[&str], rows: impl IntoIterator<Item = Vec<Value>>) -> Bytes {
    let mut body = serde_json::to_vec(&json!({ "columns": columns })).unwrap();
    for (index, row) in rows.into_iter().enumerate() {
        body.extend(serde_json::to_vec(&json!({ "row": [index as u64 + 1, row] })).unwrap());
    }
    body.extend(br#"{"eoq":{"time":0.0}}"#);
    body.into()
}
