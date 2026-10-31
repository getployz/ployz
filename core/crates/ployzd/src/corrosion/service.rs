//! Runs the Corrosion agent in a Docker container the daemon creates and keeps.

use std::{
    collections::HashMap,
    fs,
    future::Future,
    net::{Ipv4Addr, SocketAddr},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use bollard::{
    Docker,
    models::{
        ContainerCreateBody, HostConfig, Mount, MountType, RestartPolicy, RestartPolicyNameEnum,
    },
};
use rustix::fs::{IFlags, Mode, OFlags};
use serde::Serialize;

use crate::{
    docker::{DesiredContainer, Ensure, ManagedService},
    filesystem::atomic_write,
};
use ployz_core::{CORROSION_API_PORT, CORROSION_GOSSIP_PORT};

use super::{AdminClient, ApiClient, Error, ReplicatedStore, Statement};

/// Built from the `ployz` branch of getployz/corrosion: upstream plus the patches its README lists.
pub const IMAGE: &str = "ghcr.io/getployz/corrosion:20261031-28f00b417271@sha256:384f1e20dbbe187135c4c75ae0eb41943df64c2306b5b4eecffa88ba04b46cc1";
pub const DEFAULT_CONTAINER_NAME: &str = "ployz-corrosion";
const TOKEN_FILE: &str = ".api-token";
const SCHEMA: &str = include_str!("schema.sql");
const START_TIMEOUT: Duration = Duration::from_secs(4 * 60 + 30);
const KEPT_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Where Corrosion keeps its files under the Ployz data and run directories.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrosionPaths {
    pub data_dir: PathBuf,
    pub run_dir: PathBuf,
}

impl CorrosionPaths {
    #[must_use]
    pub fn under(ployz_data_dir: &Path, ployz_run_dir: &Path) -> Self {
        Self {
            data_dir: Self::data_dir_under(ployz_data_dir),
            run_dir: ployz_run_dir.join("corrosion"),
        }
    }

    #[must_use]
    pub fn data_dir_under(ployz_data_dir: &Path) -> PathBuf {
        ployz_data_dir.join("corrosion")
    }

    #[must_use]
    pub fn token_file(&self) -> PathBuf {
        self.data_dir.join(TOKEN_FILE)
    }

    #[must_use]
    pub fn admin_socket(&self) -> PathBuf {
        self.run_dir.join("admin.sock")
    }
}

pub struct CorrosionConfig {
    paths: CorrosionPaths,
    api_address: SocketAddr,
    gossip_address: SocketAddr,
    container_name: String,
    bootstrap: Vec<SocketAddr>,
}

impl CorrosionConfig {
    #[must_use]
    pub fn new(
        data_dir: impl Into<PathBuf>,
        run_dir: impl Into<PathBuf>,
        api_address: SocketAddr,
        gossip_address: SocketAddr,
        container_name: impl Into<String>,
    ) -> Self {
        Self {
            paths: CorrosionPaths {
                data_dir: data_dir.into(),
                run_dir: run_dir.into(),
            },
            api_address,
            gossip_address,
            container_name: container_name.into(),
            bootstrap: Vec::new(),
        }
    }

    #[must_use]
    pub fn local(data_dir: impl Into<PathBuf>, run_dir: impl Into<PathBuf>) -> Self {
        Self::new(
            data_dir,
            run_dir,
            SocketAddr::from((Ipv4Addr::LOCALHOST, CORROSION_API_PORT)),
            SocketAddr::from((Ipv4Addr::LOCALHOST, CORROSION_GOSSIP_PORT)),
            DEFAULT_CONTAINER_NAME,
        )
    }

    #[must_use]
    pub fn with_bootstrap(mut self, peers: impl IntoIterator<Item = SocketAddr>) -> Self {
        self.bootstrap = peers.into_iter().collect();
        self
    }

    pub async fn start(&self) -> Result<RunningCorrosion, Error> {
        bounded_start(async {
            let files = self.install()?;
            let api = ApiClient::new(self.api_address, &files.token)?;
            let admin = AdminClient::new(self.paths.admin_socket());
            let docker = Docker::connect_with_socket_defaults()?;
            let service = DockerService {
                service: managed(docker, self.container_name.clone()),
                data_dir: self.paths.data_dir.clone(),
                run_dir: self.paths.run_dir.clone(),
            };
            let ensured = service.start(&files).await?;
            // Only now does the running Corrosion sync each commit itself. An older one,
            // still serving while the new image pulls, relies on the flag.
            let refused = clear_sync_writes(&self.paths.data_dir);
            if let Some(first) = refused.first() {
                tracing::warn!(
                    data_dir = %self.paths.data_dir.display(),
                    refused = refused.len(),
                    error = %ployz_core::error_chain::inline(first),
                    "Corrosion's commits stay slow: files in its data directory still write every page synchronously"
                );
            }
            wait_ready_or_replace(
                ensured == Ensure::Keep,
                || async { api.query(Statement::new("SELECT 1", [])).await.is_ok() },
                async {
                    service.service.remove().await?;
                    service.start(&files).await?;
                    Ok(())
                },
            )
            .await?;
            let kept = service.clone();
            let keeper = tokio::spawn(keep(
                move || {
                    let (service, files) = (kept.clone(), files.clone());
                    async move { service.start(&files).await }
                },
                KEEP_EVERY,
            ));
            Ok(RunningCorrosion {
                store: ReplicatedStore::new(api),
                admin,
                service,
                keeper,
            })
        })
        .await
    }

    fn install(&self) -> Result<InstalledFiles, Error> {
        create_private_dir(&self.paths.data_dir)?;
        create_private_dir(&self.paths.run_dir)?;
        let token = load_or_create_token(&self.paths.token_file())?;
        let schema_path = self.paths.data_dir.join("schema.sql");
        atomic_write(&schema_path, SCHEMA.as_bytes(), 0o644)?;

        let config = FileConfig {
            db: DbConfig {
                path: self.paths.data_dir.join("store.db"),
                schema_paths: vec![schema_path],
                synchronous: "full",
            },
            gossip: GossipConfig {
                addr: self.gossip_address,
                bootstrap: self.sorted_bootstrap(),
                plaintext: true,
            },
            api: ApiConfig {
                addr: self.api_address,
                authz: AuthzConfig {
                    bearer_token: token.clone(),
                },
            },
            admin: AdminConfig {
                path: self.paths.admin_socket(),
            },
        };
        let encoded = toml::to_string(&config)?;
        atomic_write(
            &self.paths.data_dir.join("config.toml"),
            encoded.as_bytes(),
            0o600,
        )?;
        Ok(InstalledFiles {
            token,
            config: encoded.into_bytes(),
        })
    }

    // Peer order varies between record reads; an unsorted list would replace Corrosion on restart.
    fn sorted_bootstrap(&self) -> Vec<String> {
        let mut peers = self.bootstrap.clone();
        peers.sort_unstable();
        peers.dedup();
        peers.iter().map(ToString::to_string).collect()
    }
}

#[derive(Clone)]
struct InstalledFiles {
    token: String,
    config: Vec<u8>,
}

/// Removes the Corrosion container and its run directory without a running
/// handle, for a reset the daemon did not finish before it stopped.
pub async fn remove_retained(run_dir: &Path) -> Result<(), Error> {
    let docker = Docker::connect_with_socket_defaults()?;
    remove(&managed(docker, DEFAULT_CONTAINER_NAME), run_dir).await
}

/// Seconds a replaced or removed Corrosion gets after SIGTERM. Its graceful exit waits up
/// to 5 s for gossip connections that peers keep open to go idle, and the daemon waits
/// on that stop (#3680). Nothing durable rides on it: each commit is synced before the
/// API answers, and anything a peer missed reaches it through sync, as after a crash.
const STOP_GRACE_SECONDS: i32 = 1;

fn managed(docker: Docker, name: impl Into<String>) -> ManagedService {
    ManagedService::host(docker, name, IMAGE).with_stop_grace(STOP_GRACE_SECONDS)
}

async fn remove(service: &ManagedService, run_dir: &Path) -> Result<(), Error> {
    service.remove().await?;
    match fs::remove_dir_all(run_dir) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error.into()),
        _ => Ok(()),
    }
}

