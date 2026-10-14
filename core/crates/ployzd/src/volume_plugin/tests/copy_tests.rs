//! The SetVolumeCopy routes over fake ZFS: each cell's effect, its resend, its refusals,
//! convergence after a failure at every sub-step, and two Machines moving one Volume
//! through the real send server with every request replayed late.

use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use axum::{Router, body::Body, extract::Request, http::StatusCode, response::IntoResponse};
use tokio::{net::TcpListener, process::Command, sync::Semaphore};
use tokio_util::sync::CancellationToken;

use super::super::transfer::{Receives, Stop, ZfsChild};

use super::first_pool_tests::fake_first_pool;
use super::inspect_tests::{set_property, start};
use super::*;

const ROOT: &str = "tank/ployz/data";
const SLOT: &str = "tank/ployz-mirror/data";
const SLOT_FS: &str = "tank/ployz-mirror/data/fs";
const BOUND: u64 = 1_073_741_824;

type Server = tokio::task::JoinHandle<io::Result<()>>;

fn set_snapshots(test: &TestDir, dataset: &str, snapshots: &[(&str, u64)]) {
    let lines: String = snapshots
        .iter()
        .enumerate()
        .map(|(index, (name, guid))| {
            format!(
                "{dataset}@{name}\t{guid}\t{}\n",
                1_700_000_000 - index as u64
            )
        })
        .collect();
    let directory = test.0.join("props").join(dataset);
    fs::create_dir_all(&directory).unwrap();
    fs::write(directory.join("snapshots"), lines).unwrap();
}

/// A writer root, mounted, with `snapshots` newest first.
fn writer(test: &TestDir, snapshots: &[(&str, u64)]) -> (PathBuf, Server) {
    let started = start(test, USABLE_POOL, &["root", "volume", "mounted"]);
    set_snapshots(test, ROOT, snapshots);
    started
}

/// A slot holding a received copy with `snapshots` newest first.
fn slot(test: &TestDir, snapshots: &[(&str, u64)]) -> (PathBuf, Server) {
    let started = start(test, USABLE_POOL, &["root", "mirror", "mirror-fs"]);
    set_snapshots(test, SLOT_FS, snapshots);
    started
}

fn guid(copy: &Value) -> &str {
    copy.pointer("/Ok/newest/guid")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{copy}"))
}

fn reason(reply: &Value) -> &str {
    reply
        .pointer("/Err/details/reason")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("{reply}"))
}

/// The commands that change ZFS, in order; lists and gets are left out.
fn effects(log: &str) -> Vec<String> {
    log.lines()
        .filter(|line| {
            let args = line.strip_prefix("zfs ").unwrap_or("");
            !args.is_empty() && !args.starts_with("list") && !args.starts_with("get")
        })
        .map(str::to_owned)
        .collect()
}

/// Every marker and property file under the fixture, so two runs can be compared.
fn state(test: &TestDir) -> Vec<(String, String)> {
    fn walk(root: &Path, dir: &Path, into: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                walk(root, &path, into);
            } else if path.is_file()
                && !matches!(
                    name.as_str(),
                    "commands"
                        | "fake-zfs"
                        | "zfs"
                        | "zpool"
                        | "receive-pid"
                        | "fail-command"
                        | "next-guid"
                )
            {
                let relative = path.strip_prefix(root).unwrap().display().to_string();
                into.push((relative, fs::read_to_string(&path).unwrap()));
            }
        }
    }
    let mut files = Vec::new();
    walk(&test.0, &test.0, &mut files);
    files.sort();
    files
}

/// Runs `request` clean, then once per effect command with that command failing, and
/// checks the resend ends in the clean run's reply and state.
async fn converges_after_each_failure(
    route: &str,
    request: Value,
    fixture: impl Fn(&TestDir) -> (PathBuf, Server),
) {
    let test = TestDir::new();
    let (socket, server) = fixture(&test);
    let clean = post(&socket, route, request.clone()).await;
    assert!(clean.get("Ok").is_some(), "{clean}");
    let steps = effects(&commands(&test));
    assert!(!steps.is_empty());
    let end = state(&test);
    server.abort();
    for step in &steps {
        let test = TestDir::new();
        let (socket, server) = fixture(&test);
        fs::create_dir_all(test.0.join("props")).unwrap();
        fs::write(
            test.0.join("props/fail-command"),
            step.strip_prefix("zfs ").unwrap(),
        )
        .unwrap();
        let failed = post(&socket, route, request.clone()).await;
        assert!(failed.get("Err").is_some(), "{step} failing: {failed}");
        assert!(
            !test.0.join("props/fail-command").exists(),
            "{step} never ran"
        );
        let resent = post(&socket, route, request.clone()).await;
        assert_eq!(resent, clean, "resend after {step} failed");
        assert_eq!(state(&test), end, "state after {step} failed");
        server.abort();
    }
}

#[tokio::test]
async fn the_cut_parks_the_root_in_the_slot_at_the_target() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("w-1-2", 12), ("w-1-1", 11)]);

    let request = json!({"name": "data", "target": "f-1"});
    let reply = post(&socket, "/Volume.Copy.Cut", request.clone()).await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("slot")));
    assert_eq!(reply.pointer("/Ok/readonly"), Some(&json!(true)));
    assert_eq!(reply.pointer("/Ok/newest/name"), Some(&json!("f-1")));
    assert_eq!(reply.pointer("/Ok/pull"), Some(&Value::Null));
    assert_eq!(
        effects(&commands(&test)),
        [
            "zfs set readonly=on tank/ployz/data",
            "zfs set ployz:cut=on tank/ployz/data",
            "zfs snapshot tank/ployz/data@f-1",
            "zfs unmount tank/ployz/data",
            "zfs create -o canmount=off -o mountpoint=/var/lib/ployz-mirror -o readonly=on tank/ployz-mirror",
            "zfs create -o canmount=off -o readonly=on -o refquota=1073741824 tank/ployz-mirror/data",
            "zfs rename tank/ployz/data tank/ployz-mirror/data/fs",
        ]
    );
    assert_eq!(property(&test, SLOT_FS, "ployz:cut").as_deref(), Some("on"));
    assert_eq!(
        property(&test, SLOT, "refquota").as_deref(),
        Some("1073741824")
    );
    assert_eq!(snapshot_names(&test, SLOT_FS), ["f-1", "w-1-2", "w-1-1"]);

    let before = commands(&test);
    assert_eq!(post(&socket, "/Volume.Copy.Cut", request).await, reply);
    assert!(effects(&commands(&test)[before.len()..]).is_empty());
    server.abort();
}

/// Docker drops a Volume's labels and options when Get says it does not exist.
#[tokio::test]
async fn docker_still_gets_a_root_cut_into_its_slot() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("w-1-1", 11)]);
    let cut = post(
        &socket,
        "/Volume.Copy.Cut",
        json!({"name": "data", "target": "f-1"}),
    )
    .await;
    assert_eq!(cut.pointer("/Ok/kind"), Some(&json!("slot")), "{cut}");

    let got = post(&socket, "/VolumeDriver.Get", json!({"Name": "data"})).await;
    assert_eq!(got.get("Err"), Some(&json!("")), "{got}");
    assert_eq!(got.pointer("/Volume/Name"), Some(&json!("data")), "{got}");
    assert_eq!(got.pointer("/Volume/Mountpoint"), Some(&json!("")), "{got}");

    let listed = post(&socket, "/VolumeDriver.List", json!({})).await;
    assert_eq!(listed, json!({"Volumes": [], "Err": ""}));
    server.abort();
}

#[tokio::test]
async fn the_cut_converges_after_a_failure_at_every_step() {
    converges_after_each_failure(
        "/Volume.Copy.Cut",
        json!({"name": "data", "target": "f-1"}),
        |test| {
            let started = writer(test, &[("w-1-1", 11)]);
            set_property(test, ROOT, "written@f-1", "0");
            started
        },
    )
    .await;
}

#[tokio::test]
async fn the_cut_refuses_a_root_written_after_an_existing_target() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("f-1", 13), ("w-1-1", 11)]);

    let reply = post(
        &socket,
        "/Volume.Copy.Cut",
        json!({"name": "data", "target": "f-1"}),
    )
    .await;
    assert_eq!(reason(&reply), "written");
    assert_eq!(
        reply.pointer("/Err/details/copy/kind"),
        Some(&json!("root"))
    );
    assert!(effects(&commands(&test)).is_empty(), "{}", commands(&test));
    server.abort();
}

#[tokio::test]
async fn the_cut_refuses_a_machine_holding_a_different_copy() {
    let test = TestDir::new();
    let (socket, server) = slot(&test, &[("w-1-1", 11)]);

    let reply = post(
        &socket,
        "/Volume.Copy.Cut",
        json!({"name": "data", "target": "f-1"}),
    )
    .await;
    assert_eq!(reason(&reply), "no_transition");
    assert!(effects(&commands(&test)).is_empty());
    server.abort();
}

#[tokio::test]
async fn a_run_snapshot_is_taken_once_and_the_newest_two_kept() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("w-1-2", 12), ("w-1-1", 11)]);

    let request = json!({"name": "data", "snapshot": "w-1-3"});
    let reply = post(&socket, "/Volume.Copy.Snapshot", request.clone()).await;
    assert_eq!(reply.pointer("/Ok/newest/name"), Some(&json!("w-1-3")));
    assert_eq!(snapshot_names(&test, ROOT), ["w-1-3", "w-1-2"]);

    let before = commands(&test);
    assert_eq!(post(&socket, "/Volume.Copy.Snapshot", request).await, reply);
    assert!(effects(&commands(&test)[before.len()..]).is_empty());
    server.abort();
}

#[tokio::test]
async fn a_run_snapshot_is_skipped_when_nothing_was_written_since_the_newest() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("w-1-2", 12)]);
    set_property(&test, ROOT, "written@w-1-2", "0");

    let reply = post(
        &socket,
        "/Volume.Copy.Snapshot",
        json!({"name": "data", "snapshot": "w-1-3"}),
    )
    .await;
    assert_eq!(reply.pointer("/Ok/newest/name"), Some(&json!("w-1-2")));
    assert!(effects(&commands(&test)).is_empty());
    server.abort();
}

#[tokio::test]
async fn a_run_snapshot_converges_after_a_failure_at_every_step() {
    converges_after_each_failure(
        "/Volume.Copy.Snapshot",
        json!({"name": "data", "snapshot": "w-1-3"}),
        |test| writer(test, &[("w-1-2", 12), ("w-1-1", 11)]),
    )
    .await;
}

#[tokio::test]
async fn forget_drops_every_run_snapshot() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("w-1-2", 12), ("w-1-1", 11)]);

    let reply = post(&socket, "/Volume.Copy.Forget", json!({"name": "data"})).await;
    assert_eq!(reply.pointer("/Ok/newest"), Some(&Value::Null));
    assert!(snapshot_names(&test, ROOT).is_empty());
    let before = commands(&test);
    assert_eq!(
        post(&socket, "/Volume.Copy.Forget", json!({"name": "data"})).await,
        reply
    );
    assert!(effects(&commands(&test)[before.len()..]).is_empty());
    server.abort();
}

#[tokio::test]
async fn forget_converges_after_a_failure_at_every_step() {
    converges_after_each_failure("/Volume.Copy.Forget", json!({"name": "data"}), |test| {
        writer(test, &[("w-1-2", 12), ("w-1-1", 11)])
    })
    .await;
}

#[tokio::test]
async fn the_writer_route_promotes_a_pulled_slot() {
    let test = TestDir::new();
    let (socket, server) = slot(&test, &[("f-1", 13)]);

    let request = json!({"name": "data", "at": "13", "epoch": 2});
    let reply = post(&socket, "/Volume.Copy.Writer", request.clone()).await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("root")));
    assert_eq!(reply.pointer("/Ok/readonly"), Some(&json!(false)));
    assert_eq!(reply.pointer("/Ok/epoch"), Some(&json!(2)));
    assert_eq!(reply.pointer("/Ok/newest/name"), Some(&json!("f-1")));
    assert_eq!(
        effects(&commands(&test)),
        [
            "zfs inherit ployz:pulled tank/ployz-mirror/data/fs",
            "zfs set ployz:epoch=2 tank/ployz-mirror/data/fs",
            "zfs set ployz:promote=on tank/ployz-mirror/data/fs",
            "zfs rename tank/ployz-mirror/data/fs tank/ployz/data",
            "zfs destroy tank/ployz-mirror/data",
            "zfs inherit mountpoint tank/ployz/data",
            "zfs set readonly=off tank/ployz/data",
        ]
    );
    assert_eq!(
        property(&test, ROOT, "ployz:promote").as_deref(),
        Some("on")
    );
    assert!(!test.0.join("mirror").exists());

    let before = commands(&test);
    assert_eq!(post(&socket, "/Volume.Copy.Writer", request).await, reply);
    assert!(effects(&commands(&test)[before.len()..]).is_empty());
    server.abort();
}

