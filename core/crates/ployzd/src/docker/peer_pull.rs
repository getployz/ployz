//! Pull an image from another Machine's ingest TCP destination into local Docker.

use std::{
    collections::BTreeSet,
    io::Write as _,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use ployz_core::{
    ImageDigestReference, ImageIngestDestination, PeerImagePull, image_server_reference,
    retained_digest, retention_tag,
};
use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    process::Command,
    sync::watch,
};
use tokio_util::sync::CancellationToken;

use super::{Error, LocalDocker};

/// The repository every staging name sits under, behind the pull's loopback host.
const STAGING: &str = "ployz-staging";

/// How long one sweep pass may run. A pass past it ends, its Docker commands killed, and
/// the next pass starts over: a sweep is safe to repeat, so this bounds only how long a
/// hung Docker can hold the sweep back.
const PASS_DEADLINE: Duration = Duration::from_secs(30);

/// This run of the daemon's part of each staging name: its pid and start time, as
/// fixed-width lowercase hex.
fn boot() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(|| {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{:08x}{started:032x}", std::process::id())
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The Docker CLI, aimed at the Engine the API client talks to. The host is named on
/// every command, so a Docker context or `DOCKER_HOST` the CLI would otherwise follow
/// never sends a pull, tag or removal to another Engine.
pub(crate) struct DockerCli {
    path: PathBuf,
    host: String,
}

impl DockerCli {
    async fn run(&self, args: &[&str]) -> Result<String, Error> {
        let output = Command::new(&self.path)
            .arg("--host")
            .arg(&self.host)
            .args(args)
            .env_remove("DOCKER_CONTEXT")
            .env_remove("DOCKER_HOST")
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
}

/// Peer pulls on one Docker: the CLI they run, the ledger of the staging names they
/// wrote, the names deliveries in flight hold, the lock that keeps retention names in
/// place, and the sweep that removes staging names beside them.
pub(crate) struct PeerPulls {
    cli: DockerCli,
    ledger: Option<PathBuf>,
    ledger_writes: Mutex<()>,
    in_flight: Mutex<BTreeSet<String>>,
    retention: tokio::sync::Mutex<()>,
    sweep: watch::Sender<Sweep>,
    deadline: Duration,
}

/// Where the one sweep is: idle, running, or running and asked to go again.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sweep {
    Idle,
    Running,
    Again,
}

impl PeerPulls {
    /// Peer pulls through the Docker CLI at `cli`, aimed at `host`, recording their
    /// staging names in `ledger`. Without a ledger, a delivery is refused before it pulls.
    pub(crate) fn new(cli: PathBuf, host: String, ledger: Option<PathBuf>) -> Self {
        Self {
            cli: DockerCli { path: cli, host },
            ledger,
            ledger_writes: Mutex::default(),
            in_flight: Mutex::default(),
            retention: tokio::sync::Mutex::default(),
            sweep: watch::Sender::new(Sweep::Idle),
            deadline: PASS_DEADLINE,
        }
    }

    pub(crate) fn cli(&self) -> &std::path::Path {
        &self.cli.path
    }

    pub(crate) fn host(&self) -> &str {
        &self.cli.host
    }

    /// The lock held by every write or removal of a retention name Ployz makes, and by
    /// the sweep from reading an image's names to removing a staging name beside its
    /// retention name. Held, the retention name stays where the sweep saw it.
    pub(super) fn retention(&self) -> &tokio::sync::Mutex<()> {
        &self.retention
    }

    #[cfg(test)]
    fn with_deadline(self, deadline: Duration) -> Self {
        Self { deadline, ..self }
    }

    /// Sweep staging names off the caller's turn. One sweep runs at a time: asked while
    /// one runs, it goes again once done, so a name it missed, or a Docker that could not
    /// answer, is caught by the next pass.
    fn sweep(self: &Arc<Self>) {
        let mut start = false;
        self.sweep.send_modify(|sweep| {
            start = *sweep == Sweep::Idle;
            *sweep = if start { Sweep::Running } else { Sweep::Again };
        });
        if !start {
            return;
        }
        let pulls = Arc::clone(self);
        tokio::spawn(async move {
            loop {
                if tokio::time::timeout(pulls.deadline, pulls.sweep_once())
                    .await
                    .is_err()
                {
                    tracing::warn!(
                        "a sweep of peer pulls' staging names ran past its deadline; the next pass starts over"
                    );
                }
                let mut done = false;
                pulls.sweep.send_modify(|sweep| {
                    done = *sweep == Sweep::Running;
                    *sweep = if done { Sweep::Idle } else { Sweep::Running };
                });
                if done {
                    break;
                }
            }
        });
    }

    /// Wait until no sweep runs or is asked for.
    #[cfg(test)]
    async fn swept(&self) {
        let mut sweep = self.sweep.subscribe();
        sweep.wait_for(|sweep| *sweep == Sweep::Idle).await.unwrap();
    }

    /// Settle each staging name the ledger holds and no delivery in flight does. Only
    /// names Ployz wrote are touched: the sweep reads the ledger, never Docker's listing.
    async fn sweep_once(&self) {
        let failed = |error: &Error| {
            tracing::warn!(
                error = %ployz_core::error_chain::inline(error),
                "could not sweep peer pulls' staging names; the next pass tries again"
            );
        };
        let entries = match self.entries() {
            Ok(entries) => entries,
            Err(error) => return failed(&error),
        };
        let mut settled = BTreeSet::new();
        for name in entries {
            if lock(&self.in_flight).contains(&name) {
                continue;
            }
            match self.settle(&name).await {
                Ok(true) => {
                    settled.insert(name);
                }
                Ok(false) => {}
                Err(error) => failed(&error),
            }
        }
        if !settled.is_empty()
            && let Err(error) = self.prune(&settled)
        {
            failed(&error);
        }
    }

    /// Remove staging name `name` where its image also holds the retention name of the
    /// digest it was pulled by, so the removal only untags. Ployz never removes an
    /// image's last name: Docker hands content it holds unnamed to the pull that asks for
    /// its digest, so a staging name may be all that names content Ployz never brought.
    /// A user's tag doesn't count, since its owner may move it at any moment, and a
    /// retention name stays put while the retention lock is held. Answers whether the
    /// ledger may drop `name`: removed, or never written by a run whose proxy has closed.
    async fn settle(&self, name: &str) -> Result<bool, Error> {
        let _retention = self.retention.lock().await;
        let names = match self
            .cli
            .run(&["image", "inspect", name, "--format", NAMES])
            .await
        {
            Ok(names) => names,
            Err(error) if no_such_image(&error) => return Ok(earlier_run(name)),
            Err(error) => return Err(error),
        };
        if !names.lines().any(|held| held == name) {
            return Ok(earlier_run(name));
        }
        let digest = name.rsplit_once('@').map(|(_, digest)| digest);
        let beside = names
            .lines()
            .any(|held| retained_digest(held).is_some_and(|kept| Some(kept.as_str()) == digest));
        if !beside {
            return Ok(false);
        }
        match self.cli.run(&["image", "rm", name]).await {
            Err(error) if !no_such_image(&error) => Err(error),
            _ => Ok(true),
        }
    }

    /// Record `name` before a pull may write it, so a sweep finds it after a cancel, a
    /// late commit or a restart.
    fn record(&self, name: &str) -> Result<(), Error> {
        let ledger = self.ledger.as_ref().ok_or_else(|| {
            Error::PeerPull("this Docker keeps no ledger of staging names".into())
        })?;
        let _writes = lock(&self.ledger_writes);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(ledger)
            .map_err(ledger_failed)?;
        writeln!(file, "{name}").map_err(ledger_failed)?;
        file.sync_data().map_err(ledger_failed)
    }

    /// The staging names the ledger holds.
    fn entries(&self) -> Result<BTreeSet<String>, Error> {
        let Some(ledger) = &self.ledger else {
            return Ok(BTreeSet::new());
        };
        let _writes = lock(&self.ledger_writes);
        match std::fs::read_to_string(ledger) {
            Ok(text) => Ok(text
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(error) => Err(ledger_failed(error)),
        }
    }

    /// Drop `settled` from the ledger, keeping every other line, written whole and then
    /// moved into place.
    fn prune(&self, settled: &BTreeSet<String>) -> Result<(), Error> {
        let Some(ledger) = &self.ledger else {
            return Ok(());
        };
        let _writes = lock(&self.ledger_writes);
        let text = std::fs::read_to_string(ledger).map_err(ledger_failed)?;
        let kept = text
            .lines()
            .filter(|line| !line.is_empty() && !settled.contains(*line))
            .map(|line| format!("{line}\n"))
            .collect::<String>();
        let next = ledger.with_extension("next");
        let mut file = std::fs::File::create(&next).map_err(ledger_failed)?;
        file.write_all(kept.as_bytes()).map_err(ledger_failed)?;
        file.sync_data().map_err(ledger_failed)?;
        std::fs::rename(&next, ledger).map_err(ledger_failed)
    }
}

fn ledger_failed(error: std::io::Error) -> Error {
    Error::PeerPull(format!(
        "the ledger of staging names failed: {}",
        ployz_core::error_chain::inline(&error)
    ))
}

/// An image inspect format that prints every name the image holds, one a line.
const NAMES: &str =
    "{{range .RepoTags}}{{println .}}{{end}}{{range .RepoDigests}}{{println .}}{{end}}";

/// Whether staging name `name` was written by an earlier run of the daemon. That run's
/// loopback proxy closed with it, so a name it never committed never will be.
fn earlier_run(name: &str) -> bool {
    name.split_once(&format!("/{STAGING}/"))
        .and_then(|(_, rest)| rest.split_once('/'))
        .is_some_and(|(run, _)| run != boot())
}

impl LocalDocker {
    /// This Docker with each sweep pass of its peer pulls bounded by `deadline`.
    #[cfg(test)]
    pub(crate) fn with_sweep_deadline(self, deadline: Duration) -> Self {
        let pulls = &self.peer_pulls;
        let pulls = PeerPulls::new(
            pulls.cli.path.clone(),
            pulls.cli.host.clone(),
            pulls.ledger.clone(),
        )
        .with_deadline(deadline);
        Self {
            peer_pulls: Arc::new(pulls),
            ..self
        }
    }

    /// Sweep the staging names peer pulls left, in the background, as the daemon starts.
    pub(crate) fn sweep_staging(&self) {
        self.peer_pulls.sweep();
    }
}

/// A staging name, unique to one delivery, recorded in the ledger and held from sweeps
/// while it runs, and the loopback proxy it pulls through.
struct Staging {
    name: String,
    pulls: Arc<PeerPulls>,
    _proxy: ImageProxy,
}

impl Staging {
    fn new(pulls: &Arc<PeerPulls>, proxy: ImageProxy, digest: &str) -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "127.0.0.1:{}/{STAGING}/{}/{}@{digest}",
            proxy.port,
            boot(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        // Held from sweeps before the ledger names it, so no sweep sees it idle.
        lock(&pulls.in_flight).insert(name.clone());
        let staging = Self {
            name,
            pulls: Arc::clone(pulls),
            _proxy: proxy,
        };
        pulls.record(&staging.name)?;
        Ok(staging)
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        lock(&self.pulls.in_flight).remove(&self.name);
    }
}

/// Pull an image from a peer Machine's ingest TCP destination into local Docker.
///
/// Docker treats `127.0.0.0/8` as an insecure registry, so the pull is proxied
/// through localhost instead of asking dockerd to speak HTTP to the WireGuard IP.
/// `platform` makes Docker fetch that variant's manifest, configuration and
/// layers; a source holding only the index fails the pull. Every delivery pulls by
/// digest, and Docker fetches only what it lacks. An optional `tag` is published only
/// after the content's digest has been verified. Once `gone` fires, a delivery that
/// has not begun its tag stops and publishes nothing; one that has completes it.
/// Staging names are swept as a delivery starts and once it ends, off its turn.
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
    docker.peer_pulls.sweep();
    let delivered = deliver(docker, pull, source, platform, gone).await;
    docker.peer_pulls.sweep();
    delivered
}

async fn deliver(
    docker: &LocalDocker,
    pull: &PeerImagePull,
    source: ImageIngestDestination,
    platform: &str,
    gone: &CancellationToken,
) -> Result<(), Error> {
    let image = pull.image();
    let requested = ployz_core::ImageRef::new(image).digest;
    if requested.is_some()
        && let Err(error) = ImageDigestReference::parse(image)
    {
        return Err(Error::PeerPull(ployz_core::error_chain::inline(&error)));
    }
    // Everything before the tag that waits on Docker or the source runs in the caller's
    // race, so a caller gone never leaves this work holding the turn.
    let staging = gone
        .run_until_cancelled(prepare(docker, image, requested, source))
        .await
        .ok_or_else(stopped)??;
    let retained = requested.and_then(|_| retention_tag(image));
    pull_and_tag(
        &docker.peer_pulls,
        image,
        staging,
        pull.tag().or(retained.as_deref()),
        platform,
        gone,
    )
    .await
}

fn stopped() -> Error {
    Error::PeerPull("the caller stopped waiting".into())
}

/// The staging name the delivery of `image` pulls to, recorded in the ledger. A tag is
/// first resolved to the digest the source serves it as.
async fn prepare(
    docker: &LocalDocker,
    image: &str,
    requested: Option<&str>,
    source: ImageIngestDestination,
) -> Result<Staging, Error> {
    if requested.is_some() && !docker.uses_containerd_store().await? {
        return Err(Error::UnsupportedImageStore);
    }
    let proxy = ImageProxy::open(source).await?;
    let digest = match requested {
        Some(digest) => digest.to_owned(),
        None => resolve(SocketAddr::from((Ipv4Addr::LOCALHOST, proxy.port)), image).await?,
    };
    Staging::new(&docker.peer_pulls, proxy, &digest)
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

/// Pull `staging` and publish it as `tag`. The staging name is left for the sweep.
async fn pull_and_tag(
    pulls: &PeerPulls,
    image: &str,
    staging: Staging,
    tag: Option<&str>,
    platform: &str,
    gone: &CancellationToken,
) -> Result<(), Error> {
    let (docker, from) = (&pulls.cli, staging.name.as_str());
    let tag = tag.unwrap_or(image);
    let verified = async {
        docker.run(&["pull", "--platform", platform, from]).await?;
        if let Some(digest) = ployz_core::ImageRef::new(image).digest
            && descriptor_digest(docker, from).await?.as_deref() != Some(digest)
        {
            return Err(Error::PeerPull(format!(
                "the image store did not retain {image}"
            )));
        }
        if let Some(digest) = retained_digest(tag) {
            retention_free(docker, from, tag, &digest).await?;
        }
        Ok(pulls.retention.lock().await)
    };
    // A caller gone stops the transfer here, killing the Docker CLI it runs, so Docker
    // drops the pull and the admission turn it holds frees for the next Deployment.
    // Verify before publishing the requested or retention tag: a failed attempt must
    // not leave a tag or remove one from an earlier delivery.
    match gone.run_until_cancelled(verified).await {
        // The publish point, under the retention lock taken in the caller's race. Once
        // begun it runs to its end, so a caller that stopped waiting finds the tag
        // either unwritten or written, and told which.
        Some(Ok(_retention)) if !gone.is_cancelled() => {
            docker.run(&["tag", "--", from, tag]).await.map(drop)
        }
        Some(Err(error)) => Err(error),
        Some(Ok(_)) | None => Err(stopped()),
    }
}

/// Whether Docker answered that the image it was asked about does not exist.
fn no_such_image(error: &Error) -> bool {
    matches!(error, Error::PeerPull(printed) if printed.contains("No such image"))
}

/// Refuse to write retention name `tag` unless `pulled` is the content it names, and
/// the name is free or already names that content. Image Cleanup trusts these names, so
/// no write, whatever its mode, moves one onto other content.
async fn retention_free(
    docker: &DockerCli,
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
async fn descriptor_digest(docker: &DockerCli, reference: &str) -> Result<Option<String>, Error> {
    let descriptor = docker
        .run(&[
            "image",
            "inspect",
            reference,
            "--format",
            "{{json .Descriptor}}",
        ])
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
    async fn staging(pulls: &Arc<PeerPulls>, image: &str) -> (Staging, String) {
        let digest = ployz_core::ImageRef::new(image)
            .digest
            .map_or_else(|| format!("sha256:{HEX}"), str::to_owned);
        let staging =
            Staging::new(pulls, ImageProxy::open(source()).await.unwrap(), &digest).unwrap();
        let name = staging.name.clone();
        (staging, name)
    }

    /// Sweep `pulls` once more and wait for it.
    async fn sweep(pulls: &Arc<PeerPulls>) {
        pulls.sweep();
        pulls.swept().await;
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
    async fn failed_digest_delivery_publishes_no_unverified_content() {
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
[ "$1" = --host ] && shift 2
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
  *) if [ "$1" = "$mode" ]; then echo "$mode-failed" >&2; exit 1; fi ;;
esac
"#,
        );
        let pulls = pulls_in(&root, docker.clone());
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
                let (staging, pulled) = staging(&pulls, &image).await;
                let result = pull_and_tag(
                    &pulls,
                    &image,
                    staging,
                    Some(tag),
                    "linux/arm64",
                    &CancellationToken::new(),
                )
                .await;
                assert_eq!(result.is_ok(), mode == "success", "{mode}: {result:?}");
                if let Err(error) = result
                    && ["inspect", "tag", "pull"].contains(&mode)
                {
                    assert!(
                        error.to_string().contains(&format!("{mode}-failed")),
                        "{error}"
                    );
                }
                let calls = fs::read_to_string(root.join("calls")).unwrap();
                if mode == "success" {
                    assert!(calls.contains(&format!("tag -- {pulled} {tag}")), "{calls}");
                }
                // A delivery removes no name itself: the sweep does, where another stays.
                assert!(
                    !calls.lines().any(|line| line.starts_with("image rm")),
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
[ "$1" = --host ] && shift 2
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
        let pulls = pulls_in(&root, docker.clone());
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
            let (staging, pulled) = staging(&pulls, &image).await;
            let result = pull_and_tag(
                &pulls,
                &image,
                staging,
                Some(&name),
                "linux/amd64",
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
        fs::write(root.join("name"), "free").unwrap();
        let (staging, _) = staging(&pulls, ordinary).await;
        let result = pull_and_tag(
            &pulls,
            ordinary,
            staging,
            None,
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await;
        result.unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// A Docker holding content `sha256:1…1` under each name in `names`, and as an unnamed
    /// image while `unnamed` exists. As Docker does, it answers a missing digest name with
    /// that unnamed image, so `image rm` of a name it does not hold deletes it, and a pull
    /// or tag of that content takes the unnamed image over into its name. While `lands`
    /// exists, a build loads the content unnamed just as the pull starts. While
    /// `missing-layer` exists, the content lacks a layer: inspect still finds it, a pull
    /// fetches the layer, and a tag fails. `pull` acts as `mode` says: it `fail`s,
    /// `hang`s, or writes the staging name, and then the content check stalls
    /// (`stall-verify`), or the tag until `release` exists (`stall-tag`). A stall or hang
    /// names its pid in the `started` FIFO first. While `unsure` exists, the names query
    /// fails as Docker does when it can't answer, and `hang-once` makes the next one hang.
    /// The name `moves` holds is moved away just after the next names query answers.
    /// Every command must name its host first: one that doesn't goes to the Engine of the
    /// CLI's own context, recorded in `context`.
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
if [ "$1" != --host ]; then printf '%s\n' "$*" >> context; echo "context engine" >&2; exit 1; fi
printf '%s\n' "$2" >> hosts
shift 2
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
    if [ -e hang-once ]; then rm hang-once; exec sleep 600; fi
    if [ -e unsure ]; then echo "Cannot connect to the Docker daemon" >&2; exit 1
    elif [ "$3" = sha256:{hex} ]; then cat names
    elif grep -qxF "$3" names; then
      cat names
      if [ -e moves ]; then grep -vxF "$(cat moves)" names > names.new; mv names.new names; rm moves; fi
    elif [ -e unnamed ]; then :
    else echo "No such image: $3" >&2; exit 1; fi ;;
  'image rm '*)
    if grep -qxF "$3" names; then grep -vxF "$3" names > names.new; mv names.new names
    elif [ -e unnamed ]; then rm unnamed; echo Deleted
    else echo "No such image: $3" >&2; exit 1; fi ;;
  'image ls'*) if [ -s names ] || [ -e unnamed ]; then echo sha256:{hex}; fi ;;
  'pull '*)
    [ -e lands ] && : > unnamed
    rm -f missing-layer
    case "$mode" in
      fail) echo pull-failed >&2; exit 1 ;;
      hang) echo $$ > started; exec sleep 600 ;;
      *) echo "$4" >> names; rm -f unnamed ;;
    esac ;;
  'tag '*)
    if [ -e missing-layer ]; then echo "content missing a layer" >&2; exit 1; fi
    if [ "$mode" = stall-tag ]; then
      echo $$ > started
      while [ ! -e release ]; do sleep 0.01; done
    fi
    case "$3" in sha256:*) rm -f unnamed ;; esac
    grep -qxF "$4" names || echo "$4" >> names ;;
