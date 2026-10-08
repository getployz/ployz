//! Holds Docker's log files for Ployz containers by hardlinking each one into
//! the Log Store as soon as Docker creates it.
//!
//! A hardlink keeps the inode, so every byte Docker writes to a file reaches
//! the store whenever the link was made, and Docker deleting its own name
//! later leaves the store's. The harvester never reads or copies log data.
//!
//! ```text
//! <DockerRootDir>/containers/            watched: a new <cid> dir
//!   <cid>/                               watched: local-logs appearing
//!     local-logs/                        watched: container.log created
//!       container.log  container.log.1 ...
//! ```
//!
//! Each sync is stateless: it compares Docker's files with the store's by
//! inode and links what the store lacks, so a missed event, an inotify
//! overflow, or a restart is repaired by the next sync.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    pin::Pin,
    time::{Duration, SystemTime},
};

use bollard::{
    Docker,
    errors::Error as DockerError,
    models::{ContainerInspectResponse, EventMessage},
    query_parameters::EventsOptionsBuilder,
};
use futures_util::{Stream, StreamExt};
use inotify::{EventMask, Inotify, WatchDescriptor, WatchMask, Watches};
use ployz_core::ContainerId;
use tokio::net::UnixListener;

use super::{
    cleanup::{self, Disk, Limits},
    docker_file_age, frame,
    layout::{
        ContainerKind, ContainerMeta, GAPS_FILE, Gap, GapReason, LogFileName, META_FILE, StoreRoot,
        create_private_dir,
    },
};
use crate::docker::{LABEL_HOOK, LABEL_MANAGED, LABEL_NAMESPACE, LABEL_SERVICE_NAME};

const LABEL_SYSTEM_MANAGED: &str = "ployzd.managed";
const LABEL_DEPLOYMENT: &str = "ployz.deployment.id";
const LOCAL_LOGS_DIR: &str = "local-logs";
/// Docker's default `max-file` for the `local` driver.
const DEFAULT_MAX_FILES: usize = 5;
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(60);
const EVENTS_RETRY: Duration = Duration::from_secs(2);
const DIR_MASK: WatchMask = WatchMask::CREATE.union(WatchMask::ONLYDIR);

type DockerEvents = Pin<Box<dyn Stream<Item = Result<EventMessage, DockerError>> + Send>>;

enum Watched {
    ContainersRoot,
    Container(ContainerId),
    LocalLogs(ContainerId),
}

#[derive(Clone, Copy)]
enum Seen {
    Ignored,
    Managed {
        max_files: usize,
        created_nanos: Option<i64>,
    },
}

pub(super) struct Harvester {
    docker: Docker,
    docker_containers: PathBuf,
    store: StoreRoot,
    watches: Watches,
    watched: HashMap<WatchDescriptor, Watched>,
    seen: HashMap<ContainerId, Seen>,
}

impl Harvester {
    pub(super) async fn run(
        docker: Docker,
        docker_root: &Path,
        store: StoreRoot,
        listener: UnixListener,
    ) -> io::Result<()> {
        let inotify = Inotify::init()?;
        let watches = inotify.watches();
        let mut inotify_events = inotify.into_event_stream([0_u8; 64 * 1024])?;
        let mut harvester = Self {
            docker,
            docker_containers: docker_root.join("containers"),
            store,
            watches,
            watched: HashMap::new(),
            seen: HashMap::new(),
        };
        let mut docker_events = Some(harvester.docker_events());
        harvester.rescan().await;
        harvester.clean();

        let mut maintenance = tokio::time::interval(MAINTENANCE_INTERVAL);
        maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        maintenance.tick().await;
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        loop {
            tokio::select! {
                event = inotify_events.next() => match event {
                    Some(Ok(event)) => harvester.on_inotify(event.wd, event.mask, event.name.as_deref()).await,
                    Some(Err(error)) => return Err(error),
                    None => return Err(io::Error::other("inotify stream ended")),
                },
                event = next_docker_event(&mut docker_events) => match event {
                    Some(Ok(message)) => harvester.on_docker_event(message).await,
                    Some(Err(error)) => {
                        tracing::warn!(%error, "Docker events stream failed; reconnecting");
                        docker_events = None;
                    }
                    None => docker_events = None,
                },
                () = tokio::time::sleep(EVENTS_RETRY), if docker_events.is_none() => {
                    docker_events = Some(harvester.docker_events());
                    harvester.rescan().await;
                }
                _ = maintenance.tick() => {
                    harvester.rescan().await;
                    harvester.clean();
                }
                accepted = listener.accept() => drop(accepted),
                _ = terminate.recv() => return Ok(()),
                result = tokio::signal::ctrl_c() => return result,
            }
        }
    }

