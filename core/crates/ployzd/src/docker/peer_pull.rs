//! Pull an image from another Machine's ingest TCP destination into local Docker.

use std::{
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use ployz_core::{
    ImageDigestReference, ImageIngestDestination, PeerImagePull, image_server_reference,
    retention_tag,
};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    process::Command,
    task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

use super::{Error, LocalDocker};

/// The repository every staging name sits under, behind the pull's loopback host. No one
/// else names an image this way, so a sweep can tell a staging name from a user's image.
const STAGING: &str = "ployz-staging";

/// This run of the daemon's part of each staging name, so a sweep at start removes only
/// names an earlier run left, never one a delivery of this run still holds.
fn boot() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(|| {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{:x}{started:x}", std::process::id())
    })
}

/// The Docker CLI peer pulls run, and the staging names whose removal failed, which
/// the next delivery removes.
pub(crate) struct PeerPulls {
    cli: PathBuf,
    leftover: Mutex<Vec<String>>,
}

impl Default for PeerPulls {
    fn default() -> Self {
        Self::new("docker".into())
    }
}

impl PeerPulls {
    pub(crate) fn new(cli: PathBuf) -> Self {
        Self {
            cli,
            leftover: Mutex::default(),
        }
    }

    /// Remove staging name `name`, off the caller's turn. Only the delivery that made the
    /// name, or a sweep after the run that made it ended, removes it. A failed removal is
    /// logged and kept for the next delivery to try again.
    fn clean_up(self: &Arc<Self>, name: String) -> JoinHandle<()> {
        let pulls = Arc::clone(self);
        tokio::spawn(async move {
            if let Err(error) = remove_staging(&pulls.cli, &name).await {
                tracing::warn!(
                    staging = %name,
                    error = %ployz_core::error_chain::inline(&error),
                    "a peer pull's staging name is left until the next delivery removes it"
                );
                pulls
                    .leftover
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(name);
            }
        })
    }

    /// Try again to remove each staging name an earlier delivery left, off the caller's
    /// turn as well.
    fn retry_leftovers(self: &Arc<Self>) -> Vec<JoinHandle<()>> {
        let leftover =
            std::mem::take(&mut *self.leftover.lock().unwrap_or_else(PoisonError::into_inner));
        leftover
            .into_iter()
            .map(|name| self.clean_up(name))
            .collect()
    }

    /// Remove the staging names an earlier run of the daemon left, which no delivery of
    /// this run will. A name this run made, and every name outside [`STAGING`], is left.
    async fn sweep(self: &Arc<Self>) -> Vec<JoinHandle<()>> {
        let failed = |error: &Error| {
            tracing::warn!(
                error = %ployz_core::error_chain::inline(error),
                "could not read images to remove staging names an earlier run left"
            );
        };
        // `image ls` hides a digest-only name when its image has other names, so the
        // names come from each image's inspect.
        let images = match docker_cli(
            &self.cli,
            &["image", "ls", "--all", "--quiet", "--no-trunc"],
        )
        .await
        {
            Ok(images) => images,
            Err(error) => {
                failed(&error);
                return Vec::new();
            }
        };
        let mut earlier = std::collections::BTreeSet::new();
        for image in images.lines().collect::<std::collections::BTreeSet<_>>() {
            match docker_cli(&self.cli, &["image", "inspect", image, "--format", NAMES]).await {
                Ok(names) => earlier.extend(
                    names
                        .lines()
                        .filter(|name| earlier_staging(name))
                        .map(str::to_owned),
                ),
                Err(error) if no_such_image(&error) => {}
                Err(error) => failed(&error),
            }
        }
        earlier
            .into_iter()
            .map(|name| self.clean_up(name))
            .collect()
    }
}

/// An image inspect format that prints every name the image holds, one a line.
const NAMES: &str =
    "{{range .RepoTags}}{{println .}}{{end}}{{range .RepoDigests}}{{println .}}{{end}}";

/// Whether `name` is a staging name an earlier run of the daemon made.
fn earlier_staging(name: &str) -> bool {
    let Some((port, path)) = name
        .strip_prefix("127.0.0.1:")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    let Some(run) = path
        .strip_prefix(STAGING)
        .and_then(|path| path.strip_prefix('/'))
    else {
        return false;
    };
    !port.is_empty()
        && port.bytes().all(|byte| byte.is_ascii_digit())
        && name.contains("@sha256:")
        && !run.starts_with(&format!("{}/", boot()))
}

