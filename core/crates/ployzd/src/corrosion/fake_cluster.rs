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
    network: Option<String>,
    /// The awaited Sealing public key as it stood when the network was written,
    /// so a test awaits the write instead of polling.
    network_written: tokio::sync::watch::Sender<Option<Option<String>>>,
    sealing_public_key: tokio::sync::watch::Sender<Option<String>>,
    sealing_awaits_recovery: Option<String>,
    machines: BTreeMap<String, String>,
    containers: BTreeMap<String, (String, String)>,
    volumes: BTreeMap<(String, String), String>,
    ingress_routes: BTreeMap<(String, String), String>,
    container_changes: broadcast::Sender<()>,
    machine_changes: broadcast::Sender<()>,
    /// No test writes a certificate; this only keeps the empty table's subscription open.
    certificate_changes: broadcast::Sender<()>,
    subscriptions: bool,
    snapshots_fail: bool,
    tokens: Vec<String>,
    /// Counts subscription requests, so a test awaits a subscribe instead of polling.
    subscribed: tokio::sync::watch::Sender<usize>,
    held: Option<HeldQuery>,
}

#[derive(Clone)]
struct HeldQuery {
    query: String,
    nth: usize,
    gate: Arc<tokio::sync::Barrier>,
}

impl ClusterKv {
    fn new(subscriptions: bool) -> Self {
        let (container_changes, _) = broadcast::channel(16);
        let (machine_changes, _) = broadcast::channel(16);
        let (certificate_changes, _) = broadcast::channel(16);
        Self {
            network: Some("10.210.0.0/16".into()),
            network_written: tokio::sync::watch::Sender::new(None),
            sealing_public_key: tokio::sync::watch::Sender::new(None),
            sealing_awaits_recovery: None,
            machines: BTreeMap::new(),
            containers: BTreeMap::new(),
            volumes: BTreeMap::new(),
            ingress_routes: BTreeMap::new(),
            container_changes,
            machine_changes,
            certificate_changes,
            subscriptions,
            snapshots_fail: false,
            tokens: Vec::new(),
            subscribed: tokio::sync::watch::Sender::new(0),
            held: None,
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

/// A store the founder's publisher has not yet written the Cluster network to.
pub(crate) async fn store_without_network() -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    let mut kv = ClusterKv::new(false);
    kv.network = None;
    serve(kv).await
}

async fn bind(with_subscriptions: bool) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    serve(ClusterKv::new(with_subscriptions)).await
}

async fn serve(kv: ClusterKv) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let kv = Arc::new(Mutex::new(kv));
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
    /// Disconnects once the serving task, and with it the listener, is dropped.
    serving: Option<std::sync::mpsc::Receiver<()>>,
}

