//! Caddy watcher recovery at the Corrosion HTTP boundary.

use super::{ingress, observation, reserved};
use crate::{
    corrosion::{
        ApiClient, CertificateMaterial, CertificateRow, Error as CorrosionError, ReplicatedStore,
        fake_cluster::events,
    },
    ingress::caddy::{CONFIG_FILE, CaddyAdmin, Error as CaddyError, run},
    ingress::{WATCH_DEBOUNCE, watch_caddy},
};
use axum::{
    Router,
    body::{Body, Bytes},
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use futures_util::StreamExt;
use ployz_core::{
    AdvertisedEndpoint, ContainerAddress, ContainerId, ContainerObservation,
    ContainerRuntimeObservation, CopyRotation, HttpProtocol, MACHINE_API_PORT, Machine, MachineId,
    MachineName, WireGuardPublicKey,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    sync::{mpsc, watch},
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
        no_down,
        caddyfile.clone(),
        directory.join("missing-admin.sock"),
        tokio::sync::watch::channel(Default::default()).0,
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
    state
        .wait_until("Caddy watcher did not resubscribe", |state| {
            state.container_opens.load(Ordering::SeqCst) >= 2
        })
        .await;
    wait_for_caddyfile(&caddyfile, "10.210.1.6:80").await;
    assert!(!watcher.is_finished(), "watcher failure exited the plane");

    state.stall_container.store(true, Ordering::SeqCst);
    state.send(b"{\"row\":[1,[]]}\n");
    state
        .wait_until(
            "Caddy watcher did not begin a stalled subscription",
            |state| state.container_opens.load(Ordering::SeqCst) >= 3,
        )
        .await;
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
        no_down,
        config_file,
        tokio::sync::watch::channel(Default::default()).0,
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
    advance(WATCH_DEBOUNCE / 3).await;
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
    advance(WATCH_DEBOUNCE * 2).await;
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
    advance(WATCH_DEBOUNCE).await;
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
    advance(WATCH_DEBOUNCE / 3).await;
    state.replace(observation(
        6,
        &machine.id,
        "api",
        Some([10, 210, 1, 7]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send_certificates(b"{\"change\":{}}\n");
    settle().await;
    advance(WATCH_DEBOUNCE / 3).await;
    state.replace(observation(
        7,
        &machine.id,
        "api",
        Some([10, 210, 1, 8]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    state.send(b"{\"change\":{}}\n");
    settle().await;

    advance(WATCH_DEBOUNCE - Duration::from_millis(1)).await;
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
    advance(WATCH_DEBOUNCE).await;
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
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
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
async fn a_write_without_an_admin_is_loaded_once_the_admin_answers() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let admin_up = Arc::new(AtomicBool::new(false));
    let caddyfile = directory.join(CONFIG_FILE);
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine,
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        {
            let admin_up = Arc::clone(&admin_up);
            move || std::future::ready(Ok(admin_up.load(Ordering::SeqCst).then(|| admin.clone())))
        },
    ));

    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    admin_up.store(true, Ordering::SeqCst);
    state.mutate(|container| {
        container
            .labels
            .insert("unrelated.example/changed".into(), "true".into());
    });
    state.send(b"{\"change\":{}}\n");

    let loaded = timeout(Duration::from_secs(1), load_rx.recv())
        .await
        .expect("an unchanged projection written without an admin was never loaded")
        .unwrap();
    assert!(loaded.contains("10.210.1.2:80"), "{loaded}");

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_store_read_error_does_not_reload_an_unchanged_projection() {
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
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    timeout(Duration::from_millis(250), load_rx.recv())
        .await
        .expect("first load waited for debounce")
        .unwrap();
    state.fail_queries.store(true, Ordering::SeqCst);
    state.send(b"{\"change\":{}}\n");
    state.wait_for_container_queries(2).await;
    state.fail_queries.store(false, Ordering::SeqCst);
    state.send(b"{\"change\":{}}\n");
    state.wait_for_container_queries(3).await;

    assert!(
        timeout(Duration::from_millis(500), load_rx.recv())
            .await
            .is_err(),
        "a store read error reloaded an unchanged projection"
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_failed_admin_probe_on_a_loaded_input_neither_reloads_nor_holds_back_acks() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let probes = Arc::new(AtomicUsize::new(0));
    state
        .lose_next_transaction_response
        .store(true, Ordering::SeqCst);
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        {
            let probes = Arc::clone(&probes);
            move || {
                std::future::ready(if probes.fetch_add(1, Ordering::SeqCst) == 0 {
                    Ok(Some(admin.clone()))
                } else {
                    Err(CaddyError::Admin("admin probe timed out".into()))
                })
            }
        },
    ));

    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;
    state.mutate(|container| {
        container
            .labels
            .insert("unrelated.example/changed".into(), "true".into());
    });
    state.send(b"{\"change\":{}}\n");

    state.wait_for_ack_transactions(2).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, &[&first])]);
    assert!(
        load_rx.try_recv().is_err(),
        "an unchanged, loaded input was posted to /load again"
    );
    assert_eq!(
        probes.load(Ordering::SeqCst),
        1,
        "an unchanged, loaded input probed the admin"
    );

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
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
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
    let attempts = Arc::new(watch::Sender::new(0));
    let admin = RetryAdmin {
        attempts: Arc::clone(&attempts),
        loads: load_tx,
        release: None,
    };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine,
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    timeout(Duration::from_secs(2), load_rx.recv())
        .await
        .expect("failed apply was not retried without another event")
        .unwrap();
    assert_eq!(*attempts.borrow(), 2);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn rotation_flips_reload_caddy_with_in_upstreams() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    let mut second = observation(
        2,
        &machine.id,
        "api",
        Some([10, 210, 1, 3]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    );
    second
        .try_update(|parts| parts.rotation = Some(CopyRotation::Pending))
        .unwrap();
    state.containers.lock().unwrap().push(second.clone());
    let caddyfile = directory.join(CONFIG_FILE);
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(run(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        directory.join("missing-admin.sock"),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
    ));
    let caddy = wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    assert!(!caddy.contains("10.210.1.3:80"), "{caddy}");

    state.set_rotation(second.container_id, CopyRotation::In);
    let caddy = wait_for_caddyfile(&caddyfile, "10.210.1.3:80").await;
    assert!(caddy.contains("10.210.1.2:80"), "{caddy}");

    state.set_rotation(first.container_id, CopyRotation::Out);
    wait_for_caddyfile_without(&caddyfile, "10.210.1.2:80").await;

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_down_machine_leaves_caddy_while_another_copy_serves() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let remote = MachineId::parse("b".repeat(32)).unwrap();
    state.containers.lock().unwrap().push(observation(
        2,
        &remote,
        "api",
        Some([10, 210, 2, 2]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    ));
    let down = Arc::new(Mutex::new(BTreeSet::new()));
    let load_down = {
        let down = Arc::clone(&down);
        move || std::future::ready(Ok(down.lock().unwrap().clone()))
    };
    let caddyfile = directory.join(CONFIG_FILE);
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(run(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        load_down,
        caddyfile.clone(),
        directory.join("missing-admin.sock"),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
    ));
    let caddy = wait_for_caddyfile(&caddyfile, "10.210.2.2:80").await;
    assert!(caddy.contains("10.210.1.2:80"), "{caddy}");

    down.lock().unwrap().insert(remote);
    state.send(b"{\"change\":{}}\n");
    let caddy = wait_for_caddyfile_without(&caddyfile, "10.210.2.2:80").await;
    assert!(caddy.contains("10.210.1.2:80"), "{caddy}");

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
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let (route_publishes, mut published) = watch::channel(Default::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, &[&first])]);

    let second = observation(
        2,
        &machine.id,
        "api",
        Some([10, 210, 1, 3]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    );
    state.containers.lock().unwrap().push(second.clone());
    state.set_rotation(first.container_id, CopyRotation::Out);
    let flipped = next_load(&mut load_rx).await;
    assert!(flipped.contains("10.210.1.3:80"), "{flipped}");
    assert!(!flipped.contains("10.210.1.2:80"), "{flipped}");
    state.wait_for_ack_transactions(2).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, &[&second])]);

    state.write_certificate("example.com", CHALLENGE);
    let challenged = next_load(&mut load_rx).await;
    assert!(challenged.contains("acme-challenge"), "{challenged}");

    let passes = timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.count == 3),
    )
    .await
    .expect("the challenge pass never finished")
    .unwrap()
    .count;
    let queries = state.container_queries();
    pause();
    advance(Duration::from_secs(5)).await;
    settle().await;
    resume();
    state.wait_for_container_queries(queries + 1).await;
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.count > passes),
    )
    .await
    .expect("the resnapshot pass never finished")
    .unwrap();

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
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let (route_publishes, mut published) = watch::channel(Default::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine,
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));
    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;
    let first_pass = timeout(
        Duration::from_secs(5),
        published.wait_for(|publishes| publishes.count >= 1),
    )
    .await
    .expect("startup's publish pass never finished")
    .unwrap()
    .count;

    state.write_certificate("example.com", CHALLENGE);
    let challenged = next_load(&mut load_rx).await;
    assert!(challenged.contains("acme-challenge"), "{challenged}");
    timeout(
        Duration::from_secs(5),
        published.wait_for(|publishes| publishes.count > first_pass),
    )
    .await
    .expect("the reload's publish pass never finished")
    .unwrap();
    assert_eq!(state.ack_transactions(), 1);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn empty_route_writes_empty_ack() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));
    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;

    state.set_rotation(first.container_id, CopyRotation::Out);
    let emptied = next_load(&mut load_rx).await;
    assert!(emptied.contains("error NOT_RUNNING 503"), "{emptied}");
    state.wait_for_ack_transactions(2).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, &[])]);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_sole_copy_withdrawn_for_its_stop_leaves_caddy_and_keeps_its_ack() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));
    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;

    // How a Volume move's cut leaves its sole copy before it stops: withdrawn, still In.
    state.mutate(|parts| {
        parts.runtime = ContainerRuntimeObservation::Running {
            health: ployz_core::HealthObservation::Stopping,
        };
    });
    state.send(b"{\"change\":{}}\n");
    let withdrawn = next_load(&mut load_rx).await;
    assert!(withdrawn.contains("error NOT_RUNNING 503"), "{withdrawn}");
    assert!(!withdrawn.contains("10.210.1.2:80"), "{withdrawn}");

    state.mutate(|parts| {
        parts.runtime = ContainerRuntimeObservation::Running {
            health: ployz_core::HealthObservation::Healthy,
        };
    });
    state.send(b"{\"change\":{}}\n");
    let running = next_load(&mut load_rx).await;
    assert!(running.contains("10.210.1.2:80"), "{running}");
    assert_eq!(state.ack_transactions(), 1);
    assert_eq!(state.last_acks(), vec![ack(&machine, &[&first])]);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn removed_service_deletes_its_ack() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));
    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;

    state
        .containers
        .lock()
        .unwrap()
        .retain(|container| container.container_id != first.container_id);
    state.send(b"{\"change\":{}}\n");
    state.wait_for_ack_transactions(2).await;
    let transactions = state.transactions.lock().unwrap().clone();
    let [_, deletes] = transactions.as_slice() else {
        panic!("expected two ack transactions: {}", transactions.len());
    };
    let [deleted] = deletes.as_slice() else {
        panic!("expected one delete statement: {}", deletes.len());
    };
    assert!(
        deleted.query.starts_with("DELETE FROM ingress_routes"),
        "{}",
        deleted.query
    );
    assert_eq!(
        deleted.params,
        vec![json!(machine.id), json!(first.identity().to_string())]
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn committed_ack_with_a_lost_response_is_rewritten_when_the_route_returns() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    let mut second = observation(
        2,
        &machine.id,
        "api",
        Some([10, 210, 1, 3]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    );
    second
        .try_update(|parts| parts.rotation = Some(CopyRotation::Pending))
        .unwrap();
    state.containers.lock().unwrap().push(second.clone());
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));
    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;

    state
        .lose_next_transaction_response
        .store(true, Ordering::SeqCst);
    state.set_rotation(second.container_id, CopyRotation::In);
    state.wait_for_ack_transactions(2).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, &[&first, &second])]);

    state.set_rotation(second.container_id, CopyRotation::Out);
    state.wait_for_ack_transactions(3).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, &[&first])]);

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
    let caddyfile = directory.join(CONFIG_FILE);
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(run(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        directory.join("missing-admin.sock"),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
    ));

    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    state.wait_for_ack_transactions(1).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, &[&first])]);

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
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let attempts = Arc::new(watch::Sender::new(0));
    let release = Arc::new(tokio::sync::Notify::new());
    let admin = RetryAdmin {
        attempts: Arc::clone(&attempts),
        loads: load_tx,
        release: Some(Arc::clone(&release)),
    };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    timeout(Duration::from_secs(2), load_rx.recv())
        .await
        .expect("failed load was not retried")
        .unwrap();
    assert_eq!(*attempts.borrow(), 2);
    // The retry is held, so only the failed load could have written an ack yet.
    assert_eq!(state.ack_transactions(), 0, "a failed load wrote an ack");
    release.notify_one();
    state.wait_for_ack_transactions(1).await;
    assert_eq!(state.last_acks(), vec![ack(&machine, &[&first])]);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn route_publishes_count_only_this_process() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let first = state.api_container();
    let stale = observation(
        3,
        &machine.id,
        "api",
        Some([10, 210, 1, 4]),
        vec![ingress("example.com", 80, HttpProtocol::Http)],
    );
    state.transactions.lock().unwrap().push(vec![Statement {
        query: "INSERT INTO ingress_routes (machine_id, service, containers) VALUES (?, ?, ?)"
            .into(),
        params: ack(&machine, &[&stale]),
    }]);
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let attempts = Arc::new(watch::Sender::new(0));
    let admin = RetryAdmin {
        attempts: Arc::clone(&attempts),
        loads: load_tx,
        release: None,
    };
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    timeout(
        Duration::from_millis(500),
        attempts.subscribe().wait_for(|attempts| *attempts > 0),
    )
    .await
    .expect("first load was not attempted")
    .unwrap();
    assert_eq!(state.ack_transactions(), 1, "only the stale row exists");
    assert_eq!(
        *published.borrow_and_update(),
        crate::ingress::PublishedRoutes::default(),
        "a stale row is not this process's publish"
    );

    timeout(Duration::from_secs(2), load_rx.recv())
        .await
        .expect("failed load was not retried")
        .unwrap();
    let first_count = timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.count >= 1),
    )
    .await
    .expect("the first publish counts")
    .unwrap()
    .count;
    assert_eq!(
        published.borrow().routes,
        BTreeMap::from([(first.identity(), BTreeSet::from([first.container_id]))]),
        "the publish carries what this process acked"
    );
    assert_eq!(state.ack_transactions(), 2);
    assert_eq!(state.last_acks(), vec![ack(&machine, &[&first])]);

    state.set_rotation(first.container_id, CopyRotation::Out);
    state.wait_for_ack_transactions(3).await;
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.count > first_count),
    )
    .await
    .expect("a later publish counts again")
    .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_follow_the_applied_config_even_when_the_ack_write_fails() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let api = state.api_container();
    state.containers.lock().unwrap().push(observation(
        4,
        &machine.id,
        "db",
        Some([10, 210, 1, 5]),
        Vec::new(),
    ));
    state
        .lose_next_transaction_response
        .store(true, Ordering::SeqCst);
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let (route_publishes, published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    next_load(&mut load_rx).await;
    state.wait_for_ack_transactions(1).await;
    settle().await;
    let published = published.borrow().clone();
    assert_eq!(
        published.count, 0,
        "the lost ack write counted as a publish"
    );
    assert_eq!(
        published.addresses,
        Some(BTreeMap::from([(
            api.identity(),
            BTreeMap::from([(api.container_id, api.address.unwrap())])
        )])),
        "Caddy routes the API copy, and the db copy has no ingress to route"
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_follow_a_loaded_config_even_when_the_caddyfile_write_fails() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let api = state.api_container();
    std::fs::create_dir_all(directory.join(CONFIG_FILE)).unwrap();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    next_load(&mut load_rx).await;
    let published = timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.addresses.is_some()),
    )
    .await
    .expect("Caddy serves the loaded config, so its addresses are recorded")
    .unwrap()
    .clone();
    assert_eq!(
        published.addresses,
        Some(BTreeMap::from([(
            api.identity(),
            BTreeMap::from([(api.container_id, api.address.unwrap())])
        )]))
    );
    assert_eq!(
        published.count, 0,
        "nothing was acked after the failed write"
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn returning_to_the_last_written_config_reloads_caddy_after_a_failed_caddyfile_write() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let api = state.api_container();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));
    assert!(next_load(&mut load_rx).await.contains("10.210.1.2:80"));
    state.wait_for_ack_transactions(1).await;

    std::fs::remove_file(directory.join(CONFIG_FILE)).unwrap();
    std::fs::create_dir_all(directory.join(CONFIG_FILE)).unwrap();
    state.set_address(api.container_id, [10, 210, 1, 7]);
    assert!(next_load(&mut load_rx).await.contains("10.210.1.7:80"));
    state.set_address(api.container_id, [10, 210, 1, 2]);
    timeout(Duration::from_secs(5), async {
        while !load_rx.recv().await.unwrap().contains("10.210.1.2:80") {}
    })
    .await
    .expect("Caddy still serves 10.210.1.7, so the first config is loaded again");
    let published = published
        .wait_for(|published| {
            published.addresses
                == Some(BTreeMap::from([(
                    api.identity(),
                    BTreeMap::from([(api.container_id, api.address.unwrap())]),
                )]))
        })
        .await
        .unwrap()
        .clone();
    assert_eq!(published.count, 1);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_leave_out_an_https_route_caddy_has_no_certificate_for() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    state.mutate(|parts| {
        parts.resolved_spec.ports = vec![ingress("secure.example.com", 8443, HttpProtocol::Https)];
    });
    let web = observation(
        4,
        &machine.id,
        "web",
        Some([10, 210, 1, 5]),
        vec![ingress("web.example.com", 80, HttpProtocol::Http)],
    );
    state.containers.lock().unwrap().push(web.clone());
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    let loaded = next_load(&mut load_rx).await;
    assert!(!loaded.contains("10.210.1.2:8443"), "{loaded}");
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| {
            published.addresses
                == Some(BTreeMap::from([(
                    web.identity(),
                    BTreeMap::from([(web.container_id, web.address.unwrap())]),
                )]))
        }),
    )
    .await
    .expect("Caddy proxies only web.example.com until secure.example.com has a certificate")
    .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_claim_nothing_while_caddy_may_serve_a_config_written_without_its_admin() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let api = state.api_container();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let admin_up = Arc::new(AtomicBool::new(true));
    let caddyfile = directory.join(CONFIG_FILE);
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        route_publishes,
        shutdown.clone(),
        {
            let admin_up = Arc::clone(&admin_up);
            move || std::future::ready(Ok(admin_up.load(Ordering::SeqCst).then(|| admin.clone())))
        },
    ));
    assert!(next_load(&mut load_rx).await.contains("10.210.1.2:80"));
    state.wait_for_ack_transactions(1).await;

    admin_up.store(false, Ordering::SeqCst);
    state.set_address(api.container_id, [10, 210, 1, 7]);
    wait_for_caddyfile(&caddyfile, "10.210.1.7:80").await;
    settle().await;
    assert_eq!(
        published.borrow().addresses,
        None,
        "Caddy serves 10.210.1.2 until it restarts from the file that routes 10.210.1.7"
    );
    state.set_address(api.container_id, [10, 210, 1, 2]);
    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    settle().await;
    assert_eq!(
        published.borrow().addresses,
        None,
        "a Caddy that restarted from the last file still routes 10.210.1.7"
    );
    state.set_address(api.container_id, [10, 210, 1, 7]);
    wait_for_caddyfile(&caddyfile, "10.210.1.7:80").await;

    admin_up.store(true, Ordering::SeqCst);
    state.send(b"{\"change\":{}}\n");
    assert!(next_load(&mut load_rx).await.contains("10.210.1.7:80"));
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| {
            published.addresses
                == Some(BTreeMap::from([(
                    api.identity(),
                    BTreeMap::from([(api.container_id, ContainerAddress([10, 210, 1, 7].into()))]),
                )]))
        }),
    )
    .await
    .expect("Caddy loaded 10.210.1.7, so it is recorded")
    .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_claim_nothing_after_a_failed_write_without_the_admin() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let api = state.api_container();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let admin_up = Arc::new(AtomicBool::new(true));
    let caddyfile = directory.join(CONFIG_FILE);
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        route_publishes,
        shutdown.clone(),
        {
            let admin_up = Arc::clone(&admin_up);
            move || std::future::ready(Ok(admin_up.load(Ordering::SeqCst).then(|| admin.clone())))
        },
    ));
    assert!(next_load(&mut load_rx).await.contains("10.210.1.2:80"));
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.addresses.is_some()),
    )
    .await
    .unwrap()
    .unwrap();

    admin_up.store(false, Ordering::SeqCst);
    std::fs::remove_file(&caddyfile).unwrap();
    std::fs::create_dir_all(&caddyfile).unwrap();
    state.set_address(api.container_id, [10, 210, 1, 7]);
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.addresses.is_none()),
    )
    .await
    .expect("a failed write may still have replaced the file Caddy restarts from")
    .unwrap();
    std::fs::remove_dir(&caddyfile).unwrap();
    state.set_address(api.container_id, [10, 210, 1, 2]);
    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    settle().await;
    assert_eq!(
        published.borrow().addresses,
        None,
        "this rewrite went without the admin, so a running Caddy may still serve its last load"
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_are_empty_on_a_machine_that_takes_no_ingress() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    state
        .machine
        .lock()
        .unwrap()
        .as_mut()
        .expect("watch fixture has a Machine row")
        .accepts_ingress = false;
    state.containers.lock().unwrap().remove(1);
    let caddyfile = directory.join(CONFIG_FILE);
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        route_publishes,
        shutdown.clone(),
        || std::future::ready(Ok(None::<TestAdmin>)),
    ));

    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.addresses == Some(BTreeMap::new())),
    )
    .await
    .expect("no Caddy runs here, so this Machine routes nothing")
    .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_claim_nothing_without_the_admin_on_a_machine_that_takes_ingress() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    state.containers.lock().unwrap().remove(1);
    let caddyfile = directory.join(CONFIG_FILE);
    let (route_publishes, published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        route_publishes,
        shutdown.clone(),
        || std::future::ready(Ok(None::<TestAdmin>)),
    ));

    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    state.wait_for_ack_transactions(1).await;
    settle().await;
    assert_eq!(
        published.borrow().addresses,
        None,
        "a Caddy this Machine has not observed yet may already serve the file"
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_are_empty_while_the_ingress_container_is_stopped() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    state
        .containers
        .lock()
        .unwrap()
        .get_mut(1)
        .expect("watch fixture has an ingress container")
        .try_update(|parts| parts.runtime = ContainerRuntimeObservation::Exited { code: 0 })
        .unwrap();
    let caddyfile = directory.join(CONFIG_FILE);
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        route_publishes,
        shutdown.clone(),
        || std::future::ready(Ok(None::<TestAdmin>)),
    ));

    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.addresses == Some(BTreeMap::new())),
    )
    .await
    .expect("a stopped Caddy serves nothing, so status has nothing to warn about")
    .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_are_empty_once_a_loaded_caddy_stops() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let admin_up = Arc::new(AtomicBool::new(true));
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        {
            let admin_up = Arc::clone(&admin_up);
            move || std::future::ready(Ok(admin_up.load(Ordering::SeqCst).then(|| admin.clone())))
        },
    ));
    assert!(next_load(&mut load_rx).await.contains("10.210.1.2:80"));
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| {
            published
                .addresses
                .as_ref()
                .is_some_and(|addresses| !addresses.is_empty())
        }),
    )
    .await
    .unwrap()
    .unwrap();

    admin_up.store(false, Ordering::SeqCst);
    state
        .containers
        .lock()
        .unwrap()
        .get_mut(1)
        .expect("watch fixture has an ingress container")
        .try_update(|parts| parts.runtime = ContainerRuntimeObservation::Exited { code: 0 })
        .unwrap();
    state.send(b"{\"change\":{}}\n");
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.addresses == Some(BTreeMap::new())),
    )
    .await
    .expect("the Caddy that loaded these routes stopped, so it serves nothing")
    .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_are_empty_once_ingress_is_turned_off_and_its_caddy_removed() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let caddyfile = directory.join(CONFIG_FILE);
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        route_publishes,
        shutdown.clone(),
        || std::future::ready(Ok(None::<TestAdmin>)),
    ));
    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;

    state
        .machine
        .lock()
        .unwrap()
        .as_mut()
        .expect("watch fixture has a Machine row")
        .accepts_ingress = false;
    state.containers.lock().unwrap().remove(1);
    state.send(b"{\"change\":{}}\n");
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.addresses == Some(BTreeMap::new())),
    )
    .await
    .expect("this Machine no longer takes ingress and runs no Caddy, so it routes nothing")
    .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_claim_nothing_while_this_machine_has_no_row() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    *state.machine.lock().unwrap() = None;
    state.containers.lock().unwrap().remove(1);
    let caddyfile = directory.join(CONFIG_FILE);
    let (route_publishes, published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        route_publishes,
        shutdown.clone(),
        || std::future::ready(Ok(None::<TestAdmin>)),
    ));

    wait_for_caddyfile(&caddyfile, "10.210.1.2:80").await;
    state.wait_for_ack_transactions(1).await;
    settle().await;
    assert_eq!(
        published.borrow().addresses,
        None,
        "without its row this Machine can't tell that it takes no ingress"
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_claim_nothing_once_a_new_caddy_replaces_the_one_that_loaded_them() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let api = state.api_container();
    let (load_tx, mut loads) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let caddyfile = directory.join(CONFIG_FILE);
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));
    assert!(next_load(&mut loads).await.contains("10.210.1.2:80"));
    state.wait_for_ack_transactions(1).await;
    let first = std::fs::read_to_string(&caddyfile).unwrap();

    std::fs::create_dir(caddyfile.with_extension("tmp")).unwrap();
    state.set_address(api.container_id, [10, 210, 1, 7]);
    assert!(next_load(&mut loads).await.contains("10.210.1.7:80"));
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| {
            published.addresses.as_ref().is_some_and(|addresses| {
                addresses
                    .get(&api.identity())
                    .and_then(|copies| copies.get(&api.container_id))
                    == Some(&ContainerAddress([10, 210, 1, 7].into()))
            })
        }),
    )
    .await
    .expect("the running Caddy loaded 10.210.1.7")
    .unwrap();
    assert_eq!(std::fs::read_to_string(&caddyfile).unwrap(), first);

    std::fs::remove_dir_all(directory.join("certs")).unwrap();
    std::fs::write(directory.join("certs"), b"blocks certificate preparation").unwrap();
    *state
        .containers
        .lock()
        .unwrap()
        .get_mut(1)
        .expect("watch fixture has an ingress container") =
        reserved(observation(10, &machine.id, "ingress", None, Vec::new()));
    state.send(b"{\"change\":{}}\n");
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.addresses.is_none()),
    )
    .await
    .expect("the new Caddy started from a file holding 10.210.1.2, so the record claims nothing")
    .unwrap();
    assert!(
        loads.try_recv().is_err(),
        "the new Caddy has loaded nothing"
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_stand_while_the_caddy_that_loaded_them_refuses_a_new_load() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let api = state.api_container();
    let (load_tx, mut loads) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));
    assert!(next_load(&mut loads).await.contains("10.210.1.2:80"));
    state.wait_for_ack_transactions(1).await;
    let id = api.identity();
    let copy = api.container_id;
    let routes_api_at = move |published: &crate::ingress::PublishedRoutes| {
        published.addresses.as_ref().is_some_and(|addresses| {
            addresses.get(&id).and_then(|copies| copies.get(&copy))
                == Some(&ContainerAddress([10, 210, 1, 2].into()))
        })
    };
    timeout(
        Duration::from_secs(5),
        published.wait_for(routes_api_at.clone()),
    )
    .await
    .expect("the running Caddy loaded 10.210.1.2")
    .unwrap();

    std::fs::remove_dir_all(directory.join("certs")).unwrap();
    std::fs::write(directory.join("certs"), b"blocks certificate preparation").unwrap();
    let queries = state.container_queries();
    state.set_address(api.container_id, [10, 210, 1, 7]);
    // The retry's query follows the refused pass's publish.
    state.wait_for_container_queries(queries + 2).await;
    assert!(
        loads.try_recv().is_err(),
        "the new config never reached Caddy"
    );
    assert!(
        routes_api_at(&published.borrow()),
        "the Caddy that loaded 10.210.1.2 still runs and still serves it"
    );

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