pub struct RunningCorrosion {
    store: ReplicatedStore,
    admin: AdminClient,
    service: DockerService,
    keeper: tokio::task::JoinHandle<()>,
}

impl Drop for RunningCorrosion {
    fn drop(&mut self) {
        self.keeper.abort();
    }
}

/// How often a running daemon checks that its Corrosion container still runs.
const KEEP_EVERY: Duration = Duration::from_secs(2);

/// Docker's `unless-stopped` policy restarts a Corrosion that crashed, not one someone
/// stopped, and every read and write on this Machine goes through it. So while the
/// daemon runs, it ensures the container again every `every`: one inspect a tick, and a
/// start or create when it's gone. Bound: it ends when the daemon drops its
/// [`RunningCorrosion`] or removes Corrosion.
async fn keep<F, Fut>(ensure: F, every: Duration)
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<Ensure, Error>>,
{
    let mut ticks = tokio::time::interval(every);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticks.tick().await;
    let mut failing = false;
    loop {
        ticks.tick().await;
        match ensure().await {
            Ok(Ensure::Keep) => failing = false,
            Ok(ensure) => {
                failing = false;
                tracing::warn!(?ensure, "Corrosion wasn't running; ployzd started it again");
            }
            Err(error) => {
                if !failing {
                    tracing::warn!(
                        error = %ployz_core::error_chain::inline(&error),
                        "Corrosion isn't running and ployzd couldn't start it; it tries again every {}s",
                        every.as_secs()
                    );
                }
                failing = true;
            }
        }
    }
}

