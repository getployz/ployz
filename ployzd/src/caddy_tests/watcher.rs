//! Caddy watcher recovery at the Corrosion HTTP boundary.

use super::{ingress, observation};
use crate::{
    caddy::{CONFIG_FILE, run},
    corrosion::{ApiClient, ReplicatedStore},
};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    response::Response,
    routing::post,
};
use futures_util::StreamExt;
use ployz_core::{
    AdvertisedEndpoint, ContainerObservation, HttpProtocol, Machine, MachineId, MachineName,
    ManagementAddress, WireGuardPublicKey,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn reconciles_resnapshots_and_retries_protocol_failures() {
    let machine = Machine {
        id: MachineId::parse("a".repeat(32)).unwrap(),
        name: MachineName::parse("node-a").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        management_address: ManagementAddress("fdcc::1".parse().unwrap()),
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51000".parse().unwrap())],
        runtime: Default::default(),
    };
    let state = WatchState {
        container: Arc::new(Mutex::new(observation(
            1,
            &machine.id,
            "api",
            Some([10, 210, 1, 2]),
            vec![ingress("example.com", 80, HttpProtocol::Http)],
        ))),
        container_opens: Arc::new(AtomicUsize::new(0)),
        stall_container: Arc::new(AtomicBool::new(false)),
        container_subscriptions: Arc::new(Mutex::new(Vec::new())),
        certificate_subscriptions: Arc::new(Mutex::new(Vec::new())),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_state = state.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new()
                .route("/v1/queries", post(query))
                .route("/v1/subscriptions", post(subscribe))
                .with_state(server_state),
        )
        .await
        .unwrap();
    });
    let directory = std::env::temp_dir().join(format!(
        "ployz-caddy-watch-retry-test-{}",
        MachineId::random()
    ));
    let caddyfile = directory.join(CONFIG_FILE);
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(run(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        caddyfile.clone(),
        directory.join("missing-admin.sock"),
        shutdown.clone(),
    ));

    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    state.replace(observation(
        2,
        &machine.id,
        "api",
        Some([10, 210, 1, 3]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(b"{\"change\":{}}\n");
    wait_for_caddyfile(&caddyfile, "10.210.1.3:80").await;

    state.replace(observation(
        3,
        &machine.id,
        "api",
        Some([10, 210, 1, 4]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(
        b"{\"columns\":[\"id\",\"container\"]}\n{\"row\":[1,[\"replacement\",\"snapshot\"]]}\n{\"eoq\":{\"time\":0.0}}\n",
    );
    let caddy = wait_for_caddyfile(&caddyfile, "10.210.1.4:80").await;
    assert!(!caddy.contains("10.210.1.3:80"), "{caddy}");

    state.replace(observation(
        4,
        &machine.id,
        "api",
        Some([10, 210, 1, 5]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(b"{\"change\":{}}\n");
    let caddy = wait_for_caddyfile(&caddyfile, "10.210.1.5:80").await;
    assert!(!caddy.contains("10.210.1.4:80"), "{caddy}");

    state.send(b"{\"row\":[1,[]]}\n");
    state.replace(observation(
        5,
        &machine.id,
        "api",
        Some([10, 210, 1, 6]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.container_opens.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Caddy watcher did not resubscribe");
    wait_for_caddyfile(&caddyfile, "10.210.1.6:80").await;
    assert!(!watcher.is_finished(), "watcher failure exited the plane");

    state.stall_container.store(true, Ordering::SeqCst);
    state.send(b"{\"row\":[1,[]]}\n");
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.container_opens.load(Ordering::SeqCst) < 3 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Caddy watcher did not begin a stalled subscription");
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(1), watcher)
        .await
        .expect("Caddy watcher ignored shutdown during subscription")
        .unwrap()
        .unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[derive(Clone)]
struct WatchState {
    container: Arc<Mutex<ContainerObservation>>,
    container_opens: Arc<AtomicUsize>,
    stall_container: Arc<AtomicBool>,
    container_subscriptions: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>>,
    certificate_subscriptions: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>>,
}

impl WatchState {
    fn replace(&self, container: ContainerObservation) {
        *self.container.lock().unwrap() = container;
    }

    fn send(&self, event: &'static [u8]) {
        self.container_subscriptions
            .lock()
            .unwrap()
            .retain(|subscription| subscription.send(Bytes::from_static(event)).is_ok());
    }
}

#[derive(Deserialize)]
struct Statement {
    query: String,
    #[serde(rename = "params")]
    _params: Vec<Value>,
}

async fn query(State(state): State<WatchState>, body: Bytes) -> Bytes {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    if statement.query == "SELECT id, container FROM containers ORDER BY id" {
        let container = state.container.lock().unwrap().clone();
        query_events(
            &["id", "container"],
            [vec![
                json!(container.container_id),
                json!(serde_json::to_string(&container).unwrap()),
            ]],
        )
    } else if statement.query == "SELECT hostname, body FROM certificates ORDER BY hostname" {
        query_events(&["hostname", "body"], [])
    } else {
        panic!("unexpected query {}", statement.query);
    }
}

async fn subscribe(State(state): State<WatchState>, body: Bytes) -> Response {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    let (columns, subscriptions, stall) =
        if statement.query == "SELECT id, container FROM containers" {
            state.container_opens.fetch_add(1, Ordering::SeqCst);
            (
                &["id", "container"][..],
                &state.container_subscriptions,
                state.stall_container.load(Ordering::SeqCst),
            )
        } else if statement.query == "SELECT hostname, body FROM certificates" {
            (
                &["hostname", "body"][..],
                &state.certificate_subscriptions,
                false,
            )
        } else {
            panic!("unexpected subscription {}", statement.query);
        };
    let (sender, receiver) = mpsc::unbounded_channel();
    if !stall {
        sender
            .send(Bytes::from(format!(
                "{}\n{{\"eoq\":{{\"time\":0.0}}}}\n",
                json!({ "columns": columns })
            )))
            .unwrap();
    }
    subscriptions.lock().unwrap().push(sender);
    Response::new(Body::from_stream(
        UnboundedReceiverStream::new(receiver).map(Ok::<_, Infallible>),
    ))
}

fn query_events(columns: &[&str], rows: impl IntoIterator<Item = Vec<Value>>) -> Bytes {
    let mut body = serde_json::to_vec(&json!({ "columns": columns })).unwrap();
    for (index, row) in rows.into_iter().enumerate() {
        body.extend(serde_json::to_vec(&json!({ "row": [index as u64 + 1, row] })).unwrap());
    }
    body.extend(br#"{"eoq":{"time":0.0}}"#);
    body.into()
}

async fn wait_for_caddyfile(path: &Path, expected: &str) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(caddyfile) = std::fs::read_to_string(path)
                && caddyfile.contains(expected)
            {
                return caddyfile;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Caddyfile never contained {expected}"))
}