#[tokio::test]
async fn the_writer_route_promotes_a_pulled_slot_on_a_pool_without_the_managed_parent() {
    let test = TestDir::new();
    let (socket, server) = start(&test, USABLE_POOL, &["mirror", "mirror-fs"]);
    set_snapshots(&test, SLOT_FS, &[("f-1", 13)]);

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 2}),
    )
    .await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("root")), "{reply}");
    let effects = effects(&commands(&test));
    let created = effects
        .iter()
        .position(|line| {
            line == "zfs create -o canmount=off -o mountpoint=/var/lib/ployz-volumes tank/ployz"
        })
        .unwrap_or_else(|| panic!("{effects:?}"));
    let renamed = effects
        .iter()
        .position(|line| line == "zfs rename tank/ployz-mirror/data/fs tank/ployz/data")
        .unwrap_or_else(|| panic!("{effects:?}"));
    assert!(created < renamed, "{effects:?}");
    assert!(test.0.join("root").exists());
    server.abort();
}

#[tokio::test]
async fn the_writer_route_aborts_a_partial_receive_before_promoting_the_slot() {
    let test = TestDir::new();
    let (socket, server) = slot(&test, &[("f-1", 13)]);
    set_property(&test, SLOT_FS, "receive_resume_token", "token-1");

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 2}),
    )
    .await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("root")), "{reply}");
    assert_eq!(reply.pointer("/Ok/newest/name"), Some(&json!("f-1")));
    assert_eq!(
        effects(&commands(&test)).first().map(String::as_str),
        Some("zfs receive -A tank/ployz-mirror/data/fs")
    );
    assert!(property(&test, ROOT, "receive_resume_token").is_none());
    server.abort();
}

#[tokio::test]
async fn the_writer_route_restores_a_cut_root_without_hiding_it_from_docker() {
    let test = TestDir::new();
    let (socket, server) = slot(&test, &[("f-1", 13)]);
    set_property(&test, SLOT_FS, "ployz:cut", "on");

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 0}),
    )
    .await;
    assert_eq!(reply.pointer("/Ok/readonly"), Some(&json!(false)));
    assert!(property(&test, ROOT, "ployz:promote").is_none());
    assert!(property(&test, ROOT, "ployz:cut").is_none());
    assert!(
        effects(&commands(&test))
            .iter()
            .all(|effect| !effect.contains("ployz:promote"))
    );
    server.abort();
}

#[tokio::test]
async fn the_writer_route_converges_after_a_failure_at_every_step() {
    converges_after_each_failure(
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 2}),
        |test| slot(test, &[("f-1", 13)]),
    )
    .await;
}

#[tokio::test]
async fn the_writer_route_refuses_a_slot_behind_the_wanted_snapshot() {
    let test = TestDir::new();
    let (socket, server) = slot(&test, &[("w-1-2", 12)]);

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 2}),
    )
    .await;
    assert_eq!(reason(&reply), "behind");
    assert_eq!(
        reply.pointer("/Err/details/copy/kind"),
        Some(&json!("slot"))
    );
    assert!(effects(&commands(&test)).is_empty());
    server.abort();
}

#[tokio::test]
async fn the_writer_route_refuses_a_root_of_a_later_epoch() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("f-1", 13)]);
    set_property(&test, ROOT, "ployz:epoch", "2");
    fs::write(test.0.join("readonly-volume"), "").unwrap();

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 1}),
    )
    .await;
    assert_eq!(reason(&reply), "no_transition");
    assert!(effects(&commands(&test)).is_empty());
    server.abort();
}

#[tokio::test]
async fn the_writer_route_raises_the_epoch_of_a_root_at_the_snapshot() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("f-1", 13)]);
    set_property(&test, ROOT, "ployz:epoch", "1");

    let request = json!({"name": "data", "at": "13", "epoch": 2});
    let reply = post(&socket, "/Volume.Copy.Writer", request.clone()).await;
    assert_eq!(reply.pointer("/Ok/epoch"), Some(&json!(2)), "{reply}");
    assert_eq!(reply.pointer("/Ok/readonly"), Some(&json!(false)));
    assert_eq!(
        effects(&commands(&test)),
        ["zfs set ployz:epoch=2 tank/ployz/data"]
    );

    let before = commands(&test);
    assert_eq!(post(&socket, "/Volume.Copy.Writer", request).await, reply);
    assert!(effects(&commands(&test)[before.len()..]).is_empty());
    server.abort();
}

#[tokio::test]
async fn the_writer_route_refuses_to_raise_the_epoch_of_a_root_behind_the_snapshot() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("w-1-2", 12)]);
    set_property(&test, ROOT, "ployz:epoch", "1");

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 2}),
    )
    .await;
    assert_eq!(reason(&reply), "behind");
    assert!(effects(&commands(&test)).is_empty());
    server.abort();
}

#[tokio::test]
async fn the_writer_route_reopens_a_read_only_root_of_its_own_epoch() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("f-1", 13)]);
    set_property(&test, ROOT, "ployz:epoch", "1");
    set_property(&test, ROOT, "ployz:cut", "on");
    fs::write(test.0.join("readonly-volume"), "").unwrap();

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "13", "epoch": 1}),
    )
    .await;
    assert_eq!(reply.pointer("/Ok/readonly"), Some(&json!(false)));
    assert_eq!(
        effects(&commands(&test)),
        [
            "zfs set readonly=off tank/ployz/data",
            "zfs inherit ployz:cut tank/ployz/data",
        ]
    );
    server.abort();
}

/// A writer Machine's send port, answering the bound and a stream text per request:
/// the first answer whose key the request URI contains, else the default one. A held
/// answer sends its headers at once and its stream only after `release`; a silent one
/// never answers.
struct Source {
    requests: Arc<StdMutex<Vec<String>>>,
    answers: Arc<StdMutex<Vec<Answer>>>,
    gate: Arc<Semaphore>,
    port: u16,
}

#[derive(Clone)]
struct Answer {
    key: String,
    status: StatusCode,
    stream: String,
    mode: Mode,
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Now,
    Held,
    Silent,
}

