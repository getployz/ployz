//! The SetVolumeCopy routes over fake ZFS: each cell's effect, its resend, its refusals,
//! convergence after a failure at every sub-step, and two Machines moving one Volume
//! through the real send server with every request replayed late.

use std::{
    sync::{Arc, Mutex as StdMutex},
    time::Duration,
};

use axum::{Router, extract::Request, http::StatusCode, response::IntoResponse};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

use super::lease_tests::{set_property, start};
use super::mirror_tests::{commands, property, snapshot_names};
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
    assert!(property(&test, SLOT, "ployz:mirror").is_none());
    assert_eq!(snapshot_names(&test, SLOT_FS), ["f-1", "w-1-2", "w-1-1"]);

    let before = commands(&test);
    assert_eq!(post(&socket, "/Volume.Copy.Cut", request).await, reply);
    assert!(effects(&commands(&test)[before.len()..]).is_empty());
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
    assert!(!commands(&test).contains("ployz:promote"));
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
/// the first answer whose key the request URI contains, else the default one.
struct Source {
    requests: Arc<StdMutex<Vec<String>>>,
    answers: Arc<StdMutex<Vec<(String, StatusCode, String)>>>,
    port: u16,
}

impl Source {
    async fn start(bound: Option<u64>, stream: &str) -> Self {
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let answers = Arc::new(StdMutex::new(vec![(
            String::new(),
            StatusCode::OK,
            stream.to_owned(),
        )]));
        let state = (Arc::clone(&requests), Arc::clone(&answers));
        let router = Router::new().fallback(move |request: Request| {
            let (requests, answers) = state.clone();
            async move {
                let uri = request.uri().to_string();
                requests.lock().unwrap().push(uri.clone());
                if uri.starts_with("/volume-bound/") {
                    return match bound {
                        Some(bound) => (StatusCode::OK, bound.to_string()).into_response(),
                        None => (StatusCode::NOT_FOUND, "no writer here").into_response(),
                    };
                }
                let answers = answers.lock().unwrap();
                let (_, status, body) = answers
                    .iter()
                    .rev()
                    .find(|(key, ..)| uri.contains(key.as_str()))
                    .unwrap();
                (*status, body.clone()).into_response()
            }
        });
        let listener = TcpListener::bind("[::1]:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(axum::serve(listener, router).into_future());
        Self {
            requests,
            answers,
            port,
        }
    }

    /// Answer requests whose URI contains `key` with `status` and `stream` from now on.
    fn answer(&self, key: &str, status: StatusCode, stream: &str) {
        self.answers
            .lock()
            .unwrap()
            .push((key.to_owned(), status, stream.to_owned()));
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
}

fn puller(test: &TestDir, markers: &[&str], port: u16) -> (PathBuf, Server) {
    for marker in markers {
        fs::write(test.0.join(marker), "").unwrap();
    }
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let socket = test.0.join("plugin.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut storage = VolumeStorage::with_programs(zpool, zfs);
    storage.send_port = port;
    storage.receive_stall = Duration::from_secs(60);
    (socket, tokio::spawn(serve(listener, storage)))
}

fn pull(target: &str) -> Value {
    json!({"name": "data", "from": "::1", "target": target})
}

/// Polls the copy until the pull landed `target` or stopped with a failure.
async fn landed(socket: &Path, target: &str) -> Value {
    for _ in 0..300 {
        let copy = post(socket, "/Volume.Copy.Inspect", json!({"name": "data"})).await;
        if copy.pointer("/Ok/newest/name") == Some(&json!(target))
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

    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
    assert_eq!(reply.pointer("/Ok/pull"), Some(&Value::Null));
    assert_eq!(reply.pointer("/Ok/newest/name"), Some(&json!("w-1-1")));
    assert!(property(&test, SLOT, "ployz:pull").is_none());
    assert_eq!(
        source.requests(),
        ["/volume-bound/data", "/volume-send/data?target=w-1-1"]
    );
    server.abort();
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
    post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    let copy = landed(&socket, "w-1-2").await;
    assert_eq!(copy.pointer("/Ok/newest/guid"), Some(&json!("12")));
    let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-2")).await;
    assert_eq!(reply.pointer("/Ok/pull"), Some(&Value::Null));
    assert_eq!(snapshot_names(&test, SLOT_FS), ["w-1-2"]);
    assert_eq!(
        source.requests(),
        [
            "/volume-send/data?target=w-1-2&base=11",
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
            "/volume-send/data?target=w-1-2&base=11",
            "/volume-send/data?target=w-1-2",
        ]
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
        let reply = post(&socket, "/Volume.Copy.Pull", pull("w-1-1")).await;
        assert_eq!(
            reply.pointer("/Ok/pull"),
            Some(&Value::Null),
            "{step}: {reply}"
        );
        assert_eq!(
            property(&test, SLOT, "refquota").as_deref(),
            Some("1073741824")
        );
        server.abort();
    }
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
#[derive(Clone, Copy)]
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
    let reply = machine.call("/Volume.Copy.Pull", pull(target)).await;
    assert_eq!(reply.pointer("/Ok/pull"), Some(&Value::Null), "{reply}");
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
    let undo = json!({"name": "data", "at": at, "epoch": 0});
    let restored = a.call("/Volume.Copy.Writer", undo.clone()).await;
    assert_eq!(
        restored.pointer("/Ok/kind"),
        Some(&json!("root")),
        "{restored}"
    );
    assert_eq!(restored.pointer("/Ok/readonly"), Some(&json!(false)));
    assert_eq!(restored.pointer("/Ok/epoch"), Some(&json!(0)));
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
        (Side::A, "/Volume.Copy.Writer", undo),
        (Side::B, "/Volume.Copy.Pull", pull("w-1-1")),
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
