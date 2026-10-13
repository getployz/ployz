//! Caddy watcher recovery at the Corrosion HTTP boundary.

use super::{ingress, observation, reserved};
use crate::{
    corrosion::{ApiClient, ReplicatedStore, fake_cluster::events},
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
    AdvertisedEndpoint, ContainerId, ContainerObservation, DeploymentLogId, HttpProtocol,
    MACHINE_API_PORT, Machine, MachineId, MachineName, ServingPointer, WireGuardPublicKey,
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

#[tokio::test]
async fn pointer_change_reloads_caddy_with_pointed_upstreams() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let pointed = observation(
        1,
        &machine.id,
        "api",
        Some([10, 210, 1, 2]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    );
    let mut newer = observation(
        2,
        &machine.id,
        "api",
        Some([10, 210, 1, 3]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    );
    newer
        .try_update(|parts| {
            parts.created_at_unix_nanos = 1;
            parts.resolved_spec.container.image = "example.test/image:next".into();
        })
        .unwrap();
    state.containers.lock().unwrap().push(newer);
    let caddyfile = directory.join(CONFIG_FILE);
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(run(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        caddyfile.clone(),
        directory.join("missing-admin.sock"),
        shutdown.clone(),
    ));
    let caddy = wait_for_caddyfile(&caddyfile, "10.210.1.3:80").await;
    assert!(!caddy.contains("10.210.1.2:80"), "{caddy}");

    let key = ServingPointer::key(&pointed.identity());
    let pointer = ServingPointer {
        deploy: DeploymentLogId::parse("d1").unwrap(),
        containers: vec![pointed.container_id],
        previous: None,
        retiring: Vec::new(),
    };
    state.write_pointer(&key, serde_json::to_string(&pointer).unwrap());
    let caddy = wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    assert!(!caddy.contains("10.210.1.3:80"), "{caddy}");

    state.write_pointer(&key, "{\"deploy\":".into());
    let caddy = wait_for_caddyfile(&caddyfile, "10.210.1.3:80").await;
    assert!(!caddy.contains("10.210.1.2:80"), "{caddy}");

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn ingress_never_reads_or_subscribes_to_ingress_routes() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    let key = ServingPointer::key(&first.identity());
    state.write_pointer(&key, pointer("d1", &first));
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

    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, "d1", &first)]);

    let second = observation(
        2,
        &machine.id,
        "api",
        Some([10, 210, 1, 3]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    );
    state.containers.lock().unwrap().push(second.clone());
    state.write_pointer(&key, pointer("d2", &second));
    let flipped = next_load(&mut load_rx).await;
    assert!(flipped.contains("10.210.1.3:80"), "{flipped}");
    state.wait_for_ack_transactions(2).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, "d2", &second)]);

    state.write_certificate("example.com", CHALLENGE);
    let challenged = next_load(&mut load_rx).await;
    assert!(challenged.contains("acme-challenge"), "{challenged}");

    let queries = state.container_queries();
    settle().await;
    pause();
    advance(Duration::from_secs(5)).await;
    settle().await;
    resume();
    timeout(Duration::from_secs(1), async {
        while state.container_queries() == queries {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("watcher did not resnapshot");
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(state.ack_transactions(), 2);
    let statements = state.statements.lock().unwrap().clone();
    assert!(
        statements
            .iter()
            .any(|(kind, sql)| *kind == Sent::Transaction
                && sql.starts_with("INSERT INTO ingress_routes")),
        "{statements:?}"
    );
    for (kind, sql) in &statements {
        assert!(
            *kind == Sent::Transaction || !sql.contains("ingress_routes"),
            "the ingress read its own acks: {kind:?} {sql}"
        );
    }

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn certificate_only_reload_writes_no_ack() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    state.write_pointer(
        &ServingPointer::key(&first.identity()),
        pointer("d1", &first),
    );
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
    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;

    state.write_certificate("example.com", CHALLENGE);
    let challenged = next_load(&mut load_rx).await;
    assert!(challenged.contains("acme-challenge"), "{challenged}");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(state.ack_transactions(), 1);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn ack_written_when_deploy_changes_with_same_routed_set() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    let key = ServingPointer::key(&first.identity());
    state.write_pointer(&key, pointer("d1", &first));
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
    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;

    state.write_pointer(&key, pointer("d2", &first));
    state.wait_for_ack_transactions(2).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, "d2", &first)]);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn ack_written_without_a_caddy_admin() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    state.write_pointer(
        &ServingPointer::key(&first.identity()),
        pointer("d1", &first),
    );
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
    state.wait_for_ack_transactions(1).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, "d1", &first)]);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn no_ack_when_the_caddy_load_fails() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    state.write_pointer(
        &ServingPointer::key(&first.identity()),
        pointer("d1", &first),
    );
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let attempts = Arc::new(AtomicUsize::new(0));
    let admin = RetryAdmin {
        attempts: Arc::clone(&attempts),
        loads: load_tx,
    };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        directory.join(CONFIG_FILE),
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    timeout(Duration::from_millis(500), async {
        while attempts.load(Ordering::SeqCst) == 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first load was not attempted");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(state.ack_transactions(), 0, "a failed load wrote an ack");

    timeout(Duration::from_secs(2), load_rx.recv())
        .await
        .expect("failed load was not retried")
        .unwrap();
    state.wait_for_ack_transactions(1).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, "d1", &first)]);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

