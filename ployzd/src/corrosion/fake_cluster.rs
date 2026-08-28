use std::{
    collections::{BTreeMap, VecDeque},
    convert::Infallible,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::StatusCode,
    routing::post,
};
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::broadcast};

use super::store::{
    AGE_ALLOCATOR, ALLOCATOR_ROW, CLAIM_FOUNDER_ALLOCATOR, PUBLISH_FOUNDING_INGRESS_PROXY_BACKEND,
    STEAL_ALLOCATOR,
};
use super::{ApiClient, ReplicatedStore};
use ployz_core::{INGRESS_PROXY_BACKEND_CLUSTER_KEY, MachineId};

const PUSH_ALLOCATOR_READ: &str = "INSERT INTO allocator_script (value) VALUES (?)";

#[derive(Clone)]
struct ClusterKv {
    network: String,
    ingress_proxy_backend: Option<String>,
    machines: BTreeMap<String, String>,
    containers: BTreeMap<String, String>,
    container_changes: broadcast::Sender<()>,
    container_subscriptions: bool,
    allocator: Option<(String, bool)>,
    allocator_script: VecDeque<(String, bool)>,
}

#[derive(Deserialize)]
struct Statement {
    query: String,
    params: Vec<Value>,
}

pub(crate) async fn store() -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    bind(None, None, false).await
}

pub(crate) async fn store_with_container_changes() -> (ReplicatedStore, tokio::task::JoinHandle<()>)
{
    bind(None, None, true).await
}

pub(crate) async fn store_with_allocator_value(
    value: &str,
) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    bind(Some((value.to_owned(), true)), None, false).await
}

pub(crate) async fn store_with_ingress_proxy_backend_value(
    value: &str,
) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    bind(None, Some(value.to_owned()), false).await
}

async fn bind(
    allocator: Option<(String, bool)>,
    ingress_proxy_backend: Option<String>,
    container_subscriptions: bool,
) -> (ReplicatedStore, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (container_changes, _) = broadcast::channel(16);
    let kv = Arc::new(Mutex::new(ClusterKv {
        network: "10.210.0.0/16".into(),
        ingress_proxy_backend,
        machines: BTreeMap::new(),
        containers: BTreeMap::new(),
        container_changes,
        container_subscriptions,
        allocator,
        allocator_script: VecDeque::new(),
    }));
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/queries", post(queries))
                .route("/v1/transactions", post(transactions))
                .route("/v1/subscriptions", post(subscriptions))
                .with_state(kv),
        )
        .await
        .unwrap();
    });
    (
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        server,
    )
}

async fn queries(State(kv): State<Arc<Mutex<ClusterKv>>>, body: Bytes) -> Vec<u8> {
    query(&kv, serde_json::from_slice(&body).unwrap()).into()
}

async fn transactions(State(kv): State<Arc<Mutex<ClusterKv>>>, body: Bytes) -> Vec<u8> {
    execute(&kv, serde_json::from_slice(&body).unwrap()).into()
}