esac
"#,
                hex = "1".repeat(64)
            ),
        );
        docker
    }

    /// Peer pulls through the Docker CLI `cli`, with their ledger in `root`.
    fn pulls_in(root: &std::path::Path, cli: PathBuf) -> Arc<PeerPulls> {
        Arc::new(PeerPulls::new(
            cli,
            "unix:///run/test-engine.sock".into(),
            Some(root.join("staging-ledger")),
        ))
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

    fn ledger(root: &std::path::Path) -> String {
        std::fs::read_to_string(root.join("staging-ledger")).unwrap_or_default()
    }

    /// Whether the fake Docker in `root` still holds content `sha256:1…1`, named or not.
    fn content(root: &std::path::Path) -> bool {
        root.join("unnamed").exists() || !names(root).is_empty()
    }

    /// The retention name of `sha256:1…1` in repository `app`.
    fn retained() -> String {
        format!("app:ployz-sha256-{HEX}")
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
        let pulls = pulls_in(&root, docker_with_unnamed_copy(&root));
        std::fs::write(root.join("mode"), "fail").unwrap();
        let image = format!("app@sha256:{HEX}");
        let (staging, _) = staging(&pulls, &image).await;

        let result = pull_and_tag(
            &pulls,
            &image,
            staging,
            None,
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await;
        sweep(&pulls).await;

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
    /// Docker CLI is killed, and no tag is written. The unnamed copy of its digest that
    /// another pull reads is left alone.
    #[tokio::test]
    async fn a_pull_whose_caller_is_gone_stops() {
        use std::fs;

        let root = std::env::temp_dir().join(format!(
            "ployz-gone-pull-{}",
            ployz_core::MachineId::random()
        ));
        let pulls = pulls_in(&root, docker_with_unnamed_copy(&root));
        fs::write(root.join("mode"), "hang").unwrap();
        let image = format!("app@sha256:{HEX}");
        let (staging, _) = staging(&pulls, &image).await;
        let gone = CancellationToken::new();

        let pull = pull_and_tag(&pulls, &image, staging, None, "linux/amd64", &gone);
        let (result, pid) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(pull, cancel_once_started(&root, &gone))
        })
        .await
        .expect("a pull whose caller is gone must end");
        sweep(&pulls).await;

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

    /// A staging name a cancelled pull wrote is untagged beside the retention name of its
    /// digest, and kept beside a user's tag or alone. Here the user's tag moves away
    /// while the sweep runs, and the staging name is then all that keeps the content.
    #[tokio::test]
    async fn a_cancelled_pulls_staging_name_is_removed_only_beside_its_retention_name() {
        let user = "127.0.0.1:5000/app:v1";
        for (other, removed) in [
            (Some(retained()), true),
            (Some(user.to_owned()), false),
            (None, false),
        ] {
            let root = std::env::temp_dir().join(format!(
                "ployz-written-pull-{}",
                ployz_core::MachineId::random()
            ));
            let pulls = pulls_in(&root, docker_with_unnamed_copy(&root));
            std::fs::remove_file(root.join("unnamed")).unwrap();
            let other = other.map(|name| format!("{name}\n")).unwrap_or_default();
            std::fs::write(root.join("names"), &other).unwrap();
            std::fs::write(root.join("mode"), "stall-verify").unwrap();
            let image = format!("app@sha256:{HEX}");
            let (staging, pulled) = staging(&pulls, &image).await;
            let gone = CancellationToken::new();

            let pull = pull_and_tag(&pulls, &image, staging, None, "linux/amd64", &gone);
            let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                tokio::join!(pull, cancel_once_started(&root, &gone))
            })
            .await
            .expect("a pull whose caller is gone must end");
            if other.trim() == user {
                std::fs::write(root.join("moves"), user).unwrap();
            }
            sweep(&pulls).await;

            assert!(result.is_err(), "{result:?}");
            assert!(content(&root), "beside {other:?}: deleted the content");
            let expected = if removed {
                other.clone()
            } else if other.trim() == user {
                format!("{pulled}\n")
            } else {
                format!("{other}{pulled}\n")
            };
            assert_eq!(names(&root), expected, "beside {other:?}");
            assert_eq!(ledger(&root).is_empty(), removed, "beside {other:?}");
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// The tag is where a delivery publishes. A caller that leaves once it began finds it
    /// written and is told so, never a tag written under a reported failure.
    #[tokio::test]
    async fn a_tag_begun_before_the_caller_left_is_written_and_reported() {
        let root = std::env::temp_dir().join(format!(
            "ployz-tagging-pull-{}",
            ployz_core::MachineId::random()
        ));
        let pulls = pulls_in(&root, docker_with_unnamed_copy(&root));
        std::fs::write(root.join("mode"), "stall-tag").unwrap();
        let image = format!("app@sha256:{HEX}");
        let retained = retained();
        let (staging, pulled) = staging(&pulls, &image).await;
        let gone = CancellationToken::new();

        let pull = pull_and_tag(
            &pulls,
            &image,
            staging,
            Some(&retained),
            "linux/amd64",
            &gone,
        );
        let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(pull, async {
                cancel_once_started(&root, &gone).await;
                // Let the pull see its caller gone before the tag may finish.
                tokio::task::yield_now().await;
                std::fs::write(root.join("release"), "").unwrap();
            })
        })
        .await
        .expect("a tag must finish once released");
        sweep(&pulls).await;

        result.unwrap();
        let names = names(&root);
        assert!(names.lines().any(|name| name == retained), "{names}");
        assert!(!names.lines().any(|name| name == pulled), "{names}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A delivery through `pull_from_ingest` on a Docker run by `cli`, whose store is
    /// containerd's.
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

    /// Deliver `image` for `platform` through `docker`, then wait for the sweeps it asked
    /// for.
    async fn deliver_and_sweep(
        docker: &LocalDocker,
        image: &str,
        platform: &str,
        gone: &CancellationToken,
    ) -> Result<(), Error> {
        let pull = PeerImagePull::Reference {
            image: image.into(),
        };
        let result = pull_from_ingest(docker, &pull, source(), platform, gone).await;
        docker.peer_pulls.swept().await;
        result
    }

    /// An earlier run's staging name, beside the retention name, in the ledger as that run
    /// left it.
    fn earlier_staging(root: &std::path::Path) -> String {
        let earlier = format!(
            "127.0.0.1:41000/{STAGING}/{}/3@sha256:{HEX}",
            "e".repeat(40)
        );
        std::fs::write(root.join("names"), format!("{}\n{earlier}\n", retained())).unwrap();
        std::fs::write(root.join("staging-ledger"), format!("{earlier}\n")).unwrap();
        earlier
    }

    /// The sweep as the daemon starts may find Docker unable to answer. The next
    /// delivery's sweep tries again.
    #[tokio::test]
    async fn a_startup_sweep_docker_could_not_answer_is_retried_by_the_next_delivery() {
        let root = std::env::temp_dir().join(format!(
            "ployz-unsure-sweep-{}",
            ployz_core::MachineId::random()
        ));
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("mode"), "deliver").unwrap();
        let earlier = earlier_staging(&root);

        std::fs::write(root.join("unsure"), "").unwrap();
        docker.sweep_staging();
        docker.peer_pulls.swept().await;
        assert!(names(&root).contains(&earlier));
        std::fs::remove_file(root.join("unsure")).unwrap();
        deliver_and_sweep(
            &docker,
            "app@sha256:invalid",
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();

        assert_eq!(names(&root), format!("{}\n", retained()));
        assert_eq!(ledger(&root), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A sweep stuck on a hung Docker at start ends at its deadline, killing what it ran,
    /// and the next pass settles what the stuck one could not, with no restart.
    #[tokio::test]
    async fn a_sweep_hung_at_start_lets_the_next_delivery_settle_its_staging_name() {
        let root = std::env::temp_dir().join(format!(
            "ployz-hung-sweep-{}",
            ployz_core::MachineId::random()
        ));
        let docker = delivery_docker(docker_with_unnamed_copy(&root))
            .await
            .with_sweep_deadline(std::time::Duration::from_millis(300));
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("mode"), "deliver").unwrap();
        earlier_staging(&root);
        std::fs::write(root.join("hang-once"), "").unwrap();

        docker.sweep_staging();
        tokio::time::timeout(std::time::Duration::from_secs(20), async {
            deliver_and_sweep(
                &docker,
                &format!("app@sha256:{HEX}"),
                "linux/amd64",
                &CancellationToken::new(),
            )
            .await
        })
        .await
        .expect("a hung sweep must end at its deadline")
        .unwrap();

        assert!(!root.join("hang-once").exists());
        assert_eq!(names(&root), format!("{}\n", retained()));
        assert_eq!(ledger(&root), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Docker may still commit a cancelled pull's staging name after its delivery ended and
    /// swept. The next delivery's sweep catches it: untagged beside the retention name,
    /// kept alone.
    #[tokio::test]
    async fn a_staging_name_committed_after_its_delivery_is_swept_by_the_next() {
        for other in [Some(retained()), None] {
            let root = std::env::temp_dir().join(format!(
                "ployz-late-staging-{}",
                ployz_core::MachineId::random()
            ));
            let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
            std::fs::remove_file(root.join("unnamed")).unwrap();
            let other = other.map(|name| format!("{name}\n")).unwrap_or_default();
            std::fs::write(root.join("mode"), "hang").unwrap();
            let gone = CancellationToken::new();
            let image = format!("app@sha256:{HEX}");
            let delivery = deliver_and_sweep(&docker, &image, "linux/amd64", &gone);
            let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                tokio::join!(delivery, cancel_once_started(&root, &gone))
            })
            .await
            .expect("a delivery whose caller is gone must end");
            result.unwrap_err();

            let calls = std::fs::read_to_string(root.join("calls")).unwrap();
            let pulled = calls
                .lines()
                .find_map(|line| line.strip_prefix("pull --platform linux/amd64 "))
                .unwrap()
                .to_owned();
            std::fs::write(root.join("names"), format!("{other}{pulled}\n")).unwrap();
            deliver_and_sweep(
                &docker,
                "app@sha256:invalid",
                "linux/amd64",
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();

            let expected = if other.is_empty() {
                format!("{pulled}\n")
            } else {
                other
            };
            assert_eq!(names(&root), expected);
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// Docker hands content it holds unnamed to a pull that asks for its digest: a Free
    /// Build's output that lands just as the pull starts, or an index that holds only
    /// amd64 asked for arm64. A delivery cancelled after that leaves the content, under
    /// its staging name.
    #[tokio::test]
    async fn content_a_cancelled_pull_took_over_survives() {
        for (took_over, platform) in [("lands", "linux/amd64"), ("index", "linux/arm64")] {
            let root = std::env::temp_dir().join(format!(
                "ployz-taken-over-{}",
                ployz_core::MachineId::random()
            ));
            let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
            if took_over == "lands" {
                std::fs::remove_file(root.join("unnamed")).unwrap();
                std::fs::write(root.join("lands"), "").unwrap();
            }
            std::fs::write(root.join("mode"), "stall-verify").unwrap();
            let gone = CancellationToken::new();
            let image = format!("app@sha256:{HEX}");
            let delivery = deliver_and_sweep(&docker, &image, platform, &gone);
            let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
                tokio::join!(delivery, cancel_once_started(&root, &gone))
            })
            .await
            .expect("a delivery whose caller is gone must end");
            result.unwrap_err();

            let calls = std::fs::read_to_string(root.join("calls")).unwrap();
            assert!(
                calls
                    .lines()
                    .any(|line| line.starts_with(&format!("pull --platform {platform} "))),
                "{took_over}: {calls}"
            );
            assert!(content(&root), "{took_over}: deleted the content: {calls}");
            assert!(
                names(&root).contains(&format!("/{STAGING}/")),
                "{took_over}"
            );
            std::fs::remove_dir_all(root).unwrap();
        }
    }

    /// A delivery stages under a name of its own. A user's own image named on the loopback
    /// port the pull happened to get is not that name, so a failed pull leaves it alone.
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
[ "$1" = --host ] && shift 2
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

        let result = pull_from_ingest(
            &docker,
            &pull,
            source,
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await;
        docker.peer_pulls.swept().await;

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

    /// Docker can find an image's manifest and configuration while a layer is missing, so
    /// a delivery pulls by digest even when Docker seems to hold the content. The pull
    /// fetches what is missing, and only then is the content tagged.
    #[tokio::test]
    async fn a_held_image_missing_a_layer_is_pulled_before_its_tag() {
        let root = std::env::temp_dir().join(format!(
            "ployz-missing-layer-{}",
            ployz_core::MachineId::random()
        ));
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::write(root.join("mode"), "deliver").unwrap();
        std::fs::write(root.join("missing-layer"), "").unwrap();

        deliver_and_sweep(
            &docker,
            &format!("app@sha256:{HEX}"),
            "linux/arm64",
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        let pulled = calls.lines().position(|line| line.starts_with("pull "));
        let tagged = calls.lines().position(|line| line.starts_with("tag "));
        assert!(pulled.is_some() && pulled < tagged, "{calls}");
        assert_eq!(names(&root), format!("{}\n", retained()));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A Machine may hold a digest unnamed, as a build's output or a source's copy that
    /// other Deployments pull. A delivery of that digest to it, cancelled before its tag,
    /// leaves the content: the pull took the copy over, and its staging name, the only
    /// name the content has, stays.
    #[tokio::test]
    async fn an_unnamed_copy_survives_a_cancelled_delivery_to_its_machine() {
        let root = std::env::temp_dir().join(format!(
            "ployz-unnamed-held-{}",
            ployz_core::MachineId::random()
        ));
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::write(root.join("mode"), "stall-verify").unwrap();
        let gone = CancellationToken::new();
        let image = format!("app@sha256:{HEX}");

        let delivery = deliver_and_sweep(&docker, &image, "linux/amd64", &gone);
        let (result, _) = tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(delivery, cancel_once_started(&root, &gone))
        })
        .await
        .expect("a delivery whose caller is gone must end");

        assert!(result.is_err(), "{result:?}");
        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        assert!(content(&root), "deleted the copy: {calls}");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Every Docker command a delivery and its sweeps run names the host the API client
    /// uses. One that didn't would reach whatever Engine the CLI's context names.
    #[tokio::test]
    async fn every_docker_command_names_the_api_clients_host() {
        let root = std::env::temp_dir().join(format!(
            "ployz-cli-host-{}",
            ployz_core::MachineId::random()
        ));
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("mode"), "deliver").unwrap();
        earlier_staging(&root);

        docker.sweep_staging();
        deliver_and_sweep(
            &docker,
            &format!("app@sha256:{HEX}"),
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(
            !root.join("context").exists(),
            "{:?}",
            std::fs::read_to_string(root.join("context"))
        );
        let hosts = std::fs::read_to_string(root.join("hosts")).unwrap();
        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        assert!(
            calls.lines().any(|line| line.starts_with("pull ")),
            "{calls}"
        );
        assert!(
            calls.lines().any(|line| line.starts_with("image rm ")),
            "{calls}"
        );
        assert_eq!(hosts.lines().count(), calls.lines().count());
        assert!(
            hosts.lines().all(|host| host == docker.peer_pulls.host()),
            "{hosts}"
        );
        assert!(docker.peer_pulls.host().starts_with("tcp://127.0.0.1:"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A sweep touches only staging names its ledger holds, never one it finds in Docker's
    /// listing, so a user's image named like a staging name stays, alone or beside the
    /// retention name. Of the ledger's names, those beside their retention name go, from
    /// any run; one a delivery still holds stays.
    #[tokio::test]
    async fn a_sweep_touches_only_the_ledgers_names_beside_their_retention_name() {
        let root = std::env::temp_dir().join(format!(
            "ployz-staging-sweep-{}",
            ployz_core::MachineId::random()
        ));
        let pulls = pulls_in(&root, docker_with_unnamed_copy(&root));
        std::fs::remove_file(root.join("unnamed")).unwrap();
        let run = "e".repeat(40);
        // A user's tag shaped like a staging name, and the digest name Docker lists for it.
        let users = [
            format!("127.0.0.1:5000/{STAGING}/{run}/7:release"),
            format!("127.0.0.1:5000/{STAGING}/{run}/7@sha256:{HEX}"),
        ];
        let listed = |names: &[String]| {
            let mut names = names.to_vec();
            names.sort();
            names
                .iter()
                .map(|name| format!("{name}\n"))
                .collect::<String>()
        };
        std::fs::write(root.join("names"), listed(&users)).unwrap();
        sweep(&pulls).await;
        assert_eq!(names(&root), listed(&users));

        let (_flying, in_flight) = staging(&pulls, &format!("app@sha256:{HEX}")).await;
        let swept = [
            format!("127.0.0.1:41000/{STAGING}/{run}/3@sha256:{HEX}"),
            format!("127.0.0.1:41001/{STAGING}/{}/4@sha256:{HEX}", boot()),
        ];
        let kept = [&users[..], &[retained(), in_flight.clone()]].concat();
        std::fs::write(
            root.join("names"),
            listed(&[&swept[..], &kept[..]].concat()),
        )
        .unwrap();
        std::fs::write(
            root.join("staging-ledger"),
            listed(&[&swept[..], std::slice::from_ref(&in_flight)].concat()),
        )
        .unwrap();

        sweep(&pulls).await;

        assert_eq!(names(&root), listed(&kept));
        assert_eq!(ledger(&root), format!("{in_flight}\n"));
        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        assert!(
            !calls
                .lines()
                .any(|line| users.iter().any(|user| line.contains(user.as_str()))),
            "{calls}"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
