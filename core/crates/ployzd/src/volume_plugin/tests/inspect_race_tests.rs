//! Volume.Copy.Inspect while a pull's receive task finishes between Inspect's reads.

use std::{
    os::unix::fs::PermissionsExt as _,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{body::Body, extract::Request, response::IntoResponse};
use tokio::{net::TcpListener, process::Command, sync::Semaphore};

use super::super::transfer::{Receives, ZfsChild};
use super::*;

const BOUND: u64 = 1_073_741_824;

/// A writer's send port that answers the bound at once and sends `stream` only once
/// the returned gate has a permit.
async fn held_source(stream: &'static str) -> (u16, Arc<Semaphore>) {
    let gate = Arc::new(Semaphore::new(0));
    let held = Arc::clone(&gate);
    let router = Router::new().fallback(move |request: Request| {
        let gate = Arc::clone(&held);
        async move {
            if request.uri().path().starts_with("/volume-bound/") {
                return BOUND.to_string().into_response();
            }
            let body = futures_util::stream::once(async move {
                let _permit = gate.acquire().await;
                Ok::<_, std::convert::Infallible>(stream)
            });
            Body::from_stream(body).into_response()
        }
    });
    let listener = TcpListener::bind("[::1]:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(axum::serve(listener, router).into_future());
    (port, gate)
}

/// Wraps `zfs` so a read of the slot's `ployz:pull` waits while `<test>/gate` exists,
/// after touching `<test>/gate-reached`. Inspect reads that property after the slot's
/// readonly, newest snapshot and resume token.
fn gated(test: &TestDir, zfs: &Path) -> PathBuf {
    let directory = test.0.join("gated");
    fs::create_dir(&directory).unwrap();
    let wrapper = directory.join("zfs");
    let (gate, reached) = (test.0.join("gate"), test.0.join("gate-reached"));
    fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\n\
             if [ \"$*\" = 'get -H -o value ployz:pull tank/ployz-mirror/data' ] && [ -e '{gate}' ]; then\n\
             \ttouch '{reached}'\n\
             \twhile [ -e '{gate}' ]; do sleep 0.01; done\n\
             fi\n\
             exec '{zfs}' \"$@\"\n",
            gate = gate.display(),
            reached = reached.display(),
            zfs = zfs.display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    wrapper
}

async fn until(mut done: impl FnMut() -> bool) {
    while !done() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn an_inspect_a_breaking_receive_lands_between_answers_the_copy_before_or_after_it() {
    let (port, stream) = held_source("break\n").await;
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let mut storage = VolumeStorage::with_programs(zpool, gated(&test, &zfs));
    storage.send_port = port;
    let receives = storage.receives.clone();
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(UnixListener::bind(&socket).unwrap(), storage));
    let name: DockerVolumeName = "data".parse().unwrap();
    let inspect = || post(&socket, "/Volume.Copy.Inspect", json!({"name": "data"}));

    let pull = json!({"name": "data", "from": "::1", "target": "w-1-1"});
    post(&socket, "/Volume.Copy.Pull", pull).await;
    until(|| test.0.join("props/receive-ready").exists()).await;
    let before = inspect().await;

    fs::write(test.0.join("gate"), "").unwrap();
    let during = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/Volume.Copy.Inspect", json!({"name": "data"})).await }
    });
    until(|| during.is_finished() || test.0.join("gate-reached").exists()).await;
    stream.add_permits(1);
    until(|| !receives.running(&name)).await;
    fs::remove_file(test.0.join("gate")).unwrap();
    let during = during.await.unwrap();
    let after = inspect().await;

    assert_eq!(after.pointer("/Ok/resume_token"), Some(&json!("token-1")));
    assert!(
        after
            .pointer("/Ok/pull/failure")
            .is_some_and(Value::is_string),
        "{after}"
    );
    assert!(
        during == before || during == after,
        "during: {during}\nbefore: {before}\nafter: {after}"
    );
    server.abort();
}

/// A receive task that panics publishes its failure before the `zfs` child it dropped has
/// exited, and that child can still write the slot.
#[tokio::test]
async fn an_inspect_a_panicked_receive_child_writes_under_answers_the_copy_before_or_after_it() {
    let (port, stream) = held_source("break\n").await;
    stream.add_permits(1);
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let mut storage = VolumeStorage::with_programs(zpool, gated(&test, &zfs));
    storage.send_port = port;
    let receives = storage.receives.clone();
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(UnixListener::bind(&socket).unwrap(), storage));
    let name: DockerVolumeName = "data".parse().unwrap();
    let inspect = || post(&socket, "/Volume.Copy.Inspect", json!({"name": "data"}));

    let pull = json!({"name": "data", "from": "::1", "target": "w-1-1"});
    post(&socket, "/Volume.Copy.Pull", pull).await;
    until(|| test.0.join("props/receive-ready").exists()).await;
    until(|| !receives.running(&name)).await;
    let before = inspect().await;

    let (held, token) = (
        test.0.join("child-gate"),
        test.0
            .join("props/tank/ployz-mirror/data/fs/receive_resume_token"),
    );
    fs::write(&held, "").unwrap();
    let panicked = Arc::new(AtomicBool::new(false));
    receives.spawn(&name, |stop| {
        let mut child = ZfsChild::spawn(
            Command::new("sh").args([
                "-c",
                &format!(
                    "while [ -e '{held}' ]; do sleep 0.01; done; echo token-2 > '{token}.new'; mv '{token}.new' '{token}'",
                    held = held.display(),
                    token = token.display(),
                ),
            ]),
            &stop,
        )
        .unwrap();
        child.spare_from_kill();
        let panicked = Arc::clone(&panicked);
        async move {
            let _child = child;
            panicked.store(true, Ordering::SeqCst);
            panic!("the receive task broke");
        }
    });
    until(|| panicked.load(Ordering::SeqCst)).await;

    fs::write(test.0.join("gate"), "").unwrap();
    let during = tokio::spawn({
        let socket = socket.clone();
        async move { post(&socket, "/Volume.Copy.Inspect", json!({"name": "data"})).await }
    });
    until(|| during.is_finished() || test.0.join("gate-reached").exists()).await;
    fs::remove_file(&held).unwrap();
    until(|| !receives.running(&name)).await;
    fs::remove_file(test.0.join("gate")).unwrap();
    let during = during.await.unwrap();
    let after = inspect().await;

    assert_eq!(after.pointer("/Ok/resume_token"), Some(&json!("token-2")));
    assert_eq!(
        after.pointer("/Ok/pull/failure"),
        Some(&json!("the receive task panicked")),
        "{after}"
    );
    assert!(
        during == before || during == after,
        "during: {during}\nbefore: {before}\nafter: {after}"
    );
    server.abort();
}

