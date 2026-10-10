use std::{
    collections::BTreeMap,
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
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::broadcast};

use super::{ApiClient, ReplicatedStore};

#[derive(Clone)]
struct ClusterKv {
    network: String,
    machines: BTreeMap<String, String>,
    containers: BTreeMap<String, (String, String)>,
    volumes: BTreeMap<(String, String), String>,
    container_changes: broadcast::Sender<()>,
    machine_changes: broadcast::Sender<()>,
    subscriptions: bool,
    snapshots_fail: bool,
    tokens: Vec<String>,
}

impl ClusterKv {
    fn new(subscriptions: bool) -> Self {
        let (container_changes, _) = broadcast::channel(16);
        let (machine_changes, _) = broadcast::channel(16);
        Self {
            network: "10.210.0.0/16".into(),
            machines: BTreeMap::new(),
            containers: BTreeMap::new(),
            volumes: BTreeMap::new(),
            container_changes,
            machine_changes,
            subscriptions,
            snapshots_fail: false,
            tokens: Vec::new(),
        }
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
    bind(false).await
}

/// A store whose Machine and Container subscriptions fire on every publish.
pub(crate) async fn store_with_subscriptions() -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    bind(true).await
}

async fn bind(with_subscriptions: bool) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let kv = Arc::new(Mutex::new(ClusterKv::new(with_subscriptions)));
    let server = tokio::spawn(async move {
        axum::serve(listener, router(kv)).await.unwrap();
    });
    (
        ReplicatedStore::new(ApiClient::http1(address, &test_token()).unwrap()),
        server,
    )
}

fn router(kv: Arc<Mutex<ClusterKv>>) -> Router {
    Router::new()
        .route("/v1/queries", post(queries))
        .route("/v1/transactions", post(transactions))
        .route("/v1/subscriptions", post(subscriptions))
        .with_state(kv)
}

/// A loopback address that stays free after a test releases it. The kernel
/// autobinds ports only from the ephemeral range, which starts at 32768 on
/// Linux, and the random 127/8 address keeps concurrent tests apart.
pub(crate) fn unclaimed_loopback() -> SocketAddrV4 {
    let [a, b, c, low, high, ..] = RandomState::new().hash_one(()).to_le_bytes();
    let port = 1024 + u16::from_le_bytes([low, high]) % (32768 - 1024);
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
    kv: Arc<Mutex<ClusterKv>>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl FakeCluster {
    pub(crate) fn start() -> Self {
        let listener = std::net::TcpListener::bind(unclaimed_loopback()).unwrap();
        let mut cluster = Self {
            address: listener.local_addr().unwrap(),
            kv: Arc::new(Mutex::new(ClusterKv::new(true))),
            runtime: None,
        };
        cluster.serve(listener);
        cluster
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

    pub(crate) fn stop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }

    pub(crate) fn restart(&mut self) {
        self.stop();
        self.serve(std::net::TcpListener::bind(self.address).unwrap());
    }

    pub(crate) fn fail_snapshots(&self, fail: bool) {
        self.kv.lock().unwrap().snapshots_fail = fail;
    }

    pub(crate) fn subscription_tokens(&self) -> Vec<String> {
        self.kv.lock().unwrap().tokens.clone()
    }

    fn serve(&mut self, listener: std::net::TcpListener) {
        listener.set_nonblocking(true).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let kv = self.kv.clone();
        runtime.spawn(async move {
            let listener = TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router(kv)).await.unwrap();
        });
        self.runtime = Some(runtime);
    }
}