impl LocalDocker {
    /// Remove the staging names an earlier run of the daemon left behind, in the
    /// background. Each failure is logged and kept for the next delivery.
    pub(crate) async fn sweep_earlier_staging(&self) {
        drop(self.peer_pulls.sweep().await);
    }
}

/// What a delivery publishes from: content Docker already held, by its ID, or a staging
/// name it pulls to. Only a staging name is removed afterwards, and it holds only what
/// that delivery brought: a pull over content Docker holds unnamed would take that copy
/// over, and removing the name would then delete it.
enum Prepared {
    Held(String),
    Staged(Staging),
}

/// A staging name, unique to one delivery, and the loopback proxy it pulls through.
struct Staging {
    name: String,
    _proxy: ImageProxy,
}

impl Staging {
    fn new(proxy: ImageProxy, digest: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "127.0.0.1:{}/{STAGING}/{}/{}@{digest}",
            proxy.port,
            boot(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        Self {
            name,
            _proxy: proxy,
        }
    }
}

impl Prepared {
    /// The reference the delivery checks and tags from.
    fn reference(&self) -> &str {
        match self {
            Self::Held(id) => id,
            Self::Staged(staging) => &staging.name,
        }
    }
}

/// Pull an image from a peer Machine's ingest TCP destination into local Docker.
///
/// Docker treats `127.0.0.0/8` as an insecure registry, so the pull is proxied
/// through localhost instead of asking dockerd to speak HTTP to the WireGuard IP.
/// `platform` makes Docker fetch that variant's manifest, configuration and
/// layers; a source holding only the index fails the pull. Content Docker already
/// holds for `platform` is not pulled again. An optional `tag` is published only
/// after the content's digest has been verified. Once `gone` fires, a delivery that
/// has not begun its tag stops and publishes nothing; one that has completes it. The
/// staging name is removed after this returns, so a stalled removal holds no
/// admission turn.
///
/// # Errors
///
/// Returns when the localhost proxy cannot listen, the source has no such image,
/// Docker cannot pull or tag, or `gone` fired before the tag.
pub(crate) async fn pull_from_ingest(
    docker: &LocalDocker,
    pull: &PeerImagePull,
    source: ImageIngestDestination,
    platform: &str,
    gone: &CancellationToken,
) -> Result<(), Error> {
    deliver(docker, pull, source, platform, gone).await.0
}

/// [`pull_from_ingest`], answering also the cleanup it does not wait for, so a test can.
async fn deliver(
    docker: &LocalDocker,
    pull: &PeerImagePull,
    source: ImageIngestDestination,
    platform: &str,
    gone: &CancellationToken,
) -> (Result<(), Error>, Option<JoinHandle<()>>) {
    docker.peer_pulls.retry_leftovers();
    let image = pull.image();
    let requested = ployz_core::ImageRef::new(image).digest;
    if requested.is_some()
        && let Err(error) = ImageDigestReference::parse(image)
    {
        return (
            Err(Error::PeerPull(ployz_core::error_chain::inline(&error))),
            None,
        );
    }
    // Everything before the tag that waits on Docker or the source runs in the caller's
    // race, so a caller gone never leaves this work holding the turn.
    let prepared = match gone
        .run_until_cancelled(prepare(docker, image, requested, source, platform))
        .await
    {
        Some(Ok(prepared)) => prepared,
        Some(Err(error)) => return (Err(error), None),
        None => return (Err(stopped()), None),
    };
    let retained = requested.and_then(|_| retention_tag(image));
    pull_and_tag(
        &docker.peer_pulls,
        image,
        prepared,
        pull.tag().or(retained.as_deref()),
        platform,
        gone,
    )
    .await
}

fn stopped() -> Error {
    Error::PeerPull("the caller stopped waiting".into())
}

/// Find what the delivery of `image` publishes from: the content, if Docker holds it for
/// `platform` already, or a new staging name to pull it to. A tag is first resolved to
/// the digest the source serves it as.
async fn prepare(
    docker: &LocalDocker,
    image: &str,
    requested: Option<&str>,
    source: ImageIngestDestination,
    platform: &str,
) -> Result<Prepared, Error> {
    let containerd = docker.uses_containerd_store().await?;
    if requested.is_some() && !containerd {
        return Err(Error::UnsupportedImageStore);
    }
    let proxy = ImageProxy::open(source).await?;
    let digest = match requested {
        Some(digest) => digest.to_owned(),
        None => resolve(SocketAddr::from((Ipv4Addr::LOCALHOST, proxy.port)), image).await?,
    };
    // Only the containerd store names an image by the digest it was pulled by.
    if containerd && holds(&docker.peer_pulls.cli, &digest, platform).await? {
        return Ok(Prepared::Held(digest));
    }
    Ok(Prepared::Staged(Staging::new(proxy, &digest)))
}

/// The digest the image server at `server` serves tag `image` as, asked as Docker would.
async fn resolve(server: SocketAddr, image: &str) -> Result<String, Error> {
    const MANIFESTS: &str = "application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.v2+json";
    let reference = image_server_reference(server, image).ok_or_else(|| {
        Error::PeerPull(format!(
            "{image} is a tag from a registry with a port, which a Machine can't serve by name; pin it by digest"
        ))
    })?;
    let path = reference
        .strip_prefix(&format!("{server}/"))
        .unwrap_or(&reference);
    let (repository, tag) = match path.rsplit_once(':') {
        Some((repository, tag)) if !tag.contains('/') => (repository, tag),
        _ => (path, "latest"),
    };
    let failed = |error: reqwest::Error| {
        Error::PeerPull(format!(
            "could not ask the source Machine for {image}'s digest: {}",
            ployz_core::error_chain::inline(&error.without_url())
        ))
    };
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .map_err(failed)?
        .head(format!("http://{server}/v2/{repository}/manifests/{tag}"))
        .header(reqwest::header::ACCEPT, MANIFESTS)
        .send()
        .await
        .map_err(failed)?;
    if !response.status().is_success() {
        return Err(Error::PeerPull(format!(
            "the source Machine has no {image}: {}",
            response.status()
        )));
    }
    response
        .headers()
        .get("Docker-Content-Digest")
        .and_then(|digest| digest.to_str().ok())
        .and_then(|digest| ployz_core::ImageDigest::parse(digest).ok())
        .map(|digest| digest.as_str().to_owned())
        .ok_or_else(|| Error::PeerPull(format!("the source Machine named no digest for {image}")))
}

/// Whether Docker holds `digest` as an image's ID, with `platform`'s content. A copy it
/// holds without that platform's content still needs the pull.
async fn holds(docker: &std::path::Path, digest: &str, platform: &str) -> Result<bool, Error> {
    let held = docker_cli(
        docker,
        &[
            "image",
            "inspect",
            "--platform",
            platform,
            digest,
            "--format",
            "{{.Id}}",
        ],
    )
    .await;
    match held {
        Ok(_) => Ok(true),
        Err(error)
            if no_such_image(&error)
                || docker_said(&error, "does not provide the specified platform") =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

/// Publish `prepared` as `tag`, pulling it first when staged, then hand the staging name,
/// if any, to a cleanup this does not wait for, returned so a test can.
async fn pull_and_tag(
    pulls: &Arc<PeerPulls>,
    image: &str,
    prepared: Prepared,
    tag: Option<&str>,
    platform: &str,
    gone: &CancellationToken,
) -> (Result<(), Error>, Option<JoinHandle<()>>) {
    let (docker, from) = (pulls.cli.as_path(), prepared.reference());
    let staged = matches!(prepared, Prepared::Staged(_));
    let tag = tag.unwrap_or(image);
    let verified = async {
        if staged {
            docker_cli(docker, &["pull", "--platform", platform, from]).await?;
            if let Some(digest) = ployz_core::ImageRef::new(image).digest
                && descriptor_digest(docker, from).await?.as_deref() != Some(digest)
            {
                return Err(Error::PeerPull(format!(
                    "the image store did not retain {image}"
                )));
            }
        }
        if let Some(digest) = ployz_core::retained_digest(tag) {
            retention_free(docker, from, tag, &digest).await?;
        }
        Ok(())
    };
    // A caller gone stops the transfer here, killing the Docker CLI it runs, so Docker
    // drops the pull and the admission turn it holds frees for the next Deployment.
    // Verify before publishing the requested or retention tag: a failed attempt must
    // not leave a tag or remove one from an earlier delivery.
    let result = match gone.run_until_cancelled(verified).await {
        // The publish point. Once begun it runs to its end, so a caller that stopped
        // waiting finds the tag either unwritten or written, and told which.
        Some(Ok(())) if !gone.is_cancelled() => docker_cli(docker, &["tag", "--", from, tag])
            .await
            .map(drop),
        Some(Err(error)) => Err(error),
        Some(Ok(())) | None => Err(stopped()),
    };
    let cleanup = match prepared {
        Prepared::Staged(staging) => Some(pulls.clean_up(staging.name)),
        Prepared::Held(_) => None,
    };
    (result, cleanup)
}

/// Remove staging name `pulled` if Docker holds it under that very name, so removing
/// it removes only it. A pull that failed or was stopped may never have written it,
/// and Docker resolves a missing digest name to an unnamed image of that digest, such
/// as the source's own copy that another Deployment is still pulling, which `image rm`
/// would then delete. The name is unique to its delivery and only that delivery's
/// cleanup removes it, so a name found here is still there for `image rm`. Only
/// Docker's "No such image" counts as absent: any other failure leaves the name for a
/// later try.
async fn remove_staging(docker: &std::path::Path, pulled: &str) -> Result<(), Error> {
    let names = docker_cli(docker, &["image", "inspect", pulled, "--format", NAMES]).await;
    match names {
        Ok(names) if names.lines().any(|name| name == pulled) => {}
        Ok(_) => return Ok(()),
        Err(error) if no_such_image(&error) => return Ok(()),
        Err(error) => return Err(error),
    }
    match docker_cli(docker, &["image", "rm", pulled]).await {
        Err(error) if !no_such_image(&error) => Err(error),
        _ => Ok(()),
    }
}

/// Whether Docker answered that the image it was asked about does not exist.
fn no_such_image(error: &Error) -> bool {
    docker_said(error, "No such image")
}

/// Whether the Docker CLI failed printing `text`.
fn docker_said(error: &Error, text: &str) -> bool {
    matches!(error, Error::PeerPull(printed) if printed.contains(text))
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
        Err(error) if no_such_image(&error) => Ok(()),
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

    fn source() -> ImageIngestDestination {
        ImageIngestDestination {
            management_address: ployz_core::ManagementAddress("fdcc::7".parse().unwrap()),
            port: UNREGISTRY_PORT,
        }
    }

    const HEX: &str = "1111111111111111111111111111111111111111111111111111111111111111";

    /// A staging name to pull `image` to, by its digest or by `sha256:1…1`, and that name.
    async fn staging(image: &str) -> (Prepared, String) {
        let digest = ployz_core::ImageRef::new(image)
            .digest
            .map_or_else(|| format!("sha256:{HEX}"), str::to_owned);
        let staging = Staging::new(ImageProxy::open(source()).await.unwrap(), &digest);
        let name = staging.name.clone();
        (Prepared::Staged(staging), name)
    }

    /// Wait for `cleanup`, if the delivery left one.
    async fn settle(cleanup: Option<JoinHandle<()>>) {
        if let Some(cleanup) = cleanup {
            cleanup.await.unwrap();
        }
    }

    #[tokio::test]
    async fn invalid_digest_reference_is_rejected_before_any_docker_work() {
        for image in ["api@sha256:invalid", "api@sha512:invalid"] {
            let pull = PeerImagePull::Reference {
                image: image.into(),
            };
            let (runtime, _fake) = crate::docker::test_support::fake_runtime().await;
            let error = pull_from_ingest(
                &runtime.local_docker(),
                &pull,
                source(),
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
        let pull = PeerImagePull::Reference {
            image: format!("api@sha256:{}", "a".repeat(64)),
        };
        let (runtime, fake) = crate::docker::test_support::fake_runtime().await;

        let error = pull_from_ingest(
            &runtime.local_docker(),
            &pull,
            source(),
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
        use std::fs;

        let root = std::env::temp_dir().join(format!(
            "ployz-peer-pull-{}",
            ployz_core::MachineId::random()
        ));
        fs::create_dir(&root).unwrap();
        let docker = root.join("docker");
        crate::docker::test_support::write_script(
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
        );
        let pulls = Arc::new(PeerPulls::new(docker));
        let digest = format!("sha256:{}", "1".repeat(64));
        let image = format!("example.test/api@{digest}");
        let retained = image.replace("@sha256:", ":ployz-sha256-");
        fs::write(
            root.join("descriptor"),
            serde_json::json!({"digest": digest}).to_string(),
        )
        .unwrap();
        for tag in [&retained, "example.test/api:latest"] {
            for mode in ["inspect", "json", "digest", "tag", "pull", "success"] {
                fs::write(root.join("mode"), mode).unwrap();
                fs::write(root.join("calls"), "").unwrap();
                let (staging, pulled) = staging(&image).await;
                let (result, cleanup) = pull_and_tag(
                    &pulls,
                    &image,
                    staging,
                    Some(tag),
                    "linux/arm64",
                    &CancellationToken::new(),
                )
                .await;
                settle(cleanup).await;
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
        use std::fs;

        let root = std::env::temp_dir().join(format!(
            "ployz-retention-{}",
            ployz_core::MachineId::random()
        ));
        fs::create_dir(&root).unwrap();
        let docker = root.join("docker");
        crate::docker::test_support::write_script(
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
        );
        let pulls = Arc::new(PeerPulls::new(docker));
        let descriptor = |hex: char| {
            serde_json::json!({"digest": format!("sha256:{}", hex.to_string().repeat(64))})
                .to_string()
        };
        let image = format!("app@sha256:{}", "1".repeat(64));
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
            let (staging, pulled) = staging(&image).await;
            let (result, cleanup) = pull_and_tag(
                &pulls,
                &image,
                staging,
                Some(&name),
                "linux/amd64",
                &CancellationToken::new(),
            )
            .await;
            settle(cleanup).await;
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
        fs::write(root.join("name"), "free").unwrap();
        let (staging, _) = staging(ordinary).await;
        let (result, cleanup) = pull_and_tag(
            &pulls,
            ordinary,
            staging,
            None,
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await;
        settle(cleanup).await;
        result.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// A Docker holding content `sha256:1…1` under each name in `names`, and as an unnamed
    /// image while `unnamed` exists. As Docker does, it answers a missing digest name with
    /// that unnamed image, so `image rm` of a staging name a pull never wrote deletes it,
    /// and a pull or tag of that content takes the unnamed image over into its name.
    /// `pull` acts as `mode` says: it `fail`s, `hang`s, or writes the staging name, and
    /// then the content check stalls (`stall-verify`), or the tag until `release` exists
    /// (`stall-tag`). A stall or hang names its pid in the `started` FIFO first. While
    /// `unsure` exists, the names query fails as Docker does when it can't answer.
    fn docker_with_unnamed_copy(root: &std::path::Path) -> PathBuf {
        use std::fs;

        fs::create_dir(root).unwrap();
        fs::write(root.join("names"), "").unwrap();
        fs::write(root.join("unnamed"), "").unwrap();
        assert!(
            std::process::Command::new("mkfifo")
                .arg(root.join("started"))
                .status()
                .unwrap()
                .success()
        );
        let docker = root.join("docker");
        crate::docker::test_support::write_script(
            &docker,
            &format!(
                r#"#!/bin/sh
cd "$(dirname "$0")"
printf '%s\n' "$*" >> calls
mode=$(cat mode)
held() {{ [ -e unnamed ] || grep -q "@sha256:{hex}$" names; }}
if [ "$1 $2 $3" = 'image inspect --platform' ]; then
  if [ "$5" = sha256:{hex} ] && held; then echo "$5"; exit 0; fi
  echo "No such image: $5" >&2; exit 1
fi
case "$1 $2 $5" in
  'image inspect {{{{json'*)
    if [ "$mode" = stall-verify ]; then echo $$ > started; exec sleep 600; fi
    if grep -qxF "$3" names || {{ [ "$3" = sha256:{hex} ] && held; }}; then
      echo '{{"digest":"sha256:{hex}"}}'
    else echo "No such image: $3" >&2; exit 1; fi ;;
  'image inspect '*)
    if [ -e unsure ]; then echo "Cannot connect to the Docker daemon" >&2; exit 1
    elif [ "$3" = sha256:{hex} ]; then cat names
    elif grep -qxF "$3" names; then echo "$3"
    elif [ -e unnamed ]; then :
    else echo "No such image: $3" >&2; exit 1; fi ;;
  'image rm '*)
    if grep -qxF "$3" names; then grep -vxF "$3" names > names.new; mv names.new names
    elif [ -e unnamed ]; then rm unnamed; echo Deleted
    else echo "No such image: $3" >&2; exit 1; fi ;;
  'pull '*)
    case "$mode" in
      fail) echo pull-failed >&2; exit 1 ;;
      hang) echo $$ > started; exec sleep 600 ;;
      *) echo "$4" >> names; rm -f unnamed ;;
    esac ;;
  'image ls'*) if [ -s names ] || [ -e unnamed ]; then echo sha256:{hex}; fi ;;
  'tag '*)
    if [ "$mode" = stall-tag ]; then
      echo $$ > started
      while [ ! -e release ]; do sleep 0.01; done
    fi
    case "$3" in sha256:*) rm -f unnamed ;; esac
    echo "$4" >> names ;;
esac
"#,
                hex = "1".repeat(64)
            ),
        );
        docker
    }