/// A failed task whose dropped child never exits answers its failure once the hold has
/// passed. That child is stuck, and hiding the failure for it would hold the pull in
/// flight forever.
#[tokio::test]
async fn an_inspect_answers_a_failure_whose_dropped_child_never_exits_once_the_hold_passes() {
    let (port, stream) = held_source("break\n").await;
    stream.add_permits(1);
    let test = TestDir::new();
    fs::write(test.0.join("root"), "").unwrap();
    let (zpool, zfs) = fake_zfs(&test.0, USABLE_POOL);
    let mut storage = VolumeStorage::with_programs(zpool, zfs);
    storage.send_port = port;
    storage.receives = Receives::holding(Duration::from_millis(200));
    let receives = storage.receives.clone();
    let socket = test.0.join("plugin.sock");
    let server = tokio::spawn(serve(UnixListener::bind(&socket).unwrap(), storage));
    let name: DockerVolumeName = "data".parse().unwrap();
    let inspect = || post(&socket, "/Volume.Copy.Inspect", json!({"name": "data"}));

    let pull = json!({"name": "data", "from": "::1", "target": "w-1-1"});
    post(&socket, "/Volume.Copy.Pull", pull).await;
    until(|| test.0.join("props/receive-ready").exists()).await;
    until(|| !receives.running(&name)).await;

    let held = test.0.join("child-gate");
    fs::write(&held, "").unwrap();
    receives.spawn(&name, |stop| {
        let mut child = ZfsChild::spawn(
            Command::new("sh").args([
                "-c",
                &format!("while [ -e '{}' ]; do sleep 0.01; done", held.display()),
            ]),
            &stop,
        )
        .unwrap();
        child.spare_from_kill();
        async move {
            let _child = child;
            panic!("the receive task broke");
        }
    });

    let panicked = json!("the receive task panicked");
    let answered = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let copy = inspect().await;
            if copy.pointer("/Ok/pull/failure") == Some(&panicked) {
                return copy;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(
        receives.running(&name),
        "the child exited, so this proves nothing"
    );
    assert!(
        answered.is_ok(),
        "the failure stayed hidden behind a stuck child"
    );
    assert_eq!(receives.failure(&name).as_deref(), panicked.as_str());

    fs::remove_file(&held).unwrap();
    until(|| !receives.running(&name)).await;
    server.abort();
}