impl FakeCluster {
    pub(crate) fn start() -> Self {
        let listener = std::net::TcpListener::bind(unclaimed_loopback()).unwrap();
        let mut cluster = Self {
            address: listener.local_addr().unwrap(),
            kv: Arc::new(Mutex::new(ClusterKv::new(true))),
            runtime: None,
            serving: None,
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

    pub(crate) fn fail_snapshots(&self, fail: bool) {
        self.kv.lock().unwrap().snapshots_fail = fail;
    }

    /// Hold the answer to the `nth` run of `query`, read before the hold, until the
    /// returned gate is waited on twice: once to see it held, once to release it.
    pub(crate) fn hold_query(&self, query: &str, nth: usize) -> Arc<tokio::sync::Barrier> {
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        self.kv.lock().unwrap().held = Some(HeldQuery {
            query: query.to_owned(),
            nth,
            gate: Arc::clone(&gate),
        });
        gate
    }

    pub(crate) fn write_undecodable_route(&self, machine: &str, service: &str) {
        let mut kv = self.kv.lock().unwrap();
        kv.ingress_routes.insert(
            (machine.to_owned(), service.to_owned()),
            "not json".to_owned(),
        );
    }

    /// Write a Machine row whose document has not fully replicated.
    pub(crate) fn write_incomplete_machine(&self, machine: &str) {
        let mut kv = self.kv.lock().unwrap();
        kv.machines.insert(machine.to_owned(), "{}".to_owned());
        let _ = kv.machine_changes.send(());
    }

    pub(crate) async fn sealing_public_key(&self) -> String {
        let mut published = self.kv.lock().unwrap().sealing_public_key.subscribe();
        let key = published.wait_for(Option::is_some).await.unwrap();
        key.clone().unwrap()
    }

    pub(crate) fn write_sealing_awaits_recovery(&self, key: &str) {
        self.kv.lock().unwrap().sealing_awaits_recovery = Some(key.to_owned());
    }

    /// The awaited Sealing public key as it stood when the Cluster network was written.
    pub(crate) async fn awaited_key_at_network(&self) -> Option<String> {
        let mut written = self.kv.lock().unwrap().network_written.subscribe();
        let awaited = written.wait_for(Option::is_some).await.unwrap();
        awaited.clone().unwrap()
    }

    pub(crate) fn clear_network(&self) {
        self.kv.lock().unwrap().network = None;
    }

    pub(crate) fn subscription_tokens(&self) -> Vec<String> {
        self.kv.lock().unwrap().tokens.clone()
    }

    pub(crate) async fn subscriptions(&self, count: usize) {
        let mut subscribed = self.kv.lock().unwrap().subscribed.subscribe();
        subscribed.wait_for(|seen| *seen >= count).await.unwrap();
    }

    fn serve(&mut self, listener: std::net::TcpListener) {
        listener.set_nonblocking(true).unwrap();
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let kv = self.kv.clone();
        let (serving, stopped) = std::sync::mpsc::channel();
        runtime.spawn(async move {
            let _serving = serving;
            let listener = TcpListener::from_std(listener).unwrap();
            axum::serve(listener, router(kv)).await.unwrap();
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

async fn queries(
    State(kv): State<Arc<Mutex<ClusterKv>>>,
    body: Bytes,
) -> Result<Vec<u8>, StatusCode> {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    let gate = {
        let mut kv = kv.lock().unwrap();
        let due = kv.held.as_mut().is_some_and(|held| {
            if held.query == statement.query {
                held.nth -= 1;
            }
            held.nth == 0
        });
        if due {
            kv.held.take().map(|held| held.gate)
        } else {
            None
        }
    };
    let answer = query(&kv, statement);
    if let Some(gate) = gate {
        gate.wait().await;
        gate.wait().await;
    }
    answer.map(Into::into)
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
    kv.subscribed.send_modify(|seen| *seen += 1);
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
        "SELECT hostname, body FROM certificates" => (
            kv.certificate_changes.subscribe(),
            "{\"columns\":[\"hostname\",\"body\"]}\n{\"eoq\":{\"time\":0.0}}\n",
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
        "SELECT id, container FROM containers WHERE machine_id = ? ORDER BY id" => {
            let machine_id = text_param(&statement.params, 0);
            events(
                &["id", "container"],
                kv.containers
                    .iter()
                    .filter(|(_, (owner, _))| owner == machine_id)
                    .map(|(id, (_, container))| vec![json!(id), json!(container)]),
            )
        }
        "SELECT name, volume FROM volumes WHERE machine_id = ? ORDER BY name" => {
            let machine_id = text_param(&statement.params, 0);
            events(
                &["name", "volume"],
                kv.volumes
                    .iter()
                    .filter(|((owner, _), _)| owner == machine_id)
                    .map(|((_, name), volume)| vec![json!(name), json!(volume)]),
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
        "SELECT value FROM cluster WHERE key = ?" => {
            let value = match text_param(&statement.params, 0) {
                "sealing_public_key" => kv.sealing_public_key.borrow().clone(),
                "sealing_awaits_recovery" => kv.sealing_awaits_recovery.clone(),
                _ => None,
            };
            events(&["value"], value.map(|value| vec![json!(value)]))
        }
        "SELECT value FROM cluster WHERE key = 'network'" => events(
            &["value"],
            kv.network.iter().map(|network| vec![json!(network)]),
        ),
        "SELECT machine_id, service_id, containers FROM ingress_routes" => events(
            &["machine_id", "service_id", "containers"],
            kv.ingress_routes
                .iter()
                .map(|((machine_id, service_id), containers)| {
                    vec![json!(machine_id), json!(service_id), json!(containers)]
                }),
        ),
        "SELECT machine_id, service_id, containers FROM ingress_routes WHERE service_id = ?" => {
            let service_id = text_param(&statement.params, 0);
            events(
                &["machine_id", "service_id", "containers"],
                kv.ingress_routes
                    .iter()
                    .filter(|((_, routed), _)| routed == service_id)
                    .map(|((machine_id, service_id), containers)| {
                        vec![json!(machine_id), json!(service_id), json!(containers)]
                    }),
            )
        }
        "SELECT hostname, body FROM certificates ORDER BY hostname" => {
            events(&["hostname", "body"], Vec::new())
        }
        super::store::VERSION_QUERY => events(&["site_id", "db_version"], Vec::new()),
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
            query
                if query.starts_with("INSERT INTO cluster (key, value) VALUES ('network', ?)") =>
            {
                kv.network = Some(text_param(&statement.params, 0).to_owned());
                let awaited = kv.sealing_awaits_recovery.clone();
                kv.network_written.send_replace(Some(awaited));
            }
            query
                if query.starts_with(
                    "INSERT INTO cluster (key, value) VALUES ('sealing_public_key', ?)",
                ) =>
            {
                if kv.sealing_public_key.borrow().is_none() {
                    kv.sealing_public_key
                        .send_replace(Some(text_param(&statement.params, 0).to_owned()));
                }
            }
            query
                if query.starts_with(
                    "INSERT INTO cluster (key, value) VALUES ('sealing_awaits_recovery', ?)",
                ) =>
            {
                if kv.sealing_awaits_recovery.is_none() {
                    kv.sealing_awaits_recovery = Some(text_param(&statement.params, 0).to_owned());
                }
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
            query if query.starts_with("INSERT INTO ingress_routes (machine_id, service_id,") => {
                kv.ingress_routes.insert(
                    (
                        text_param(&statement.params, 0).to_owned(),
                        text_param(&statement.params, 1).to_owned(),
                    ),
                    text_param(&statement.params, 2).to_owned(),
                );
            }
            "DELETE FROM ingress_routes WHERE machine_id = ?" => {
                let machine_id = text_param(&statement.params, 0);
                kv.ingress_routes
                    .retain(|(owner, _), _| owner != machine_id);
            }
            query if query.starts_with("DELETE FROM ingress_routes") => {
                kv.ingress_routes.remove(&(
                    text_param(&statement.params, 0).to_owned(),
                    text_param(&statement.params, 1).to_owned(),
                ));
            }
            "DELETE FROM containers WHERE id = ? AND machine_id = ?" => {
                let id = text_param(&statement.params, 0);
                if kv
                    .containers
                    .get(id)
                    .is_some_and(|(owner, _)| owner == text_param(&statement.params, 1))
                {
                    kv.containers.remove(id);
                    let _ = kv.container_changes.send(());
                }
            }
            "DELETE FROM volumes WHERE machine_id = ? AND name = ?" => {
                kv.volumes.remove(&(
                    text_param(&statement.params, 0).to_owned(),
                    text_param(&statement.params, 1).to_owned(),
                ));
            }
            query
                if query.starts_with(
                    "UPDATE machines SET info = json_set(info, '$.runtime.leaving'",
                ) =>
            {
                set_leaving(&mut kv, text_param(&statement.params, 0), true);
            }
            query
                if query.starts_with(
                    "UPDATE machines SET info = json_remove(info, '$.runtime.leaving')",
                ) =>
            {
                set_leaving(&mut kv, text_param(&statement.params, 0), false);
            }
            "DELETE FROM volumes WHERE machine_id = ?" => {
                let machine_id = text_param(&statement.params, 0);
                kv.volumes.retain(|(owner, _), _| owner != machine_id);
            }
            "DELETE FROM containers WHERE machine_id = ?" => {
                let machine_id = text_param(&statement.params, 0);
                kv.containers.retain(|_, (owner, _)| owner != machine_id);
                let _ = kv.container_changes.send(());
            }
            "DELETE FROM machines WHERE id = ?" => {
                if kv
                    .machines
                    .remove(text_param(&statement.params, 0))
                    .is_some()
                {
                    let _ = kv.machine_changes.send(());
                }
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

/// `json_set`/`json_remove` of `$.runtime.leaving` on `machine_id`'s row, if it has one.
fn set_leaving(kv: &mut ClusterKv, machine_id: &str, leaving: bool) {
    let Some(info) = kv.machines.get_mut(machine_id) else {
        return;
    };
    let mut row: serde_json::Value = serde_json::from_str(info).unwrap();
    let runtime = row
        .get_mut("runtime")
        .and_then(serde_json::Value::as_object_mut)
        .expect("a Machine row has a runtime object");
    if leaving {
        runtime.insert("leaving".to_owned(), json!(true));
    } else {
        runtime.remove("leaving");
    }
    *info = row.to_string();
    let _ = kv.machine_changes.send(());
}