    fn docker_events(&self) -> DockerEvents {
        let filters = HashMap::from([("type", vec!["container"]), ("event", vec!["start", "die"])]);
        let options = EventsOptionsBuilder::default().filters(&filters).build();
        Box::pin(self.docker.events(Some(options)))
    }

    /// Walks Docker's containers dir and syncs every container in it. This
    /// repairs anything an event missed.
    async fn rescan(&mut self) {
        if let Err(error) = self.watch(
            &self.docker_containers.clone(),
            DIR_MASK,
            Watched::ContainersRoot,
        ) {
            tracing::warn!(%error, "cannot watch Docker's containers dir");
            return;
        }
        let entries = match fs::read_dir(&self.docker_containers) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!(%error, "cannot list Docker's containers dir");
                return;
            }
        };
        let present: HashSet<ContainerId> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| container_id(&entry.file_name()))
            .collect();
        self.seen.retain(|id, _| present.contains(id));
        for id in present {
            self.discovered(id).await;
        }
    }

    async fn on_inotify(&mut self, wd: WatchDescriptor, mask: EventMask, name: Option<&OsStr>) {
        if mask.contains(EventMask::Q_OVERFLOW) {
            tracing::warn!("inotify queue overflowed; rescanning");
            self.rescan().await;
            return;
        }
        if mask.contains(EventMask::IGNORED) {
            self.watched.remove(&wd);
            return;
        }
        if !mask.contains(EventMask::CREATE) {
            return;
        }
        match self.watched.get(&wd) {
            Some(Watched::ContainersRoot) => {
                if let Some(id) = name.and_then(container_id) {
                    self.discovered(id).await;
                }
            }
            Some(Watched::Container(id)) => {
                if name == Some(OsStr::new(LOCAL_LOGS_DIR)) {
                    let id = *id;
                    self.consider(&id).await;
                }
            }
            Some(Watched::LocalLogs(id)) => {
                let id = *id;
                self.sync(&id);
            }
            None => {}
        }
    }

    async fn on_docker_event(&mut self, message: EventMessage) {
        let Some(id) = message
            .actor
            .and_then(|actor| actor.id)
            .and_then(|id| ContainerId::parse(id).ok())
        else {
            return;
        };
        if matches!(self.seen.get(&id), Some(Seen::Managed { .. })) {
            self.refresh_meta(&id).await;
        }
    }

    /// Watches a new container dir and, once Docker has made its log dir,
    /// decides whether to hold it.
    async fn discovered(&mut self, id: ContainerId) {
        if matches!(self.seen.get(&id), Some(Seen::Ignored)) {
            return;
        }
        let dir = self.docker_containers.join(id.as_str());
        if let Err(error) = self.watch(&dir, DIR_MASK, Watched::Container(id)) {
            tracing::debug!(container = %id, %error, "container dir went away");
            return;
        }
        if dir.join(LOCAL_LOGS_DIR).is_dir() {
            self.consider(&id).await;
        }
    }

    async fn consider(&mut self, id: &ContainerId) {
        if !self.seen.contains_key(id) {
            let Some(inspected) = self.inspect(id).await else {
                return;
            };
            let Some(meta) = container_meta(&inspected) else {
                self.seen.insert(*id, Seen::Ignored);
                self.unwatch_container(id);
                return;
            };
            let dir = self.store.container(id);
            if let Err(error) = create_private_dir(&dir).and_then(|()| write_meta(&dir, &meta)) {
                tracing::error!(container = %id, %error, "cannot create the container's Log Store dir");
                return;
            }
            self.seen.insert(
                *id,
                Seen::Managed {
                    max_files: max_files(&inspected),
                    created_nanos: inspected
                        .created
                        .as_ref()
                        .and_then(|at| rfc3339_nanos(&at.to_string())),
                },
            );
        }
        if !matches!(self.seen.get(id), Some(Seen::Managed { .. })) {
            return;
        }
        let local_logs = self.local_logs(id);
        if let Err(error) = self.watch(&local_logs, WatchMask::CREATE, Watched::LocalLogs(*id)) {
            tracing::debug!(container = %id, %error, "log dir went away");
            return;
        }
        self.sync(id);
    }

    fn sync(&self, id: &ContainerId) {
        let Some(Seen::Managed {
            max_files,
            created_nanos,
        }) = self.seen.get(id).copied()
        else {
            return;
        };
        match sync_container(
            &self.local_logs(id),
            &self.store.container(id),
            max_files,
            created_nanos,
        ) {
            Ok(synced) => {
                if synced.linked > 0 {
                    tracing::debug!(container = %id, linked = synced.linked, "held new log files");
                }
                if let Some(gap) = synced.gap {
                    tracing::warn!(container = %id, from = gap.from, to = gap.to, "Docker deleted log files the store never held");
                }
            }
            Err(error) => {
                tracing::error!(container = %id, %error, "cannot hold the container's log files")
            }
        }
    }

    async fn refresh_meta(&mut self, id: &ContainerId) {
        let Some(inspected) = self.inspect(id).await else {
            return;
        };
        if let Some(meta) = container_meta(&inspected)
            && let Err(error) = write_meta(&self.store.container(id), &meta)
        {
            tracing::warn!(container = %id, %error, "cannot update container metadata");
        }
    }

    async fn inspect(&self, id: &ContainerId) -> Option<ContainerInspectResponse> {
        match self.docker.inspect_container(id.as_str(), None).await {
            Ok(inspected) => Some(inspected),
            Err(DockerError::DockerResponseServerError {
                status_code: 404, ..
            }) => None,
            Err(error) => {
                tracing::warn!(container = %id, %error, "cannot inspect container");
                None
            }
        }
    }

    fn clean(&self) {
        let disk = match Disk::measure(&self.store) {
            Ok(disk) => disk,
            Err(error) => {
                tracing::warn!(%error, "cannot measure the Log Store's filesystem");
                return;
            }
        };
        let limits = Limits::for_filesystem(disk.total_bytes);
        let exists = |id: &ContainerId| self.docker_containers.join(id.as_str()).exists();
        match cleanup::run(&self.store, &limits, disk, SystemTime::now(), exists) {
            Ok(report) if report.files_deleted > 0 || report.containers_removed > 0 => {
                tracing::info!(
                    files = report.files_deleted,
                    bytes = report.bytes_freed,
                    containers = report.containers_removed,
                    "cleaned the Log Store"
                )
            }
            Ok(_) => {}
            Err(error) => tracing::warn!(%error, "cannot clean the Log Store"),
        }
    }

    fn local_logs(&self, id: &ContainerId) -> PathBuf {
        self.docker_containers
            .join(id.as_str())
            .join(LOCAL_LOGS_DIR)
    }

    fn watch(&mut self, path: &Path, mask: WatchMask, watched: Watched) -> io::Result<()> {
        let wd = self.watches.add(path, mask)?;
        self.watched.insert(wd, watched);
        Ok(())
    }

    fn unwatch_container(&mut self, id: &ContainerId) {
        let wd = self
            .watched
            .iter()
            .find(|(_, watched)| matches!(watched, Watched::Container(watched) if watched == id))
            .map(|(wd, _)| wd.clone());
        if let Some(wd) = wd {
            self.watched.remove(&wd);
            drop(self.watches.remove(wd));
        }
    }
}