const CHALLENGE: &str = r#"{"challenge_token":"LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0","challenge_response":"LoqXcYV8q5ONbJQxbmR7SCTNo3tiAXDfowyjxAjEuX0.AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}"#;

#[tokio::test]
async fn caddy_refusing_one_certificate_still_applies_every_other_route() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    state.mutate(|parts| {
        parts.resolved_spec.ports = vec![ingress("good.example.com", 8443, HttpProtocol::Https)];
    });
    state.containers.lock().unwrap().push(observation(
        4,
        &machine.id,
        "web",
        Some([10, 210, 1, 5]),
        vec![ingress("bad.example.com", 8443, HttpProtocol::Https)],
    ));
    state.write_certificates(&[
        ("bad.example.com", material_body("bad.example.com")),
        ("good.example.com", material_body("good.example.com")),
    ]);
    let api = state.api_container();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = RefusingAdmin {
        refused: "https://bad.example.com {",
        loads: load_tx,
        running: Arc::default(),
    };
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine,
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    let loaded = timeout(Duration::from_secs(5), async {
        loop {
            let loaded = load_rx.recv().await.unwrap();
            if loaded.contains("https://good.example.com {") {
                return loaded;
            }
        }
    })
    .await
    .expect("one refused certificate blocked every other route");
    assert!(!loaded.contains("https://bad.example.com {"), "{loaded}");
    let caddyfile = wait_for_caddyfile(
        &directory.join(CONFIG_FILE),
        "# - bad.example.com: Caddy refused",
    )
    .await;
    assert!(
        caddyfile.contains("https://good.example.com {"),
        "{caddyfile}"
    );
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| {
            published
                .addresses
                .as_ref()
                .is_some_and(|addresses| addresses.contains_key(&api.identity()))
        }),
    )
    .await
    .expect("the route beside the refused certificate was not claimed")
    .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn an_undecodable_certificate_row_does_not_block_other_routes() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    state.mutate(|parts| {
        parts.resolved_spec.ports = vec![ingress("good.example.com", 8443, HttpProtocol::Https)];
    });
    state.write_certificates(&[
        ("Not A Hostname", "{}".to_owned()),
        ("example.org", "not json".to_owned()),
        ("good.example.com", material_body("good.example.com")),
    ]);
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine,
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    let loaded = next_load(&mut load_rx).await;
    assert!(loaded.contains("https://good.example.com {"), "{loaded}");

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_write_without_an_admin_pins_only_certificates_caddy_loaded() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    state.mutate(|parts| {
        parts.resolved_spec.ports = vec![ingress("good.example.com", 8443, HttpProtocol::Https)];
    });
    let web = observation(
        4,
        &machine.id,
        "web",
        Some([10, 210, 1, 5]),
        vec![ingress("bad.example.com", 8443, HttpProtocol::Https)],
    );
    state.containers.lock().unwrap().push(web);
    state.write_certificates(&[
        ("bad.example.com", material_body("bad.example.com")),
        ("good.example.com", material_body("good.example.com")),
        ("new.example.com", material_body("new.example.com")),
    ]);
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = RefusingAdmin {
        refused: "https://bad.example.com {",
        loads: load_tx,
        running: Arc::default(),
    };
    let admin_up = Arc::new(AtomicBool::new(true));
    let caddyfile = directory.join(CONFIG_FILE);
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        {
            let admin_up = Arc::clone(&admin_up);
            move || std::future::ready(Ok(admin_up.load(Ordering::SeqCst).then(|| admin.clone())))
        },
    ));
    wait_for_caddyfile(&caddyfile, "# - bad.example.com: Caddy refused").await;
    while load_rx.try_recv().is_ok() {}

    admin_up.store(false, Ordering::SeqCst);
    state.containers.lock().unwrap().push(observation(
        5,
        &machine.id,
        "new",
        Some([10, 210, 1, 6]),
        vec![
            ingress("new.example.com", 8443, HttpProtocol::Https),
            ingress("plain.example.com", 80, HttpProtocol::Http),
        ],
    ));
    state.send(b"{\"change\":{}}\n");

    let written = wait_for_caddyfile(&caddyfile, "10.210.1.6:80").await;
    assert!(written.contains("https://good.example.com {"), "{written}");
    assert!(!written.contains("https://bad.example.com {"), "{written}");
    assert!(!written.contains("https://new.example.com {"), "{written}");
    assert!(load_rx.try_recv().is_err());

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn a_restart_beside_a_refused_certificate_never_drops_what_caddy_serves() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    state.mutate(|parts| {
        parts.resolved_spec.ports = vec![ingress("good.example.com", 8443, HttpProtocol::Https)];
    });
    state.containers.lock().unwrap().push(observation(
        4,
        &machine.id,
        "web",
        Some([10, 210, 1, 5]),
        vec![ingress("bad.example.com", 8443, HttpProtocol::Https)],
    ));
    let good = material("good.example.com");
    let stem = crate::ingress::certificate_file_stem(
        &ployz_core::IngressHost::parse("good.example.com").unwrap(),
        &good,
    );
    state.write_certificates(&[
        ("bad.example.com", material_body("bad.example.com")),
        (
            "good.example.com",
            CertificateRow::from_parts(Some((good, ployz_core::ClusterRoute::Direct)), None)
                .encode()
                .unwrap(),
        ),
    ]);
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = RefusingAdmin {
        refused: "https://bad.example.com {",
        loads: load_tx,
        running: Arc::new(Mutex::new(format!(
            "https://good.example.com {{\n\ttls /config/caddy/certs/{stem}.crt /config/caddy/certs/{stem}.key\n}}\n"
        ))),
    };
    let caddyfile = directory.join(CONFIG_FILE);
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine,
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));

    wait_for_caddyfile(&caddyfile, "# - bad.example.com: Caddy refused").await;
    let mut loads = 0;
    while let Ok(loaded) = load_rx.try_recv() {
        assert!(loaded.contains("https://good.example.com {"), "{loaded}");
        loads += 1;
    }
    assert!(loads > 0);

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn routed_addresses_follow_a_probe_load_when_the_next_apply_fails() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    let api = state.api_container();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = FailingAfter {
        inner: RefusingAdmin {
            refused: "https://bad.example.com {",
            loads: load_tx,
            running: Arc::default(),
        },
        loads_left: Arc::new(AtomicUsize::new(2)),
    };
    let (route_publishes, mut published) =
        tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine.clone(),
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        directory.join(CONFIG_FILE),
        route_publishes,
        shutdown.clone(),
        move || std::future::ready(Ok(Some(admin.clone()))),
    ));
    let first = BTreeMap::from([(
        api.identity(),
        BTreeMap::from([(api.container_id, api.address.unwrap())]),
    )]);
    assert!(next_load(&mut load_rx).await.contains("10.210.1.2:80"));
    published
        .wait_for(|published| published.addresses.as_ref() == Some(&first))
        .await
        .unwrap();

    state.write_certificates(&[("bad.example.com", material_body("bad.example.com"))]);
    state.containers.lock().unwrap().push(observation(
        4,
        &machine.id,
        "web",
        Some([10, 210, 1, 5]),
        vec![ingress("bad.example.com", 8443, HttpProtocol::Https)],
    ));
    state.set_address(api.container_id, [10, 210, 1, 7]);
    assert!(next_load(&mut load_rx).await.contains("10.210.1.7:80"));
    timeout(
        Duration::from_secs(5),
        published.wait_for(|published| published.addresses.as_ref() != Some(&first)),
    )
    .await
    .expect("the probe loaded 10.210.1.7, yet a failed apply still claims 10.210.1.2")
    .unwrap();

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn certificates_caddy_loaded_before_a_failed_caddyfile_write_survive_an_admin_outage() {
    let WatcherFixture {
        machine,
        state,
        address,
        server,
        directory,
    } = watcher_fixture().await;
    state.mutate(|parts| {
        parts.resolved_spec.ports = vec![ingress("good.example.com", 8443, HttpProtocol::Https)];
    });
    state.write_certificates(&[("good.example.com", material_body("good.example.com"))]);
    let caddyfile = directory.join(CONFIG_FILE);
    std::fs::create_dir_all(&caddyfile).unwrap();
    let (load_tx, mut load_rx) = mpsc::unbounded_channel();
    let admin = TestAdmin { loads: load_tx };
    let admin_up = Arc::new(AtomicBool::new(true));
    let shutdown = CancellationToken::new();
    let watcher = tokio::spawn(watch_caddy(
        machine,
        ReplicatedStore::new(ApiClient::http1(address, &"a".repeat(64)).unwrap()),
        no_down,
        caddyfile.clone(),
        tokio::sync::watch::channel(Default::default()).0,
        shutdown.clone(),
        {
            let admin_up = Arc::clone(&admin_up);
            move || std::future::ready(Ok(admin_up.load(Ordering::SeqCst).then(|| admin.clone())))
        },
    ));
    let loaded = next_load(&mut load_rx).await;
    assert!(loaded.contains("https://good.example.com {"), "{loaded}");

    admin_up.store(false, Ordering::SeqCst);
    std::fs::remove_dir(&caddyfile).unwrap();
    let written = wait_for_caddyfile(&caddyfile, "").await;
    assert!(written.contains("https://good.example.com {"), "{written}");

    shutdown.cancel();
    watcher.await.unwrap().unwrap();
    server.abort();
    std::fs::remove_dir_all(directory).unwrap();
}

