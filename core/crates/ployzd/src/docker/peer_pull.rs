//! Pull an image from another Machine's ingest TCP destination into local Docker.

use std::{
    collections::BTreeSet,
    io::Write as _,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Output,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
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

/// How long the ledger keeps a staging name Docker doesn't hold, from when it was
/// recorded. A pull killed mid-transfer may still be committed by Docker, even after the
/// daemon restarts, so its row waits this long for the name to appear.
const GRACE: Duration = Duration::from_secs(60 * 60);

/// The retention lock, held: the proof each ledger read and write asks for.
type Held<'lock> = tokio::sync::MutexGuard<'lock, ()>;

/// This run of the daemon's part of each staging name: its pid and start time, as
/// fixed-width lowercase hex.
fn boot() -> &'static str {
    static BOOT: OnceLock<String> = OnceLock::new();
    BOOT.get_or_init(|| {
        let started = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{:08x}{started:032x}", std::process::id())
    })
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Seconds since the epoch.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

/// The Docker CLI, aimed at the Engine the API client talks to. The host is named on
/// every command: the CLI then ignores any Docker context or `DOCKER_HOST` it would
/// otherwise follow, so no pull, tag or removal reaches another Engine.
pub(crate) struct DockerCli {
    path: PathBuf,
    host: String,
}

impl DockerCli {
    async fn output(&self, args: &[&str]) -> Result<Output, Error> {
        Command::new(&self.path)
            .arg("--host")
            .arg(&self.host)
            .args(args)
            .kill_on_drop(true)
            .output()
            .await
            .map_err(|error| Error::PeerPull(ployz_core::error_chain::inline(&error)))
    }

    async fn run(&self, args: &[&str]) -> Result<String, Error> {
        let output = self.output(args).await?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(failure(&output))
        }
    }
}

fn failure(output: &Output) -> Error {
    Error::PeerPull(String::from_utf8_lossy(&output.stderr).trim().into())
}

/// One staging name the ledger holds, and when it was recorded, in seconds since the
/// epoch.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    at: u64,
    name: String,
}

/// Peer pulls on one Docker: the CLI they run, the ledger of the staging names they
/// wrote, the names deliveries in flight hold, the lock that keeps retention names in
/// place, and the sweep that removes staging names beside them.
pub(crate) struct PeerPulls {
    cli: DockerCli,
    ledger: Option<PathBuf>,
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

    /// The lock held by every write or removal of a retention name Ployz makes, by every
    /// read and write of the ledger, and by the sweep from reading an image's names to
    /// removing a staging name beside its retention name. Held, the retention name stays
    /// where the sweep saw it.
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
    /// A delivery that succeeds removes its own staging name, so the sweep settles what
    /// failed, was cancelled or ran before a crash.
    async fn sweep_once(&self) {
        let rows = {
            let held = self.retention.lock().await;
            self.rows(&held)
        };
        let rows = match rows {
            Ok(rows) => rows,
            Err(error) => return sweep_failed(&error),
        };
        for row in rows {
            if lock(&self.in_flight).contains(&row.name) {
                continue;
            }
            if let Err(error) = self.settle(&row.name).await {
                sweep_failed(&error);
            }
        }
    }

    /// Remove staging name `name` where its image also holds the retention name of the
    /// digest it was pulled by, so the removal only untags, and drop its row. Ployz never
    /// removes an image's last name: Docker hands content it holds unnamed to the pull
    /// that asks for its digest, so a staging name may be all that names content Ployz
    /// never brought. A user's tag doesn't count, since its owner may move it at any
    /// moment, and a retention name stays put while the retention lock is held. A name
    /// Docker doesn't hold loses its row only once [`GRACE`] has passed since it was
    /// recorded.
    ///
    /// The row goes to the ledger's end first and stays there unless removed, so a name
    /// whose Docker commands stall until the pass's deadline is tried after the rest next
    /// time.
    async fn settle(&self, name: &str) -> Result<(), Error> {
        let held = self.retention.lock().await;
        let mut rows = self.rows(&held)?;
        let Some(at) = rows.iter().position(|row| row.name == name) else {
            return Ok(());
        };
        let row = rows.remove(at);
        rows.push(row.clone());
        self.save(&held, &rows)?;
        let names = match self
            .cli
            .run(&["image", "inspect", name, "--format", NAMES])
            .await
        {
            Ok(names) => names,
            Err(error) if no_such_image(&error) => String::new(),
            Err(error) => return Err(error),
        };
        if !names.lines().any(|listed| listed == name) {
            if now().saturating_sub(row.at) < GRACE.as_secs() {
                return Ok(());
            }
            return self.forget(&held, name);
        }
        let digest = name.rsplit_once('@').map(|(_, digest)| digest);
        let beside = names.lines().any(|listed| {
            retained_digest(listed).is_some_and(|kept| Some(kept.as_str()) == digest)
        });
        if !beside {
            return Ok(());
        }
        match self.cli.run(&["image", "rm", name]).await {
            Err(error) if !no_such_image(&error) => Err(error),
            _ => self.forget(&held, name),
        }
    }