    /// Wait for the fake Docker to stall in `root`, then cancel `gone`, answering the
    /// stalled process's pid.
    async fn cancel_once_started(root: &std::path::Path, gone: &CancellationToken) -> String {
        use tokio::io::AsyncBufReadExt as _;

        // Opened for writing too, so the FIFO shows no end before the stall writes it.
        let started = tokio::net::unix::pipe::OpenOptions::new()
            .read_write(true)
            .open_receiver(root.join("started"))
            .unwrap();
        let mut pid = String::new();
        tokio::io::BufReader::new(started)
            .read_line(&mut pid)
            .await
            .unwrap();
        gone.cancel();
        pid.trim().to_owned()
    }

    fn names(root: &std::path::Path) -> String {
        std::fs::read_to_string(root.join("names")).unwrap()
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
        let pulls = Arc::new(PeerPulls::new(docker_with_unnamed_copy(&root)));
        std::fs::write(root.join("mode"), "fail").unwrap();
        let image = format!("app@sha256:{}", "1".repeat(64));
        let (staging, _) = staging(&image).await;

        let (result, cleanup) = pull_and_tag(
            &pulls,
            &image,
            staging,
            None,
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await;
        settle(cleanup).await;

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
        let pulls = Arc::new(PeerPulls::new(docker_with_unnamed_copy(&root)));
        fs::write(root.join("mode"), "hang").unwrap();
        let image = format!("app@sha256:{}", "1".repeat(64));
        let (staging, _) = staging(&image).await;
        let gone = CancellationToken::new();

        let pull = pull_and_tag(&pulls, &image, staging, None, "linux/amd64", &gone);
        let ((result, cleanup), pid) =
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                tokio::join!(pull, cancel_once_started(&root, &gone))
            })
            .await
            .expect("a pull whose caller is gone must end");
        settle(cleanup).await;

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

