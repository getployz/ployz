//! Pull an image from another Machine's ingest TCP destination into local Docker.

use std::net::{Ipv4Addr, SocketAddr};

use ployz_core::{
    ImageDigestReference, ImageIngestDestination, PeerImagePull, image_server_reference,
    retention_tag,
};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use super::{Error, LocalDocker};

/// Pull an image from a peer Machine's ingest TCP destination into local Docker.
///
/// Docker treats `127.0.0.0/8` as an insecure registry, so the pull is proxied
/// through localhost instead of asking dockerd to speak HTTP to the WireGuard IP.
/// `platform` makes Docker fetch that variant's manifest, configuration and
/// layers; a source holding only the index fails the pull. An optional `tag`
/// is published only after the pulled digest has been verified. Once `gone`
/// fires, the transfer stops and only its cleanup runs.
///
/// # Errors
///
/// Returns when the localhost proxy cannot listen, Docker cannot pull or tag, or
/// `gone` fired first.
pub(crate) async fn pull_from_ingest(
    docker: &LocalDocker,
    pull: &PeerImagePull,
    source: ImageIngestDestination,
    platform: &str,
    gone: &CancellationToken,
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
    let pulled = image_server_reference(SocketAddr::from((Ipv4Addr::LOCALHOST, proxy.port)), image)
        .ok_or_else(|| {
            Error::PeerPull(format!(
                "{image} is a tag from a registry with a port, which a Machine can't serve by name; pin it by digest"
            ))
        })?;
    pull_and_tag(
        image,
        &pulled,
        pull.tag().or(retained.as_deref()),
        platform,
        std::path::Path::new("docker"),
        gone,
    )
    .await
}

async fn pull_and_tag(
    image: &str,
    pulled: &str,
    tag: Option<&str>,
    platform: &str,
    docker: &std::path::Path,
    gone: &CancellationToken,
) -> Result<(), Error> {
    let transfer = async {
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
    };
    // A caller gone stops the transfer here, killing the Docker CLI it runs, so Docker
    // drops the pull and the admission turn it holds frees for the next Deployment.
    let result = gone
        .run_until_cancelled(transfer)
        .await
        .unwrap_or_else(|| Err(Error::PeerPull("the caller stopped waiting".into())));
    if holds_name(docker, pulled).await {
        let _ = docker_cli(docker, &["image", "rm", pulled]).await;
    }
    result
}

/// Whether Docker holds `pulled` under that very name, so removing it removes only it. A
/// pull that failed or was stopped may never have written it, and Docker resolves a
/// missing digest name to an unnamed image of that digest, such as the source's own
/// copy that another Deployment is still pulling, which `image rm` would then delete. A
/// tag name resolves only to itself.
async fn holds_name(docker: &std::path::Path, pulled: &str) -> bool {
    if !pulled.contains('@') {
        return true;
    }
    docker_cli(
        docker,
        &[
            "image",
            "inspect",
            pulled,
            "--format",
            "{{range .RepoTags}}{{println .}}{{end}}{{range .RepoDigests}}{{println .}}{{end}}",
        ],
    )
    .await
    .is_ok_and(|names| names.lines().any(|name| name == pulled))
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
        .kill_on_drop(true)
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
            let error = pull_from_ingest(
                &runtime.local_docker(),
                &pull,
                source,
                "linux/amd64",
                &CancellationToken::new(),
            )
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

        let error = pull_from_ingest(
            &runtime.local_docker(),
            &pull,
            source,
            "linux/amd64",
            &CancellationToken::new(),
        )
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
case "$1 $2 $5" in
  'image inspect {{range'*)
    if [ "$mode" = pull ]; then echo "No such image: $3" >&2; exit 1; fi
    echo "$3"; exit 0 ;;
esac
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
                let result = pull_and_tag(
                    &image,
                    &pulled,
                    Some(tag),
                    "linux/arm64",
                    &docker,
                    &CancellationToken::new(),
                )
                .await;
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
                // A failed pull wrote no staging name, so there is none to remove.
                let removed = (mode != "pull").then(|| format!("image rm {pulled}"));
                assert_eq!(
                    calls
                        .lines()
                        .last()
                        .filter(|line| line.starts_with("image rm")),
                    removed.as_deref(),
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
case "$1 $2 $5" in
  'image inspect {{range'*) echo "$3"; exit 0 ;;
