//! Volume.Copy.Inspect while a pull's receive task finishes between Inspect's reads.

use std::{os::unix::fs::PermissionsExt as _, sync::Arc, time::Duration};

use axum::{body::Body, extract::Request, response::IntoResponse};
use tokio::{net::TcpListener, sync::Semaphore};

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