    /// A caller that stops waiting once the pull wrote its staging name leaves no name
    /// behind: the cleanup removes the name it wrote, and only that name. A delivery
    /// stages only content Docker did not hold, so here it holds none.
    #[tokio::test]
    async fn a_staging_name_written_before_the_caller_left_is_removed() {
        let root = std::env::temp_dir().join(format!(
            "ployz-written-pull-{}",
            ployz_core::MachineId::random()
        ));
        let pulls = Arc::new(PeerPulls::new(docker_with_unnamed_copy(&root)));
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("names"), "127.0.0.1:5000/app:v1\n").unwrap();
        std::fs::write(root.join("mode"), "stall-verify").unwrap();
        let image = format!("app@sha256:{}", "1".repeat(64));
        let (staging, pulled) = staging(&image).await;
        let gone = CancellationToken::new();

        let pull = pull_and_tag(&pulls, &image, staging, None, "linux/amd64", &gone);
        let ((result, cleanup), _) =
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                tokio::join!(pull, async {
                    let pid = cancel_once_started(&root, &gone).await;
                    // The staging name exists when the caller leaves.
                    assert!(names(&root).lines().any(|name| name == pulled));
                    // The staging name exists when the caller leaves.
                    pid
                })
            })
            .await
            .expect("a pull whose caller is gone must end");
        settle(cleanup).await;

        assert!(result.is_err(), "{result:?}");
        assert_eq!(
            names(&root),
            "127.0.0.1:5000/app:v1\n",
            "the staging name is left"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The tag is where a delivery publishes. A caller that leaves once it began finds it
    /// written and is told so, never a tag written under a reported failure.
    #[tokio::test]
    async fn a_tag_begun_before_the_caller_left_is_written_and_reported() {
        let root = std::env::temp_dir().join(format!(
            "ployz-tagging-pull-{}",
            ployz_core::MachineId::random()
        ));
        let pulls = Arc::new(PeerPulls::new(docker_with_unnamed_copy(&root)));
        std::fs::write(root.join("mode"), "stall-tag").unwrap();
        let image = format!("app@sha256:{}", "1".repeat(64));
        let retained = format!("app:ployz-sha256-{}", "1".repeat(64));
        let (staging, pulled) = staging(&image).await;
        let gone = CancellationToken::new();

        let pull = pull_and_tag(
            &pulls,
            &image,
            staging,
            Some(&retained),
            "linux/amd64",
            &gone,
        );
        let ((result, cleanup), ()) =
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                tokio::join!(pull, async {
                    cancel_once_started(&root, &gone).await;
                    // Let the pull see its caller gone before the tag may finish.
                    tokio::task::yield_now().await;
                    std::fs::write(root.join("release"), "").unwrap();
                })
            })
            .await
            .expect("a tag must finish once released");
        settle(cleanup).await;

        result.unwrap();
        let names = names(&root);
        assert!(names.lines().any(|name| name == retained), "{names}");
        assert!(!names.lines().any(|name| name == pulled), "{names}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A staging name Docker could not say it holds is neither taken as gone nor removed
    /// blind: it is kept, and the next delivery removes it.
    #[tokio::test]
    async fn a_staging_name_docker_could_not_check_is_removed_by_the_next_delivery() {
        let root = std::env::temp_dir().join(format!(
            "ployz-unsure-pull-{}",
            ployz_core::MachineId::random()
        ));
        let pulls = Arc::new(PeerPulls::new(docker_with_unnamed_copy(&root)));
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("mode"), "deliver").unwrap();
        std::fs::write(root.join("unsure"), "").unwrap();
        let image = format!("app@sha256:{}", "1".repeat(64));
        let retained = format!("app:ployz-sha256-{}", "1".repeat(64));
        let (staging, pulled) = staging(&image).await;

        let (result, cleanup) = pull_and_tag(
            &pulls,
            &image,
            staging,
            Some(&retained),
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await;
        settle(cleanup).await;
        result.unwrap();
        assert!(names(&root).lines().any(|name| name == pulled));

        std::fs::remove_file(root.join("unsure")).unwrap();
        for retry in pulls.retry_leftovers() {
            retry.await.unwrap();
        }

        let names = names(&root);
        assert!(!names.lines().any(|name| name == pulled), "{names}");
        assert!(names.lines().any(|name| name == retained), "{names}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A delivery through `deliver` on a Docker run by `cli`, whose store is containerd's.
    async fn delivery_docker(cli: PathBuf) -> LocalDocker {
        let (runtime, _fake) = crate::docker::test_support::fake_runtime_with(
            crate::docker::test_support::FakeDocker {
                containerd_store: true,
                docker_cli: Some(cli),
                ..Default::default()
            },
        )
        .await;
        runtime.local_docker()
    }

    /// A delivery stages content Docker did not hold under a name no one else writes. A
    /// user's own image named on the loopback port the pull happened to get is not that
    /// name, so a failed pull leaves it alone.
    #[tokio::test]
    async fn a_user_image_on_the_pulls_port_survives_a_failed_pull() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        let root = std::env::temp_dir().join(format!(
            "ployz-user-image-{}",
            ployz_core::MachineId::random()
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("user"), "").unwrap();
        let cli = root.join("docker");
        // Docker holds the user's `127.0.0.1:<any port>/app:v1`, and the pull fails.
        crate::docker::test_support::write_script(
            &cli,
            r#"#!/bin/sh
cd "$(dirname "$0")"
printf '%s\n' "$*" >> calls
user() { case "$1" in 127.0.0.1:*/app:v1) [ -e user ] ;; *) false ;; esac; }
case "$1 $2" in
  'image inspect')
    if [ "$3" = --platform ]; then echo "No such image: $5" >&2; exit 1; fi
    if user "$3"; then echo "$3"; else echo "No such image: $3" >&2; exit 1; fi ;;
  'image rm')
    if user "$3"; then rm user; else echo "No such image: $3" >&2; exit 1; fi ;;
  'pull '*) echo pull-failed >&2; exit 1 ;;