    /// Untag staging name `name` beside the retention name just written for its digest,
    /// and drop its row. A failure is left to the sweep.
    async fn untag(&self, held: &Held<'_>, name: &str) {
        let untagged = match self.cli.run(&["image", "rm", name]).await {
            Err(error) if !no_such_image(&error) => Err(error),
            _ => self.forget(held, name),
        };
        if let Err(error) = untagged {
            tracing::warn!(
                staging = name,
                error = %ployz_core::error_chain::inline(&error),
                "could not remove a delivered image's staging name; the sweep tries again"
            );
        }
    }

    /// Run cleanup `work` until it ends, the caller is `gone`, or the deadline passes. One
    /// that stops early kills the Docker commands it ran and leaves the rest to the sweep.
    async fn bounded(&self, gone: &CancellationToken, work: impl Future<Output = ()>) {
        if gone
            .run_until_cancelled(tokio::time::timeout(self.deadline, work))
            .await
            .is_none_or(|ended| ended.is_err())
        {
            tracing::warn!("a peer pull's cleanup stopped before it ended; the sweep finishes it");
        }
    }

    /// Drop `name`'s row if Docker doesn't hold the name. Asked once the pull that would
    /// write it has ended in Docker, so a name absent now never comes.
    async fn forget_unwritten(&self, name: &str) {
        let held = self.retention.lock().await;
        let absent = match self
            .cli
            .run(&["image", "inspect", name, "--format", NAMES])
            .await
        {
            Ok(names) => !names.lines().any(|listed| listed == name),
            Err(error) => no_such_image(&error),
        };
        if absent && let Err(error) = self.forget(&held, name) {
            sweep_failed(&error);
        }
    }

    /// Record `name` before a pull may write it, so a sweep finds it after a cancel, a
    /// late commit or a restart.
    fn record(&self, held: &Held<'_>, name: &str) -> Result<(), Error> {
        if self.ledger.is_none() {
            return Err(Error::PeerPull(
                "this Docker keeps no ledger of staging names".into(),
            ));
        }
        let mut rows = self.rows(held)?;
        rows.push(Row {
            at: now(),
            name: name.to_owned(),
        });
        self.save(held, &rows)
    }

    fn forget(&self, held: &Held<'_>, name: &str) -> Result<(), Error> {
        let mut rows = self.rows(held)?;
        rows.retain(|row| row.name != name);
        self.save(held, &rows)
    }

