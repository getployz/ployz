use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
    let socket_dir = tempfile::TempDir::new().expect("Docker API stub directory");
    let socket_path = socket_dir.path().join("docker.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind Docker API stub");
    let attempts = Arc::new(AtomicUsize::new(0));
    let server_attempts = attempts.clone();
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
            server_attempts.fetch_add(1, Ordering::SeqCst);
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
        attempts,
        socket_dir,
    )
}
