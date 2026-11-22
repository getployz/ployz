//! Pull an image from another Machine's ingest TCP destination into local Docker.

use std::net::SocketAddr;

use ployz_core::{ImageDigestReference, ImageIngestDestination, PeerImagePull, retention_tag};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    process::Command,
};

use super::{Error, LocalDocker};

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
    docker: &LocalDocker,
    pull: &PeerImagePull,
    source: ImageIngestDestination,
    platform: &str,
) -> Result<(), Error> {
    let image = pull.image();
    let retained = if image.contains('@') {
        ImageDigestReference::parse(image)
            .map_err(|error| Error::PeerPull(ployz_core::error_chain::inline(&error)))?;
        if !docker.uses_containerd_store().await? {
            return Err(Error::UnsupportedImageStore);
        }
        retention_tag(image)
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
        if let Some(digest) = ployz_core::ImageRef::new(image).digest
            && descriptor_digest(docker, pulled).await?.as_deref() != Some(digest)
        {
            return Err(Error::PeerPull(format!(
                "the image store did not retain {image}"
            )));
        }
        let tag = tag.unwrap_or(image);
        if let Some(digest) = ployz_core::retained_digest(tag) {
            retention_free(docker, pulled, tag, &digest).await?;
        }
        // Verify before publishing the requested or retention tag: a failed
        // attempt must not leave a tag or remove one from an earlier delivery.
        docker_cli(docker, &["tag", "--", pulled, tag]).await?;
        Ok(())
    }
    .await;
    let _ = docker_cli(docker, &["image", "rm", pulled]).await;
    result
}

/// Refuse to write retention name `tag` unless `pulled` is the content it names, and
/// the name is free or already names that content. Image Cleanup trusts these names, so
/// no write, whatever its mode, moves one onto other content.
async fn retention_free(
    docker: &std::path::Path,
    pulled: &str,
    tag: &str,
    digest: &ployz_core::ImageDigest,
) -> Result<(), Error> {
    if descriptor_digest(docker, pulled).await?.as_deref() != Some(digest.as_str()) {
        return Err(Error::RetentionNameTaken(tag.to_owned()));
    }
    match descriptor_digest(docker, tag).await {
        Ok(held) if held.as_deref() == Some(digest.as_str()) => Ok(()),
        Ok(_) => Err(Error::RetentionNameTaken(tag.to_owned())),
        Err(Error::PeerPull(printed)) if printed.contains("No such image") => Ok(()),
        Err(error) => Err(error),
    }
}

/// The digest of the content Docker holds under `reference`.
async fn descriptor_digest(
    docker: &std::path::Path,
    reference: &str,
) -> Result<Option<String>, Error> {
    let descriptor = docker_cli(
        docker,
        &[
            "image",
            "inspect",
            reference,
            "--format",
            "{{json .Descriptor}}",
        ],
    )
    .await?;
    let descriptor: serde_json::Value = serde_json::from_str(&descriptor)?;
    Ok(descriptor
        .get("digest")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned))
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
            .map_err(|error| Error::PeerPull(ployz_core::error_chain::inline(&error)))?;
        let port = listener
            .local_addr()
            .map_err(|error| Error::PeerPull(ployz_core::error_chain::inline(&error)))?
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
        .map_err(|error| Error::PeerPull(ployz_core::error_chain::inline(&error)))?;
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
            let (runtime, _fake) = crate::docker::test_support::fake_runtime().await;
            let error = pull_from_ingest(&runtime.local_docker(), &pull, source, "linux/amd64")
                .await
                .unwrap_err();
            assert!(matches!(&error, Error::PeerPull(_)), "{error}");
            assert!(error.to_string().contains("digest"), "{error}");
        }
    }

    #[tokio::test]
    async fn a_digest_pull_is_refused_when_docker_has_no_containerd_image_store() {
        let source = ImageIngestDestination {
            management_address: ployz_core::ManagementAddress("fdcc::7".parse().unwrap()),
            port: UNREGISTRY_PORT,
        };
        let pull = PeerImagePull::Reference {
            image: format!("api@sha256:{}", "a".repeat(64)),
        };
        let (runtime, fake) = crate::docker::test_support::fake_runtime().await;

        let error = pull_from_ingest(&runtime.local_docker(), &pull, source, "linux/amd64")
            .await
            .unwrap_err();

        assert!(matches!(error, Error::UnsupportedImageStore), "{error}");
        assert_eq!(fake.requests.lock().unwrap().len(), 1);
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

    /// Image Cleanup keeps what a retention name holds, so a peer copy writes one only
    /// where it is free or already names the copied content, and only for that content.
    #[tokio::test]
    async fn a_retention_name_is_never_moved_onto_other_content() {
        use std::{fs, os::unix::fs::PermissionsExt as _};

        let root = std::env::temp_dir().join(format!(
            "ployz-retention-{}",
            ployz_core::MachineId::random()
        ));
        fs::create_dir(&root).unwrap();
        let docker = root.join("docker");
        fs::write(
            &docker,
            r#"#!/bin/sh
cd "$(dirname "$0")"
printf '%s\n' "$*" >> calls
case "$1 $2" in
  'image inspect')
    case "$3" in
      127.0.0.1:*) cat pulled ;;
      *) case "$(cat name)" in
           free) echo "Error response from daemon: No such image: $3" >&2; exit 1 ;;
           *) cat name ;;
         esac ;;
    esac ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
        let descriptor = |hex: char| {
            serde_json::json!({"digest": format!("sha256:{}", hex.to_string().repeat(64))})
                .to_string()
        };
        let image = format!("app@sha256:{}", "1".repeat(64));
        let pulled = format!("127.0.0.1:1234/{image}");
        let name = format!("app:ployz-sha256-{}", "1".repeat(64));
        // What the pull delivered, what the name holds now, and whether it is written.
        for (delivered, held, written) in [
            ('1', None, true),
            ('1', Some('1'), true),
            ('1', Some('2'), false),
            ('2', None, false),
        ] {
            fs::write(root.join("pulled"), descriptor(delivered)).unwrap();
            fs::write(
                root.join("name"),
                held.map_or_else(|| "free".to_owned(), descriptor),
            )
            .unwrap();
            fs::write(root.join("calls"), "").unwrap();
            let result = pull_and_tag(&image, &pulled, Some(&name), "linux/amd64", &docker).await;
            let calls = fs::read_to_string(root.join("calls")).unwrap();
            assert_eq!(
                calls.contains(&format!("tag -- {pulled} {name}")),
                written,
                "{delivered} over {held:?}: {calls}"
            );
            assert_eq!(
                result.is_ok(),
                written,
                "{delivered} over {held:?}: {result:?}"
            );
            if held == Some('2') {
                assert!(
                    matches!(result, Err(Error::RetentionNameTaken(ref taken)) if *taken == name),
                    "{result:?}"
                );
            }
        }
        // An ordinary tag that only starts like a retention name is no retention name.
        let ordinary = "app:ployz-sha256-staging";
        let pulled = format!("127.0.0.1:1234/{ordinary}");
        fs::write(root.join("name"), "free").unwrap();
        pull_and_tag(ordinary, &pulled, None, "linux/amd64", &docker)
            .await
            .unwrap();
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