impl Drop for FakeCluster {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn queries(
    State(kv): State<Arc<Mutex<ClusterKv>>>,
    body: Bytes,
) -> Result<Vec<u8>, StatusCode> {
    query(&kv, serde_json::from_slice(&body).unwrap()).map(Into::into)
}

async fn transactions(State(kv): State<Arc<Mutex<ClusterKv>>>, body: Bytes) -> Vec<u8> {
    execute(&kv, serde_json::from_slice(&body).unwrap()).into()
}

async fn subscriptions(
    State(kv): State<Arc<Mutex<ClusterKv>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Body, StatusCode> {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    let mut kv = kv.lock().unwrap();
    if let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    {
        kv.tokens.push(token.to_owned());
    }
    if !kv.subscriptions {
        return Err(StatusCode::NOT_FOUND);
    }
    let (receiver, columns) = match statement.query.as_str() {
        "SELECT id, machine_id, container FROM containers" => (
            kv.container_changes.subscribe(),
            "{\"columns\":[\"id\",\"machine_id\",\"container\"]}\n{\"eoq\":{\"time\":0.0}}\n",
        ),
        "SELECT id, info FROM machines" => (
            kv.machine_changes.subscribe(),
            "{\"columns\":[\"id\",\"info\"]}\n{\"eoq\":{\"time\":0.0}}\n",
        ),
        query => panic!("unexpected subscription {query}"),
    };
    drop(kv);
    let snapshot =
        stream::once(async move { Ok::<_, Infallible>(Bytes::from_static(columns.as_bytes())) });
    let changes = stream::unfold(receiver, |mut receiver| async move {
        loop {
            match receiver.recv().await {
                Ok(()) => {
                    return Some((
                        Ok::<_, Infallible>(Bytes::from_static(b"{\"change\":{}}\n")),
                        receiver,
                    ));
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    });
    Ok(Body::from_stream(snapshot.chain(changes)))
}

fn query(kv: &Mutex<ClusterKv>, statement: Statement) -> Result<Bytes, StatusCode> {
    let kv = kv.lock().unwrap();
    Ok(match statement.query.as_str() {
        "SELECT id, machine_id, container FROM containers ORDER BY id" => {
            if kv.snapshots_fail {
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            events(
                &["id", "machine_id", "container"],
                kv.containers.iter().map(|(id, (owner, container))| {
                    vec![json!(id), json!(owner), json!(container)]
                }),
            )
        }
        "SELECT id, info FROM machines ORDER BY name" => events(
            &["id", "info"],
            kv.machines
                .iter()
                .map(|(id, info)| vec![json!(id), json!(info)]),
        ),
        "SELECT info FROM machines WHERE id = ?" => {
            let id = text_param(&statement.params, 0);
            events(&["info"], kv.machines.get(id).map(|info| vec![json!(info)]))
        }
        "SELECT machine_id, container FROM containers WHERE id = ?" => {
            let id = text_param(&statement.params, 0);
            events(
                &["machine_id", "container"],
                kv.containers
                    .get(id)
                    .map(|(owner, container)| vec![json!(owner), json!(container)]),
            )
        }
        "SELECT container FROM containers WHERE id = ?" => {
            let id = text_param(&statement.params, 0);
            events(
                &["container"],
                kv.containers
                    .get(id)
                    .map(|(_, container)| vec![json!(container)]),
            )
        }
        "SELECT machine_id, name, volume FROM volumes ORDER BY machine_id, name" => events(
            &["machine_id", "name", "volume"],
            kv.volumes.iter().map(|((machine_id, name), volume)| {
                vec![json!(machine_id), json!(name), json!(volume)]
            }),
        ),
        "SELECT volume FROM volumes WHERE machine_id = ? AND name = ?" => {
            let key = (
                text_param(&statement.params, 0).to_owned(),
                text_param(&statement.params, 1).to_owned(),
            );
            events(
                &["volume"],
                kv.volumes.get(&key).map(|volume| vec![json!(volume)]),
            )
        }
        "SELECT value FROM cluster WHERE key = 'network'" => {
            events(&["value"], vec![vec![json!(kv.network)]])
        }
        "SELECT site_id, db_version FROM crsql_db_versions" => {
            events(&["site_id", "db_version"], Vec::new())
        }
        query => panic!("unexpected query {query}"),
    })
}

fn execute(kv: &Mutex<ClusterKv>, statements: Vec<Statement>) -> Bytes {
    let mut kv = kv.lock().unwrap();
    for statement in &statements {
        match statement.query.as_str() {
            query if query.starts_with("INSERT INTO machines (id, info)") => {
                kv.machines.insert(
                    text_param(&statement.params, 0).to_owned(),
                    text_param(&statement.params, 1).to_owned(),
                );
                let _ = kv.machine_changes.send(());
            }
            query if query.starts_with("INSERT INTO volumes (machine_id, name, volume)") => {
                kv.volumes.insert(
                    (
                        text_param(&statement.params, 0).to_owned(),
                        text_param(&statement.params, 1).to_owned(),
                    ),
                    text_param(&statement.params, 2).to_owned(),
                );
            }
            query if query.starts_with("INSERT INTO containers (id, container,") => {
                kv.containers.insert(
                    text_param(&statement.params, 0).to_owned(),
                    (
                        text_param(&statement.params, 2).to_owned(),
                        text_param(&statement.params, 1).to_owned(),
                    ),
                );
                let _ = kv.container_changes.send(());
            }
            query => panic!("unexpected statement {query}"),
        }
    }
    serde_json::to_vec(&json!({
        "results": vec![json!({"rows_affected": 1, "time": 0.0}); statements.len()],
        "time": 0.0,
    }))
    .unwrap()
    .into()
}

fn text_param(params: &[Value], index: usize) -> &str {
    params
        .get(index)
        .and_then(Value::as_str)
        .expect("statement parameter")
}

pub(crate) fn events(columns: &[&str], rows: impl IntoIterator<Item = Vec<Value>>) -> Bytes {
    let mut body = serde_json::to_vec(&json!({ "columns": columns })).unwrap();
    for (index, row) in rows.into_iter().enumerate() {
        body.extend(serde_json::to_vec(&json!({ "row": [index as u64 + 1, row] })).unwrap());
    }
    body.extend(br#"{"eoq":{"time":0.0}}"#);
    body.into()
}
