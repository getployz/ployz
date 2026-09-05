//! Caddy watcher recovery at the Corrosion HTTP boundary.

use super::{ingress, observation, reserved};
use crate::{
    corrosion::{ApiClient, ReplicatedStore},
    ingress::caddy::{CONFIG_FILE, CaddyAdmin, Error as CaddyError, run},
    ingress::watch_caddy,
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
    AdvertisedEndpoint, ContainerId, ContainerObservation, HttpProtocol, MACHINE_API_PORT, Machine,
    MachineId, MachineName, ManagementAddress, WireGuardPublicKey,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    convert::Infallible,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::mpsc,
    task::JoinHandle,
    time::{advance, pause, resume, timeout},
};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn reconciles_resnapshots_and_retries_protocol_failures() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
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

#[tokio::test]
async fn first_snapshot_loads_immediately_and_bursts_load_only_the_latest_snapshot() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let shutdown = CancellationToken::new();
    let config_file = directory.join(CONFIG_FILE);
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        config_file,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    let first = timeout(Duration::from_millis(250), load_rx.recv())
        .await
        .expect("first load waited for debounce")
        .unwrap();
    assert!(first.contains("10.210.1.2:80"), "{first}");

    state.mutate(|container| {
        container
            .labels
            .insert("unrelated.example/changed".into(), "true".into());
    });
    state.send(b"{\"change\":{}}\n");
    assert!(
        timeout(Duration::from_millis(500), load_rx.recv())
            .await
            .is_err(),
        "unrelated Container Observation churn loaded Caddy"
    );

    pause();
    state.replace(observation(
        2,
        &machine.id,
        "api",
        Some([10, 210, 1, 3]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(b"{\"change\":{}}\n");
    settle().await;
    advance(Duration::from_millis(100)).await;
    state.replace(observation(
        3,
        &machine.id,
        "api",
        Some([10, 210, 1, 4]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(
        b"{\"columns\":[\"id\",\"container\"]}\n{\"row\":[1,[\"replacement\",\"snapshot\"]]}\n",
    );
    settle().await;
    advance(Duration::from_millis(200)).await;
    settle().await;
    assert!(
        load_rx.try_recv().is_err(),
        "incomplete snapshot loaded before it ended"
    );
    state.replace(observation(
        4,
        &machine.id,
        "api",
        Some([10, 210, 1, 5]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(b"{\"row\":[1,[\"replacement\",\"snapshot\"]]}\n{\"eoq\":{\"time\":0.0}}\n");
    settle().await;
    advance(Duration::from_millis(300)).await;
    resume();
    let after_snapshot = timeout(Duration::from_secs(1), load_rx.recv())
        .await
        .expect("completed snapshot did not load after quiet")
        .unwrap();
    assert!(after_snapshot.contains("10.210.1.5:80"), "{after_snapshot}");
    assert!(
        !after_snapshot.contains("10.210.1.4:80"),
        "{after_snapshot}"
    );

    pause();
    state.replace(observation(
        5,
        &machine.id,
        "api",
        Some([10, 210, 1, 6]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(b"{\"change\":{}}\n");
    settle().await;
    advance(Duration::from_millis(100)).await;
    state.replace(observation(
        6,
        &machine.id,
        "api",
        Some([10, 210, 1, 7]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send_certificates(b"{\"change\":{}}\n");
    settle().await;
    advance(Duration::from_millis(100)).await;
    state.replace(observation(
        7,
        &machine.id,
        "api",
        Some([10, 210, 1, 8]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(b"{\"change\":{}}\n");
    settle().await;

    advance(Duration::from_millis(299)).await;
    settle().await;
    assert!(load_rx.try_recv().is_err(), "burst loaded before quiet");
    advance(Duration::from_millis(1)).await;
    resume();
    let latest = timeout(Duration::from_secs(1), load_rx.recv())
        .await
        .expect("burst did not load after quiet")
        .unwrap();
    assert!(latest.contains("10.210.1.8:80"), "{latest}");
    assert!(!latest.contains("10.210.1.6:80"), "{latest}");
    assert!(!latest.contains("10.210.1.7:80"), "{latest}");
    assert!(load_rx.try_recv().is_err(), "burst produced extra loads");

    pause();
    advance(Duration::from_secs(2)).await;
    state.replace(observation(
        8,
        &machine.id,
        "api",
        Some([10, 210, 1, 9]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(b"{\"change\":{}}\n");
    settle().await;
    advance(Duration::from_millis(300)).await;
    resume();
    let isolated = timeout(Duration::from_secs(1), load_rx.recv())
        .await
        .expect("isolated update did not load")
        .unwrap();
    assert!(isolated.contains("10.210.1.9:80"), "{isolated}");

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn replacing_the_local_caddy_process_reloads_an_unchanged_projection() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine,
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        directory.join(CONFIG_FILE),
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    timeout(Duration::from_millis(250), load_rx.recv())
        .await
        .expect("first Caddy process was not loaded")
        .unwrap();
    state.replace_ingress_id(ContainerId::parse("e".repeat(64)).unwrap());
    state.send(b"{\"change\":{}}\n");

    timeout(Duration::from_secs(1), load_rx.recv())
        .await
        .expect("replacement Caddy process did not receive the unchanged projection")
        .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn periodic_resnapshot_recovers_a_missed_change_notification() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        directory.join(CONFIG_FILE),
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    timeout(Duration::from_millis(250), load_rx.recv())
        .await
        .expect("first snapshot was not loaded")
        .unwrap();
    settle().await;
    pause();
    state.replace(observation(
        2,
        &machine.id,
        "api",
        Some([10, 210, 1, 3]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    advance(Duration::from_secs(5)).await;
    settle().await;
    resume();

    let recovered = timeout(Duration::from_secs(1), load_rx.recv())
        .await
        .expect("periodic resnapshot did not recover the missed notification")
        .unwrap();
    assert!(recovered.contains("10.210.1.3:80"), "{recovered}");
    settle().await;

    pause();
    state.replace(observation(
        3,
        &machine.id,
        "api",
        Some([10, 210, 1, 4]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(
        b"{\"columns\":[\"id\",\"container\"]}\n{\"row\":[1,[\"replacement\",\"snapshot\"]]}\n",
    );
    settle().await;
    advance(Duration::from_millis(300)).await;
    settle().await;
    assert!(
        load_rx.try_recv().is_err(),
        "an incomplete subscription snapshot was applied after debounce"
    );
    advance(Duration::from_secs(4)).await;
    settle().await;
    assert!(
        load_rx.try_recv().is_err(),
        "an incomplete subscription snapshot bypassed the bounded resnapshot wait"
    );
    advance(Duration::from_secs(1)).await;
    settle().await;
    resume();

    let recovered = timeout(Duration::from_secs(1), load_rx.recv())
        .await
        .expect("periodic resnapshot did not recover the incomplete subscription snapshot")
        .unwrap();
    assert!(recovered.contains("10.210.1.4:80"), "{recovered}");
    assert_eq!(
        state.container_opens.load(Ordering::SeqCst),
        2,
        "stuck snapshot recovery must replace the subscription"
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn transient_apply_failure_retries_without_a_replicated_event() {
    let WatcherFixture {
        machine,
        address,
        server,
        directory,
        ..
    } = watcher_fixture().await;
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let attempts = Arc::new(AtomicUsize::new(0));
    let admin = RetryAdmin {
        attempts: Arc::clone(&attempts),
        loads: load_tx,
    };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine,
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        directory.join(CONFIG_FILE),
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    timeout(Duration::from_secs(2), load_rx.recv())
        .await
        .expect("failed apply was not retried without another event")
        .unwrap();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

struct WatcherFixture {
    machine: Machine,
    state: WatchState,
    address: SocketAddr,
    server: JoinHandle<()>,
    directory: PathBuf,
}

async fn watcher_fixture() -> WatcherFixture {
    let machine = Machine {
        id: MachineId::parse("a".repeat(32)).unwrap(),
        name: MachineName::parse("node-a").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        management_address: ManagementAddress("fdcc::1".parse().unwrap()),
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("192.0.2.1:{MACHINE_API_PORT}").parse().unwrap(),
        )],
        runtime: Default::default(),
    };
    let state = WatchState {
        containers: Arc::new(Mutex::new(vec![
            observation(
                1,
                &machine.id,
                "api",
                Some([10, 210, 1, 2]),
                vec![ingress("example.com", 80, HttpProtocol::Http)],
            ),
            reserved(observation(9, &machine.id, "ingress", None, Vec::new())),
        ])),
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
    let directory =
        std::env::temp_dir().join(format!("ployz-caddy-watch-test-{}", MachineId::random()));
    WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    }
}

#[derive(Clone)]
struct TestAdmin {
    loads: mpsc::UnboundedSender<String>,
}

impl CaddyAdmin for TestAdmin {
    async fn adapt(&self, caddyfile: &str) -> Result<String, CaddyError> {
        Ok(json!({ "caddyfile": caddyfile }).to_string())
    }

    async fn load(&self, json: &str) -> Result<(), CaddyError> {
        let caddyfile = serde_json::from_str::<Value>(json)
            .unwrap()
            .get("caddyfile")
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        self.loads.send(caddyfile).unwrap();
        Ok(())
    }
}

#[derive(Clone)]
struct RetryAdmin {
    attempts: Arc<AtomicUsize>,
    loads: mpsc::UnboundedSender<()>,
}

impl CaddyAdmin for RetryAdmin {
    async fn adapt(&self, _caddyfile: &str) -> Result<String, CaddyError> {
        Ok("{}".into())
    }

    async fn load(&self, _json: &str) -> Result<(), CaddyError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(CaddyError::Admin("temporary load failure".into()));
        }
        self.loads.send(()).unwrap();
        Ok(())
    }
}

async fn settle() {
    for _ in 0..10 {
        tokio::task::yield_now().await;
    }
}

#[derive(Clone)]
struct WatchState {
    containers: Arc<Mutex<Vec<ContainerObservation>>>,
    container_opens: Arc<AtomicUsize>,
    stall_container: Arc<AtomicBool>,
    container_subscriptions: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>>,
    certificate_subscriptions: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>>,
}

impl WatchState {
    fn replace(&self, container: ContainerObservation) {
        *self
            .containers
            .lock()
            .unwrap()
            .first_mut()
            .expect("watch fixture has an API container") = container;
    }

    fn mutate(&self, mutate: impl FnOnce(&mut ployz_core::ContainerObservationParts)) {
        self.containers
            .lock()
            .unwrap()
            .first_mut()
            .expect("watch fixture has an API container")
            .try_update(mutate)
            .unwrap();
    }

    fn replace_ingress_id(&self, container_id: ContainerId) {
        self.containers
            .lock()
            .unwrap()
            .get_mut(1)
            .expect("watch fixture has an ingress container")
            .try_update(|parts| parts.container_id = container_id)
            .unwrap();
    }

    fn send(&self, event: &'static [u8]) {
        self.container_subscriptions
            .lock()
            .unwrap()
            .retain(|subscription| subscription.send(Bytes::from_static(event)).is_ok());
    }

    fn send_certificates(&self, event: &'static [u8]) {
        self.certificate_subscriptions
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
    if statement.query == "SELECT value FROM cluster WHERE key = ?" {
        query_events(&["value"], [vec![json!("caddy")]])
    } else if statement.query == "SELECT id, container FROM containers ORDER BY id" {
        let containers = state.containers.lock().unwrap();
        query_events(
            &["id", "container"],
            containers.iter().map(|container| {
                vec![
                    json!(container.container_id),
                    json!(serde_json::to_string(container).unwrap()),
                ]
            }),
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