impl Source {
    async fn start(bound: Option<u64>, stream: &str) -> Self {
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let answers = Arc::new(StdMutex::new(vec![Answer {
            key: String::new(),
            status: StatusCode::OK,
            stream: stream.to_owned(),
            mode: Mode::Now,
        }]));
        let gate = Arc::new(Semaphore::new(0));
        let state = (
            Arc::clone(&requests),
            Arc::clone(&answers),
            Arc::clone(&gate),
        );
        let router = Router::new().fallback(move |request: Request| {
            let (requests, answers, gate) = state.clone();
            async move {
                let uri = request.uri().to_string();
                requests.lock().unwrap().push(uri.clone());
                if uri.starts_with("/volume-bound/") {
                    return match bound {
                        Some(bound) => (StatusCode::OK, bound.to_string()).into_response(),
                        None => (StatusCode::NOT_FOUND, "no writer here").into_response(),
                    };
                }
                let answer = answers
                    .lock()
                    .unwrap()
                    .iter()
                    .rev()
                    .find(|answer| uri.contains(answer.key.as_str()))
                    .cloned()
                    .unwrap();
                match answer.mode {
                    Mode::Now => return (answer.status, answer.stream).into_response(),
                    Mode::Silent => std::future::pending::<()>().await,
                    Mode::Held => {}
                }
                let body = futures_util::stream::once(async move {
                    let _permit = gate.acquire().await;
                    Ok::<_, std::convert::Infallible>(answer.stream)
                });
                (answer.status, Body::from_stream(body)).into_response()
            }
        });
        let listener = TcpListener::bind("[::1]:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(axum::serve(listener, router).into_future());
        Self {
            requests,
            answers,
            gate,
            port,
        }
    }

    /// Answer requests whose URI contains `key` with `status` and `stream` from now on.
    fn answer(&self, key: &str, status: StatusCode, stream: &str) {
        self.push(key, status, stream, Mode::Now);
    }

    /// Like `answer`, but holding each stream until `release`.
    fn hold(&self, key: &str, stream: &str) {
        self.push(key, StatusCode::OK, stream, Mode::Held);
    }

    /// Never answer requests whose URI contains `key`, not even with headers.
    fn silence(&self, key: &str) {
        self.push(key, StatusCode::OK, "", Mode::Silent);
    }

    fn release(&self) {
        self.gate.add_permits(Semaphore::MAX_PERMITS);
    }

    fn push(&self, key: &str, status: StatusCode, stream: &str, mode: Mode) {
        self.answers.lock().unwrap().push(Answer {
            key: key.to_owned(),
            status,
            stream: stream.to_owned(),
            mode,
        });
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn puller(test: &TestDir, markers: &[&str], port: u16) -> (PathBuf, Server) {
    puller_stalling_after(test, markers, port, Duration::from_secs(60))
}

fn puller_stalling_after(
    test: &TestDir,
    markers: &[&str],
    port: u16,
    stall: Duration,
) -> (PathBuf, Server) {
    for marker in markers {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut storage = VolumeStorage::with_programs(zpool, zfs);
    storage.send_port = port;
    storage.receive_stall = stall;
    (socket, tokio::spawn(serve(listener, storage)))
}

fn pull(target: &str) -> Value {
    json!({"name": "data", "from": "::1", "target": target})
}

/// Polls the copy until the pull landed `target` read-only or stopped with a failure.
/// The receive shows `target` before the pull checks the copy's properties.
async fn landed(socket: &Path, target: &str) -> Value {
    for _ in 0..300 {
        let copy = post(socket, "/Volume.Copy.Inspect", json!({"name": "data"})).await;
        if (copy.pointer("/Ok/newest/name") == Some(&json!(target))
            && copy.pointer("/Ok/readonly") == Some(&json!(true)))
            || copy
                .pointer("/Ok/pull/failure")
                .is_some_and(|failure| !failure.is_null())
        {
            return copy;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the pull never settled");
}

/// Resends the Pull until it finishes the landed receive. Inspect shows the target a
/// moment before the receive task records its outcome, and a resend in that window
/// still finds the receive running.
async fn finished(socket: &Path, target: &str) -> Value {
    for _ in 0..300 {
        let reply = post(socket, "/Volume.Copy.Pull", pull(target)).await;
        if reply.pointer("/Ok/pull") == Some(&Value::Null) {
            return reply;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the pull of {target} never finished");
}

#[tokio::test]
async fn a_pull_into_no_slot_sizes_the_slot_from_the_writer_and_lands_the_target() {
    let source = Source::start(Some(BOUND), "snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root"], source.port);

    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("slot")));
    assert_eq!(reply.pointer("/Ok/newest"), Some(&Value::Null));
    assert_eq!(reply.pointer("/Ok/pull/target"), Some(&json!("w-1-1")));
    assert_eq!(
        source.requests().first().map(String::as_str),
        Some("/volume-bound/data"),
        "{:?}",
        source.requests()
    );
    let copy = landed(&socket, "w-1-1").await;
    assert_eq!(copy.pointer("/Ok/newest/guid"), Some(&json!("77")));
    assert_eq!(copy.pointer("/Ok/pull/target"), Some(&json!("w-1-1")));
    assert_eq!(
        property(&test, SLOT, "refquota").as_deref(),
        Some("1073741824")
    );
    assert_eq!(property(&test, SLOT_FS, "readonly").as_deref(), Some("on"));

    let reply = finished(&socket, "w-1-1").await;
    assert_eq!(reply.pointer("/Ok/newest/name"), Some(&json!("w-1-1")));
    assert!(property(&test, SLOT, "ployz:pull").is_none());
    assert_eq!(
        source.requests(),
        ["/volume-bound/data", "/volume-send/data?target=w-1-1"]
    );
    server.abort();
}

#[tokio::test]
async fn a_pull_that_leaves_the_copy_writable_fails() {
    let source = Source::start(Some(BOUND), "snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "readonly-lost"], source.port);

    post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    let copy = landed(&socket, "w-1-1").await;
    let failure = copy.pointer("/Ok/pull/failure").unwrap().as_str().unwrap();
    assert!(failure.contains("readonly=off"), "{failure}");
    server.abort();
}

#[tokio::test]
async fn a_stalled_receive_process_fails_the_pull_and_dies() {
    for marker in ["stall-read", "stall-exit"] {
        let source = Source::start(Some(BOUND), &"x".repeat(2 * 1024 * 1024)).await;
        let test = TestDir::new();
        fs::create_dir_all(test.0.join("props")).unwrap();
        fs::write(test.0.join("props").join(marker), "").unwrap();
        let (socket, server) =
            puller_stalling_after(&test, &["root"], source.port, Duration::from_millis(200));

        post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
        let copy = landed(&socket, "w-1-1").await;
        let failure = copy.pointer("/Ok/pull/failure").unwrap().as_str().unwrap();
        assert!(failure.contains("stalled"), "{marker}: {failure}");
        let pid = fs::read_to_string(test.0.join("props/receive-pid")).unwrap();
        for _ in 0..100 {
            if !Path::new(&format!("/proc/{}", pid.trim())).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            !Path::new(&format!("/proc/{}", pid.trim())).exists(),
            "{marker}: receive child survived its deadline"
        );
        let reply = post(&socket, "/Volume.Copy.Destroy", json!({"name": "data"})).await;
        assert!(reply.get("Ok").is_some(), "{marker}: {reply}");
        server.abort();
    }
}

#[tokio::test]
async fn a_pull_resumes_an_interrupted_receive_then_prunes_to_the_target() {
    let source = Source::start(Some(BOUND), "break\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);

    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    let copy = landed(&socket, "w-1-2").await;
    assert!(
        copy.pointer("/Ok/pull/failure")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("connection reset")
    );
    assert_eq!(copy.pointer("/Ok/resume_token"), Some(&json!("token-1")));

    source.answer("token=", StatusCode::OK, "snapshot w-1-2 12\n");
    // The first resend answers the failure; the second resumes.
    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    let copy = landed(&socket, "w-1-2").await;
    assert_eq!(copy.pointer("/Ok/newest/guid"), Some(&json!("12")));
    finished(&socket, "w-1-2").await;
    assert_eq!(snapshot_names(&test, SLOT_FS), ["w-1-2"]);
    assert_eq!(
        source.requests(),
        [
            "/volume-bound/data",
            "/volume-send/data?target=w-1-2&base=11",
            "/volume-bound/data",
            "/volume-send/data?target=w-1-2&token=token-1",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn a_pull_resent_after_its_receive_failed_answers_the_failure_once_then_retries() {
    let source = Source::start(Some(BOUND), "break\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);

    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    landed(&socket, "w-1-2").await;
    let sent = source.requests().len();

    source.answer("token=", StatusCode::OK, "snapshot w-1-2 12\n");
    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    assert!(
        reply
            .pointer("/Ok/pull/failure")
            .and_then(Value::as_str)
            .is_some_and(|failure| failure.contains("connection reset")),
        "{reply}"
    );
    assert_eq!(source.requests().len(), sent);

    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    let copy = landed(&socket, "w-1-2").await;
    assert_eq!(
        copy.pointer("/Ok/newest/guid"),
        Some(&json!("12")),
        "{copy}"
    );
    server.abort();
}

#[tokio::test]
async fn a_pull_resent_after_a_restart_resumes_the_stopped_receive_by_its_token() {
    let source = Source::start(Some(BOUND), "snapshot w-1-2 12\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
    set_property(&test, SLOT, "ployz:pull", "w-1-2");
    set_property(&test, SLOT_FS, "receive_resume_token", "token-1");

    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    assert_eq!(
        reply.pointer("/Ok/pull/target"),
        Some(&json!("w-1-2")),
        "{reply}"
    );
    let copy = landed(&socket, "w-1-2").await;
    assert_eq!(
        copy.pointer("/Ok/newest/guid"),
        Some(&json!("12")),
        "{copy}"
    );
    finished(&socket, "w-1-2").await;
    assert_eq!(
        source.requests(),
        [
            "/volume-bound/data",
            "/volume-send/data?target=w-1-2&token=token-1",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn a_pull_whose_base_the_writer_lost_starts_over_from_a_full_send() {
    let source = Source::start(Some(BOUND), "snapshot w-1-2 12\n").await;
    source.answer(
        "base=",
        StatusCode::NOT_FOUND,
        "snapshot 11 is not on tank/ployz/data",
    );
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);

    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    let copy = landed(&socket, "w-1-2").await;
    assert_eq!(
        copy.pointer("/Ok/newest/guid"),
        Some(&json!("12")),
        "{copy}"
    );
    assert!(commands(&test).contains("zfs destroy -r tank/ployz-mirror/data/fs"));
    assert_eq!(
        source.requests(),
        [
            "/volume-bound/data",
            "/volume-send/data?target=w-1-2&base=11",
            "/volume-send/data?target=w-1-2",
        ]
    );
    server.abort();
}

#[tokio::test]
async fn a_pull_resent_with_its_first_expect_continues_a_full_send_that_emptied_the_slot() {
    let source = Source::start(Some(BOUND), "").await;
    source.hold("target=w-1-2", "snapshot w-1-2 12\n");
    source.answer(
        "base=",
        StatusCode::NOT_FOUND,
        "snapshot 11 is not on tank/ployz/data",
    );
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
    let machine = Machine { test, socket };
    let expect = mirror_expect(&machine.copy().await);

    machine.call("/Volume.Copy.Pull", pull("w-1-2")).await;
    for _ in 0..300 {
        if commands(&machine.test).contains("zfs destroy -r tank/ployz-mirror/data/fs") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let copy = machine.copy().await;
    assert_eq!(copy.pointer("/Ok/newest"), Some(&Value::Null), "{copy}");

    let reply = set_mirror(&machine, &expect, "w-1-2").await;
    assert_eq!(
        reply.pointer("/Ok/pull/target"),
        Some(&json!("w-1-2")),
        "{reply}"
    );
    source.release();
    let copy = landed(&machine.socket, "w-1-2").await;
    assert_eq!(
        copy.pointer("/Ok/newest/guid"),
        Some(&json!("12")),
        "{copy}"
    );
    finished(&machine.socket, "w-1-2").await;
    server.abort();
}

#[tokio::test]
async fn a_pull_whose_target_the_writer_lacks_keeps_the_slot_copy() {
    let source = Source::start(Some(BOUND), "").await;
    source.answer(
        "",
        StatusCode::NOT_FOUND,
        "no snapshot w-1-2 on tank/ployz/data",
    );
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);

    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    let copy = landed(&socket, "w-1-2").await;
    assert!(
        copy.pointer("/Ok/pull/failure")
            .is_some_and(|failure| !failure.is_null()),
        "{copy}"
    );
    assert_eq!(
        copy.pointer("/Ok/newest/guid"),
        Some(&json!("11")),
        "{copy}"
    );
    assert_eq!(snapshot_names(&test, SLOT_FS), ["w-1-1"]);
    assert!(
        !commands(&test).contains("destroy -r"),
        "{}",
        commands(&test)
    );
    server.abort();
}

#[tokio::test]
async fn a_pull_whose_token_the_writer_lost_after_a_first_receive_broke_lands_in_one_pull() {
    let source = Source::start(Some(BOUND), "break\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root"], source.port);

    post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    let copy = landed(&socket, "w-1-1").await;
    assert_eq!(
        copy.pointer("/Ok/resume_token"),
        Some(&json!("token-1")),
        "{copy}"
    );

    source.answer("target=w-1-1", StatusCode::OK, "snapshot w-1-1 77\n");
    source.answer("token=", StatusCode::NOT_FOUND, "token-1 is not resumable");
    post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    let copy = landed(&socket, "w-1-1").await;
    assert_eq!(
        copy.pointer("/Ok/newest/guid"),
        Some(&json!("77")),
        "{copy}"
    );
    assert_eq!(
        source.requests().get(3..),
        Some(
            &[
                "/volume-send/data?target=w-1-1&token=token-1".to_owned(),
                "/volume-send/data?target=w-1-1".to_owned(),
            ][..]
        )
    );
    server.abort();
}

#[tokio::test]
async fn a_pull_for_another_target_while_a_receive_runs_refuses_busy_and_keeps_the_label() {
    let source = Source::start(Some(BOUND), "snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root"], source.port);
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(test.0.join("props/stall-read"), "").unwrap();

    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(
        reply.pointer("/Ok/pull/target"),
        Some(&json!("w-1-1")),
        "{reply}"
    );
    let before = commands(&test);

    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    assert_eq!(reason(&reply), "busy", "{reply}");
    assert_eq!(
        property(&test, SLOT, "ployz:pull").as_deref(),
        Some("w-1-1")
    );
    assert!(effects(&commands(&test)[before.len()..]).is_empty());

    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(
        reply.pointer("/Ok/pull/target"),
        Some(&json!("w-1-1")),
        "{reply}"
    );
    server.abort();
}

#[tokio::test]
async fn departure_during_a_receive_stops_it_and_destroys_the_mirror() {
    let source = Source::start(Some(BOUND), "snapshot w-1-1 77\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root"], source.port);
    fs::create_dir_all(test.0.join("props")).unwrap();
    fs::write(test.0.join("props/stall-read"), "").unwrap();
    post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    let pid = test.0.join("props/receive-pid");
    for _ in 0..300 {
        if pid.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let pid = fs::read_to_string(pid).unwrap();

    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
    assert!(!Path::new(&format!("/proc/{}", pid.trim())).exists());
    let copy = post(&socket, "/Volume.Copy.Inspect", json!({"name": "data"})).await;
    assert_eq!(copy, json!({"Ok": null}));
    assert!(!test.0.join("mirror").exists());
    server.abort();
}

/// Waits for the fake receive to start, and answers its pid.
async fn receive_pid(test: &TestDir) -> String {
    let pid = test.0.join("props/receive-pid");
    for _ in 0..300 {
        if pid.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fs::read_to_string(pid).unwrap().trim().to_owned()
}

#[tokio::test]
async fn departure_stops_a_pull_into_a_slot_it_keeps() {
    for (mark, value) in [("ployz:cut", "on"), ("ployz:epoch", "3")] {
        let source = Source::start(Some(BOUND), "snapshot w-1-2 12\n").await;
        let test = TestDir::new();
        let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
        set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
        set_property(&test, SLOT_FS, mark, value);
        fs::write(test.0.join("props/stall-read"), "").unwrap();
        post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
        let pid = receive_pid(&test).await;
        let scan =
            "list -Hp -o name,refquota,used,usedbydataset,mountpoint,mounted,readonly -r tank";
        fs::write(test.0.join("props/stall-command"), scan).unwrap();

        let departure = tokio::spawn({
            let socket = socket.clone();
            async move { post(&socket, "/Storage.Demote", json!(null)).await }
        });
        let scanning = format!("zfs {scan}");
        for _ in 0..500 {
            let commands = fs::read_to_string(test.0.join("commands")).unwrap_or_default();
            if commands.lines().any(|line| line == scanning) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        // A receive still running at the scan lands now, before the departure judges.
        fs::remove_file(test.0.join("props/stall-read")).unwrap();
        for _ in 0..500 {
            if !Path::new(&format!("/proc/{pid}")).exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        fs::remove_file(test.0.join("props/stall-command")).unwrap();
        let reply = departure.await.unwrap();
        assert_eq!(
            reply,
            json!({"Ok": {"demoted": [], "stuck": [], "failed": []}}),
            "{mark}"
        );
        assert!(test.0.join("mirror-fs").exists(), "{mark}");
        assert_eq!(property(&test, SLOT_FS, mark).as_deref(), Some(value));
        server.abort();
    }
}

#[tokio::test]
async fn a_landed_pull_drops_the_writer_marks_so_the_departure_destroys_the_mirror() {
    for (mark, value) in [
        ("ployz:cut", "on"),
        ("ployz:epoch", "3"),
        ("ployz:promote", "on"),
        ("ployz:seed", "11"),
    ] {
        let source = Source::start(Some(BOUND), "snapshot w-1-2 12\n").await;
        let test = TestDir::new();
        let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
        set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
        set_property(&test, SLOT_FS, mark, value);

        post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
        finished(&socket, "w-1-2").await;
        assert_eq!(property(&test, SLOT_FS, mark), None, "{mark}");
        assert_eq!(
            property(&test, SLOT_FS, "ployz:pulled").as_deref(),
            Some("12"),
            "{mark}"
        );

        let reply = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_eq!(
            reply,
            json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}}),
            "{mark}"
        );
        assert!(!test.0.join("mirror").exists(), "{mark}");
        server.abort();
    }
}

#[tokio::test]
async fn a_pull_of_the_slots_newest_keeps_its_cut() {
    let source = Source::start(Some(BOUND), "snapshot w-1-1 11\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
    set_property(&test, SLOT_FS, "ployz:cut", "on");

    finished(&socket, "w-1-1").await;
    assert_eq!(
        property(&test, SLOT_FS, "ployz:cut").as_deref(),
        Some("on"),
        "nothing newer landed, so the cut may still be the only copy"
    );
    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": [], "stuck": [], "failed": []}})
    );
    assert!(test.0.join("mirror-fs").exists());
    server.abort();
}

/// The state a pull leaves when the daemon stops between landing the target and the
/// resend that settles it, on a slot whose own past left a mark: the slot's parent still
/// names the pull. A release before the pulled mark leaves a slot its newest point never
/// marked, which a cut's same-newest resend leaves too.
fn landed_before_a_restart(test: &TestDir, mark: &str, value: &str) {
    set_snapshots(test, SLOT_FS, &[("w-1-2", 12)]);
    set_property(test, SLOT, "ployz:pull", "w-1-2");
    set_property(test, SLOT_FS, mark, value);
}

#[tokio::test]
async fn a_resend_of_the_slots_newest_keeps_its_marks_and_marks_no_pull() {
    for (mark, value) in [("ployz:cut", "on"), ("ployz:epoch", "3")] {
        let test = TestDir::new();
        let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], 1);
        landed_before_a_restart(&test, mark, value);

        let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
        assert_eq!(reply.pointer("/Ok/pull"), Some(&Value::Null), "{reply}");
        assert_eq!(property(&test, SLOT_FS, mark).as_deref(), Some(value));
        assert_eq!(property(&test, SLOT_FS, "ployz:pulled"), None, "{mark}");
        let reply = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_eq!(
            reply,
            json!({"Ok": {"demoted": [], "stuck": [], "failed": []}}),
            "{mark}"
        );
        assert!(test.0.join("mirror-fs").exists(), "{mark}");
        server.abort();
    }
}

#[tokio::test]
async fn a_pull_stopped_over_a_marked_slot_leaves_it_kept() {
    for (mark, value) in [("ployz:cut", "on"), ("ployz:epoch", "3")] {
        let source = Source::start(Some(BOUND), "snapshot w-1-3 13\n").await;
        let test = TestDir::new();
        let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
        landed_before_a_restart(&test, mark, value);
        fs::write(test.0.join("props/stall-read"), "").unwrap();
        post(&socket, "/Volume.Copy.Pull", pull("w-1-3")).await;
        let pid = receive_pid(&test).await;

        let reply = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_eq!(
            reply,
            json!({"Ok": {"demoted": [], "stuck": [], "failed": []}}),
            "{mark}"
        );
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "{mark}: the receive still runs"
        );
        assert!(test.0.join("mirror-fs").exists(), "{mark}");
        assert_eq!(property(&test, SLOT_FS, "ployz:pulled"), None, "{mark}");
        server.abort();
    }
}

#[tokio::test]
async fn a_pull_whose_mark_cleanup_fails_leaves_a_slot_the_departure_keeps() {
    for (mark, value) in [("ployz:cut", "on"), ("ployz:epoch", "3")] {
        let source = Source::start(Some(BOUND), "snapshot w-1-2 12\n").await;
        let test = TestDir::new();
        let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
        set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
        set_property(&test, SLOT_FS, mark, value);
        fs::write(test.0.join("inherit-fails"), "").unwrap();

        post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
        let copy = landed(&socket, "w-1-2").await;
        assert_eq!(
            copy.pointer("/Ok/newest/name"),
            Some(&json!("w-1-2")),
            "{copy}"
        );
        post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
        assert_eq!(property(&test, SLOT_FS, "ployz:pulled"), None, "{mark}");

        let reply = post(&socket, "/Storage.Demote", json!(null)).await;
        assert_eq!(
            reply,
            json!({"Ok": {"demoted": [], "stuck": [], "failed": []}}),
            "{mark}"
        );
        assert!(test.0.join("mirror-fs").exists(), "{mark}");
        server.abort();
    }
}

/// The epoch a promote sets describes the slot's newest point, so a promote stopped before
/// its rename leaves a slot the departure keeps, pulled or not.
#[tokio::test]
async fn departure_keeps_a_pulled_slot_whose_promote_stopped_before_the_rename() {
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], 1);
    set_snapshots(&test, SLOT_FS, &[("w-1-2", 12)]);
    set_property(&test, SLOT, "ployz:pull", "w-1-2");
    set_property(&test, SLOT_FS, "ployz:pulled", "12");
    fs::write(
        test.0.join("props/fail-command"),
        "rename tank/ployz-mirror/data/fs tank/ployz/data",
    )
    .unwrap();

    let reply = post(
        &socket,
        "/Volume.Copy.Writer",
        json!({"name": "data", "at": "12", "epoch": 4}),
    )
    .await;
    assert!(reply.get("Err").is_some(), "{reply}");
    assert_eq!(property(&test, SLOT_FS, "ployz:pulled"), None);
    assert_eq!(
        property(&test, SLOT_FS, "ployz:epoch").as_deref(),
        Some("4")
    );
    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": [], "stuck": [], "failed": []}})
    );
    assert!(test.0.join("mirror-fs").exists());
    server.abort();
}

/// v0.3.0-beta.1 promoted a pulled slot without dropping its pull record, so a promote
/// it stopped before the rename leaves the record naming the newest point.
#[tokio::test]
async fn departure_keeps_a_beta1_promote_that_stopped_before_the_rename() {
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], 1);
    landed_before_a_restart(&test, "ployz:epoch", "2");
    set_property(&test, SLOT_FS, "ployz:promote", "on");

    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": [], "stuck": [], "failed": []}})
    );
    assert!(test.0.join("mirror-fs").exists());
    server.abort();
}

/// A pull of the point a cut slot already holds lands nothing, so the slot never records
/// a pull: a daemon stopped partway through must not leave the cut looking pulled.
#[tokio::test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
async fn a_pull_of_a_cut_slots_own_newest_never_records_a_pull() {
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], 1);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
    set_property(&test, SLOT_FS, "ployz:cut", "on");
    fs::write(
        test.0.join("props/fail-command"),
        "inherit ployz:pull tank/ployz-mirror/data",
    )
    .unwrap();

    post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(property(&test, SLOT, "ployz:pull"), None);
    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name": "data"})).await["Err"],
        json!(""),
        "Docker keeps its record for a cut Volume"
    );
    let reply = post(&socket, "/Storage.Demote", json!(null)).await;
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": [], "stuck": [], "failed": []}})
    );
    assert!(test.0.join("mirror-fs").exists());
    server.abort();
}