    /// The ledger's rows, in order. Ledger reads and writes are synchronous on purpose: a
    /// sweep pass cut at its deadline drops between them, never in one, so no write runs
    /// on after the lock is released. A ledger that won't parse is set aside as
    /// `<ledger>.kept-<n>` and read as empty: the names it held stay in Docker, which is
    /// the disk a cancelled delivery may leave, and none is removed on a guess.
    fn rows(&self, _held: &Held<'_>) -> Result<Vec<Row>, Error> {
        let Some(ledger) = &self.ledger else {
            return Ok(Vec::new());
        };
        let bytes = match std::fs::read(ledger) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(ledger_failed(error)),
        };
        if let Some(rows) = String::from_utf8(bytes).ok().as_deref().and_then(parse) {
            return Ok(rows);
        }
        let kept = (0_u32..)
            .map(|n| sibling(ledger, &format!("kept-{n}")))
            .find(|kept| !kept.exists())
            .unwrap_or_else(|| sibling(ledger, "kept"));
        std::fs::rename(ledger, &kept).map_err(ledger_failed)?;
        tracing::warn!(
            kept = %kept.display(),
            "the ledger of staging names could not be read; set it aside and started a new one"
        );
        Ok(Vec::new())
    }

    /// Write `rows` as the whole ledger: to a file beside it, synced, then moved over it.
    fn save(&self, _held: &Held<'_>, rows: &[Row]) -> Result<(), Error> {
        let Some(ledger) = &self.ledger else {
            return Ok(());
        };
        let text = rows
            .iter()
            .map(|row| format!("{} {}\n", row.at, row.name))
            .collect::<String>();
        let next = sibling(ledger, "next");
        let mut file = std::fs::File::create(&next).map_err(ledger_failed)?;
        file.write_all(text.as_bytes()).map_err(ledger_failed)?;
        file.sync_all().map_err(ledger_failed)?;
        std::fs::rename(&next, ledger).map_err(ledger_failed)?;
        match ledger.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            Some(dir) => std::fs::File::open(dir)
                .and_then(|dir| dir.sync_all())
                .map_err(ledger_failed),
            None => Ok(()),
        }
    }
}

/// The rows of ledger text `text`, each `<recorded at> <name>` on a line of its own, or
/// `None` when any line is not one.
fn parse(text: &str) -> Option<Vec<Row>> {
    if !text.is_empty() && !text.ends_with('\n') {
        return None;
    }
    text.lines()
        .map(|line| {
            let (at, name) = line.split_once(' ')?;
            if name.is_empty() || name.contains(char::is_whitespace) {
                return None;
            }
            Some(Row {
                at: at.parse().ok()?,
                name: name.to_owned(),
            })
        })
        .collect()
}

/// `path` with `.<suffix>` appended to its file name.
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut sibling = path.as_os_str().to_owned();
    sibling.push(format!(".{suffix}"));
    PathBuf::from(sibling)
}

fn sweep_failed(error: &Error) {
    tracing::warn!(
        error = %ployz_core::error_chain::inline(error),
        "could not sweep peer pulls' staging names; the next pass tries again"
    );
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
    digest: String,
    pulls: Arc<PeerPulls>,
    _proxy: ImageProxy,
}

