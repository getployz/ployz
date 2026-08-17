//! Pull an image from another Machine's ingest TCP destination into local Docker.

use std::net::SocketAddr;

use ployz_core::ImageIngestDestination;
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    process::Command,
};

use super::Error;

/// Pull `image` from a peer Machine's ingest TCP destination into local Docker.
///
/// Docker treats `127.0.0.0/8` as an insecure registry, so the pull is proxied
/// through localhost instead of asking dockerd to speak HTTP to the WireGuard IP.
///
/// # Errors
///
/// Returns when the localhost proxy cannot listen or Docker cannot pull or tag.
pub(crate) async fn pull_from_ingest(
    image: &str,
    source: ImageIngestDestination,
) -> Result<(), Error> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| Error::PeerPull(error.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|error| Error::PeerPull(error.to_string()))?
        .port();
    let source_addr = SocketAddr::from((source.gateway.0, source.port));
    let proxy = tokio::spawn(proxy_to_source(listener, source_addr));
    let pulled = localhost_registry_reference(port, image);
    let result = async {
        docker_cli(["pull", &pulled]).await?;
        docker_cli(["tag", &pulled, image]).await?;
        let _ = docker_cli(["image", "rm", &pulled]).await;
        Ok(())
    }
    .await;
    proxy.abort();
    result
}

fn localhost_registry_reference(port: u16, image: &str) -> String {
    format!("127.0.0.1:{port}/{image}")
}

async fn proxy_to_source(listener: TcpListener, source: SocketAddr) {
    loop {
        let Ok((mut inbound, _)) = listener.accept().await else {
            break;
        };
        tokio::spawn(async move {
            let Ok(mut outbound) = TcpStream::connect(source).await else {
                return;
            };
            let _ = copy_bidirectional(&mut inbound, &mut outbound).await;
        });
    }
}

async fn docker_cli<const N: usize>(args: [&str; N]) -> Result<(), Error> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .await
        .map_err(|error| Error::PeerPull(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::PeerPull(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localhost_pull_reference_keeps_the_image_path() {
        assert_eq!(
            localhost_registry_reference(51500, "busybox:1.37.0"),
            "127.0.0.1:51500/busybox:1.37.0"
        );
        assert_eq!(
            localhost_registry_reference(9, "registry.test/team/api:v1"),
            "127.0.0.1:9/registry.test/team/api:v1"
        );
    }
}