/// A receive that fails on its own deadline has reaped its child by the time it answers:
/// a `zfs receive` still exiting holds the slot busy for the departure's destroy.
#[tokio::test]
async fn a_receive_that_stalls_answers_only_once_its_child_is_gone() {
    for (marker, stream) in [
        ("stall-exit", "snapshot w-1-1 77\n".to_owned()),
        ("stall-read", "x".repeat(1 << 20)),
    ] {
        let source = Source::start(Some(BOUND), &stream).await;
        let test = TestDir::new();
        fs::write(test.0.join("root"), "").unwrap();
        fs::write(test.0.join("mirror"), "").unwrap();
        fs::create_dir_all(test.0.join("props")).unwrap();
        fs::write(test.0.join("props").join(marker), "").unwrap();
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let mut storage = VolumeStorage::with_programs(zpool, zfs);
        storage.send_port = source.port;
        storage.receive_stall = Duration::from_millis(200);
        let stream = ployz_core::SendStream {
            name: "data".parse().unwrap(),
            source: ployz_core::SendSource::Full {
                target: "w-1-1".parse().unwrap(),
            },
        };

        let received = storage
            .receive(
                ployz_core::ManagementAddress("::1".parse().unwrap()),
                &stream,
                SLOT_FS,
                BOUND,
                "test",
                &Stop::unowned(),
            )
            .await;
        // No await between the answer and the check: a child only killed on drop is
        // still a zombie here, and so still listed in /proc.
        let pid = fs::read_to_string(test.0.join("props/receive-pid")).unwrap();
        assert!(received.is_err(), "{marker}");
        assert!(
            !Path::new(&format!("/proc/{}", pid.trim())).exists(),
            "{marker}: the receive answered before its child was reaped"
        );
    }
}