impl Staging {
    async fn new(pulls: &Arc<PeerPulls>, proxy: ImageProxy, digest: &str) -> Result<Self, Error> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let name = format!(
            "127.0.0.1:{}/{STAGING}/{}/{}@{digest}",
            proxy.port,
            boot(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let held = pulls.retention.lock().await;
        lock(&pulls.in_flight).insert(name.clone());
        let staging = Self {
            name,
            digest: digest.to_owned(),
            pulls: Arc::clone(pulls),
            _proxy: proxy,
        };
        pulls.record(&held, &staging.name)?;
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
/// has not begun its tag stops and publishes nothing; one that has completes it, and
/// removes its staging name. What a failed or cancelled delivery leaves is swept as a
/// delivery starts and once it ends, off its turn.
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
    // Every delivery names its content by the retention name of the digest it pulled,
    // the one name its staging name is removed beside. A tag pull also writes its tag and
    // a Publish its own name, for the same digest, resolved once. The retention name sits
    // in the repository the content is published under, registry included: a Publish
    // pulls by a transport reference that names only the source's path.
    let retained = retention_tag(&format!(
        "{}@{}",
        ployz_core::image_repository(pull.tag().unwrap_or(image)),
        staging.digest
    ));
    let published = match (pull.tag(), requested) {
        (Some(tag), _) => Some(tag),
        (None, None) => Some(image),
        (None, Some(_)) => None,
    };
    let names = retained
        .as_deref()
        .into_iter()
        .chain(published.filter(|name| Some(*name) != retained.as_deref()))
        .collect::<Vec<_>>();
    pull_and_tag(&docker.peer_pulls, image, staging, &names, platform, gone).await
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
    Staging::new(&docker.peer_pulls, proxy, &digest).await
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

/// Why a delivery failed before its publish point.
enum Failed {
    /// Docker's pull ended with this failure, so a staging name Docker doesn't hold now it
    /// never will.
    Unpulled(Error),
    /// Anything else, the staging name perhaps written.
    After(Error),
}

impl From<Error> for Failed {
    fn from(error: Error) -> Self {
        Self::After(error)
    }
}

/// Pull `from` for `platform`. A pull whose CLI exits with a failure on its own has ended
/// in Docker; one killed by a signal may still be running there.
async fn pull(docker: &DockerCli, platform: &str, from: &str) -> Result<(), Failed> {
    let output = docker
        .output(&["pull", "--platform", platform, from])
        .await
        .map_err(Failed::Unpulled)?;
    match output.status.code() {
        _ if output.status.success() => Ok(()),
        Some(_) => Err(Failed::Unpulled(failure(&output))),
        None => Err(Failed::After(failure(&output))),
    }
}

/// Pull `staging` and publish it under `names`, in order. Once the retention name of its
/// digest is among them, the staging name is removed before this returns.
async fn pull_and_tag(
    pulls: &PeerPulls,
    image: &str,
    staging: Staging,
    names: &[&str],
    platform: &str,
    gone: &CancellationToken,
) -> Result<(), Error> {
    let (docker, from) = (&pulls.cli, staging.name.as_str());
    let verified = async {
        pull(docker, platform, from).await?;
        if let Some(digest) = ployz_core::ImageRef::new(image).digest
            && descriptor_digest(docker, from).await?.as_deref() != Some(digest)
        {
            return Err(Failed::After(Error::PeerPull(format!(
                "the image store did not retain {image}"
            ))));
        }
        for name in names {
            if let Some(digest) = retained_digest(name) {
                retention_free(docker, from, name, &digest).await?;
            }
        }
        Ok::<_, Failed>(pulls.retention.lock().await)
    };
    // A caller gone stops the transfer here, killing the Docker CLI it runs, so Docker
    // drops the pull and the admission turn it holds frees for the next Deployment.
    // Verify before publishing the requested or retention tag: a failed attempt must
    // not leave a tag or remove one from an earlier delivery.
    match gone.run_until_cancelled(verified).await {
        // The publish point, under the retention lock taken in the caller's race. Once
        // begun it runs to its end, so a caller that stopped waiting finds the names
        // either unwritten or written, and told which. The retention name goes first, and
        // the staging name goes beside it. That untag is cleanup, not publishing: it races
        // the caller and a deadline, so a Docker that hangs on it can't keep the lock and
        // the turn, and a staging name it leaves is swept later.
        Some(Ok(held)) if !gone.is_cancelled() => {
            for name in names {
                docker.run(&["tag", "--", from, name]).await?;
            }
            if names.iter().any(|name| {
                retained_digest(name).is_some_and(|kept| kept.as_str() == staging.digest)
            }) {
                pulls.bounded(gone, pulls.untag(&held, from)).await;
            }
            Ok(())
        }
        Some(Err(Failed::Unpulled(error))) => {
            pulls.bounded(gone, pulls.forget_unwritten(from)).await;
            Err(error)
        }
        Some(Err(Failed::After(error))) => Err(error),
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
        let staging = Staging::new(pulls, ImageProxy::open(source()).await.unwrap(), &digest)
            .await
            .unwrap();
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
                    &[tag],
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
                // A delivery removes only its own staging name, once it has written the
                // retention name of its digest beside it.
                let removed = calls
                    .lines()
                    .filter(|line| line.starts_with("image rm"))
                    .collect::<Vec<_>>();
                let untagged = format!("image rm {pulled}");
                let expected = if mode == "success" && *tag == retained {
                    vec![untagged.as_str()]
                } else {
                    Vec::new()
                };
                assert_eq!(removed, expected, "{mode}: {calls}");
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
                &[&name],
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
            &[ordinary],
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
    /// fails as Docker does when it can't answer, and `hang-once` makes the next one hang,
    /// naming its pid in `started`. A names query for a name `hangs` lists hangs every
    /// time.
    /// The name `moves` holds is moved away just after the next names query answers. While
    /// `rm-hangs` exists, `image rm` names its pid in `started` and hangs.
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
    if [ -e hang-once ]; then rm hang-once; echo $$ > started; exec sleep 600; fi
    if [ -e hangs ] && grep -qxF "$3" hangs; then exec sleep 600; fi
    if [ -e unsure ]; then echo "Cannot connect to the Docker daemon" >&2; exit 1
    elif [ "$3" = sha256:{hex} ]; then cat names
    elif grep -qxF "$3" names; then
      cat names
      if [ -e moves ]; then grep -vxF "$(cat moves)" names > names.new; mv names.new names; rm moves; fi
    elif [ -e unnamed ]; then :
    else echo "No such image: $3" >&2; exit 1; fi ;;
  'image rm '*)
    if [ -e rm-hangs ]; then echo $$ > started; exec sleep 600; fi
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
        let pid = started(root).await;
        gone.cancel();
        pid
    }

    /// Wait for the fake Docker to stall or hang in `root`, answering its pid.
    async fn started(root: &std::path::Path) -> String {
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
        pid.trim().to_owned()
    }

    fn names(root: &std::path::Path) -> String {
        std::fs::read_to_string(root.join("names")).unwrap()
    }

    /// The names the ledger in `root` holds, one a line, in its order.
    fn ledger(root: &std::path::Path) -> String {
        std::fs::read_to_string(root.join("staging-ledger"))
            .unwrap_or_default()
            .lines()
            .map(|row| format!("{}\n", row.split_once(' ').unwrap().1))
            .collect()
    }

    /// Ledger rows for `names`, recorded `ago` seconds back.
    fn recorded(names: &[String], ago: u64) -> String {
        names
            .iter()
            .map(|name| format!("{} {name}\n", now() - ago))
            .collect()
    }

    /// Whether process `pid` has exited, waiting up to two seconds for it.
    fn exited(pid: &str) -> bool {
        let stat = format!("/proc/{pid}/stat");
        let ended = || {
            std::fs::read_to_string(&stat).map_or(true, |stat| {
                stat.rsplit_once(") ")
                    .is_some_and(|(_, rest)| rest.starts_with('Z'))
            })
        };
        for _ in 0..200 {
            if ended() {
                return true;
            }
            // real-wait: a killed process exits on the kernel's schedule, with nothing to await.
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        ended()
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
            &[],
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

        let pull = pull_and_tag(&pulls, &image, staging, &[], "linux/amd64", &gone);
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
        assert!(exited(&pid), "the Docker pull still runs");
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

            let pull = pull_and_tag(&pulls, &image, staging, &[], "linux/amd64", &gone);
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

        let written = [retained.as_str()];
        let pull = pull_and_tag(&pulls, &image, staging, &written, "linux/amd64", &gone);
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
        std::fs::write(
            root.join("staging-ledger"),
            recorded(std::slice::from_ref(&earlier), 0),
        )
        .unwrap();
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
            .with_sweep_deadline(std::time::Duration::from_secs(5));
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("mode"), "deliver").unwrap();
        earlier_staging(&root);
        std::fs::write(root.join("hang-once"), "").unwrap();

        docker.sweep_staging();
        let hung = started(&root).await;
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
        assert!(exited(&hung), "the hung `image inspect` still runs");
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

    /// A source that answers one `HEAD` for a tag with digest `sha256:1…1`, as the Machine
    /// image server does, and the request it got.
    async fn serving_one_digest() -> (ImageIngestDestination, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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
        (source, asked)
    }

    /// A tag pull resolves its tag once, pulls that digest, and names it by the digest's
    /// retention name and by the tag. Beside the retention name, the sweep removes the
    /// staging name.
    #[tokio::test]
    async fn a_tag_pull_leaves_the_retention_name_of_its_digest_and_no_staging_name() {
        let root = std::env::temp_dir().join(format!(
            "ployz-tag-pull-{}",
            ployz_core::MachineId::random()
        ));
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("mode"), "deliver").unwrap();
        let (source, asked) = serving_one_digest().await;
        let pull = PeerImagePull::Reference {
            image: "app:v1".into(),
        };

        pull_from_ingest(
            &docker,
            &pull,
            source,
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        docker.peer_pulls.swept().await;

        assert!(
            asked
                .await
                .unwrap()
                .starts_with("HEAD /v2/app/manifests/v1 ")
        );
        let calls = std::fs::read_to_string(root.join("calls")).unwrap();
        assert!(
            calls.contains("pull --platform linux/amd64 127.0.0.1:")
                && calls.contains(&format!("@sha256:{HEX}\n")),
            "{calls}"
        );
        assert_eq!(names(&root), format!("{}\napp:v1\n", retained()));
        assert_eq!(ledger(&root), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A Publish names the pinned digest by its retention name and by its own name. Beside
    /// the retention name, the sweep removes the staging name.
    #[tokio::test]
    async fn a_publish_leaves_the_retention_name_of_its_digest_and_no_staging_name() {
        let root =
            std::env::temp_dir().join(format!("ployz-publish-{}", ployz_core::MachineId::random()));
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("mode"), "deliver").unwrap();
        let pull = PeerImagePull::Publish {
            image: ImageDigestReference::parse(format!("app@sha256:{HEX}")).unwrap(),
            tag: "app:live".into(),
        };

        pull_from_ingest(
            &docker,
            &pull,
            source(),
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        docker.peer_pulls.swept().await;

        assert_eq!(names(&root), format!("{}\napp:live\n", retained()));
        assert_eq!(ledger(&root), "");
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
            recorded(&[&swept[..], std::slice::from_ref(&in_flight)].concat(), 0),
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

    /// A fresh directory under the temp dir, named from `prefix`. Not yet created.
    fn temp(prefix: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{prefix}-{}", ployz_core::MachineId::random()))
    }

    /// A Docker whose names are the lines of `store`, each `<name> <digest>`. A digest's
    /// content exists while any name holds it, so removing its last name frees it.
    fn docker_with_a_store(root: &std::path::Path) -> PathBuf {
        std::fs::create_dir(root).unwrap();
        std::fs::write(root.join("store"), "").unwrap();
        let docker = root.join("docker");
        crate::docker::test_support::write_script(
            &docker,
            r#"#!/bin/sh
cd "$(dirname "$0")"
[ "$1" = --host ] && shift 2
printf '%s\n' "$*" >> calls
digest() { awk -v name="$1" '$1 == name { print $2 }' store; }
drop() { awk -v name="$1" '$1 != name' store > store.new; mv store.new store; }
case "$1 $2" in
  'image inspect')
    held=$(digest "$3")
    if [ -z "$held" ]; then echo "No such image: $3" >&2; exit 1; fi
    case "$5" in
      '{{json'*) echo "{\"digest\":\"$held\"}" ;;
      *) awk -v held="$held" '$2 == held { print $1 }' store ;;
    esac ;;
  'image rm')
    if [ -z "$(digest "$3")" ]; then echo "No such image: $3" >&2; exit 1; fi
    drop "$3" ;;
  'pull --platform') echo "$4 ${4##*@}" >> store ;;
  'tag --') held=$(digest "$3"); drop "$4"; echo "$4 $held" >> store ;;
esac
"#,
        );
        docker
    }