async fn next_docker_event(
    events: &mut Option<DockerEvents>,
) -> Option<Result<EventMessage, DockerError>> {
    match events {
        Some(events) => events.next().await,
        None => std::future::pending().await,
    }
}

fn container_id(name: &OsStr) -> Option<ContainerId> {
    ContainerId::parse(name.to_str()?).ok()
}

/// What a sync did.
#[derive(Debug, Default, Eq, PartialEq)]
pub(super) struct Synced {
    pub linked: usize,
    pub gap: Option<Gap>,
}

struct DockerFile {
    age: u64,
    ino: u64,
    path: PathBuf,
}

/// Links every file in Docker's `local_logs` that `store_dir` does not hold,
/// oldest first, and records a gap when Docker may have deleted files the
/// store never held.
///
/// Docker deletes only its oldest file, and only once it has `max_files`.
/// So when its set is full and its oldest file is new to the store, files
/// before that one may be lost.
pub(super) fn sync_container(
    local_logs: &Path,
    store_dir: &Path,
    max_files: usize,
    created_nanos: Option<i64>,
) -> io::Result<Synced> {
    let mut docker_files = Vec::new();
    for entry in fs::read_dir(local_logs)? {
        let entry = entry?;
        let Some(age) = entry.file_name().to_str().and_then(docker_file_age) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if metadata.is_file() {
            docker_files.push(DockerFile {
                age,
                ino: metadata.ino(),
                path: entry.path(),
            });
        }
    }
    docker_files.sort_by_key(|file| std::cmp::Reverse(file.age));

    let mut held = Vec::new();
    for entry in fs::read_dir(store_dir)? {
        let entry = entry?;
        if let Some(name) = LogFileName::parse(entry.file_name().as_encoded_bytes()) {
            held.push(name);
        }
    }
    held.sort();
    let held_inos: HashSet<u64> = held.iter().map(|name| name.ino).collect();

    let mut synced = Synced::default();
    let Some(oldest) = docker_files.first() else {
        return Ok(synced);
    };
    if docker_files.len() >= max_files && !held_inos.contains(&oldest.ino) {
        let docker_inos: HashSet<u64> = docker_files.iter().map(|file| file.ino).collect();
        let before = held
            .iter()
            .rev()
            .find(|name| !docker_inos.contains(&name.ino))
            .and_then(|name| fs::read(store_dir.join(name.to_string())).ok())
            .and_then(|bytes| frame::last_ts(&bytes))
            .or(created_nanos);
        let after = fs::read(&oldest.path)
            .ok()
            .and_then(|bytes| frame::first_ts(&bytes));
        if let (Some(from), Some(to)) = (before, after)
            && to > from
        {
            let gap = Gap {
                from,
                to,
                reason: GapReason::NotCaptured,
            };
            append_gap(store_dir, &gap)?;
            synced.gap = Some(gap);
        }
    }

    let last_seq = held.last().map_or(0, |name| name.seq);
    let unheld = docker_files
        .iter()
        .filter(|file| !held_inos.contains(&file.ino));
    for (seq, file) in (last_seq + 1..).zip(unheld) {
        let name = LogFileName { seq, ino: file.ino };
        match fs::hard_link(&file.path, store_dir.join(name.to_string())) {
            Ok(()) => synced.linked += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                tracing::warn!(file = %file.path.display(), "Docker deleted a log file before the store held it");
            }
            Err(error) => return Err(error),
        }
    }
    Ok(synced)
}