/// A child the kill leaves running past its owner, as a `zfs` blocked in the kernel is.
fn slow_child(stop: &Stop) -> (ZfsChild, u32) {
    let mut child = ZfsChild::spawn(Command::new("sleep").arg("1.5"), stop).unwrap();
    child.spare_from_kill();
    let pid = child.id().unwrap();
    (child, pid)
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Stops every task in `receives` and answers whether `pid` had exited by then.
async fn stopped_after(receives: &Receives, pid: u32) -> bool {
    let stuck = receives.stop_all(Duration::from_secs(5)).await;
    assert!(stuck.is_empty(), "{stuck:?}");
    !alive(pid)
}

#[tokio::test]
async fn a_zfs_child_dropped_without_a_reap_is_killed() {
    let receives = Receives::default();
    let name: DockerVolumeName = "data".parse().unwrap();
    let mut pid = 0;
    receives.spawn(&name, |stop| {
        let child = ZfsChild::spawn(Command::new("sleep").arg("30"), &stop).unwrap();
        pid = child.id().unwrap();
        async move {
            drop(child);
            Ok(())
        }
    });
    assert!(stopped_after(&receives, pid).await);
}

#[tokio::test]
async fn a_receive_task_finishes_only_once_a_dropped_child_exits() {
    let receives = Receives::default();
    let name: DockerVolumeName = "data".parse().unwrap();
    let mut pid = 0;
    receives.spawn(&name, |stop| {
        let (child, spawned) = slow_child(&stop);
        pid = spawned;
        async move {
            drop(child);
            Ok(())
        }
    });
    assert!(
        stopped_after(&receives, pid).await,
        "the task finished before its child exited"
    );
}

/// The shape of the seed's copy (seed.rs `copy_snapshot`): its send is running when the
/// receive fails to spawn.
#[tokio::test]
async fn a_receive_task_whose_second_spawn_fails_finishes_once_the_first_child_exits() {
    let receives = Receives::default();
    let name: DockerVolumeName = "data".parse().unwrap();
    let mut pid = 0;
    receives.spawn(&name, |stop| {
        let (send, spawned) = slow_child(&stop);
        pid = spawned;
        async move {
            let _send = send;
            let _receive = ZfsChild::spawn(&mut Command::new("/nonexistent/zfs"), &stop)
                .map_err(|error| format!("could not run zfs receive: {error}"))?;
            Ok(())
        }
    });
    assert!(
        stopped_after(&receives, pid).await,
        "the task finished before its send exited"
    );
    assert!(receives.failure(&name).is_some());
}

#[tokio::test]
async fn a_receive_task_that_panics_with_a_live_child_finishes_once_it_exits() {
    let receives = Receives::default();
    let name: DockerVolumeName = "data".parse().unwrap();
    let mut pid = 0;
    receives.spawn(&name, |stop| {
        let (child, spawned) = slow_child(&stop);
        pid = spawned;
        async move {
            let _child = child;
            panic!("the receive task broke");
        }
    });
    assert!(
        stopped_after(&receives, pid).await,
        "the task finished before its child exited"
    );
    assert!(!receives.running(&name), "a panicked task reads as running");
    assert!(receives.failure(&name).is_some());
}

/// A child the kill leaves running until the test removes `gate`, as a `zfs` blocked in
/// the kernel is until the kernel lets it go.
fn held_child(stop: &Stop, gate: &Path) -> ZfsChild {
    fs::write(gate, "").unwrap();
    let mut child = ZfsChild::spawn(
        Command::new("sh").args([
            "-c",
            &format!("while [ -e '{}' ]; do sleep 0.01; done", gate.display()),
        ]),
        stop,
    )
    .unwrap();
    child.spare_from_kill();
    child
}

/// A task of `name` that panics while `gate` holds its child, answered once its failure
/// is published and the child is still alive.
async fn panicked_with_a_held_child(receives: &Receives, name: &DockerVolumeName, gate: &Path) {
    receives.spawn(name, |stop| {
        let child = held_child(&stop, gate);
        async move {
            let _child = child;
            panic!("the receive task broke");
        }
    });
    for _ in 0..500 {
        if receives.failure(name).is_some() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the panic was never published");
}

/// A same-target retry (copy.rs `copy_pull`) answers and forgets a failure only once the
/// Volume reads as not running, so the failed task must read as running until its child
/// has exited.
#[tokio::test]
async fn a_panicked_task_reads_as_running_until_its_child_exits() {
    let test = TestDir::new();
    let gate = test.0.join("gate");
    let receives = Receives::default();
    let name: DockerVolumeName = "data".parse().unwrap();
    panicked_with_a_held_child(&receives, &name, &gate).await;
    assert!(
        receives.running(&name),
        "a retry would forget a task whose child is alive"
    );
    fs::remove_file(&gate).unwrap();
    assert!(receives.stop_all(Duration::from_secs(5)).await.is_empty());
    assert!(!receives.running(&name));
    assert!(receives.failure(&name).is_some());
}

#[tokio::test]
async fn a_forget_while_a_child_is_reaped_leaves_it_for_the_departure_to_name() {
    let test = TestDir::new();
    let gate = test.0.join("gate");
    let receives = Receives::default();
    let name: DockerVolumeName = "data".parse().unwrap();
    panicked_with_a_held_child(&receives, &name, &gate).await;
    receives.forget(&name);
    assert_eq!(
        receives.stop_all(Duration::from_millis(200)).await,
        ["data"],
        "the departure lost a child that still holds the slot"
    );
    assert!(
        receives.failure(&name).is_none(),
        "the forgotten failure came back"
    );
    fs::remove_file(&gate).unwrap();
    assert!(receives.stop_all(Duration::from_secs(5)).await.is_empty());
}

#[tokio::test]
async fn a_task_that_replaces_one_still_reaping_a_child_leaves_that_child_for_the_departure() {
    let test = TestDir::new();
    let gate = test.0.join("gate");
    let receives = Receives::default();
    let name: DockerVolumeName = "data".parse().unwrap();
    panicked_with_a_held_child(&receives, &name, &gate).await;
    receives.spawn(&name, |_stop| async { Ok(()) });
    assert_eq!(
        receives.stop_all(Duration::from_millis(200)).await,
        ["data"],
        "the departure lost a child that still holds the slot"
    );
    fs::remove_file(&gate).unwrap();
    assert!(receives.stop_all(Duration::from_secs(5)).await.is_empty());
    assert!(!receives.running(&name));
}

#[tokio::test]
async fn departure_stops_a_pull_whose_writer_never_answers() {
    let source = Source::start(Some(BOUND), "").await;
    source.silence("/volume-send/");
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root"], source.port);
    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("slot")), "{reply}");
    for _ in 0..300 {
        if source
            .requests()
            .iter()
            .any(|uri| uri.starts_with("/volume-send/"))
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let reply = tokio::time::timeout(
        Duration::from_secs(5),
        post(&socket, "/Storage.Demote", json!(null)),
    )
    .await
    .expect("the departure waits on a writer that never answers");
    assert_eq!(
        reply,
        json!({"Ok": {"demoted": ["data"], "stuck": [], "failed": []}})
    );
    assert!(!test.0.join("mirror").exists());
    server.abort();
}

#[tokio::test]
async fn a_pull_into_a_slot_smaller_than_the_writer_raises_the_slot_to_the_writer_bound() {
    let source = Source::start(Some(2 * BOUND), "snapshot w-1-2 12\n").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root", "mirror", "mirror-fs"], source.port);
    set_snapshots(&test, SLOT_FS, &[("w-1-1", 11)]);
    set_property(&test, SLOT, "refquota", &BOUND.to_string());

    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    let copy = landed(&socket, "w-1-2").await;
    assert_eq!(
        copy.pointer("/Ok/newest/guid"),
        Some(&json!("12")),
        "{copy}"
    );
    assert_eq!(
        property(&test, SLOT, "refquota").as_deref(),
        Some("2147483648")
    );
    assert!(
        commands(&test).contains("refquota=2147483648 tank/ployz-mirror/data/fs"),
        "{}",
        commands(&test)
    );
    server.abort();
}

#[tokio::test]
async fn a_pull_refuses_a_writer_that_has_no_copy_to_send() {
    let source = Source::start(None, "").await;
    let test = TestDir::new();
    let (socket, server) = puller(&test, &["root"], source.port);

    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(reason(&reply), "no_source");
    assert!(effects(&commands(&test)).is_empty());
    server.abort();
}

#[tokio::test]
async fn a_pull_converges_after_a_failure_before_the_receive_starts() {
    let source = Source::start(Some(BOUND), "snapshot w-1-1 77\n").await;
    for step in [
        "create -o canmount=off -o mountpoint=/var/lib/ployz-mirror -o readonly=on tank/ployz-mirror",
        "create -o canmount=off -o readonly=on -o refquota=1073741824 tank/ployz-mirror/data",
        "set ployz:pull=w-1-1 tank/ployz-mirror/data",
    ] {
        let test = TestDir::new();
        let (socket, server) = puller(&test, &["root"], source.port);
        fs::create_dir_all(test.0.join("props")).unwrap();
        fs::write(test.0.join("props/fail-command"), step).unwrap();
        let failed = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
        assert!(failed.get("Err").is_some(), "{step}: {failed}");
        assert!(
            !test.0.join("props/fail-command").exists(),
            "{step} never ran"
        );
        let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
        assert!(reply.get("Ok").is_some(), "{step}: {reply}");
        landed(&socket, "w-1-1").await;
        finished(&socket, "w-1-1").await;
        assert_eq!(
            property(&test, SLOT, "refquota").as_deref(),
            Some("1073741824")
        );
        server.abort();
    }
}

fn poolless(test: &TestDir, port: u16) -> (PathBuf, Server) {
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut storage = fake_first_pool(&test.0, 4096);
    storage.send_port = port;
    (socket, tokio::spawn(serve(listener, storage)))
}

#[tokio::test]
async fn a_pull_onto_a_machine_without_a_pool_creates_the_pool_sized_for_the_writer() {
    let source = Source::start(Some(BOUND), "").await;
    source.hold("/volume-send/", "");
    let test = TestDir::new();
    let (socket, server) = poolless(&test, source.port);

    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("slot")), "{reply}");
    assert_eq!(reply.pointer("/Ok/pull/target"), Some(&json!("w-1-1")));
    let log = commands(&test);
    assert_eq!(log.matches("zpool create").count(), 1, "{log}");
    assert!(log.contains("fallocate -l 2147483648 "), "{log}");
    assert_eq!(
        fs::read_to_string(test.0.join("slot")).unwrap(),
        format!("{BOUND}\n")
    );

    assert_eq!(
        post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await,
        reply
    );
    assert_eq!(commands(&test).matches("zpool create").count(), 1);
    source.release();
    server.abort();
}

#[tokio::test]
async fn a_pull_that_fails_after_creating_the_first_pool_converges_on_resend() {
    let source = Source::start(Some(BOUND), "").await;
    source.hold("/volume-send/", "");
    let test = TestDir::new();
    fs::write(test.0.join("fail-slot-once"), "").unwrap();
    let (socket, server) = poolless(&test, source.port);

    let failed = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert!(failed.get("Err").is_some(), "{failed}");
    assert!(test.0.join("pool").exists());

    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(reply.pointer("/Ok/kind"), Some(&json!("slot")), "{reply}");
    assert_eq!(commands(&test).matches("zpool create").count(), 1);
    assert!(test.0.join("slot").exists());
    source.release();
    server.abort();
}

#[tokio::test]
async fn a_machine_without_a_pool_has_no_copy_to_inspect_forget_or_destroy() {
    let test = TestDir::new();
    let (socket, server) = poolless(&test, ployz_core::VOLUME_SEND_PORT);

    for route in [
        "/Volume.Copy.Inspect",
        "/Volume.Copy.Forget",
        "/Volume.Copy.Destroy",
    ] {
        assert_eq!(
            post(&socket, route, json!({"name": "data"})).await,
            json!({"Ok": null}),
            "{route}"
        );
    }
    let writer = json!({"name": "data", "at": "13", "epoch": 2});
    let reply = post(&socket, "/Volume.Copy.Writer", writer).await;
    assert!(reply.get("Err").is_some(), "{reply}");
    let log = commands(&test);
    assert!(!log.contains("zpool create"), "{log}");
    assert!(!log.contains("fallocate"), "{log}");
    server.abort();
}

#[tokio::test]
async fn destroy_drops_the_slot_and_leaves_a_root_alone() {
    let test = TestDir::new();
    let (socket, server) = slot(&test, &[("w-1-1", 11)]);

    let reply = post(&socket, "/Volume.Copy.Destroy", json!({"name": "data"})).await;
    assert_eq!(reply, json!({"Ok": null}));
    assert!(!test.0.join("mirror").exists());
    assert_eq!(
        post(&socket, "/Volume.Copy.Destroy", json!({"name": "data"})).await,
        reply
    );
    assert_eq!(
        effects(&commands(&test)),
        ["zfs destroy -r tank/ployz-mirror/data"]
    );
    server.abort();

    let test = TestDir::new();
    let (socket, server) = writer(&test, &[]);
    let reply = post(&socket, "/Volume.Copy.Destroy", json!({"name": "data"})).await;
    assert_eq!(reason(&reply), "no_transition");
    assert!(effects(&commands(&test)).is_empty());
    server.abort();
}

#[tokio::test]
async fn destroy_refuses_a_root_without_both_seed_markers() {
    for markers in [&[("ployz:promote", "on")][..], &[("ployz:seed", "77")][..]] {
        let test = TestDir::new();
        let (socket, server) = writer(&test, &[("w-1-1", 11)]);
        for (property, value) in markers {
            set_property(&test, ROOT, property, value);
        }
        let reply = post(&socket, "/Volume.Copy.Destroy", json!({"name": "data"})).await;
        assert_eq!(reason(&reply), "no_transition", "{markers:?}");
        assert_eq!(
            reply.pointer("/Err/details/copy/seed"),
            Some(&json!(false)),
            "{markers:?}"
        );
        assert!(effects(&commands(&test)).is_empty(), "{markers:?}");
        assert!(test.0.join("volume").exists(), "{markers:?}");
        server.abort();
    }
}

#[tokio::test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
async fn docker_sees_a_cut_root_through_its_slot_until_the_root_is_back() {
    let test = TestDir::new();
    let (socket, server) = writer(&test, &[("w-1-1", 11)]);
    post(
        &socket,
        "/Volume.Copy.Cut",
        json!({"name": "data", "target": "f-1"}),
    )
    .await;

    assert_eq!(
        post(&socket, "/VolumeDriver.Get", json!({"Name": "data"})).await,
        json!({"Volume": {"Name": "data", "Mountpoint": "", "Status": {"bound_bytes": 1073741824, "used_bytes": 0}}, "Err": ""})
    );
    let mount = post(
        &socket,
        "/VolumeDriver.Mount",
        json!({"Name": "data", "ID": "c"}),
    )
    .await;
    assert!(
        mount["Err"].as_str().unwrap().contains("Volume data"),
        "{mount}"
    );
    assert_eq!(
        post(&socket, "/VolumeDriver.Remove", json!({"Name": "data"})).await,
        json!({"Err": ""})
    );
    assert!(test.0.join("mirror-fs").exists());
    server.abort();

    let test = TestDir::new();
    let (socket, server) = slot(&test, &[("w-1-1", 11)]);
    let get = post(&socket, "/VolumeDriver.Get", json!({"Name": "data"})).await;
    assert!(
        get["Err"].as_str().unwrap().contains("does not exist"),
        "{get}"
    );
    server.abort();
}