const CHALLENGE: &str = r#"{"challenge_token":"LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0","challenge_response":"LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#;

fn pointer(deploy: &str, container: &ContainerObservation) -> String {
    serde_json::to_string(&ServingPointer {
        deploy: DeploymentLogId::parse(deploy).unwrap(),
        containers: vec![container.container_id],
        previous: None,
        retiring: Vec::new(),
    })
    .unwrap()
}

fn ack(machine: &Machine, deploy: &str, container: &ContainerObservation) -> Vec<Value> {
    vec![
        json!(machine.id),
        json!(container.identity().to_string()),
        json!(deploy),
        json!(serde_json::to_string(&[&container.container_id]).unwrap()),
    ]
}

async fn next_load(loads: &mut mpsc::UnboundedReceiver<String>) -> String {
    timeout(Duration::from_secs(2), loads.recv())
        .await
        .expect("Caddy was not loaded")
        .unwrap()
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
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::parse("a".repeat(32)).unwrap(),
        name: MachineName::parse("node-a").unwrap(),
        subnet: "10.210.1.0/24".parse().unwrap(),
        public_key: WireGuardPublicKey([1; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("192.0.2.1:{MACHINE_API_PORT}").parse().unwrap(),
        )],
        runtime: Default::default(),
        build_concurrency: None,
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
        pointers: Arc::new(Mutex::new(Vec::new())),
        pointer_subscriptions: Arc::new(Mutex::new(Vec::new())),
        certificates: Arc::new(Mutex::new(Vec::new())),
        statements: Arc::new(Mutex::new(Vec::new())),
        transactions: Arc::new(Mutex::new(Vec::new())),
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
                .route("/v1/transactions", post(transact))
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
    pointers: Arc<Mutex<Vec<(String, String)>>>,
    pointer_subscriptions: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>>,
    certificates: Arc<Mutex<Vec<(String, String)>>>,
    statements: Arc<Mutex<Vec<(Sent, String)>>>,
    transactions: Arc<Mutex<Vec<Vec<Statement>>>>,
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

    fn api_container(&self) -> ContainerObservation {
        self.containers
            .lock()
            .unwrap()
            .first()
            .expect("watch fixture has an API container")
            .clone()
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

    fn write_pointer(&self, key: &str, value: String) {
        *self.pointers.lock().unwrap() = vec![(key.to_owned(), value)];
        self.pointer_subscriptions
            .lock()
            .unwrap()
            .retain(|subscription| {
                subscription
                    .send(Bytes::from_static(b"{\"change\":{}}\n"))
                    .is_ok()
            });
    }

    fn write_certificate(&self, hostname: &str, body: &str) {
        *self.certificates.lock().unwrap() = vec![(hostname.to_owned(), body.to_owned())];
        self.send_certificates(b"{\"change\":{}}\n");
    }

    fn container_queries(&self) -> usize {
        self.statements
            .lock()
            .unwrap()
            .iter()
            .filter(|(kind, sql)| *kind == Sent::Query && sql.contains("FROM containers"))
            .count()
    }

    fn ack_transactions(&self) -> usize {
        self.transactions.lock().unwrap().len()
    }

    fn last_acks(&self) -> Vec<Vec<Value>> {
        self.transactions
            .lock()
            .unwrap()
            .last()
            .expect("an ack transaction was written")
            .iter()
            .map(|statement| {
                assert!(
                    statement.query.contains("ingress_routes"),
                    "{}",
                    statement.query
                );
                statement.params.clone()
            })
            .collect()
    }

    async fn wait_for_ack_transactions(&self, count: usize) {
        timeout(Duration::from_secs(5), async {
            while self.ack_transactions() < count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("ack transaction {count} was never written"));
    }

    fn send_certificates(&self, event: &'static [u8]) {
        self.certificate_subscriptions
            .lock()
            .unwrap()
            .retain(|subscription| subscription.send(Bytes::from_static(event)).is_ok());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Sent {
    Query,
    Subscription,
    Transaction,
}

#[derive(Clone, Deserialize)]
struct Statement {
    query: String,
    params: Vec<Value>,
}

async fn query(State(state): State<WatchState>, body: Bytes) -> Bytes {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    state
        .statements
        .lock()
        .unwrap()
        .push((Sent::Query, statement.query.clone()));
    if statement.query == "SELECT id, machine_id, container FROM containers ORDER BY id" {
        let containers = state.containers.lock().unwrap();
        events(
            &["id", "machine_id", "container"],
            containers.iter().map(|container| {
                vec![
                    json!(container.container_id),
                    json!(container.machine_id),
                    json!(serde_json::to_string(container).unwrap()),
                ]
            }),
        )
    } else if statement.query == "SELECT hostname, body FROM certificates ORDER BY hostname" {
        let certificates = state.certificates.lock().unwrap();
        events(
            &["hostname", "body"],
            certificates
                .iter()
                .map(|(hostname, body)| vec![json!(hostname), json!(body)]),
        )
    } else if statement.query
        == "SELECT key AS serving_key, value FROM cluster WHERE key LIKE 'serving/%'"
    {
        let pointers = state.pointers.lock().unwrap();
        events(
            &["serving_key", "value"],
            pointers
                .iter()
                .map(|(key, value)| vec![json!(key), json!(value)]),
        )
    } else {
        panic!("unexpected query {}", statement.query);
    }
}

async fn subscribe(State(state): State<WatchState>, body: Bytes) -> Response {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    state
        .statements
        .lock()
        .unwrap()
        .push((Sent::Subscription, statement.query.clone()));
    let (columns, subscriptions, stall) =
        if statement.query == "SELECT id, machine_id, container FROM containers" {
            state.container_opens.fetch_add(1, Ordering::SeqCst);
            (
                &["id", "machine_id", "container"][..],
                &state.container_subscriptions,
                state.stall_container.load(Ordering::SeqCst),
            )
        } else if statement.query == "SELECT hostname, body FROM certificates" {
            (
                &["hostname", "body"][..],
                &state.certificate_subscriptions,
                false,
            )
        } else if statement.query
            == "SELECT key AS serving_key, value FROM cluster WHERE key LIKE 'serving/%'"
        {
            (
                &["serving_key", "value"][..],
                &state.pointer_subscriptions,
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

async fn transact(State(state): State<WatchState>, body: Bytes) -> Bytes {
    let statements: Vec<Statement> = serde_json::from_slice(&body).unwrap();
    state.statements.lock().unwrap().extend(
        statements
            .iter()
            .map(|statement| (Sent::Transaction, statement.query.clone())),
    );
    let results = vec![json!({"rows_affected": 1, "time": 0.0}); statements.len()];
    state.transactions.lock().unwrap().push(statements);
    serde_json::to_vec(&json!({ "results": results, "time": 0.0 }))
        .unwrap()
        .into()
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