esac
"#,
        );
        // The source answers the tag's digest, as the Machine image server does.
        let registry = tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let source = ImageIngestDestination {
            management_address: ployz_core::ManagementAddress(std::net::Ipv6Addr::LOCALHOST),
            port: registry.local_addr().unwrap().port(),
        };
        let asked = tokio::spawn(async move {
            let (mut connection, _) = registry.accept().await.unwrap();
            let mut request = vec![0; 4096];
            let read = connection.read(&mut request).await.unwrap();
            connection
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nDocker-Content-Digest: sha256:{HEX}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            request.truncate(read);
            String::from_utf8_lossy(&request).into_owned()
        });
        let docker = delivery_docker(cli).await;
        let pull = PeerImagePull::Reference {
            image: "app:v1".into(),
        };

        let (result, cleanup) = deliver(
            &docker,
            &pull,
            source,
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await;
        settle(cleanup).await;

        assert!(result.is_err(), "{result:?}");
        let asked = asked.await.unwrap();
        assert!(asked.starts_with("HEAD /v2/app/manifests/v1 "), "{asked}");
        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        assert!(
            calls.contains(&format!("@sha256:{HEX}")) && calls.contains("/ployz-staging/"),
            "{calls}"
        );
        assert!(
            root.join("user").exists(),
            "removed the user's image: {calls}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Content Docker holds already has nothing to transfer: the delivery names it by its
    /// ID, with no pull and no staging name, and the unnamed copy it was is kept.
    #[tokio::test]
    async fn a_held_digest_is_tagged_without_a_pull() {
        let root = std::env::temp_dir().join(format!(
            "ployz-held-pull-{}",
            ployz_core::MachineId::random()
        ));
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::write(root.join("mode"), "deliver").unwrap();
        let pull = PeerImagePull::Reference {
            image: format!("app@sha256:{HEX}"),
        };

        let (result, cleanup) = deliver(
            &docker,
            &pull,
            source(),
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await;
        settle(cleanup).await;

        result.unwrap();
        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        assert!(
            !calls.lines().any(|line| line.starts_with("pull ")),
            "{calls}"
        );
        assert!(
            calls.contains(&format!("tag -- sha256:{HEX} app:ployz-sha256-{HEX}")),
            "{calls}"
        );
        assert_eq!(names(&root), format!("app:ployz-sha256-{HEX}\n"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A Machine may hold a digest unnamed, as a build's output or a source's copy that
    /// other Deployments pull. A delivery of that digest to it, cancelled before its tag,
    /// leaves the copy: a pull would have taken it over into a staging name, and the
    /// cleanup would have deleted it with that name.
    #[tokio::test]
    async fn an_unnamed_copy_survives_a_cancelled_delivery_to_its_machine() {
        let root = std::env::temp_dir().join(format!(
            "ployz-unnamed-held-{}",
            ployz_core::MachineId::random()
        ));
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::write(root.join("mode"), "stall-verify").unwrap();
        let pull = PeerImagePull::Reference {
            image: format!("app@sha256:{HEX}"),
        };
        let gone = CancellationToken::new();

        let delivery = deliver(&docker, &pull, source(), "linux/amd64", &gone);
        let ((result, cleanup), _) =
            tokio::time::timeout(std::time::Duration::from_secs(30), async {
                tokio::join!(delivery, cancel_once_started(&root, &gone))
            })
            .await
            .expect("a delivery whose caller is gone must end");
        settle(cleanup).await;

        assert!(result.is_err(), "{result:?}");
        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        assert!(root.join("unnamed").exists(), "deleted the copy: {calls}");
        assert_eq!(names(&root), "", "{calls}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A restart removes the staging names an earlier run left, which no delivery will,
    /// and nothing else: not this run's, not a user's loopback-registry image, and not a
    /// `ployz-staging` path on another host.
    #[tokio::test]
    async fn a_restart_sweeps_only_the_staging_names_an_earlier_run_left() {
        let root = std::env::temp_dir().join(format!(
            "ployz-staging-sweep-{}",
            ployz_core::MachineId::random()
        ));
        let pulls = Arc::new(PeerPulls::new(docker_with_unnamed_copy(&root)));
        std::fs::remove_file(root.join("unnamed")).unwrap();
        let earlier = format!(
            "127.0.0.1:41000/ployz-staging/{}/3@sha256:{HEX}",
            "e".repeat(32)
        );
        let kept = [
            format!("127.0.0.1:41001/ployz-staging/{}/4@sha256:{HEX}", boot()),
            "127.0.0.1:5000/app:v1".to_owned(),
            format!("127.0.0.1:5000/app@sha256:{HEX}"),
            format!(
                "example.test/ployz-staging/{}/5@sha256:{HEX}",
                "e".repeat(32)
            ),
        ];
        std::fs::write(
            root.join("names"),
            format!("{earlier}\n{}\n", kept.join("\n")),
        )
        .unwrap();

        for removal in pulls.sweep().await {
            removal.await.unwrap();
        }

        assert_eq!(names(&root), format!("{}\n", kept.join("\n")));
        std::fs::remove_dir_all(root).unwrap();
    }
}