fn append_gap(store_dir: &Path, gap: &Gap) -> io::Result<()> {
    use std::io::Write as _;
    let mut line = serde_json::to_vec(gap).map_err(io::Error::other)?;
    line.push(b'\n');
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(store_dir.join(GAPS_FILE))?
        .write_all(&line)
}

fn write_meta(dir: &Path, meta: &ContainerMeta) -> io::Result<()> {
    let json = serde_json::to_vec(meta).map_err(io::Error::other)?;
    crate::filesystem::atomic_write(&dir.join(META_FILE), &json, 0o600)
}

/// The container's metadata when Ployz manages it.
fn container_meta(inspected: &ContainerInspectResponse) -> Option<ContainerMeta> {
    let labels = inspected.config.as_ref()?.labels.as_ref()?;
    if !labels.contains_key(LABEL_MANAGED) && !labels.contains_key(LABEL_SYSTEM_MANAGED) {
        return None;
    }
    let kind = if labels.contains_key(LABEL_HOOK) {
        ContainerKind::PreDeployHook
    } else if labels.contains_key(LABEL_SERVICE_NAME) {
        ContainerKind::Service
    } else {
        ContainerKind::System
    };
    let state = inspected.state.as_ref();
    let finished_at = state
        .and_then(|state| state.finished_at.clone())
        .filter(|at| !at.starts_with("0001-"));
    let ended = finished_at.is_some() && state.and_then(|state| state.running) != Some(true);
    Some(ContainerMeta {
        service: labels.get(LABEL_SERVICE_NAME).cloned(),
        namespace: labels.get(LABEL_NAMESPACE).cloned(),
        deployment: labels.get(LABEL_DEPLOYMENT).cloned(),
        replica: inspected
            .name
            .as_deref()
            .unwrap_or_default()
            .trim_start_matches('/')
            .to_owned(),
        kind,
        started_at: state.and_then(|state| state.started_at.clone()),
        finished_at: finished_at.filter(|_| ended),
        exit_code: state.and_then(|state| state.exit_code).filter(|_| ended),
        oom_killed: state.and_then(|state| state.oom_killed).unwrap_or(false),
    })
}

fn max_files(inspected: &ContainerInspectResponse) -> usize {
    inspected
        .host_config
        .as_ref()
        .and_then(|host| host.log_config.as_ref())
        .and_then(|log| log.config.as_ref())
        .and_then(|config| config.get("max-file"))
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_MAX_FILES)
}