    /// Two deliveries to a Machine under disk pressure, then Image Cleanup, with no sweep
    /// between. Each delivery removed its own staging name, so the older digest's
    /// retention name is its last name, and Image Cleanup's removal frees its content.
    #[tokio::test]
    async fn a_delivered_staging_name_is_gone_before_image_cleanup_runs() {
        let root = temp("ployz-delivered-cleanup");
        let pulls = pulls_in(&root, docker_with_a_store(&root));
        let digests = ['a', 'b'].map(|hex| format!("sha256:{}", hex.to_string().repeat(64)));
        for digest in &digests {
            let image = format!("app@{digest}");
            let retained = retention_tag(&image).unwrap();
            let (staging, _) = staging(&pulls, &image).await;
            pull_and_tag(
                &pulls,
                &image,
                staging,
                &[&retained],
                "linux/amd64",
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        }

        let store = std::fs::read_to_string(root.join("store")).unwrap();
        let now = i64::try_from(now()).unwrap();
        let images = digests
            .iter()
            .zip([2, 1])
            .map(|(digest, days)| ployz_core::ImageSummary {
                id: digest.clone(),
                repo_tags: store
                    .lines()
                    .filter_map(|line| line.split_once(' '))
                    .filter(|(name, held)| held == digest && !name.contains('@'))
                    .map(|(name, _)| name.to_owned())
                    .collect(),
                created: 0,
                size: 1,
                containers: 0,
                platforms: Vec::new(),
                last_tagged: Some(now - days * 24 * 60 * 60),
            })
            .collect::<Vec<_>>();
        let removed = crate::docker::images::superseded(&images, "app", true, now);
        assert_eq!(
            removed,
            [retention_tag(&format!("app@{}", digests[0])).unwrap()]
        );
        for name in &removed {
            pulls.cli.run(&["image", "rm", name]).await.unwrap();
        }

        let store = std::fs::read_to_string(root.join("store")).unwrap();
        assert!(!store.contains(&digests[0]), "{store}");
        assert!(!store.contains(STAGING), "{store}");
        assert_eq!(ledger(&root), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A ledger that won't parse, such as one an append tore, is set aside, and the next
    /// staging name is recorded in a new one, whole. Once Docker holds that name beside
    /// its retention name, the sweep removes it.
    #[tokio::test]
    async fn a_torn_ledger_is_set_aside_and_every_name_stays_tracked() {
        let root = temp("ployz-torn-ledger");
        let pulls = pulls_in(&root, docker_with_unnamed_copy(&root));
        std::fs::remove_file(root.join("unnamed")).unwrap();
        let torn = format!("{} 127.0.0.1:41000/{STAGING}/", now());
        std::fs::write(root.join("staging-ledger"), &torn).unwrap();

        let (staging, pulled) = staging(&pulls, &format!("app@sha256:{HEX}")).await;
        assert_eq!(ledger(&root), format!("{pulled}\n"));
        assert_eq!(
            std::fs::read_to_string(root.join("staging-ledger.kept-0")).unwrap(),
            torn
        );
        drop(staging);

        std::fs::write(root.join("names"), format!("{}\n{pulled}\n", retained())).unwrap();
        sweep(&pulls).await;
        assert_eq!(names(&root), format!("{}\n", retained()));
        assert_eq!(ledger(&root), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A pull that Docker ended with a failure wrote no staging name and never will, so
    /// its row goes before the delivery returns. Many such pulls leave no rows.
    #[tokio::test]
    async fn failed_pulls_leave_no_rows_in_the_ledger() {
        let root = temp("ployz-failed-pulls");
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("mode"), "fail").unwrap();

        for _ in 0..20 {
            deliver_and_sweep(
                &docker,
                &format!("app@sha256:{HEX}"),
                "linux/amd64",
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        }

        assert_eq!(ledger(&root), "");
        assert_eq!(names(&root), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// The last run cancelled a pull that Docker may still commit. After a restart, its
    /// row waits out the grace period for the name, and once Docker commits it beside
    /// the retention name the sweep removes it. A row whose name never came within the
    /// grace period goes.
    #[tokio::test]
    async fn a_name_committed_after_a_restart_is_still_removed() {
        let root = temp("ployz-restart-commit");
        let cli = docker_with_unnamed_copy(&root);
        std::fs::remove_file(root.join("unnamed")).unwrap();
        let run = "e".repeat(40);
        let late = format!("127.0.0.1:41000/{STAGING}/{run}/3@sha256:{HEX}");
        let lapsed = format!("127.0.0.1:41001/{STAGING}/{run}/4@sha256:{HEX}");
        std::fs::write(
            root.join("staging-ledger"),
            recorded(std::slice::from_ref(&late), 60)
                + &recorded(std::slice::from_ref(&lapsed), 2 * 60 * 60),
        )
        .unwrap();
        let pulls = pulls_in(&root, cli);

        sweep(&pulls).await;
        assert_eq!(ledger(&root), format!("{late}\n"));

        std::fs::write(root.join("names"), format!("{}\n{late}\n", retained())).unwrap();
        sweep(&pulls).await;
        assert_eq!(names(&root), format!("{}\n", retained()));
        assert_eq!(ledger(&root), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A pass that stalls on one name until its deadline keeps the removals it made, and
    /// the next pass reaches the names after the stalled one.
    #[tokio::test]
    async fn a_stalled_name_keeps_no_other_from_the_sweep() {
        let root = temp("ployz-stalled-name");
        let pulls = Arc::new(
            PeerPulls::new(
                docker_with_unnamed_copy(&root),
                "unix:///run/test-engine.sock".into(),
                Some(root.join("staging-ledger")),
            )
            .with_deadline(std::time::Duration::from_secs(5)),
        );
        std::fs::remove_file(root.join("unnamed")).unwrap();
        let run = "e".repeat(40);
        let [first, stalled, last] =
            [3, 4, 5].map(|n| format!("127.0.0.1:41000/{STAGING}/{run}/{n}@sha256:{HEX}"));
        std::fs::write(
            root.join("names"),
            format!("{}\n{first}\n{stalled}\n{last}\n", retained()),
        )
        .unwrap();
        std::fs::write(
            root.join("staging-ledger"),
            recorded(&[first.clone(), stalled.clone(), last.clone()], 0),
        )
        .unwrap();
        std::fs::write(root.join("hangs"), &stalled).unwrap();

        sweep(&pulls).await;
        assert!(!names(&root).contains(&first), "{}", names(&root));
        assert_eq!(ledger(&root), format!("{last}\n{stalled}\n"));

        sweep(&pulls).await;
        assert_eq!(names(&root), format!("{}\n{stalled}\n", retained()));
        assert_eq!(ledger(&root), format!("{stalled}\n"));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A Publish pulls by a transport reference naming only the source's path. Its
    /// retention name sits in the repository it publishes to, registry included.
    #[tokio::test]
    async fn a_publish_writes_its_retention_name_in_the_published_repository() {
        let root = temp("ployz-publish-registry");
        let docker = delivery_docker(docker_with_unnamed_copy(&root)).await;
        std::fs::remove_file(root.join("unnamed")).unwrap();
        std::fs::write(root.join("mode"), "deliver").unwrap();
        let pull = PeerImagePull::Publish {
            image: ImageDigestReference::parse(format!("acme/app@sha256:{HEX}")).unwrap(),
            tag: "registry.example/acme/app:v1".into(),
        };

        pull_from_ingest(
            &docker,
            &pull,
            source(),
            "linux/amd64",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        docker.peer_pulls.swept().await;

        assert_eq!(
            names(&root),
            format!("registry.example/acme/app:ployz-sha256-{HEX}\nregistry.example/acme/app:v1\n")
        );
        assert_eq!(ledger(&root), "");
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Docker may write a delivery's names and then hang on untagging its staging name.
    /// The delivery still answers once its caller leaves or its deadline passes, freeing
    /// the retention lock and its turn for the next delivery and the Machine's exclusive
    /// work. The sweep removes the staging name later.
    #[tokio::test]
    async fn a_hung_untag_holds_neither_the_lock_nor_the_turn() {
        for cancelled in [true, false] {
            let root = temp("ployz-hung-untag");
            let pulls = Arc::new(
                PeerPulls::new(
                    docker_with_unnamed_copy(&root),
                    "unix:///run/test-engine.sock".into(),
                    Some(root.join("staging-ledger")),
                )
                .with_deadline(std::time::Duration::from_secs(2)),
            );
            std::fs::remove_file(root.join("unnamed")).unwrap();
            std::fs::write(root.join("mode"), "deliver").unwrap();
            std::fs::write(root.join("rm-hangs"), "").unwrap();
            let image = format!("app@sha256:{HEX}");
            let retained = retained();
            let (staging, pulled) = staging(&pulls, &image).await;
            let gone = CancellationToken::new();
            let written = [retained.as_str()];

            let delivery = pull_and_tag(&pulls, &image, staging, &written, "linux/amd64", &gone);
            let (result, pid) = tokio::time::timeout(std::time::Duration::from_secs(20), async {
                tokio::join!(delivery, async {
                    let pid = started(&root).await;
                    if cancelled {
                        gone.cancel();
                    }
                    pid
                })
            })
            .await
            .expect("a hung untag must not hold the delivery");

            result.unwrap();
            assert!(pulls.retention.try_lock().is_ok(), "cancelled: {cancelled}");
            assert!(exited(&pid), "the hung `image rm` still runs");
            assert_eq!(names(&root), format!("{pulled}\n{retained}\n"));
            std::fs::remove_file(root.join("rm-hangs")).unwrap();
            sweep(&pulls).await;
            assert_eq!(names(&root), format!("{retained}\n"));
            assert_eq!(ledger(&root), "");
            std::fs::remove_dir_all(root).unwrap();
        }
    }
}
