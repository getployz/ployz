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
    let retained = if image.contains('@') {
        ployz_build::remote::validate_remote_context(&format!("docker-image://{image}"))
            .map_err(|error| Error::PeerPull(error.to_string()))?;
        if !super::LocalDocker::connect()?
            .uses_containerd_store()
            .await?
        {
            return Err(Error::UnsupportedImageStore);
        }
        Some(image.replace("@sha256:", ":ployz-sha256-"))
    } else {
        None
    };
    let proxy = ImageProxy::open(source).await?;
    let pulled = localhost_registry_reference(proxy.port, image);
    docker_cli(["pull", &pulled]).await?;
    // Docker cannot tag repository@digest. A content-specific tag retains
    // that repository's digest without another client's tag overwriting it.
    docker_cli(["tag", &pulled, retained.as_deref().unwrap_or(image)]).await?;
    if let Some((_, digest)) = image.split_once('@') {
        let descriptor = docker_cli([
            "image",
            "inspect",
            image,
            "--format",
            "{{json .Descriptor}}",
        ])
        .await?;
        let descriptor: serde_json::Value = serde_json::from_str(&descriptor)?;
        if descriptor.get("digest").and_then(serde_json::Value::as_str) != Some(digest) {
            return Err(Error::PeerPull(format!(
                "the image store did not retain {image}"
            )));
        }
    }
    let _ = docker_cli(["image", "rm", &pulled]).await;
    Ok(())
}

fn localhost_registry_reference(port: u16, image: &str) -> String {
    format!("127.0.0.1:{port}/{image}")
}

/// A connection-scoped loopback bridge to a Machine image server. Both Docker
/// and BuildKit accept loopback HTTP without changing global registry policy.
pub(crate) struct ImageProxy {
    pub(crate) port: u16,
    task: tokio::task::JoinHandle<()>,
}

impl ImageProxy {
    pub(crate) async fn open(source: ImageIngestDestination) -> Result<Self, Error> {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|error| Error::PeerPull(error.to_string()))?;
        let port = listener
            .local_addr()
            .map_err(|error| Error::PeerPull(error.to_string()))?
            .port();
        let source = SocketAddr::from((source.management_address.0, source.port));
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((mut inbound, _)) = accepted else { break };
                        connections.spawn(async move {
                            if let Ok(mut outbound) = TcpStream::connect(source).await {
                                let _ = copy_bidirectional(&mut inbound, &mut outbound).await;
                            }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Ok(Self { port, task })
    }
}
impl Drop for ImageProxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn docker_cli<const N: usize>(args: [&str; N]) -> Result<String, Error> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .await
        .map_err(|error| Error::PeerPull(error.to_string()))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(Error::PeerPull(
            String::from_utf8_lossy(&output.stderr).trim().into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::UNREGISTRY_PORT;

    #[test]
    fn localhost_pull_reference_keeps_the_image_path() {
        assert_eq!(
            localhost_registry_reference(UNREGISTRY_PORT, "busybox:1.37.0"),
            format!("127.0.0.1:{UNREGISTRY_PORT}/busybox:1.37.0")
        );
        assert_eq!(
            localhost_registry_reference(9, "registry.test/team/api:v1"),
            "127.0.0.1:9/registry.test/team/api:v1"
        );
    }
}