/// One Machine: its plugin socket and its send server over the same fake ZFS.
struct Machine {
    test: TestDir,
    socket: PathBuf,
}

impl Machine {
    async fn call(&self, route: &str, request: Value) -> Value {
        post(&self.socket, route, request).await
    }

    async fn copy(&self) -> Value {
        self.call("/Volume.Copy.Inspect", json!({"name": "data"}))
            .await
    }
}

/// Two Machines, each pulling from the other's real send server.
async fn pair(a_markers: &[&str], b_markers: &[&str]) -> (Machine, Machine) {
    let a_listener = TcpListener::bind("[::1]:0").await.unwrap();
    let b_listener = TcpListener::bind("[::1]:0").await.unwrap();
    let a_port = a_listener.local_addr().unwrap().port();
    let b_port = b_listener.local_addr().unwrap().port();
    let mut machines = Vec::new();
    for (markers, listener, peer) in [
        (a_markers, a_listener, b_port),
        (b_markers, b_listener, a_port),
    ] {
        let test = TestDir::new();
        for marker in markers {
            fs::write(test.0.join(marker), "").unwrap();
        }
        let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
        let socket = test.0.join("plugin.sock");
        let unix = UnixListener::bind(&socket).unwrap();
        let mut storage = VolumeStorage::with_programs(zpool.clone(), zfs.clone());
        storage.send_port = peer;
        storage.receive_stall = Duration::from_secs(60);
        tokio::spawn(serve(unix, storage));
        tokio::spawn(ployzd::volume_send::serve(
            listener,
            zfs,
            zpool,
            CancellationToken::new(),
        ));
        machines.push(Machine { test, socket });
    }
    let b = machines.pop().unwrap();
    let a = machines.pop().unwrap();
    (a, b)
}

/// One request of the move, addressed to one of the two Machines.
#[derive(Clone, Copy, Debug)]
enum Side {
    A,
    B,
}

async fn send(a: &Machine, b: &Machine, (side, route, request): &(Side, &str, Value)) -> Value {
    let machine = match side {
        Side::A => a,
        Side::B => b,
    };
    machine.call(route, request.clone()).await
}

async fn writable_roots(a: &Machine, b: &Machine) -> usize {
    let mut count = 0;
    for machine in [a, b] {
        let copy = machine.copy().await;
        assert!(copy.get("Ok").is_some(), "{copy}");
        if copy.pointer("/Ok/kind") == Some(&json!("root"))
            && copy.pointer("/Ok/readonly") == Some(&json!(false))
        {
            count += 1;
        }
    }
    count
}

async fn pull_until_pruned(machine: &Machine, target: &str) {
    let reply = machine.call("/Volume.Copy.Pull", pull(target)).await;
    assert!(reply.get("Ok").is_some(), "{reply}");
    let copy = landed(&machine.socket, target).await;
    assert_eq!(
        copy.pointer("/Ok/newest/name"),
        Some(&json!(target)),
        "{copy}"
    );
    finished(&machine.socket, target).await;
}

#[tokio::test]
async fn a_move_between_two_machines_survives_every_request_arriving_late() {
    let (a, b) = pair(&["root", "volume", "mounted"], &["root"]).await;

    let snapshot = |name: &str| json!({"name": "data", "snapshot": name});
    assert!(
        a.call("/Volume.Copy.Snapshot", snapshot("w-1-1"))
            .await
            .get("Ok")
            .is_some()
    );
    pull_until_pruned(&b, "w-1-1").await;
    assert!(
        a.call("/Volume.Copy.Snapshot", snapshot("w-1-2"))
            .await
            .get("Ok")
            .is_some()
    );
    pull_until_pruned(&b, "w-1-2").await;
    assert_eq!(snapshot_names(&a.test, ROOT), ["w-1-2", "w-1-1"]);
    let cut = a
        .call("/Volume.Copy.Cut", json!({"name": "data", "target": "f-1"}))
        .await;
    assert_eq!(cut.pointer("/Ok/kind"), Some(&json!("slot")), "{cut}");
    pull_until_pruned(&b, "f-1").await;
    let at = guid(&b.copy().await).to_owned();
    let writer = b
        .call(
            "/Volume.Copy.Writer",
            json!({"name": "data", "at": at, "epoch": 1}),
        )
        .await;
    assert_eq!(
        writer.pointer("/Ok/readonly"),
        Some(&json!(false)),
        "{writer}"
    );
    assert_eq!(writer.pointer("/Ok/epoch"), Some(&json!(1)));
    assert_eq!(
        a.call("/Volume.Copy.Destroy", json!({"name": "data"}))
            .await,
        json!({"Ok": null})
    );
    assert_eq!(writable_roots(&a, &b).await, 1);
    assert!(
        commands(&b.test).contains(
            "zfs receive -u -s -o readonly=on -o refquota=1073741824 tank/ployz-mirror/data/fs"
        ),
        "{}",
        commands(&b.test)
    );

    let requests = [
        (Side::A, "/Volume.Copy.Snapshot", snapshot("w-1-1")),
        (Side::B, "/Volume.Copy.Pull", pull("w-1-1")),
        (Side::A, "/Volume.Copy.Snapshot", snapshot("w-1-2")),
        (Side::B, "/Volume.Copy.Pull", pull("w-1-2")),
        (
            Side::A,
            "/Volume.Copy.Cut",
            json!({"name": "data", "target": "f-1"}),
        ),
        (Side::B, "/Volume.Copy.Pull", pull("f-1")),
        (
            Side::B,
            "/Volume.Copy.Writer",
            json!({"name": "data", "at": at, "epoch": 1}),
        ),
        (Side::A, "/Volume.Copy.Destroy", json!({"name": "data"})),
    ];
    let end = (state(&a.test), state(&b.test));
    for request in requests.iter().chain(requests.iter().rev()) {
        let reply = send(&a, &b, request).await;
        assert!(
            reply.get("Ok").is_some() || reply.pointer("/Err/details/reason").is_some(),
            "{} {:?}: {reply}",
            request.1,
            request.2
        );
        assert_eq!(
            writable_roots(&a, &b).await,
            1,
            "after {} {:?}",
            request.1,
            request.2
        );
        assert_eq!(
            (state(&a.test), state(&b.test)),
            end,
            "after {} {:?}",
            request.1,
            request.2
        );
    }
}

#[tokio::test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
async fn an_undone_cut_restores_the_writer_and_refuses_to_cut_at_a_stale_target() {
    let (a, b) = pair(&["root", "volume", "mounted"], &["root"]).await;

    assert!(
        a.call(
            "/Volume.Copy.Snapshot",
            json!({"name": "data", "snapshot": "w-1-1"})
        )
        .await
        .get("Ok")
        .is_some()
    );
    pull_until_pruned(&b, "w-1-1").await;
    let cut = a
        .call("/Volume.Copy.Cut", json!({"name": "data", "target": "f-1"}))
        .await;
    let at = guid(&cut).to_owned();
    let forget = json!({"name": "data"});
    assert_eq!(
        b.call("/Volume.Copy.Destroy", forget.clone()).await,
        json!({"Ok": null})
    );
    let undo = json!({"name": "data", "at": at, "epoch": 1});
    let restored = a.call("/Volume.Copy.Writer", undo.clone()).await;
    assert_eq!(
        restored.pointer("/Ok/kind"),
        Some(&json!("root")),
        "{restored}"
    );
    assert_eq!(restored.pointer("/Ok/readonly"), Some(&json!(false)));
    assert_eq!(restored.pointer("/Ok/epoch"), Some(&json!(1)));
    assert!(property(&a.test, ROOT, "ployz:promote").is_none());
    assert_eq!(
        a.call("/VolumeDriver.Get", json!({"Name": "data"})).await["Err"],
        json!("")
    );
    assert_eq!(writable_roots(&a, &b).await, 1);

    let end = (state(&a.test), state(&b.test));
    let requests = [
        (
            Side::A,
            "/Volume.Copy.Cut",
            json!({"name": "data", "target": "f-1"}),
        ),
        (Side::B, "/Volume.Copy.Destroy", forget),
        (Side::A, "/Volume.Copy.Writer", undo),
    ];
    for request in requests.iter().chain(requests.iter().rev()) {
        let reply = send(&a, &b, request).await;
        if request.1 == "/Volume.Copy.Cut" {
            assert_eq!(reason(&reply), "written", "{reply}");
        } else {
            assert!(reply.get("Ok").is_some(), "{reply}");
        }
        assert_eq!(writable_roots(&a, &b).await, 1);
        assert_eq!(
            (state(&a.test), state(&b.test)),
            end,
            "after {} {:?}",
            request.1,
            request.2
        );
    }
}

/// What the daemon does for a `SetVolumeCopy` Writer want against a slot: plan against
/// the inspected copy, and call the Writer route only on `MirrorToWriter`.
async fn set_writer(machine: &Machine, expect: ployz_core::ExpectedCopy, at: &str) -> Value {
    let copy = parsed_copy(&machine.copy().await);
    let want = ployz_core::WantedCopy::Writer {
        at: ployz_core::SnapshotGuid::new(at.parse().unwrap()),
        epoch: ployz_core::WriterEpoch::new(1),
        start: None,
    };
    let plan = ployz_core::plan(copy.as_ref(), false, &expect, &want);
    if plan != ployz_core::CopyPlan::Transition(ployz_core::CopyTransition::MirrorToWriter) {
        return json!({"Planned": format!("{plan:?}")});
    }
    machine
        .call(
            "/Volume.Copy.Writer",
            json!({"name": "data", "at": at, "epoch": 1}),
        )
        .await
}

fn parsed_copy(reply: &Value) -> Option<ployz_core::VolumeCopy> {
    serde_json::from_value(reply.get("Ok").cloned().unwrap()).unwrap()
}

fn mirror_expect(copy: &Value) -> ployz_core::ExpectedCopy {
    let Some(ployz_core::VolumeCopy::Slot {
        slot,
        newest: Some(newest),
        ..
    }) = parsed_copy(copy)
    else {
        panic!("not a slot with a snapshot: {copy}");
    };
    ployz_core::ExpectedCopy::Mirror {
        newest: newest.guid,
        slot,
    }
}

#[tokio::test]
#[expect(clippy::indexing_slicing, reason = "JSON fixture assertions")]
async fn a_writer_planned_against_a_forgotten_slot_refuses_the_slot_pulled_again() {
    let (a, b) = pair(&["root", "volume", "mounted"], &["root"]).await;
    assert!(
        a.call(
            "/Volume.Copy.Snapshot",
            json!({"name": "data", "snapshot": "w-1-1"})
        )
        .await
        .get("Ok")
        .is_some()
    );
    pull_until_pruned(&b, "w-1-1").await;
    let first = b.copy().await;
    assert_eq!(
        b.call("/Volume.Copy.Destroy", json!({"name": "data"}))
            .await,
        json!({"Ok": null})
    );
    pull_until_pruned(&b, "w-1-1").await;
    let again = b.copy().await;
    let at = guid(&again).to_owned();
    assert_eq!(guid(&first), at);
    assert_ne!(first["Ok"]["slot"], again["Ok"]["slot"]);

    let stale = set_writer(&b, mirror_expect(&first), &at).await;
    assert_eq!(stale, json!({"Planned": "Refuse(Mismatch)"}));
    assert_eq!(b.copy().await, again);
    assert_eq!(writable_roots(&a, &b).await, 1);

    let promoted = set_writer(&b, mirror_expect(&again), &at).await;
    assert_eq!(
        promoted.pointer("/Ok/kind"),
        Some(&json!("root")),
        "{promoted}"
    );
    assert_eq!(promoted.pointer("/Ok/readonly"), Some(&json!(false)));
}