async fn subscriptions(
    State(kv): State<Arc<Mutex<ClusterKv>>>,
    body: Bytes,
) -> Result<Body, StatusCode> {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    assert_eq!(statement.query, "SELECT id, container FROM containers");
    let kv = kv.lock().unwrap();
    if !kv.container_subscriptions {
        return Err(StatusCode::NOT_FOUND);
    }
    let receiver = kv.container_changes.subscribe();
    drop(kv);
    let snapshot = stream::once(async {
        Ok::<_, Infallible>(Bytes::from_static(
            b"{\"columns\":[\"id\",\"container\"]}\n{\"eoq\":{\"time\":0.0}}\n",
        ))
    });
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

fn query(kv: &Mutex<ClusterKv>, statement: Statement) -> Bytes {
    let mut kv = kv.lock().unwrap();
    match statement.query.as_str() {
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
        "SELECT container FROM containers WHERE id = ?" => {
            let id = text_param(&statement.params, 0);
            events(
                &["container"],
                kv.containers
                    .get(id)
                    .map(|container| vec![json!(container)]),
            )
        }
        "SELECT value FROM cluster WHERE key = 'network'" => {
            events(&["value"], vec![vec![json!(kv.network)]])
        }
        "SELECT value FROM cluster WHERE key = ?"
            if text_param(&statement.params, 0) == INGRESS_PROXY_BACKEND_CLUSTER_KEY =>
        {
            events(
                &["value"],
                kv.ingress_proxy_backend
                    .iter()
                    .map(|backend| vec![json!(backend)]),
            )
        }
        ALLOCATOR_ROW => {
            if let Some(row) = kv.allocator_script.pop_front() {
                kv.allocator = Some(row);
            }
            events(
                &["allocator", "quiet"],
                kv.allocator
                    .iter()
                    .map(|(id, quiet)| vec![json!(id), json!(u8::from(*quiet))]),
            )
        }
        "SELECT site_id, db_version FROM crsql_db_versions" => {
            events(&["site_id", "db_version"], Vec::new())
        }
        query => panic!("unexpected query {query}"),
    }
}

fn execute(kv: &Mutex<ClusterKv>, statements: Vec<Statement>) -> Bytes {
    let mut kv = kv.lock().unwrap();
    for statement in &statements {
        match statement.query.as_str() {
            CLAIM_FOUNDER_ALLOCATOR => {
                let id = text_param(&statement.params, 0).to_owned();
                kv.allocator.get_or_insert((id, true));
            }
            STEAL_ALLOCATOR => {
                kv.allocator = Some((text_param(&statement.params, 0).to_owned(), false));
            }
            AGE_ALLOCATOR => {
                if let Some((_, quiet)) = &mut kv.allocator {
                    *quiet = true;
                }
            }
            PUSH_ALLOCATOR_READ => {
                kv.allocator_script
                    .push_back((text_param(&statement.params, 0).to_owned(), true));
            }
            PUBLISH_FOUNDING_INGRESS_PROXY_BACKEND => {
                assert_eq!(
                    text_param(&statement.params, 0),
                    INGRESS_PROXY_BACKEND_CLUSTER_KEY
                );
                if kv.ingress_proxy_backend.is_none() {
                    kv.ingress_proxy_backend = Some(text_param(&statement.params, 1).to_owned());
                }
            }
            query if query.starts_with("INSERT INTO machines (id, info,") => {
                kv.machines.insert(
                    text_param(&statement.params, 0).to_owned(),
                    text_param(&statement.params, 1).to_owned(),
                );
            }
            query if query.starts_with("INSERT INTO containers (id, container,") => {
                kv.containers.insert(
                    text_param(&statement.params, 0).to_owned(),
                    text_param(&statement.params, 1).to_owned(),
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

fn events(columns: &[&str], rows: impl IntoIterator<Item = Vec<Value>>) -> Bytes {
    let mut body = serde_json::to_vec(&json!({ "columns": columns })).unwrap();
    for (index, row) in rows.into_iter().enumerate() {
        body.extend(serde_json::to_vec(&json!({ "row": [index as u64 + 1, row] })).unwrap());
    }
    body.extend(br#"{"eoq":{"time":0.0}}"#);
    body.into()
}

pub(crate) async fn age_allocator(store: &ReplicatedStore) {
    store
        .api()
        .execute([super::Statement::new(AGE_ALLOCATOR, [])])
        .await
        .unwrap();
}

/// Next `allocator()` reads: `current`, `current`, then `then` (quiet).
pub(crate) async fn name_allocator_on_reread(
    store: &ReplicatedStore,
    current: &MachineId,
    then: &MachineId,
) {
    store
        .api()
        .execute([
            super::Statement::new(PUSH_ALLOCATOR_READ, [json!(current)]),
            super::Statement::new(PUSH_ALLOCATOR_READ, [json!(current)]),
            super::Statement::new(PUSH_ALLOCATOR_READ, [json!(then)]),
        ])
        .await
        .unwrap();
}
