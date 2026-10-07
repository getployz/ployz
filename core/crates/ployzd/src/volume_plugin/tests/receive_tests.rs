//! StartReceive and InspectReceive over fake ZFS and a fake writer serving send streams.

use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use axum::{Router, body::Body, extract::Request, response::IntoResponse};
use futures_util::{StreamExt as _, stream};
use tokio::{net::TcpListener, sync::Notify};

use super::fake_zfs::SLOT_BOUND_BYTES;
use super::lease_tests::set_property;
use super::mirror_tests::{at, commands, property, snapshot_names};
use super::*;

/// A writer Machine that answers every send request with the stream text the test chose.
struct Writer {
    requests: Arc<StdMutex<Vec<String>>>,
    stream: Arc<StdMutex<String>>,
    /// While held, responses wait on `release` before any byte is sent.
    hold: Arc<StdMutex<bool>>,
    release: Arc<Notify>,
    stall: Arc<StdMutex<bool>>,
    port: u16,
}

impl Writer {
    async fn start(stream: &str) -> Self {
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let stream = Arc::new(StdMutex::new(stream.to_owned()));
        let hold = Arc::new(StdMutex::new(false));
        let release = Arc::new(Notify::new());
        let stall = Arc::new(StdMutex::new(false));
        let state = (
            Arc::clone(&requests),
            Arc::clone(&stream),
            Arc::clone(&hold),
            Arc::clone(&release),
            Arc::clone(&stall),
        );
        let router = Router::new().fallback(move |request: Request| {
            let (requests, stream, hold, release, stall) = state.clone();
            async move {
                let uri = request.uri().to_string();
                requests.lock().unwrap().push(uri);
                if *hold.lock().unwrap() {
                    release.notified().await;
                }
                let body = stream.lock().unwrap().clone();
                if *stall.lock().unwrap() {
                    let sent = stream::once(async move { Ok::<_, io::Error>(body) });
                    return Body::from_stream(sent.chain(stream::pending())).into_response();
                }
                body.into_response()
            }
        });
        let listener = TcpListener::bind("[::1]:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(axum::serve(listener, router).into_future());
        Self {
            requests,
            stream,
            hold,
            release,
            stall,
            port,
        }
    }

    fn answer(&self, stream: &str) {
        *self.stream.lock().unwrap() = stream.to_owned();
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn start(
    test: &TestDir,
    markers: &[&str],
    writer: &Writer,
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    start_stalling_after(test, markers, writer, Duration::from_secs(60))
}

fn start_stalling_after(
    test: &TestDir,
    markers: &[&str],
    writer: &Writer,
    stall: Duration,
) -> (PathBuf, tokio::task::JoinHandle<io::Result<()>>) {
    for marker in markers {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut storage = VolumeStorage::with_programs(zpool, zfs);
    storage.send_port = writer.port;
    storage.receive_stall = stall;
    let server = tokio::spawn(serve(listener, storage));
    (socket, server)
}

fn receive(seq: u16, round: u32, fields: Value) -> Value {
    let mut request = at(1, seq, round, 3, json!({"from": "::1", "target": "w-1-1"}));
    for (key, value) in fields.as_object().unwrap() {
        request
            .as_object_mut()
            .unwrap()
            .insert(key.clone(), value.clone());
    }
    request
}

/// Polls InspectReceive until the receive of `round` is no longer running.
async fn settled(socket: &Path, round: u32) -> Value {
    for _ in 0..200 {
        let view = post(
            socket,
            "/Volume.InspectReceive",
            json!({"name": "data", "round": round}),
        )
        .await;
        let state = view
            .pointer("/Ok/status/state")
            .unwrap_or_else(|| panic!("{view}"));
        if state != "running" {
            return view;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("receive never settled");
}

#[tokio::test]
async fn a_full_receive_lands_the_target_read_only_within_the_bound() {
    let writer = Writer::start("snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    let (socket, server) = start(&test, &["root", "mirror"], &writer);

    let response = post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "adopt");
    assert_eq!(
        property(&test, "tank/ployz-mirror/data", "ployz:receive").as_deref(),
        Some("1:4.1.3:w-1-1")
    );

    let view = settled(&socket, 1).await;
    assert_eq!(
        view,
        json!({"Ok": {
            "round": 1,
            "status": {
                "state": "done",
                "newest": {"name": "w-1-1", "guid": 77, "created_unix_seconds": 1_700_000_000},
            },
        }})
    );
    assert_eq!(writer.requests(), ["/volume-send/data?target=w-1-1"]);
    let log = commands(&test);
    assert!(
        log.contains(&format!(
            "zfs receive -u -s -o readonly=on -o refquota={SLOT_BOUND_BYTES} tank/ployz-mirror/data/fs\n"
        )),
        "{log}"
    );
    let after_receive = log.split("zfs receive").nth(1).unwrap();
    assert!(
        after_receive.contains("zfs set readonly=on tank/ployz-mirror/data/fs\n"),
        "{log}"
    );
    assert!(
        after_receive.contains(&format!(
            "zfs set refquota={SLOT_BOUND_BYTES} tank/ployz-mirror/data/fs\n"
        )),
        "{log}"
    );
    assert!(after_receive.contains("zfs get -Hp -o value readonly tank/ployz-mirror/data/fs\n"));
    assert!(after_receive.contains("zfs get -Hp -o value refquota tank/ployz-mirror/data/fs\n"));

    let inspect = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(inspect.pointer("/Ok/copy/readonly").unwrap(), true);
    assert_eq!(inspect.pointer("/Ok/copy/newest/guid").unwrap(), 77);
    server.abort();
}

#[tokio::test]
async fn an_incremental_receive_names_its_base() {
    let writer = Writer::start("snapshot w-1-2 78\n").await;
    let test = TestDir::new();
    set_property(
        &test,
        "tank/ployz-mirror/data/fs",
        "snapshots",
        "tank/ployz-mirror/data/fs@w-1-1\t77\t1700000000",
    );
    let (socket, server) = start(&test, &["root", "mirror", "mirror-fs"], &writer);

    let request = receive(4, 2, json!({"base": 77, "target": "w-1-2"}));
    post(&socket, "/Volume.StartReceive", request).await;
    let view = settled(&socket, 2).await;
    assert_eq!(view.pointer("/Ok/status/state").unwrap(), "done");
    assert_eq!(view.pointer("/Ok/status/newest/guid").unwrap(), 78);
    assert_eq!(
        writer.requests(),
        ["/volume-send/data?target=w-1-2&base=77"]
    );
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/data/fs"),
        ["w-1-2", "w-1-1"]
    );
    server.abort();
}

#[tokio::test]
async fn a_broken_stream_is_resumable_and_resumes_by_token() {
    let writer = Writer::start("break\n").await;
    let test = TestDir::new();
    let (socket, server) = start(&test, &["root", "mirror"], &writer);

    post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    let view = settled(&socket, 1).await;
    assert_eq!(
        view.pointer("/Ok/status").unwrap(),
        &json!({"state": "resumable", "target": "w-1-1", "token": "token-1"})
    );
    let inspect = post(&socket, "/Volume.Inspect", json!({"name": "data"})).await;
    assert_eq!(inspect.pointer("/Ok/copy/resume_token").unwrap(), "token-1");

    writer.answer("snapshot w-1-1 77\n");
    let request = receive(5, 1, json!({"resume_token": "token-1"}));
    let response = post(&socket, "/Volume.StartReceive", request).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "admit");
    let view = settled(&socket, 1).await;
    assert_eq!(view.pointer("/Ok/status/state").unwrap(), "done");
    assert_eq!(
        writer.requests(),
        [
            "/volume-send/data?target=w-1-1",
            "/volume-send/data?token=token-1"
        ]
    );
    assert_eq!(
        property(&test, "tank/ployz-mirror/data/fs", "readonly").as_deref(),
        Some("on")
    );
    server.abort();
}

#[tokio::test]
async fn a_stalled_stream_frees_the_slot_and_resumes_by_token() {
    let writer = Writer::start("break\n").await;
    *writer.stall.lock().unwrap() = true;
    let test = TestDir::new();
    let stall = Duration::from_millis(200);
    let (socket, server) = start_stalling_after(&test, &["root", "mirror"], &writer, stall);

    let started = tokio::time::Instant::now();
    post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    let view = settled(&socket, 1).await;
    assert!(started.elapsed() >= stall, "settled before the deadline");
    assert_eq!(
        view.pointer("/Ok/status").unwrap(),
        &json!({"state": "resumable", "target": "w-1-1", "token": "token-1"})
    );

    set_property(
        &test,
        "tank/ployz-mirror/data/fs",
        "receive_resume_token",
        "-",
    );
    let failed = settled(&socket, 1).await;
    assert_eq!(failed.pointer("/Ok/status/state").unwrap(), "failed");
    let reason = failed
        .pointer("/Ok/status/reason")
        .unwrap()
        .as_str()
        .unwrap();
    assert!(reason.contains("stream:"), "{reason}");
    set_property(
        &test,
        "tank/ployz-mirror/data/fs",
        "receive_resume_token",
        "token-1",
    );

    *writer.stall.lock().unwrap() = false;
    writer.answer("snapshot w-1-1 77\n");
    let request = receive(5, 1, json!({"resume_token": "token-1"}));
    let response = post(&socket, "/Volume.StartReceive", request).await;
    assert_eq!(
        response
            .pointer("/Ok/decision")
            .unwrap_or_else(|| panic!("{response}")),
        "admit"
    );
    let view = settled(&socket, 1).await;
    assert_eq!(view.pointer("/Ok/status/state").unwrap(), "done", "{view}");
    assert_eq!(
        writer.requests(),
        [
            "/volume-send/data?target=w-1-1",
            "/volume-send/data?token=token-1"
        ]
    );
    server.abort();
}

#[tokio::test]
async fn a_receive_that_leaves_the_copy_writable_fails() {
    let writer = Writer::start("snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    let (socket, server) = start(&test, &["root", "mirror", "readonly-lost"], &writer);

    post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    let view = settled(&socket, 1).await;
    assert_eq!(view.pointer("/Ok/status/state").unwrap(), "failed");
    let reason = view.pointer("/Ok/status/reason").unwrap().as_str().unwrap();
    assert!(reason.contains("readonly=off"), "{reason}");
    server.abort();
}

#[tokio::test]
async fn a_replayed_start_after_completion_answers_without_receiving_again() {
    let writer = Writer::start("snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    let (socket, server) = start(&test, &["root", "mirror"], &writer);

    post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    settled(&socket, 1).await;
    let response = post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    assert_eq!(response.pointer("/Ok/decision").unwrap(), "replay");
    assert_eq!(response.pointer("/Ok/copy/newest/guid").unwrap(), 77);
    assert_eq!(writer.requests().len(), 1);
    assert_eq!(commands(&test).matches("zfs receive").count(), 1);
    server.abort();
}

#[tokio::test]
async fn a_replayed_start_whose_receive_never_began_starts_it() {
    let writer = Writer::start("snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    // The plugin died after recording 4.1.3 and before the receive created `fs`.
    set_property(&test, "tank/ployz", "ployz:lease.data", "1:4.1.3:closed");
    let (socket, server) = start(&test, &["root", "mirror"], &writer);

    let response = post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    assert_eq!(
        response
            .pointer("/Ok/decision")
            .unwrap_or_else(|| panic!("{response}")),
        "replay"
    );
    let view = settled(&socket, 1).await;
    assert_eq!(view.pointer("/Ok/status/state").unwrap(), "done", "{view}");
    assert_eq!(writer.requests(), ["/volume-send/data?target=w-1-1"]);
    assert_eq!(
        snapshot_names(&test, "tank/ployz-mirror/data/fs"),
        ["w-1-1"]
    );
    server.abort();
}

#[tokio::test]
async fn a_running_receive_makes_the_slot_busy() {
    let writer = Writer::start("snapshot w-1-1 77\n").await;
    *writer.hold.lock().unwrap() = true;
    let test = TestDir::new();
    let (socket, server) = start(&test, &["root", "mirror"], &writer);

    post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    let view = post(
        &socket,
        "/Volume.InspectReceive",
        json!({"name": "data", "round": 1}),
    )
    .await;
    assert_eq!(
        view.pointer("/Ok/status").unwrap(),
        &json!({"state": "running", "target": "w-1-1"})
    );
    for (route, step) in [
        ("/Volume.StartReceive", receive(5, 1, json!({}))),
        ("/Volume.PruneMirror", at(1, 5, 1, 4, json!({}))),
        ("/Volume.BeginRound", at(1, 5, 2, 0, json!({}))),
        ("/Volume.DestroyMirror", at(1, 6, 0, 0, json!({}))),
    ] {
        let response = post(&socket, route, step).await;
        assert_eq!(
            response.pointer("/Err/details/reason").unwrap(),
            "busy",
            "{route}: {response}"
        );
    }
    assert_eq!(writer.requests().len(), 1);

    *writer.hold.lock().unwrap() = false;
    writer.release.notify_waiters();
    let view = settled(&socket, 1).await;
    assert_eq!(view.pointer("/Ok/status/state").unwrap(), "done");
    let response = post(&socket, "/Volume.DestroyMirror", at(1, 7, 0, 0, json!({}))).await;
    assert_eq!(response.pointer("/Ok/copy").unwrap(), &Value::Null);
    server.abort();
}

#[tokio::test]
async fn inspect_receive_answers_for_the_recorded_round_only() {
    let writer = Writer::start("").await;
    let test = TestDir::new();
    let (socket, server) = start(&test, &["root", "mirror", "mirror-fs"], &writer);

    let view = post(
        &socket,
        "/Volume.InspectReceive",
        json!({"name": "data", "round": 1}),
    )
    .await;
    assert_eq!(
        view,
        json!({"Ok": {"round": null, "status": {"state": "idle"}}})
    );

    set_property(
        &test,
        "tank/ployz-mirror/data",
        "ployz:receive",
        "1:4.1.3:w-1-1",
    );
    let view = post(
        &socket,
        "/Volume.InspectReceive",
        json!({"name": "data", "round": 2}),
    )
    .await;
    assert_eq!(
        view,
        json!({"Ok": {"round": 1, "status": {"state": "idle"}}})
    );

    // The plugin restarted after the receive died: no task, no token, no snapshot.
    let view = post(
        &socket,
        "/Volume.InspectReceive",
        json!({"name": "data", "round": 1}),
    )
    .await;
    assert_eq!(
        view.pointer("/Ok/status").unwrap(),
        &json!({"state": "failed", "target": "w-1-1", "reason": "the receive did not complete"})
    );
    server.abort();
}

#[tokio::test]
async fn start_receive_is_fenced_and_needs_a_slot() {
    let writer = Writer::start("").await;
    let test = TestDir::new();
    set_property(&test, "tank/ployz", "ployz:lease.data", "5:2.0.0:closed");
    let (socket, server) = start(&test, &["root", "mirror"], &writer);
    let response = post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    assert_eq!(
        response.pointer("/Err/details/reason").unwrap(),
        "stale_lease"
    );
    assert!(writer.requests().is_empty());
    assert_eq!(
        property(&test, "tank/ployz-mirror/data", "ployz:receive"),
        None
    );
    server.abort();

    let test = TestDir::new();
    let (socket, server) = start(&test, &["root", "volume"], &writer);
    let response = post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
    assert_eq!(
        response.pointer("/Err/details/reason").unwrap(),
        "precondition"
    );
    assert!(writer.requests().is_empty());
    server.abort();
}

#[tokio::test]
async fn a_stalled_receive_process_fails_and_frees_the_slot() {
    for marker in ["stall-read", "stall-exit"] {
        let writer = Writer::start(&"x".repeat(2 * 1024 * 1024)).await;
        let test = TestDir::new();
        fs::create_dir_all(test.0.join("props")).unwrap();
        fs::write(test.0.join("props").join(marker), "").unwrap();
        let (socket, server) = start_stalling_after(
            &test,
            &["root", "mirror"],
            &writer,
            Duration::from_millis(200),
        );
        post(&socket, "/Volume.StartReceive", receive(4, 1, json!({}))).await;
        let view = settled(&socket, 1).await;
        assert_eq!(
            view.pointer("/Ok/status/state").unwrap(),
            "failed",
            "{marker}: {view}"
        );
        let reason = view.pointer("/Ok/status/reason").unwrap().as_str().unwrap();
        assert!(reason.contains("stalled"), "{marker}: {reason}");
        let pid = fs::read_to_string(test.0.join("props/receive-pid")).unwrap();
        for _ in 0..100 {
            if !Path::new(&format!("/proc/{}", pid.trim())).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !Path::new(&format!("/proc/{}", pid.trim())).exists(),
            "receive child survived its deadline"
        );
        for (route, step) in [
            ("/Volume.PruneMirror", at(1, 4, 1, 4, json!({}))),
            ("/Volume.BeginRound", at(1, 4, 2, 0, json!({}))),
            ("/Volume.DestroyMirror", at(1, 5, 0, 0, json!({}))),
        ] {
            let reply = post(&socket, route, step).await;
            assert!(reply.get("Ok").is_some(), "{route}: {reply}");
        }
        server.abort();
    }
}

#[tokio::test]
async fn a_replayed_receive_repairs_properties_and_clears_a_transient_failure() {
    let writer = Writer::start("snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(test.0.join("props/fail-properties"), "").unwrap();
    let (socket, server) = start(&test, &["root", "mirror"], &writer);
    let request = receive(4, 1, json!({}));
    post(&socket, "/Volume.StartReceive", request.clone()).await;
    let view = settled(&socket, 1).await;
    assert_eq!(
        view.pointer("/Ok/status/state").unwrap(),
        "failed",
        "{view}"
    );
    set_property(&test, "tank/ployz-mirror/data/fs", "readonly", "off");
    set_property(&test, "tank/ployz-mirror/data/fs", "refquota", "1");
    let failed_replay = post(&socket, "/Volume.StartReceive", request.clone()).await;
    assert!(failed_replay.get("Err").is_some(), "{failed_replay}");
    fs::remove_file(test.0.join("props/fail-properties")).unwrap();
    let reply = post(&socket, "/Volume.StartReceive", request).await;
    assert_eq!(reply.pointer("/Ok/decision").unwrap(), "replay", "{reply}");
    let view = settled(&socket, 1).await;
    assert_eq!(view.pointer("/Ok/status/state").unwrap(), "done", "{view}");
    assert_eq!(
        property(&test, "tank/ployz-mirror/data/fs", "readonly").as_deref(),
        Some("on")
    );
    assert_eq!(
        property(&test, "tank/ployz-mirror/data/fs", "refquota"),
        Some(SLOT_BOUND_BYTES.to_string())
    );
    assert_eq!(writer.requests().len(), 1);
    server.abort();
}