fn rfc3339_nanos(at: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(at)
        .ok()?
        .timestamp_nanos_opt()
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{Synced, sync_container};
    use crate::{
        observe::{
            frame::{Piece, Stream, tests::frame},
            layout::{Gap, GapReason},
        },
        test_dir::TestDir,
    };

    const T0: i64 = 1_760_000_000_000_000_000;

    struct Dirs {
        _root: TestDir,
        docker: PathBuf,
        store: PathBuf,
    }

    fn dirs() -> Dirs {
        let root = TestDir::new("ployzd-observe-harvest");
        let docker = root.0.join("local-logs");
        let store = root.0.join("store");
        fs::create_dir_all(&docker).unwrap();
        fs::create_dir_all(&store).unwrap();
        Dirs {
            _root: root,
            docker,
            store,
        }
    }

    impl Dirs {
        /// Rotates like Docker: `container.log.N` becomes `.N+1`, the oldest
        /// past `max_files` is deleted, and `container.log` starts fresh
        /// with one line at `ts`.
        fn rotate(&self, max_files: usize, ts: i64) {
            for n in (1..max_files).rev() {
                let from = if n == 1 {
                    self.docker.join("container.log")
                } else {
                    self.docker.join(format!("container.log.{}", n - 1))
                };
                if from.exists() {
                    fs::rename(from, self.docker.join(format!("container.log.{n}"))).unwrap();
                }
            }
            let line = frame(ts, Stream::Stdout, b"line", Piece::Whole);
            fs::write(self.docker.join("container.log"), line).unwrap();
        }

        fn sync(&self, max_files: usize) -> Synced {
            sync_container(&self.docker, &self.store, max_files, Some(T0)).unwrap()
        }

        fn stored(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.store)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .filter(|name| name.ends_with(".log"))
                .collect();
            names.sort_by_key(|name| name.split('-').next().unwrap().parse::<u64>().unwrap());
            names
        }

        fn gaps(&self) -> Vec<Gap> {
            fs::read_to_string(self.store.join("gaps.jsonl"))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
    }

    #[test]
    fn each_file_is_held_once_in_docker_order() {
        let dirs = dirs();
        dirs.rotate(3, T0 + 1);
        assert_eq!(dirs.sync(3).linked, 1);
        assert_eq!(dirs.sync(3).linked, 0);
        dirs.rotate(3, T0 + 2);
        dirs.rotate(3, T0 + 3);
        assert_eq!(dirs.sync(3).linked, 2);
        assert_eq!(dirs.stored().len(), 3);
        dirs.rotate(3, T0 + 4);
        assert_eq!(
            dirs.sync(3),
            Synced {
                linked: 1,
                gap: None
            }
        );

        let first_line_of = |name: &String| {
            crate::observe::frame::first_ts(&fs::read(dirs.store.join(name)).unwrap()).unwrap()
        };
        let order: Vec<i64> = dirs.stored().iter().map(first_line_of).collect();
        assert_eq!(order, [T0 + 1, T0 + 2, T0 + 3, T0 + 4]);
        assert!(dirs.gaps().is_empty());
    }

    #[test]
    fn files_docker_deleted_unseen_become_a_gap_between_what_the_store_holds() {
        let dirs = dirs();
        dirs.rotate(2, T0 + 1);
        dirs.sync(2);
        for ts in [T0 + 2, T0 + 3, T0 + 4] {
            dirs.rotate(2, ts);
        }
        let synced = dirs.sync(2);
        let gap = Gap {
            from: T0 + 1,
            to: T0 + 3,
            reason: GapReason::NotCaptured,
        };
        assert_eq!(
            synced,
            Synced {
                linked: 2,
                gap: Some(gap)
            }
        );
        assert_eq!(dirs.gaps(), [gap]);
        assert_eq!(dirs.sync(2), Synced::default());
    }

    #[test]
    fn a_full_set_seen_for_the_first_time_is_a_gap_from_the_container_creation() {
        let dirs = dirs();
        for ts in [T0 + 5, T0 + 6, T0 + 7] {
            dirs.rotate(2, ts);
        }
        let synced = dirs.sync(2);
        assert_eq!(synced.linked, 2);
        assert_eq!(
            synced.gap,
            Some(Gap {
                from: T0,
                to: T0 + 6,
                reason: GapReason::NotCaptured
            })
        );
    }

    #[test]
    fn a_set_docker_has_not_filled_is_never_a_gap() {
        let dirs = dirs();
        dirs.rotate(3, T0 + 5);
        dirs.rotate(3, T0 + 6);
        assert_eq!(
            dirs.sync(3),
            Synced {
                linked: 2,
                gap: None
            }
        );
    }

    #[test]
    fn docker_names_that_are_not_rotations_are_left_alone() {
        let dirs = dirs();
        dirs.rotate(3, T0 + 1);
        fs::write(dirs.docker.join("container.log.1.gz"), b"compressed").unwrap();
        fs::write(dirs.docker.join("container.log.tmp"), b"tmp").unwrap();
        assert_eq!(dirs.sync(3).linked, 1);
    }
}