/// What the daemon does for a `SetVolumeCopy` Mirror want: plan against the inspected
/// copy, call the Pull route on a pull transition, and answer the copy when at the want.
async fn set_mirror(machine: &Machine, expect: &ployz_core::ExpectedCopy, target: &str) -> Value {
    let copy = parsed_copy(&machine.copy().await);
    let want = ployz_core::WantedCopy::Mirror {
        from: Some(ployz_core::ManagementAddress("::1".parse().unwrap())),
        target: target.parse().unwrap(),
        drain: ployz_core::Drain::NONE,
    };
    match ployz_core::plan(copy.as_ref(), false, expect, &want) {
        ployz_core::CopyPlan::AtWant => json!({"AtWant": copy}),
        ployz_core::CopyPlan::Transition(
            ployz_core::CopyTransition::MirrorToMirror | ployz_core::CopyTransition::NoneToMirror,
        ) => machine.call("/Volume.Copy.Pull", pull(target)).await,
        plan @ (ployz_core::CopyPlan::Refuse(_) | ployz_core::CopyPlan::Transition(_)) => {
            json!({"Planned": format!("{plan:?}")})
        }
    }
}

#[tokio::test]
async fn a_pull_resent_after_it_landed_finishes_it_whatever_the_expect_says() {
    let (a, b) = pair(&["root", "volume", "mounted"], &["root"]).await;
    let snapshot = |name: &str| json!({"name": "data", "snapshot": name});
    assert!(
        a.call("/Volume.Copy.Snapshot", snapshot("w-1-1"))
            .await
            .get("Ok")
            .is_some()
    );
    pull_until_pruned(&b, "w-1-1").await;
    assert!(
        a.call("/Volume.Copy.Snapshot", snapshot("w-1-2"))
            .await
            .get("Ok")
            .is_some()
    );
    let expect = mirror_expect(&b.copy().await);

    let started = set_mirror(&b, &expect, "w-1-2").await;
    assert_eq!(
        started.pointer("/Ok/pull/target"),
        Some(&json!("w-1-2")),
        "{started}"
    );
    landed(&b.socket, "w-1-2").await;
    let mut finished = set_mirror(&b, &expect, "w-1-2").await;
    // Inspect shows the target before the receive task records its outcome.
    for _ in 0..300 {
        if finished.pointer("/Ok/pull") == Some(&Value::Null) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        finished = set_mirror(&b, &expect, "w-1-2").await;
    }
    assert_eq!(
        finished.pointer("/Ok/pull"),
        Some(&Value::Null),
        "{finished}"
    );
    assert_eq!(finished.pointer("/Ok/newest/name"), Some(&json!("w-1-2")));
    assert_eq!(snapshot_names(&b.test, SLOT_FS), ["w-1-2"]);
    assert!(property(&b.test, SLOT, "ployz:pull").is_none());

    let again = set_mirror(&b, &expect, "w-1-2").await;
    assert_eq!(again.pointer("/AtWant/pull"), Some(&Value::Null), "{again}");
    assert_eq!(again.pointer("/AtWant/newest/name"), Some(&json!("w-1-2")));
}

/// The Container half the daemon runs beside the plugin on one Machine: the Containers
/// Docker has, by id or creation key, and the ones running.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Docker {
    known: Vec<&'static str>,
    running: Vec<&'static str>,
}

