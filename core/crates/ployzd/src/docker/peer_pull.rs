//! Pull an image from another Machine's ingest TCP destination into local Docker.

use std::net::SocketAddr;

use ployz_core::{ImageDigestReference, ImageIngestDestination, PeerImagePull};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    process::Command,
};

use super::Error;

/// Pull an image from a peer Machine's ingest TCP destination into local Docker.
///
/// Docker treats `127.0.0.0/8` as an insecure registry, so the pull is proxied
/// through localhost instead of asking dockerd to speak HTTP to the WireGuard IP.
/// `platform` makes Docker fetch that variant's manifest, configuration and
/// layers; a source holding only the index fails the pull. An optional `tag`
/// is published only after the pulled digest has been verified.
///
/// # Errors
///
/// Returns when the localhost proxy cannot listen or Docker cannot pull or tag.
pub(crate) async fn pull_from_ingest(
    pull: &PeerImagePull,
    source: ImageIngestDestination,
    platform: &str,
) -> Result<(), Error> {
    let image = pull.image();
    let retained = if image.contains('@') {
        ImageDigestReference::parse(image).map_err(|error| Error::PeerPull(error.to_string()))?;
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
    pull_and_tag(
        image,
        &pulled,
        pull.tag().or(retained.as_deref()),
        platform,
        std::path::Path::new("docker"),
    )
    .await
}

async fn pull_and_tag(
    image: &str,
    pulled: &str,
    tag: Option<&str>,
    platform: &str,
    docker: &std::path::Path,
) -> Result<(), Error> {
    let result = async {
        docker_cli(docker, &["pull", "--platform", platform, pulled]).await?;
        if let Some((_, digest)) = image.split_once('@') {
            let descriptor = docker_cli(
                docker,
                &[
                    "image",
                    "inspect",
                    pulled,
                    "--format",
                    "{{json .Descriptor}}",
                ],
            )
            .await?;
            let descriptor: serde_json::Value = serde_json::from_str(&descriptor)?;
            if descriptor.get("digest").and_then(serde_json::Value::as_str) != Some(digest) {
                return Err(Error::PeerPull(format!(
                    "the image store did not retain {image}"
                )));
            }
        }
        // Verify before publishing the requested or retention tag: a failed
        // attempt must not leave a tag or remove one from an earlier delivery.
        docker_cli(docker, &["tag", "--", pulled, tag.unwrap_or(image)]).await?;
        Ok(())
    }
    .await;
    let _ = docker_cli(docker, &["image", "rm", pulled]).await;
    result
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

async fn docker_cli(docker: &std::path::Path, args: &[&str]) -> Result<String, Error> {
    let output = Command::new(docker)
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

    #[tokio::test]
    async fn invalid_digest_reference_is_rejected_before_any_docker_work() {
        let source = ImageIngestDestination {
            management_address: ployz_core::ManagementAddress("fdcc::7".parse().unwrap()),
            port: UNREGISTRY_PORT,
        };
        for image in ["api@sha256:invalid", "api@sha512:invalid"] {
            let pull = PeerImagePull::Reference {
                image: image.into(),
            };
            let error = pull_from_ingest(&pull, source, "linux/amd64")
                .await
                .unwrap_err();
            assert!(matches!(&error, Error::PeerPull(_)), "{error}");
            assert!(error.to_string().contains("digest"), "{error}");
        }
    }

    #[tokio::test]
    async fn failed_digest_delivery_cleans_up_without_publishing_unverified_content() {
        use std::{fs, os::unix::fs::PermissionsExt as _};

        let root = std::env::temp_dir().join(format!(
            "ployz-peer-pull-{}",
            ployz_core::MachineId::random()
        ));
        fs::create_dir(&root).unwrap();
        let docker = root.join("docker");
        fs::write(
            &docker,
            r#"#!/bin/sh
cd "$(dirname "$0")"
printf '%s\n' "$*" >> calls
mode=$(cat mode)
case "$1 $2" in
  'image inspect')
    case "$mode" in
      inspect) echo inspect-failed >&2; exit 1 ;;
      json) echo not-json ;;
      digest) echo '{"digest":"sha256:wrong"}' ;;
      *) cat descriptor ;;
    esac ;;
  'image rm') echo cleanup-failed >&2; exit 1 ;;
  *) if [ "$1" = "$mode" ]; then echo "$mode-failed" >&2; exit 1; fi ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
        let digest = format!("sha256:{}", "1".repeat(64));
        let image = format!("example.test/api@{digest}");
        let retained = image.replace("@sha256:", ":ployz-sha256-");
        let pulled = format!("127.0.0.1:1234/{image}");
        fs::write(
            root.join("descriptor"),
            serde_json::json!({"digest": digest}).to_string(),
        )
        .unwrap();
        for tag in [&retained, "example.test/api:latest"] {
            for mode in ["inspect", "json", "digest", "tag", "pull", "success"] {
                fs::write(root.join("mode"), mode).unwrap();
                fs::write(root.join("calls"), "").unwrap();
                let result = pull_and_tag(&image, &pulled, Some(tag), "linux/arm64", &docker).await;
                assert_eq!(result.is_ok(), mode == "success", "{mode}: {result:?}");
                if let Err(error) = result {
                    assert!(!error.to_string().contains("cleanup-failed"), "{error}");
                    if ["inspect", "tag", "pull"].contains(&mode) {
                        assert!(
                            error.to_string().contains(&format!("{mode}-failed")),
                            "{error}"
                        );
                    }
                }
                let calls = fs::read_to_string(root.join("calls")).unwrap();
                if mode == "success" {
                    assert!(calls.contains(&format!("tag -- {pulled} {tag}")), "{calls}");
                }
                assert_eq!(
                    calls.lines().last(),
                    Some(format!("image rm {pulled}").as_str()),
                    "{mode}: {calls}"
                );
                assert_eq!(
                    calls.lines().any(|line| line.starts_with("tag ")),
                    ["tag", "success"].contains(&mode),
                    "{mode}: {calls}"
                );
                // The destination's variant is named on every attempt: Docker must
                // fetch that manifest and its content, not merely the index.
                assert!(
                    calls.contains(&format!("pull --platform linux/arm64 {pulled}")),
                    "{mode}: {calls}"
                );
                assert!(
                    !calls.contains(&format!("image rm {retained}")),
                    "must preserve previously retained content"
                );
            }
        }
        fs::remove_dir_all(root).unwrap();
    }

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