impl RunningCorrosion {
    #[must_use]
    pub fn store(&self) -> &ReplicatedStore {
        &self.store
    }

    #[must_use]
    pub fn admin_client(&self) -> AdminClient {
        self.admin.clone()
    }

    pub async fn membership_states(&self) -> Result<Vec<super::MembershipState>, Error> {
        self.admin.membership_states().await
    }

    pub async fn cleanup(&mut self) -> Result<(), Error> {
        self.keeper.abort();
        if !self.keeper.is_finished() {
            let _ = (&mut self.keeper).await;
        }
        remove(&self.service.service, &self.service.run_dir).await
    }
}

#[derive(Clone)]
struct DockerService {
    service: ManagedService,
    data_dir: PathBuf,
    run_dir: PathBuf,
}

impl DockerService {
    async fn start(&self, files: &InstalledFiles) -> Result<Ensure, Error> {
        let owner = fs::metadata(&self.data_dir)?;
        let config = container_config(
            &self.data_dir,
            &self.run_dir,
            format!("{}:{}", owner.uid(), owner.gid()),
        );
        self.service
            .ensure_host(DesiredContainer::new(
                config,
                &[&files.config, SCHEMA.as_bytes()],
            ))
            .await
            .map_err(Into::into)
    }
}

fn container_config(data_dir: &Path, run_dir: &Path, user: String) -> ContainerCreateBody {
    let mounts = [data_dir, run_dir]
        .into_iter()
        .map(|path| Mount {
            typ: Some(MountType::BIND),
            source: Some(path.to_string_lossy().into_owned()),
            target: Some(path.to_string_lossy().into_owned()),
            ..Default::default()
        })
        .collect();
    ContainerCreateBody {
        image: Some(IMAGE.into()),
        cmd: Some(vec![
            "corrosion".into(),
            "agent".into(),
            "-c".into(),
            data_dir.join("config.toml").to_string_lossy().into_owned(),
        ]),
        user: Some(user),
        labels: Some(HashMap::from([("ployzd.managed".into(), String::new())])),
        host_config: Some(HostConfig {
            network_mode: Some("host".into()),
            restart_policy: Some(RestartPolicy {
                name: Some(RestartPolicyNameEnum::UNLESS_STOPPED),
                ..Default::default()
            }),
            log_config: Some(crate::docker::container_log_config()),
            mounts: Some(mounts),
            ..Default::default()
        }),
        ..Default::default()
    }
}