esac
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
            let result = pull_and_tag(
                &image,
                &pulled,
                Some(&name),
                "linux/amd64",
                &docker,
                &CancellationToken::new(),
            )
            .await;
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
        pull_and_tag(
            ordinary,
            &pulled,
            None,
            "linux/amd64",
            &docker,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// A Docker that answers a missing digest name with an unnamed image of that digest,
    /// as Docker does, so `image rm` of a staging name a pull never wrote deletes that image.
    /// `pull` acts as `mode` says: it `fail`s, or it `hang`s after naming its pid in `started`.
    fn docker_with_unnamed_copy(root: &std::path::Path) -> std::path::PathBuf {
        use std::{fs, os::unix::fs::PermissionsExt as _};

        fs::create_dir(root).unwrap();
        fs::write(root.join("names"), "").unwrap();
        fs::write(root.join("unnamed"), "").unwrap();
        let docker = root.join("docker");
        fs::write(
            &docker,
            r#"#!/bin/sh
cd "$(dirname "$0")"
printf '%s\n' "$*" >> calls
case "$1 $2" in
  'image inspect')
    if grep -qxF "$3" names; then echo "$3"
    elif [ -e unnamed ]; then :
    else echo "No such image: $3" >&2; exit 1; fi ;;
  'image rm')
    if grep -qxF "$3" names; then grep -vxF "$3" names > names.new; mv names.new names
    elif [ -e unnamed ]; then rm unnamed; echo Deleted
    else echo "No such image: $3" >&2; exit 1; fi ;;
  pull*)
    case "$(cat mode)" in
      fail) echo pull-failed >&2; exit 1 ;;
      hang) echo $$ > started; exec sleep 600 ;;
    esac ;;
esac
"#,
        )
        .unwrap();
        fs::set_permissions(&docker, fs::Permissions::from_mode(0o700)).unwrap();
        docker
    }

    /// Two Deployments may pull one digest at once, from a source whose copy has no name.
    /// A pull that failed before writing its staging name must not delete that copy, which
    /// the other pull still reads.
    #[tokio::test]
    async fn a_failed_pull_removes_no_other_image_of_its_digest() {
        let root = std::env::temp_dir().join(format!(
            "ployz-failed-pull-{}",
            ployz_core::MachineId::random()
        ));
        let docker = docker_with_unnamed_copy(&root);
        std::fs::write(root.join("mode"), "fail").unwrap();
        let image = format!("app@sha256:{}", "1".repeat(64));
        let pulled = format!("127.0.0.1:1234/{image}");

        let result = pull_and_tag(
            &image,
            &pulled,
            None,
            "linux/amd64",
            &docker,
            &CancellationToken::new(),
        )
        .await;

        assert!(result.is_err(), "{result:?}");
        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        assert!(
            root.join("unnamed").exists(),
            "deleted the other copy: {calls}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A cancelled Deployment's transfer must not run on: it holds the admission turn the
    /// next Deployment's creates wait for. Once the caller is gone, the pull ends, its
    /// Docker CLI is killed, and no tag is written. The pull never wrote its staging name,
    /// so the unnamed copy of its digest that another pull reads is left alone.
    #[tokio::test]
    async fn a_pull_whose_caller_is_gone_stops_and_cleans_up() {
        use std::fs;

        let root = std::env::temp_dir().join(format!(
            "ployz-gone-pull-{}",
            ployz_core::MachineId::random()
        ));
        let docker = docker_with_unnamed_copy(&root);
        fs::write(root.join("mode"), "hang").unwrap();
        let started = root.join("started");
        assert!(
            std::process::Command::new("mkfifo")
                .arg(&started)
                .status()
                .unwrap()
                .success()
        );
        let image = format!("app@sha256:{}", "1".repeat(64));
        let pulled = format!("127.0.0.1:1234/{image}");
        let gone = CancellationToken::new();

        let pull = pull_and_tag(&image, &pulled, None, "linux/amd64", &docker, &gone);
        let pid = async {
            let started = started.clone();
            let pid = tokio::task::spawn_blocking(move || fs::read_to_string(started))
                .await
                .unwrap()
                .unwrap();
            gone.cancel();
            pid.trim().to_owned()
        };
        let (result, pid) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(pull, pid)
        })
        .await
        .expect("a pull whose caller is gone must end");

        assert!(result.is_err(), "{result:?}");
        let calls = fs::read_to_string(root.join("calls")).unwrap();
        assert!(
            root.join("unnamed").exists(),
            "deleted the other copy: {calls}"
        );
        assert!(
            !calls.lines().any(|line| line.starts_with("tag ")),
            "{calls}"
        );
        let stat = format!("/proc/{pid}/stat");
        let ended = || {
            fs::read_to_string(&stat).map_or(true, |stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with('Z'))
            })
        };
        for _ in 0..200 {
            if ended() {
                break;
            }
            // real-wait: the killed pull exits on the kernel's schedule, with nothing to await.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(ended(), "the Docker pull still runs");
        fs::remove_dir_all(root).unwrap();
    }
}