/// A Writer's start as Cloud sends it.
#[derive(Clone, Copy, Debug)]
enum Run {
    Kept(&'static str),
    Create(&'static str),
}

/// One `SetVolumeCopy` as Cloud sends it to one Machine.
#[derive(Clone, Debug)]
struct Set {
    side: Side,
    expect: ployz_core::ExpectedCopy,
    want: ployz_core::WantedCopy,
    run: Option<Run>,
}

/// What `set_copy_admitted` does for `set`: the start's state from Docker, the plan, then
/// the plugin route and the Container half of the transition. A Writer start counts as
/// running when any holder runs, and a Kept Container Docker lost refuses NoTransition.
async fn set_copy(machine: &Machine, docker: &mut Docker, set: &Set) -> Value {
    use ployz_core::{CopyPlan, CopyTransition, WantedCopy};
    let copy = parsed_copy(&machine.copy().await);
    let (start_running, gone) = match set.run {
        Some(Run::Kept(id)) => (!docker.running.is_empty(), !docker.known.contains(&id)),
        Some(Run::Create(_)) => (!docker.running.is_empty(), false),
        None => (false, false),
    };
    let transition = match ployz_core::plan(copy.as_ref(), start_running, &set.expect, &set.want) {
        CopyPlan::AtWant => return json!({"Ok": copy}),
        CopyPlan::Refuse(reason) => return json!({"Refused": format!("{reason:?}")}),
        CopyPlan::Transition(_) if gone => return json!({"Refused": "NoTransition"}),
        CopyPlan::Transition(transition) => transition,
    };
    match (transition, &set.want) {
        (
            CopyTransition::WriterToWriter | CopyTransition::MirrorToWriter,
            WantedCopy::Writer { at, epoch, .. },
        ) => {
            let promote = match &copy {
                Some(ployz_core::VolumeCopy::Root {
                    readonly,
                    epoch: held,
                    ..
                }) => *readonly || held != epoch,
                _ => true,
            };
            let reply = if promote {
                machine
                    .call(
                        "/Volume.Copy.Writer",
                        json!({"name": "data", "at": at, "epoch": epoch}),
                    )
                    .await
            } else {
                json!({"Ok": copy})
            };
            if reply.get("Ok").is_some()
                && let Some(Run::Kept(id) | Run::Create(id)) = set.run
            {
                if !docker.known.contains(&id) {
                    docker.known.push(id);
                }
                if !docker.running.contains(&id) {
                    docker.running.push(id);
                }
            }
            reply
        }
        (CopyTransition::WriterToMirror, WantedCopy::Mirror { target, .. }) => {
            docker.running.clear();
            machine
                .call(
                    "/Volume.Copy.Cut",
                    json!({"name": "data", "target": target}),
                )
                .await
        }
        (CopyTransition::WriterToSnapshot, WantedCopy::Snapshot { name }) => {
            machine
                .call(
                    "/Volume.Copy.Snapshot",
                    json!({"name": "data", "snapshot": name}),
                )
                .await
        }
        (CopyTransition::WriterToForget, WantedCopy::Forget) => {
            machine
                .call("/Volume.Copy.Forget", json!({"name": "data"}))
                .await
        }
        (
            CopyTransition::MirrorToMirror | CopyTransition::NoneToMirror,
            WantedCopy::Mirror { target, .. },
        ) => {
            machine
                .call("/Volume.Copy.Pull", pull(&target.to_string()))
                .await
        }
        (CopyTransition::MirrorToForget | CopyTransition::SeedToForget, WantedCopy::Forget) => {
            machine
                .call("/Volume.Copy.Destroy", json!({"name": "data"}))
                .await
        }
        (transition, want) => panic!("the planner paired {transition:?} with {want:?}"),
    }
}

/// Cloud's side of one move: two Machines, the Containers on each, and every
/// `SetVolumeCopy` sent so far, to replay late.
struct Cloud {
    a: Machine,
    b: Machine,
    docker: [Docker; 2],
    sent: Vec<Set>,
}

impl Cloud {
    async fn start() -> Self {
        let (a, b) = pair(&["root", "volume", "mounted"], &["root"]).await;
        let docker = [
            Docker {
                known: vec!["a1"],
                running: vec!["a1"],
            },
            Docker::default(),
        ];
        Self {
            a,
            b,
            docker,
            sent: Vec::new(),
        }
    }

    fn machine(&self, side: Side) -> &Machine {
        match side {
            Side::A => &self.a,
            Side::B => &self.b,
        }
    }

    async fn replay(&mut self, set: &Set) -> Value {
        let [a, b] = &mut self.docker;
        let (machine, docker) = match set.side {
            Side::A => (&self.a, a),
            Side::B => (&self.b, b),
        };
        set_copy(machine, docker, set).await
    }

    async fn send(&mut self, set: Set) -> Value {
        let reply = self.replay(&set).await;
        assert!(reply.get("Ok").is_some(), "{set:?}: {reply}");
        self.sent.push(set);
        reply
    }

    /// Resend one Mirror want, expect unchanged, until its pull landed and finished.
    async fn mirror(&mut self, side: Side, expect: ployz_core::ExpectedCopy, target: &str) {
        let set = Set {
            side,
            expect,
            want: mirror_want(Some(target)),
            run: None,
        };
        for _ in 0..300 {
            let reply = self.replay(&set).await;
            assert!(reply.get("Ok").is_some(), "{set:?}: {reply}");
            if reply.pointer("/Ok/pull") == Some(&Value::Null)
                && reply.pointer("/Ok/newest/name") == Some(&json!(target))
            {
                self.sent.push(set);
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the pull of {target} never finished");
    }

    fn running(&self, side: Side) -> &[&'static str] {
        let [a, b] = &self.docker;
        match side {
            Side::A => &a.running,
            Side::B => &b.running,
        }
    }

    async fn copy(&self, side: Side) -> Value {
        self.machine(side).copy().await
    }

    /// At most one writable root, at most one running holder, and a holder only on it.
    async fn assert_one_writer(&self, after: &Set) {
        let mut writers = 0;
        let mut holders = 0;
        let [a, b] = &self.docker;
        for (side, docker) in [(Side::A, a), (Side::B, b)] {
            let copy = self.copy(side).await;
            let writable = copy.pointer("/Ok/kind") == Some(&json!("root"))
                && copy.pointer("/Ok/readonly") == Some(&json!(false));
            writers += usize::from(writable);
            holders += docker.running.len();
            assert!(
                docker.running.is_empty() || writable,
                "a holder runs on {side:?} without a writable root after {after:?}: {copy}"
            );
        }
        assert!(writers <= 1, "{writers} writable roots after {after:?}");
        assert!(holders <= 1, "{holders} running holders after {after:?}");
    }

    /// Every request sent so far, again, in order and then in reverse.
    async fn replay_late(&mut self) {
        let sent = self.sent.clone();
        for set in sent.iter().chain(sent.iter().rev()) {
            let reply = self.replay(set).await;
            assert!(
                reply.get("Ok").is_some() || reply.get("Refused").is_some(),
                "{set:?}: {reply}"
            );
            self.assert_one_writer(set).await;
        }
    }
}

fn writer_expect(epoch: u32) -> ployz_core::ExpectedCopy {
    ployz_core::ExpectedCopy::Writer {
        epoch: ployz_core::WriterEpoch::new(epoch),
    }
}

fn mirror_want(from: Option<&str>) -> ployz_core::WantedCopy {
    ployz_core::WantedCopy::Mirror {
        from: from.map(|_| ployz_core::ManagementAddress("::1".parse().unwrap())),
        target: from.unwrap_or("f-1").parse().unwrap(),
        drain: ployz_core::Drain::NONE,
    }
}

fn snapshot_want(name: &str) -> ployz_core::WantedCopy {
    ployz_core::WantedCopy::Snapshot {
        name: name.parse().unwrap(),
    }
}

fn writer_want(at: &str, epoch: u32) -> ployz_core::WantedCopy {
    ployz_core::WantedCopy::Writer {
        at: ployz_core::SnapshotGuid::new(at.parse().unwrap()),
        epoch: ployz_core::WriterEpoch::new(epoch),
        start: Some(ployz_core::Start::Kept {
            container_id: ployz_core::ContainerId::parse("a".repeat(64)).unwrap(),
        }),
    }
}

/// Two rounds: A takes a run snapshot, B pulls it.
async fn rounds(cloud: &mut Cloud) {
    for (round, expect_b) in [("w-1-1", None), ("w-1-2", Some(()))] {
        cloud
            .send(Set {
                side: Side::A,
                expect: writer_expect(0),
                want: snapshot_want(round),
                run: None,
            })
            .await;
        let expect = match expect_b {
            None => ployz_core::ExpectedCopy::None,
            Some(()) => mirror_expect(&cloud.copy(Side::B).await),
        };
        cloud.mirror(Side::B, expect, round).await;
    }
}

/// The cut on A at f-1, then B's pull of it.
async fn cut(cloud: &mut Cloud) {
    cloud
        .send(Set {
            side: Side::A,
            expect: writer_expect(0),
            want: mirror_want(None),
            run: None,
        })
        .await;
    let expect = mirror_expect(&cloud.copy(Side::B).await);
    cloud.mirror(Side::B, expect, "f-1").await;
}

#[tokio::test]
async fn every_request_of_a_move_replayed_late_through_the_planner_keeps_one_writer() {
    // The move: rounds, the cut, B's pull, step 5 on B, step 6 on A.
    let mut cloud = Cloud::start().await;
    rounds(&mut cloud).await;
    cut(&mut cloud).await;
    let b = cloud.copy(Side::B).await;
    cloud
        .send(Set {
            side: Side::B,
            expect: mirror_expect(&b),
            want: writer_want(guid(&b), 1),
            run: Some(Run::Create("k1")),
        })
        .await;
    let a = cloud.copy(Side::A).await;
    cloud
        .send(Set {
            side: Side::A,
            expect: mirror_expect(&a),
            want: ployz_core::WantedCopy::Forget,
            run: None,
        })
        .await;
    assert_eq!(cloud.copy(Side::A).await, json!({"Ok": null}));
    let end = (
        state(&cloud.a.test),
        state(&cloud.b.test),
        cloud.docker.clone(),
    );
    assert_eq!(cloud.running(Side::B), ["k1"]);
    cloud.replay_late().await;
    assert_eq!(
        (
            state(&cloud.a.test),
            state(&cloud.b.test),
            cloud.docker.clone()
        ),
        end
    );

    // Undo before the cut: A raises its epoch over its own snapshot, B forgets its slot.
    let mut cloud = Cloud::start().await;
    rounds(&mut cloud).await;
    let a = cloud.copy(Side::A).await;
    let b = cloud.copy(Side::B).await;
    cloud
        .send(Set {
            side: Side::A,
            expect: writer_expect(0),
            want: writer_want(guid(&a), 1),
            run: Some(Run::Kept("a1")),
        })
        .await;
    cloud
        .send(Set {
            side: Side::B,
            expect: mirror_expect(&b),
            want: ployz_core::WantedCopy::Forget,
            run: None,
        })
        .await;
    cloud.sent.push(Set {
        side: Side::A,
        expect: writer_expect(0),
        want: mirror_want(None),
        run: None,
    });
    cloud.replay_late().await;
    assert_eq!(cloud.running(Side::A), ["a1"]);

    // Undo after the cut: B forgets its slot first, then A takes the writer back at
    // the cut's snapshot one epoch up. Step 5, planned before the undo, arrives late.
    let mut cloud = Cloud::start().await;
    rounds(&mut cloud).await;
    cut(&mut cloud).await;
    let a = cloud.copy(Side::A).await;
    let b = cloud.copy(Side::B).await;
    cloud
        .send(Set {
            side: Side::B,
            expect: mirror_expect(&b),
            want: ployz_core::WantedCopy::Forget,
            run: None,
        })
        .await;
    cloud
        .send(Set {
            side: Side::A,
            expect: mirror_expect(&a),
            want: writer_want(guid(&a), 1),
            run: Some(Run::Kept("a1")),
        })
        .await;
    assert_eq!(cloud.running(Side::A), ["a1"]);
    cloud.sent.push(Set {
        side: Side::B,
        expect: mirror_expect(&b),
        want: writer_want(guid(&b), 1),
        run: Some(Run::Create("k1")),
    });
    cloud.replay_late().await;
    assert_eq!(cloud.running(Side::A), ["a1"]);
    assert!(cloud.running(Side::B).is_empty());
}

#[tokio::test]
async fn a_snapshot_resent_at_its_want_prunes_past_the_newest_two() {
    let (a, _b) = pair(&["root", "volume", "mounted"], &["root"]).await;
    set_snapshots(&a.test, ROOT, &[("w-1-3", 3), ("w-1-2", 2), ("w-1-1", 1)]);
    let set = Set {
        side: Side::A,
        expect: writer_expect(0),
        want: snapshot_want("w-1-3"),
        run: None,
    };

    let reply = set_copy(&a, &mut Docker::default(), &set).await;

    assert_eq!(
        reply.pointer("/Ok/newest/name"),
        Some(&json!("w-1-3")),
        "{reply}"
    );
    assert_eq!(snapshot_names(&a.test, ROOT), ["w-1-3", "w-1-2"]);
}

/// A head writer path the crash table stops after each of its effects.
#[derive(Clone, Copy, Debug)]
enum Writing {
    Cut,
    Restore,
    PromoteAPulledSlot,
    Seed,
    FirstPull,
    IncrementalPull,
    Resend,
    SameNewestPull,
}

impl Writing {
    /// The Volume the path writes, with its route, request and the reply that ends it.
    fn drive(self) -> (&'static str, &'static str, Value) {
        match self {
            Self::Cut => (
                "data",
                "/Volume.Copy.Cut",
                json!({"name": "data", "target": "f-1"}),
            ),
            Self::Restore => (
                "data",
                "/Volume.Copy.Writer",
                json!({"name": "data", "at": "13", "epoch": 0}),
            ),
            Self::PromoteAPulledSlot => (
                "data",
                "/Volume.Copy.Writer",
                json!({"name": "data", "at": "13", "epoch": 2}),
            ),
            Self::Seed => (
                "seed",
                "/Volume.Seed",
                json!({"name": "data", "into": "seed", "guid": "77"}),
            ),
            Self::FirstPull | Self::IncrementalPull | Self::Resend => {
                ("data", "/Volume.Copy.Pull", pull("w-1-2"))
            }
            Self::SameNewestPull => ("data", "/Volume.Copy.Pull", pull("w-1-1")),
        }
    }

    fn ended(self, reply: &Value) -> bool {
        match self {
            Self::Cut => reply.pointer("/Ok/kind") == Some(&json!("slot")),
            Self::Restore | Self::PromoteAPulledSlot => {
                reply.pointer("/Ok/kind") == Some(&json!("root"))
            }
            Self::Seed => reply.pointer("/Ok/copy/kind") == Some(&json!("root")),
            Self::FirstPull | Self::IncrementalPull | Self::Resend | Self::SameNewestPull => {
                reply.pointer("/Ok/pull") == Some(&Value::Null)
            }
        }
    }

    fn fixture(self, test: &TestDir, port: u16) -> (PathBuf, Server) {
        match self {
            Self::Cut => {
                let started = writer(test, &[("w-1-1", 11)]);
                set_property(test, ROOT, "written@f-1", "0");
                started
            }
            Self::Restore => {
                let started = slot(test, &[("f-1", 13)]);
                set_property(test, SLOT_FS, "ployz:cut", "on");
                started
            }
            Self::PromoteAPulledSlot => {
                let started = slot(test, &[("f-1", 13)]);
                set_property(test, SLOT_FS, "ployz:pulled", "13");
                started
            }
            Self::Seed => {
                let started = start(test, USABLE_POOL, &["root", "volume"]);
                set_property(
                    test,
                    ROOT,
                    "snapshots",
                    "tank/ployz/data@w-1-1\t77\t1700000000",
                );
                started
            }
            Self::FirstPull => puller(test, &["root"], port),
            Self::IncrementalPull => {
                let started = puller(test, &["root", "mirror", "mirror-fs"], port);
                set_snapshots(test, SLOT_FS, &[("w-1-1", 11)]);
                for (mark, value) in [
                    ("ployz:cut", "on"),
                    ("ployz:epoch", "3"),
                    ("ployz:promote", "on"),
                    ("ployz:seed", "11"),
                ] {
                    set_property(test, SLOT_FS, mark, value);
                }
                started
            }
            Self::Resend => {
                let started = puller(test, &["root", "mirror", "mirror-fs"], port);
                landed_before_a_restart(test, "ployz:cut", "on");
                started
            }
            Self::SameNewestPull => {
                let started = puller(test, &["root", "mirror", "mirror-fs"], port);
                set_snapshots(test, SLOT_FS, &[("w-1-1", 11)]);
                set_property(test, SLOT_FS, "ployz:cut", "on");
                started
            }
        }
    }

    /// Whether the finished path leaves a slot a writer elsewhere holds the point of.
    fn received(self) -> bool {
        matches!(self, Self::FirstPull | Self::IncrementalPull)
    }

    /// Whether the fixture is such a slot before the path changes anything.
    fn starts_received(self) -> bool {
        matches!(self, Self::PromoteAPulledSlot)
    }
}

/// Drives `writing` until it ends or, once `stop` has failed after its given number of
/// earlier runs, until its request stops changing anything: the failed effect stands for
/// the daemon stopping there.
async fn drive(writing: Writing, test: &TestDir, socket: &Path, stop: Option<(&str, usize)>) {
    let (_, route, request) = writing.drive();
    let fail = test.0.join("props/fail-command");
    if let Some((stop, earlier)) = stop {
        fs::create_dir_all(test.0.join("props")).unwrap();
        fs::write(test.0.join("props/fail-skip"), earlier.to_string()).unwrap();
        fs::write(&fail, stop.strip_prefix("zfs ").unwrap()).unwrap();
    }
    for _ in 0..300 {
        if stop.is_some() && !fail.exists() {
            return;
        }
        let reply = post(socket, route, request.clone()).await;
        if reply.get("Err").is_some() || writing.ended(&reply) {
            assert!(stop.is_some() || reply.get("Ok").is_some(), "{reply}");
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{writing:?} never ended");
}

/// The departure's verdict on the copy `name` held: whether it destroyed a slot that held
/// a snapshot.
async fn departure_destroys_a_point(test: &TestDir, socket: &Path, name: &str) -> bool {
    let fs = format!("tank/ployz-mirror/{name}/fs");
    let held = test
        .0
        .join(format!(
            "{}-fs",
            if name == "data" { "mirror" } else { "seed" }
        ))
        .exists()
        && !snapshot_names(test, &fs).is_empty();
    let before = commands(test);
    let reply = post(socket, "/Storage.Demote", json!(null)).await;
    assert!(reply.get("Ok").is_some(), "{reply}");
    let destroy = format!("zfs destroy -r tank/ployz-mirror/{name}\n");
    held && commands(test)[before.len()..].contains(&destroy)
}

/// Stops each head writer path at each effect it has, and asks the departure. Only a
/// mirror pull that has marked the snapshot it received leaves a slot the departure
/// destroys; every earlier stop, and every other path once it changed anything, leaves a
/// slot it keeps.
#[tokio::test]
async fn every_stop_of_a_writer_path_leaves_a_slot_the_departure_keeps_until_a_pull_marked_it() {
    for writing in [
        Writing::Cut,
        Writing::Restore,
        Writing::PromoteAPulledSlot,
        Writing::Seed,
        Writing::FirstPull,
        Writing::IncrementalPull,
        Writing::Resend,
        Writing::SameNewestPull,
    ] {
        let (name, _, _) = writing.drive();
        let source = Source::start(Some(BOUND), "snapshot w-1-2 12\n").await;
        let test = TestDir::new();
        let (socket, server) = writing.fixture(&test, source.port);
        drive(writing, &test, &socket, None).await;
        let steps = effects(&commands(&test));
        assert!(!steps.is_empty(), "{writing:?}");
        let marked = steps
            .iter()
            .position(|step| step.starts_with("zfs set ployz:pulled="));
        assert_eq!(
            marked.is_some(),
            writing.received(),
            "{writing:?}: {steps:?}"
        );
        if let Some(marked) = marked {
            let received = steps
                .iter()
                .position(|step| step.starts_with("zfs receive"))
                .unwrap();
            assert!(received < marked, "{writing:?}: {steps:?}");
            for mark in ["ployz:cut", "ployz:epoch", "ployz:promote", "ployz:seed"] {
                let inherit = format!("zfs inherit {mark} {SLOT_FS}");
                let dropped = steps.iter().position(|step| *step == inherit);
                assert!(
                    dropped.is_some_and(|dropped| received < dropped && dropped < marked),
                    "{writing:?}: {mark} is not dropped between the receive and the mark: {steps:?}"
                );
            }
        }
        assert_eq!(
            departure_destroys_a_point(&test, &socket, name).await,
            writing.received(),
            "{writing:?} finished"
        );
        server.abort();

        for (index, step) in steps.iter().enumerate() {
            let source = Source::start(Some(BOUND), "snapshot w-1-2 12\n").await;
            let test = TestDir::new();
            let (socket, server) = writing.fixture(&test, source.port);
            let earlier = steps[..index].iter().filter(|done| *done == step).count();
            drive(writing, &test, &socket, Some((step, earlier))).await;
            assert!(
                !test.0.join("props/fail-command").exists(),
                "{writing:?}: {step} never ran"
            );
            let received = if index == 0 {
                writing.starts_received()
            } else {
                marked.is_some_and(|marked| index > marked)
            };
            assert_eq!(
                departure_destroys_a_point(&test, &socket, name).await,
                received,
                "{writing:?} stopped at {step}"
            );
            server.abort();
        }
    }
}