async fn bounded_start<F, T>(start: F) -> Result<T, Error>
where
    F: Future<Output = Result<T, Error>>,
{
    tokio::time::timeout(START_TIMEOUT, start)
        .await
        .map_err(|_| {
            Error::Api(format!(
                "Corrosion did not start within {} seconds; inspect `journalctl -u ployz -n 100 --no-pager`",
                START_TIMEOUT.as_secs(),
            ))
        })?
}

async fn wait_ready_or_replace<F, Fut, R>(kept: bool, mut ready: F, replace: R) -> Result<(), Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
    R: Future<Output = Result<(), Error>>,
{
    if kept
        && tokio::time::timeout(KEPT_READY_TIMEOUT, wait_ready(&mut ready))
            .await
            .is_err()
    {
        tracing::warn!("kept Corrosion container did not answer; replacing it");
        replace.await?;
    }
    wait_ready(ready).await;
    Ok(())
}

async fn wait_ready<F, Fut>(mut ready: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    loop {
        if ready().await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn create_private_dir(path: &Path) -> Result<(), Error> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
#[error("could not stop {} writing every page synchronously", path.display())]
struct SyncWritesError {
    path: PathBuf,
    source: std::io::Error,
}

// Daemons up to 0.3.11 set the inode sync flag on the store so a broadcast commit survived
// power loss. `synchronous = "full"` now syncs each commit, and the flag would sync every
// page on top, which held Corrosion's one write connection for minutes on a slow disk (#2925).
fn clear_sync_writes(dir: &Path) -> Vec<SyncWritesError> {
    let mut refused = Vec::new();
    sync_tree(dir, OFlags::DIRECTORY, &mut refused);
    let unsupported = [rustix::io::Errno::OPNOTSUPP, rustix::io::Errno::NOTTY]
        .map(|errno| Some(errno.raw_os_error()));
    refused.retain(|error| {
        error.source.kind() != std::io::ErrorKind::NotFound
            && !unsupported.contains(&error.source.raw_os_error())
    });
    refused
}

fn sync_tree(dir: &Path, open: OFlags, refused: &mut Vec<SyncWritesError>) {
    if let Err(error) = clear_sync_flag(dir, open) {
        refused.push(error);
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(source) => {
            refused.push(SyncWritesError {
                path: dir.to_path_buf(),
                source,
            });
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(source) => {
                refused.push(SyncWritesError {
                    path: dir.to_path_buf(),
                    source,
                });
                continue;
            }
        };
        let path = entry.path();
        match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => {
                sync_tree(&path, OFlags::DIRECTORY | OFlags::NOFOLLOW, refused)
            }
            Ok(file_type) if file_type.is_file() => {
                if let Err(error) = clear_sync_flag(&path, OFlags::NOFOLLOW) {
                    refused.push(error);
                }
            }
            Ok(_) => {}
            Err(source) => refused.push(SyncWritesError { path, source }),
        }
    }
}

fn clear_sync_flag(path: &Path, kind: OFlags) -> Result<(), SyncWritesError> {
    let set = || -> rustix::io::Result<()> {
        let fd = rustix::fs::open(
            path,
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC | kind,
            Mode::empty(),
        )?;
        let flags = rustix::fs::ioctl_getflags(&fd)?;
        if flags.contains(IFlags::SYNC) {
            rustix::fs::ioctl_setflags(&fd, flags - IFlags::SYNC)?;
        }
        Ok(())
    };
    set().map_err(|errno| SyncWritesError {
        path: path.to_path_buf(),
        source: errno.into(),
    })
}

fn load_or_create_token(path: &Path) -> Result<String, Error> {
    match fs::read_to_string(path) {
        Ok(token) if !token.is_empty() => {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
            validate_token(token)
        }
        Ok(_) => create_token(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => create_token(path),
        Err(error) => Err(error.into()),
    }
}

fn create_token(path: &Path) -> Result<String, Error> {
    let token = format!(
        "{}{}",
        ployz_core::MachineId::random(),
        ployz_core::MachineId::random()
    );
    atomic_write(path, token.as_bytes(), 0o600)?;
    Ok(token)
}

fn validate_token(token: String) -> Result<String, Error> {
    if token.len() == 64
        && token
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(token)
    } else {
        Err(Error::Protocol("invalid persisted API token".into()))
    }
}

#[derive(Serialize)]
struct FileConfig {
    db: DbConfig,
    gossip: GossipConfig,
    api: ApiConfig,
    admin: AdminConfig,
}

#[derive(Serialize)]
struct DbConfig {
    path: PathBuf,
    schema_paths: Vec<PathBuf>,
    synchronous: &'static str,
}

#[derive(Serialize)]
struct GossipConfig {
    addr: SocketAddr,
    bootstrap: Vec<String>,
    plaintext: bool,
}

#[derive(Serialize)]
struct ApiConfig {
    addr: SocketAddr,
    authz: AuthzConfig,
}

#[derive(Serialize)]
struct AuthzConfig {
    #[serde(rename = "bearer-token")]
    bearer_token: String,
}

#[derive(Serialize)]
struct AdminConfig {
    path: PathBuf,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Instant;

    #[tokio::test(start_paused = true)]
    async fn a_stopped_corrosion_is_started_again_while_the_daemon_runs() {
        let ensured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = ensured.clone();
        // Running, then someone stops it, then it runs again.
        let states = std::sync::Arc::new(std::sync::Mutex::new(
            vec![Ensure::Keep, Ensure::Start, Ensure::Keep].into_iter(),
        ));
        let keeper = tokio::spawn(keep(
            move || {
                let next = states.lock().unwrap().next().unwrap_or(Ensure::Keep);
                seen.lock().unwrap().push(next);
                async move { Ok(next) }
            },
            KEEP_EVERY,
        ));
        tokio::time::sleep(KEEP_EVERY * 3 + Duration::from_millis(1)).await;
        keeper.abort();
        assert_eq!(
            ensured
                .lock()
                .unwrap()
                .iter()
                .take(3)
                .copied()
                .collect::<Vec<_>>(),
            [Ensure::Keep, Ensure::Start, Ensure::Keep]
        );
    }

    #[test]
    fn corrosion_container_rotates_its_logs() {
        let config = container_config(Path::new("/data"), Path::new("/run"), "1:1".into());
        assert_eq!(
            config.host_config.unwrap().log_config,
            Some(crate::docker::container_log_config())
        );
    }

    #[tokio::test(start_paused = true)]
    async fn corrosion_readiness_wait_keeps_polling_after_fifteen_seconds() {
        let started = Instant::now();
        wait_ready(|| async { started.elapsed() >= Duration::from_secs(16) }).await;
        assert!(
            started.elapsed() >= Duration::from_secs(16),
            "probe must succeed only after 15 seconds, got {:?}",
            started.elapsed()
        );
    }

    async fn start_with(kept: bool, ready_after: Option<Duration>) -> (bool, Duration) {
        let started = Instant::now();
        let replaced = std::cell::Cell::new(false);
        bounded_start(wait_ready_or_replace(
            kept,
            || {
                let ready =
                    replaced.get() || ready_after.is_some_and(|after| started.elapsed() >= after);
                async move { ready }
            },
            async {
                replaced.set(true);
                Ok(())
            },
        ))
        .await
        .unwrap();
        (replaced.get(), started.elapsed())
    }

    #[tokio::test(start_paused = true)]
    async fn kept_corrosion_that_never_answers_is_replaced() {
        let (replaced, elapsed) = start_with(true, None).await;
        assert!(replaced);
        assert!(elapsed >= KEPT_READY_TIMEOUT, "{elapsed:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn answering_kept_corrosion_is_not_replaced() {
        assert!(!start_with(true, Some(Duration::from_secs(5))).await.0);
    }

    #[tokio::test(start_paused = true)]
    async fn new_corrosion_gets_the_full_wait_without_replacement() {
        let (replaced, elapsed) = start_with(false, Some(Duration::from_secs(120))).await;
        assert!(!replaced);
        assert!(elapsed >= Duration::from_secs(120), "{elapsed:?}");
    }

    #[test]
    fn install_is_byte_stable() {
        let root = tempfile::tempdir().unwrap();
        let peer = |last| SocketAddr::from(([10, 0, 0, last], 51820));
        let config = |peers: Vec<SocketAddr>| {
            CorrosionConfig::local(root.path().join("data"), root.path().join("run"))
                .with_bootstrap(peers)
        };
        let first = config(vec![peer(2), peer(1), peer(3)]).install().unwrap();
        let second = config(vec![peer(3), peer(1), peer(2), peer(1)])
            .install()
            .unwrap();
        assert_eq!(first.token, second.token);
        assert_eq!(first.config, second.config);
        assert_eq!(
            fs::read(root.path().join("data/config.toml")).unwrap(),
            second.config
        );
        let encoded = String::from_utf8(second.config).unwrap();
        assert!(
            encoded
                .contains(r#"bootstrap = ["10.0.0.1:51820", "10.0.0.2:51820", "10.0.0.3:51820"]"#),
            "{encoded}"
        );
    }

    #[test]
    fn empty_token_file_from_an_interrupted_write_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join(TOKEN_FILE);
        fs::write(&path, "").unwrap();

        let token = load_or_create_token(&path).unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), token);
        assert_eq!(load_or_create_token(&path).unwrap(), token);
        assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    }

    fn writes_synchronously(path: &Path) -> bool {
        rustix::fs::ioctl_getflags(fs::File::open(path).unwrap())
            .unwrap()
            .contains(IFlags::SYNC)
    }

    /// Flags `paths` the way daemons up to 0.3.11 did, or returns false where the
    /// filesystem refuses the flag, as tmpfs and ZFS do.
    fn flag_as_an_older_daemon(paths: &[PathBuf]) -> bool {
        paths.iter().all(|path| {
            let file = fs::File::open(path).unwrap();
            rustix::fs::ioctl_getflags(&file)
                .and_then(|flags| rustix::fs::ioctl_setflags(&file, flags | IFlags::SYNC))
                .is_ok()
        })
    }

    #[test]
    fn a_store_an_older_daemon_flagged_stops_writing_every_page_synchronously() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        fs::create_dir(dir.join("subscriptions")).unwrap();
        fs::write(dir.join("subscriptions/sub.sqlite"), "").unwrap();
        fs::write(dir.join("store.db"), "").unwrap();
        let paths = [
            dir.to_path_buf(),
            dir.join("subscriptions"),
            dir.join("subscriptions/sub.sqlite"),
            dir.join("store.db"),
        ];
        let flagged = flag_as_an_older_daemon(&paths);

        let refused = clear_sync_writes(dir);

        assert!(refused.is_empty(), "{refused:?}");
        if flagged {
            for path in &paths {
                assert!(!writes_synchronously(path), "{}", path.display());
            }
        }
    }

    #[test]
    fn one_unreadable_file_leaves_the_rest_of_the_store_cleared() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        let siblings: Vec<_> = (0..20).map(|n| dir.join(format!("store-{n}.db"))).collect();
        for sibling in &siblings {
            fs::write(sibling, "").unwrap();
        }
        if !flag_as_an_older_daemon(&siblings) {
            return;
        }
        let unreadable = dir.join("unreadable");
        fs::write(&unreadable, "").unwrap();
        fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();

        let _ = clear_sync_writes(dir);

        for sibling in &siblings {
            assert!(!writes_synchronously(sibling), "{}", sibling.display());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn corrosion_start_is_bounded() {
        let error = tokio::time::timeout(
            Duration::from_secs(5 * 60),
            bounded_start(std::future::pending::<Result<(), Error>>()),
        )
        .await
        .expect("Corrosion startup must return before the systemd startup ceiling")
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("journalctl -u ployz -n 100 --no-pager")
        );
    }
}
