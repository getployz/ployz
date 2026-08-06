use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bollard::Docker;
use ployz_core::deploy::ImageReference;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

use super::runner::DockerManagedContainerRunner;

pub(super) const TEST_ENDPOINT_SUBNET: &str = "10.42.7.0/24";

pub(super) fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

pub(super) async fn runner_with_responses(
    responses: Vec<(u16, String)>,
) -> (
    DockerManagedContainerRunner,
    Arc<AtomicUsize>,
    tempfile::TempDir,
) {
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = attempts.clone();
    let (runner, socket_dir) = stub_runner_with_responses(responses, move |_| {
        server_attempts.fetch_add(1, Ordering::SeqCst);
    })
    .await;
    (runner, attempts, socket_dir)
}

/// Like [`runner_with_responses`], but records every raw request head so a
/// test can assert on the method, path, and query Docker was sent.
pub(super) async fn recording_runner_with_responses(
    responses: Vec<(u16, String)>,
) -> (
    DockerManagedContainerRunner,
    Arc<Mutex<Vec<String>>>,
    tempfile::TempDir,
) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let server_requests = requests.clone();
    let (runner, socket_dir) = stub_runner_with_responses(responses, move |request| {
        server_requests
            .lock()
            .expect("test records Docker requests")
            .push(request);
    })
    .await;
    (runner, requests, socket_dir)
}

async fn stub_runner_with_responses(
    responses: Vec<(u16, String)>,
    on_request: impl Fn(String) + Send + 'static,
) -> (DockerManagedContainerRunner, tempfile::TempDir) {
    let socket_dir = tempfile::TempDir::new().expect("Docker API stub directory");
    let socket_path = socket_dir.path().join("docker.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind Docker API stub");
    tokio::spawn(async move {
        for (status, body) in responses {
            let (mut stream, _) = listener.accept().await.expect("accept Docker API request");
            let mut request = Vec::new();
            let mut buffer = [0; 512];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream
                    .read(&mut buffer)
                    .await
                    .expect("read Docker API request");
                assert_ne!(read, 0, "Docker API request ended before its headers");
                request.extend_from_slice(
                    buffer
                        .get(..read)
                        .expect("read length is bounded by buffer"),
                );
            }
            on_request(String::from_utf8(request).expect("Docker API request is UTF-8"));
            let reason = if status == 200 {
                "OK"
            } else {
                "Internal Server Error"
            };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write Docker API response");
        }
    });
    let docker = Docker::connect_with_socket(
        socket_path.to_str().expect("UTF-8 Docker API socket path"),
        5,
        bollard::API_DEFAULT_VERSION,
    )
    .expect("connect Docker API stub");
    (
        DockerManagedContainerRunner::connected_for_test(docker, TEST_ENDPOINT_SUBNET),
        socket_dir,
    )
}