fn material(hostname: &str) -> CertificateMaterial {
    let pair = rcgen::generate_simple_self_signed([hostname.to_owned()]).unwrap();
    CertificateMaterial::parse(pair.cert.pem(), pair.signing_key.serialize_pem()).unwrap()
}

fn material_body(hostname: &str) -> String {
    CertificateRow::from_parts(
        Some((material(hostname), ployz_core::ClusterRoute::Direct)),
        None,
    )
    .encode()
    .unwrap()
}

fn no_down() -> std::future::Ready<Result<BTreeSet<MachineId>, CorrosionError>> {
    std::future::ready(Ok(BTreeSet::new()))
}

fn ack(machine: &Machine, routed: &[&ContainerObservation]) -> Vec<Value> {
    let mut ids: Vec<_> = routed
        .iter()
        .map(|container| container.container_id)
        .collect();
    ids.sort_unstable();
    vec![
        json!(machine.id),
        json!("app/api"),
        json!(serde_json::to_string(&ids).unwrap()),
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
        recorded: Arc::new(watch::Sender::new(())),
        stall_container: Arc::new(AtomicBool::new(false)),
        container_subscriptions: Arc::new(Mutex::new(Vec::new())),
        certificate_subscriptions: Arc::new(Mutex::new(Vec::new())),
        certificates: Arc::new(Mutex::new(Vec::new())),
        statements: Arc::new(Mutex::new(Vec::new())),
        transactions: Arc::new(Mutex::new(Vec::new())),
        lose_next_transaction_response: Arc::new(AtomicBool::new(false)),
        fail_queries: Arc::new(AtomicBool::new(false)),
        machine: Arc::new(Mutex::new(Some(machine.clone()))),
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

/// Refuses any config that serves `refused`, as Caddy refuses a certificate it can't load.
#[derive(Clone)]
struct RefusingAdmin {
    refused: &'static str,
    loads: mpsc::UnboundedSender<String>,
    running: Arc<Mutex<String>>,
}

impl CaddyAdmin for RefusingAdmin {
    async fn adapt(&self, caddyfile: &str) -> Result<String, CaddyError> {
        Ok(caddyfile.to_owned())
    }

    async fn load(&self, caddyfile: &str) -> Result<(), CaddyError> {
        if caddyfile.contains(self.refused) {
            return Err(CaddyError::Admin(
                "loading certificates: x509: negative serial number".into(),
            ));
        }
        *self.running.lock().unwrap() = caddyfile.to_owned();
        self.loads.send(caddyfile.to_owned()).unwrap();
        Ok(())
    }

    async fn loaded_certificates(&self) -> Result<BTreeSet<String>, CaddyError> {
        Ok(self
            .running
            .lock()
            .unwrap()
            .lines()
            .filter_map(|line| {
                let (stem, _) = line
                    .strip_prefix("\ttls /config/caddy/certs/")?
                    .split_once(".crt ")?;
                Some(stem.to_owned())
            })
            .collect())
    }
}

/// Fails every load `inner` would accept once `loads_left` loads have succeeded.
#[derive(Clone)]
struct FailingAfter {
    inner: RefusingAdmin,
    loads_left: Arc<AtomicUsize>,
}

impl CaddyAdmin for FailingAfter {
    async fn adapt(&self, caddyfile: &str) -> Result<String, CaddyError> {
        self.inner.adapt(caddyfile).await
    }

    async fn load(&self, caddyfile: &str) -> Result<(), CaddyError> {
        if !caddyfile.contains(self.inner.refused)
            && self
                .loads_left
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_err()
        {
            return Err(CaddyError::Io(std::io::Error::other("admin socket closed")));
        }
        self.inner.load(caddyfile).await
    }

    async fn loaded_certificates(&self) -> Result<BTreeSet<String>, CaddyError> {
        self.inner.loaded_certificates().await
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

    async fn loaded_certificates(&self) -> Result<BTreeSet<String>, CaddyError> {
        Ok(BTreeSet::new())
    }
}

#[derive(Clone)]
struct RetryAdmin {
    attempts: Arc<watch::Sender<usize>>,
    loads: mpsc::UnboundedSender<()>,
    /// Holds a successful load from returning until notified.
    release: Option<Arc<tokio::sync::Notify>>,
}

impl CaddyAdmin for RetryAdmin {
    async fn adapt(&self, _caddyfile: &str) -> Result<String, CaddyError> {
        Ok("{}".into())
    }

    async fn load(&self, _json: &str) -> Result<(), CaddyError> {
        let mut first = false;
        self.attempts.send_modify(|attempts| {
            first = *attempts == 0;
            *attempts += 1;
        });
        if first {
            return Err(CaddyError::Admin("temporary load failure".into()));
        }
        self.loads.send(()).unwrap();
        if let Some(release) = &self.release {
            release.notified().await;
        }
        Ok(())
    }

    async fn loaded_certificates(&self) -> Result<BTreeSet<String>, CaddyError> {
        Ok(BTreeSet::new())
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
    /// Bumped after the fake records a statement or opens a subscription.
    recorded: Arc<watch::Sender<()>>,
    stall_container: Arc<AtomicBool>,
    container_subscriptions: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>>,
    certificate_subscriptions: Arc<Mutex<Vec<mpsc::UnboundedSender<Bytes>>>>,
    certificates: Arc<Mutex<Vec<(String, String)>>>,
    statements: Arc<Mutex<Vec<(Sent, String)>>>,
    transactions: Arc<Mutex<Vec<Vec<Statement>>>>,
    lose_next_transaction_response: Arc<AtomicBool>,
    fail_queries: Arc<AtomicBool>,
    machine: Arc<Mutex<Option<Machine>>>,
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

    fn set_rotation(&self, container_id: ContainerId, rotation: CopyRotation) {
        self.containers
            .lock()
            .unwrap()
            .iter_mut()
            .find(|container| container.container_id == container_id)
            .expect("watch fixture has the Container")
            .try_update(|parts| parts.rotation = Some(rotation))
            .unwrap();
        self.send(b"{\"change\":{}}\n");
    }

    fn set_address(&self, container_id: ContainerId, address: [u8; 4]) {
        self.containers
            .lock()
            .unwrap()
            .iter_mut()
            .find(|container| container.container_id == container_id)
            .expect("watch fixture has the Container")
            .try_update(|parts| parts.address = Some(ContainerAddress(address.into())))
            .unwrap();
        self.send(b"{\"change\":{}}\n");
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

    async fn wait_until(&self, what: &str, mut done: impl FnMut(&Self) -> bool) {
        let mut recorded = self.recorded.subscribe();
        timeout(Duration::from_secs(5), recorded.wait_for(|()| done(self)))
            .await
            .unwrap_or_else(|_| panic!("{what}"))
            .unwrap();
    }

    async fn wait_for_container_queries(&self, count: usize) {
        self.wait_until(
            &format!("container query {count} was never sent"),
            |state| state.container_queries() >= count,
        )
        .await;
    }

    async fn wait_for_ack_transactions(&self, count: usize) {
        self.wait_until(
            &format!("ack transaction {count} was never written"),
            |state| state.ack_transactions() >= count,
        )
        .await;
    }

    fn write_certificates(&self, rows: &[(&str, String)]) {
        *self.certificates.lock().unwrap() = rows
            .iter()
            .map(|(hostname, body)| ((*hostname).to_owned(), body.clone()))
            .collect();
        self.send_certificates(b"{\"change\":{}}\n");
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

async fn query(State(state): State<WatchState>, body: Bytes) -> Response {
    let statement: Statement = serde_json::from_slice(&body).unwrap();
    let fail = state.fail_queries.load(Ordering::SeqCst);
    state
        .statements
        .lock()
        .unwrap()
        .push((Sent::Query, statement.query.clone()));
    state.recorded.send_replace(());
    if fail {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
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
    } else if statement.query == "SELECT info FROM machines WHERE id = ?" {
        let machine = state.machine.lock().unwrap();
        events(
            &["info"],
            machine
                .iter()
                .map(|machine| vec![json!(serde_json::to_string(machine).unwrap())]),
        )
    } else if statement.query == "SELECT hostname, body FROM certificates ORDER BY hostname" {
        let certificates = state.certificates.lock().unwrap();
        events(
            &["hostname", "body"],
            certificates
                .iter()
                .map(|(hostname, body)| vec![json!(hostname), json!(body)]),
        )
    } else {
        panic!("unexpected query {}", statement.query);
    }
    .into_response()
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
    state.recorded.send_replace(());
    Response::new(Body::from_stream(
        UnboundedReceiverStream::new(receiver).map(Ok::<_, Infallible>),
    ))
}

async fn transact(State(state): State<WatchState>, body: Bytes) -> Response {
    let statements: Vec<Statement> = serde_json::from_slice(&body).unwrap();
    state.statements.lock().unwrap().extend(
        statements
            .iter()
            .map(|statement| (Sent::Transaction, statement.query.clone())),
    );
    let results = vec![json!({"rows_affected": 1, "time": 0.0}); statements.len()];
    state.transactions.lock().unwrap().push(statements);
    state.recorded.send_replace(());
    if state
        .lose_next_transaction_response
        .swap(false, Ordering::SeqCst)
    {
        return StatusCode::GATEWAY_TIMEOUT.into_response();
    }
    serde_json::to_vec(&json!({ "results": results, "time": 0.0 }))
        .unwrap()
        .into_response()
}

async fn wait_for_caddyfile_without(path: &Path, gone: &str) -> String {
    caddyfile_where(
        path,
        &format!("Caddyfile still contained {gone}"),
        |caddyfile| !caddyfile.contains(gone),
    )
    .await
}

async fn wait_for_caddyfile(path: &Path, expected: &str) -> String {
    caddyfile_where(
        path,
        &format!("Caddyfile never contained {expected}"),
        |caddyfile| caddyfile.contains(expected),
    )
    .await
}

async fn caddyfile_where(path: &Path, what: &str, done: impl Fn(&str) -> bool) -> String {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(caddyfile) = std::fs::read_to_string(path)
                && done(&caddyfile)
            {
                return caddyfile;
            }
            // real-wait: polls the Caddyfile on disk, which emits no write event
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"))
}
