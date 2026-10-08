use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    fs,
    io::{self, Read as _, Seek as _},
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
    docker_file_age,
    frame::{self, DamageScan},
    layout::{
        ContainerKind, ContainerMeta, GAPS_FILE, Gap, GapReason, LINKING_FILE, LogFileName,
        META_FILE, StoreRoot, create_private_dir,
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
const INSPECT_TIMEOUT: Duration = Duration::from_secs(5);
const DIR_MASK: WatchMask = WatchMask::CREATE.union(WatchMask::ONLYDIR);
const SYNC_ATTEMPTS: usize = 8;
const GAP_BOUND_READ_BYTES: u64 = 1 << 20;

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
    damage: HashMap<ContainerId, DamageCursor>,
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
            damage: HashMap::new(),
        };
        let mut docker_events = Some(harvester.docker_events());
        let mut reconnect_at = None;
        harvester.rescan().await;
        harvester.close_removed_while_down();
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
                        reconnect_at = Some(tokio::time::Instant::now() + EVENTS_RETRY);
                    }
                    None => {
                        docker_events = None;
                        reconnect_at = Some(tokio::time::Instant::now() + EVENTS_RETRY);
                    }
                },
                () = tokio::time::sleep_until(reconnect_at.unwrap_or_else(tokio::time::Instant::now)), if reconnect_at.is_some() => {
                    reconnect_at = None;
                    docker_events = Some(harvester.docker_events());
                    harvester.rescan().await;
                    harvester.refresh_all_meta().await;
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

    async fn rescan(&mut self) {
        if let Err(error) = self.watch(
            &self.docker_containers.clone(),
            DIR_MASK,
            Watched::ContainersRoot,
        ) {
            tracing::warn!(%error, "cannot watch Docker's containers dir; relying on rescans");
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
        self.damage.retain(|id, _| present.contains(id));
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
            self.sync(&id);
        }
    }

    fn close_removed_while_down(&self) {
        let Ok(entries) = fs::read_dir(self.store.containers()) else {
            return;
        };
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .ok()
            .and_then(|now| i64::try_from(now.as_nanos()).ok())
            .unwrap_or(i64::MAX);
        for id in entries
            .filter_map(Result::ok)
            .filter_map(|entry| container_id(&entry.file_name()))
        {
            if !self.docker_dir_gone(&id) {
                continue;
            }
            match close_removed_container(&self.store.container(&id), now) {
                Ok(Some(gap)) => {
                    tracing::warn!(container = %id, from = gap.from, "Docker removed the container while the harvester was down");
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(container = %id, %error, "cannot check a removed container")
                }
            }
        }
    }

    async fn discovered(&mut self, id: ContainerId) {
        if matches!(self.seen.get(&id), Some(Seen::Ignored)) {
            return;
        }
        let dir = self.docker_containers.join(id.as_str());
        if let Err(error) = self.watch(&dir, DIR_MASK, Watched::Container(id)) {
            tracing::debug!(container = %id, %error, "cannot watch the container dir");
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
            if let Some(driver) = foreign_log_driver(&inspected) {
                tracing::info!(container = %id, driver, "not holding logs Docker writes with another driver");
                self.seen.insert(*id, Seen::Ignored);
                self.unwatch_container(id);
                return;
            }
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
            tracing::debug!(container = %id, %error, "cannot watch the log dir");
        }
        self.sync(id);
    }

    fn sync(&mut self, id: &ContainerId) {
        let Some(Seen::Managed {
            max_files,
            created_nanos,
        }) = self.seen.get(id).copied()
        else {
            return;
        };
        let store_dir = self.store.container(id);
        if let Err(error) = create_private_dir(&store_dir) {
            tracing::error!(container = %id, %error, "cannot recreate the container's Log Store dir");
            return;
        }
        match sync_container(&self.local_logs(id), &store_dir, max_files, created_nanos) {
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
        let cursor = self.damage.entry(*id).or_default();
        match record_damage(&store_dir, cursor, created_nanos) {
            Ok(gaps) => {
                for gap in gaps {
                    tracing::warn!(container = %id, from = gap.from, to = gap.to, "skipped log bytes that held no valid frame");
                }
            }
            Err(error) => {
                tracing::warn!(container = %id, %error, "cannot check the container's log files for damage");
            }
        }
    }

    async fn refresh_all_meta(&mut self) {
        let managed: Vec<ContainerId> = self
            .seen
            .iter()
            .filter(|(_, seen)| matches!(seen, Seen::Managed { .. }))
            .map(|(id, _)| *id)
            .collect();
        for id in managed {
            self.refresh_meta(&id).await;
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
        let inspect = self.docker.inspect_container(id.as_str(), None);
        match tokio::time::timeout(INSPECT_TIMEOUT, inspect).await {
            Ok(Ok(inspected)) => Some(inspected),
            Ok(Err(DockerError::DockerResponseServerError {
                status_code: 404, ..
            })) => None,
            Ok(Err(error)) => {
                tracing::warn!(container = %id, %error, "cannot inspect container");
                None
            }
            Err(_) => {
                tracing::warn!(container = %id, "Docker did not answer an inspect in time");
                None
            }
        }
    }

    fn docker_dir_gone(&self, id: &ContainerId) -> bool {
        matches!(
            fs::symlink_metadata(self.docker_containers.join(id.as_str())),
            Err(error) if error.kind() == io::ErrorKind::NotFound
        )
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
        let exists = |id: &ContainerId| !self.docker_dir_gone(id);
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

pub(super) fn sync_container(
    local_logs: &Path,
    store_dir: &Path,
    max_files: usize,
    created_nanos: Option<i64>,
) -> io::Result<Synced> {
    let mut synced = Synced::default();
    for _ in 0..SYNC_ATTEMPTS {
        match sync_pass(local_logs, store_dir, max_files, created_nanos, &mut synced) {
            Ok(Pass::Done) => return Ok(synced),
            Ok(Pass::Rotated) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(synced),
            Err(error) => return Err(error),
        }
    }
    tracing::warn!(dir = %local_logs.display(), "Docker kept rotating through a sync; the next one continues");
    Ok(synced)
}

enum Pass {
    Done,
    Rotated,
}

fn sync_pass(
    local_logs: &Path,
    store_dir: &Path,
    max_files: usize,
    created_nanos: Option<i64>,
    synced: &mut Synced,
) -> io::Result<Pass> {
    let mut docker_files = Vec::new();
    let mut compressed = 0;
    for entry in fs::read_dir(local_logs)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(age) = docker_file_age(name) else {
            if is_compressed_rotation(name) {
                compressed += 1;
            }
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

    let held = held_files(store_dir)?;
    let held_inos: HashSet<u64> = held.iter().map(|name| name.ino).collect();
    let Some(oldest) = docker_files.first() else {
        return Ok(Pass::Done);
    };
    let docker_inos: HashSet<u64> = docker_files.iter().map(|file| file.ino).collect();
    let maybe_lost = may_have_lost_files(oldest, &held, &docker_files, compressed, max_files);
    let before = held
        .iter()
        .rev()
        .find(|name| !docker_inos.contains(&name.ino))
        .copied();

    let mut pass = Ok(Pass::Done);
    let mut oldest_held = None;
    let last_seq = held.last().map_or(0, |name| name.seq);
    let unheld = docker_files
        .iter()
        .filter(|file| !held_inos.contains(&file.ino));
    for (seq, file) in (last_seq + 1..).zip(unheld) {
        let name = LogFileName { seq, ino: file.ino };
        match link_exact(&file.path, store_dir, name) {
            Ok(Link::Held) => {}
            Ok(Link::Rotated) => {
                pass = Ok(Pass::Rotated);
                break;
            }
            Err(error) => {
                pass = Err(error);
                break;
            }
        }
        synced.linked += 1;
        if file.ino == oldest.ino {
            oldest_held = Some(name);
        }
    }

    if maybe_lost && let Some(oldest) = oldest_held {
        let from = before
            .and_then(|name| last_ts_of(&store_dir.join(name.to_string())))
            .or(created_nanos)
            .unwrap_or(0);
        if let Some(to) = first_ts_or_mtime(&store_dir.join(oldest.to_string()))
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
    pass
}

fn may_have_lost_files(
    oldest: &DockerFile,
    held: &[LogFileName],
    docker_files: &[DockerFile],
    compressed: usize,
    max_files: usize,
) -> bool {
    if held.iter().any(|name| name.ino == oldest.ino) {
        return false;
    }
    match held.last() {
        None => compressed > 0 || docker_files.len() + compressed >= max_files,
        Some(newest) => !docker_files.iter().any(|file| file.ino == newest.ino),
    }
}

fn held_files(store_dir: &Path) -> io::Result<Vec<LogFileName>> {
    let mut held = Vec::new();
    for entry in fs::read_dir(store_dir)? {
        if let Some(name) = LogFileName::parse(entry?.file_name().as_encoded_bytes()) {
            held.push(name);
        }
    }
    held.sort();
    Ok(held)
}

#[derive(Debug, PartialEq, Eq)]
enum Link {
    Held,
    Rotated,
}

fn link_exact(source: &Path, store_dir: &Path, name: LogFileName) -> io::Result<Link> {
    let linking = store_dir.join(LINKING_FILE);
    match fs::remove_file(&linking) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    match fs::hard_link(source, &linking) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Link::Rotated),
        result => result?,
    }
    if fs::symlink_metadata(&linking)?.ino() != name.ino {
        fs::remove_file(&linking)?;
        return Ok(Link::Rotated);
    }
    fs::rename(&linking, store_dir.join(name.to_string()))?;
    Ok(Link::Held)
}

/// Where the damage scan of one container's held files has reached. A fresh
/// cursor, after a restart, starts at the newest held file: the scan read every
/// older one before it moved past it.
#[derive(Default)]
pub(super) struct DamageCursor(Option<(LogFileName, DamageScan)>);

pub(super) fn record_damage(
    store_dir: &Path,
    cursor: &mut DamageCursor,
    created_nanos: Option<i64>,
) -> io::Result<Vec<Gap>> {
    let held = held_files(store_dir)?;
    let start = match &cursor.0 {
        Some((file, _)) => held.iter().position(|name| name.seq >= file.seq),
        None => held.len().checked_sub(1),
    };
    let Some((start, &first)) = start.and_then(|start| Some((start, held.get(start)?))) else {
        return Ok(Vec::new());
    };
    let mut scan = match cursor.0.take() {
        Some((file, scan)) if first == file => scan,
        Some((_, mut scan)) => {
            scan.next_file();
            scan
        }
        None => {
            let previous = start
                .checked_sub(1)
                .and_then(|index| held.get(index))
                .and_then(|name| last_ts_of(&store_dir.join(name.to_string())));
            DamageScan::after(previous.or(created_nanos))
        }
    };
    let mut found = Vec::new();
    for (index, name) in held.iter().enumerate().skip(start) {
        if index > start {
            scan.next_file();
        }
        let finished = index + 1 < held.len();
        let mut opened = fs::File::open(store_dir.join(name.to_string()))?;
        scan.read(&mut opened, finished, &mut found)?;
    }
    if let Some(newest) = held.last() {
        cursor.0 = Some((*newest, scan));
    }

    let mut recorded = Vec::new();
    for damage in found {
        let gap = Gap {
            from: damage.after.unwrap_or(damage.before),
            to: damage.before,
            reason: GapReason::Corrupt,
        };
        if !gap_recorded(store_dir, &gap)? {
            append_gap(store_dir, &gap)?;
            recorded.push(gap);
        }
    }
    Ok(recorded)
}

fn gap_recorded(store_dir: &Path, gap: &Gap) -> io::Result<bool> {
    let recorded = match fs::read_to_string(store_dir.join(GAPS_FILE)) {
        Ok(recorded) => recorded,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    Ok(recorded
        .lines()
        .filter_map(|line| serde_json::from_str::<Gap>(line).ok())
        .any(|line| line == *gap))
}

fn is_compressed_rotation(name: &str) -> bool {
    name.strip_suffix(".gz")
        .and_then(docker_file_age)
        .is_some_and(|age| age > 0)
}

fn first_ts_or_mtime(path: &Path) -> Option<i64> {
    let file = fs::File::open(path).ok()?;
    let mut head = Vec::new();
    (&file)
        .take(GAP_BOUND_READ_BYTES)
        .read_to_end(&mut head)
        .ok()?;
    frame::first_ts(&head).or_else(|| {
        let metadata = file.metadata().ok()?;
        metadata
            .mtime()
            .checked_mul(1_000_000_000)?
            .checked_add(metadata.mtime_nsec())
    })
}

fn last_ts_of(path: &Path) -> Option<i64> {
    let mut file = fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    file.seek(io::SeekFrom::Start(
        len.saturating_sub(GAP_BOUND_READ_BYTES),
    ))
    .ok()?;
    let mut tail = Vec::new();
    file.take(GAP_BOUND_READ_BYTES)
        .read_to_end(&mut tail)
        .ok()?;
    frame::last_ts(&tail)
}

pub(super) fn close_removed_container(store_dir: &Path, now: i64) -> io::Result<Option<Gap>> {
    let meta: ContainerMeta = match fs::read(store_dir.join(META_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if meta.finished_at.is_some() {
        return Ok(None);
    }
    let Some(from) = held_files(store_dir)?
        .last()
        .and_then(|newest| last_ts_of(&store_dir.join(newest.to_string())))
    else {
        return Ok(None);
    };
    let recorded = fs::read_to_string(store_dir.join(GAPS_FILE)).unwrap_or_default();
    let already = recorded
        .lines()
        .filter_map(|line| serde_json::from_str::<Gap>(line).ok())
        .any(|gap| gap.from == from);
    if already || now <= from {
        return Ok(None);
    }
    let gap = Gap {
        from,
        to: now,
        reason: GapReason::NotCaptured,
    };
    append_gap(store_dir, &gap)?;
    Ok(Some(gap))
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

fn foreign_log_driver(inspected: &ContainerInspectResponse) -> Option<&str> {
    let driver = inspected
        .host_config
        .as_ref()
        .and_then(|host| host.log_config.as_ref())
        .and_then(|log| log.typ.as_deref())
        .unwrap_or("");
    (driver != "local").then_some(driver)
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
    use std::{fs, os::unix::fs::MetadataExt, path::PathBuf};

    use bollard::models::{ContainerInspectResponse, HostConfig, HostConfigLogConfig};

    use super::{
        DamageCursor, Link, Synced, close_removed_container, foreign_log_driver, link_exact,
        record_damage, sync_container, write_meta,
    };
    use crate::{
        observe::{
            frame::{Piece, Stream, tests::frame},
            layout::{ContainerKind, ContainerMeta, Gap, GapReason, LINKING_FILE, LogFileName},
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

    #[test]
    fn compressed_rotations_fill_the_set() {
        let dirs = dirs();
        dirs.rotate(3, T0 + 5);
        fs::write(dirs.docker.join("container.log.1.gz"), b"gz").unwrap();
        fs::write(dirs.docker.join("container.log.2.gz"), b"gz").unwrap();
        let synced = dirs.sync(3);
        assert_eq!(synced.linked, 1);
        assert_eq!(
            synced.gap,
            Some(Gap {
                from: T0,
                to: T0 + 5,
                reason: GapReason::NotCaptured
            })
        );
    }

    #[test]
    fn a_held_file_compressed_away_before_a_set_fills_still_bounds_a_gap() {
        let dirs = dirs();
        dirs.rotate(5, T0 + 1);
        dirs.sync(5);
        for ts in [T0 + 2, T0 + 3] {
            let raw = dirs.docker.join("container.log.1");
            if raw.exists() {
                fs::remove_file(raw).unwrap();
            }
            dirs.rotate(5, ts);
        }
        fs::remove_file(dirs.docker.join("container.log.1")).unwrap();
        fs::write(dirs.docker.join("container.log.1.gz"), b"gz").unwrap();
        fs::write(dirs.docker.join("container.log.2.gz"), b"gz").unwrap();

        let synced = dirs.sync(5);
        assert_eq!(synced.linked, 1);
        assert_eq!(
            synced.gap,
            Some(Gap {
                from: T0 + 1,
                to: T0 + 3,
                reason: GapReason::NotCaptured
            })
        );
    }

    #[test]
    fn an_oldest_file_with_no_line_yet_bounds_the_gap_by_its_mtime() {
        let dirs = dirs();
        fs::write(dirs.docker.join("container.log"), b"").unwrap();
        let synced = dirs.sync(1);
        assert_eq!(synced.linked, 1);
        let gap = synced.gap.unwrap();
        assert_eq!(gap.from, T0);
        assert!(gap.to > T0);
        assert_eq!(dirs.sync(1), Synced::default());
    }

    fn lines(first: i64, count: i64) -> Vec<u8> {
        (first..first + count)
            .flat_map(|ts| frame(ts, Stream::Stdout, b"line", Piece::Whole))
            .collect()
    }

    fn torn(ts: i64) -> Vec<u8> {
        frame(ts, Stream::Stdout, &[b'x'; 300], Piece::Whole)
            .get(..120)
            .unwrap()
            .to_vec()
    }

    #[test]
    fn a_torn_frame_in_the_file_docker_is_writing_is_one_corrupt_gap() {
        let dirs = dirs();
        let active = dirs.docker.join("container.log");
        fs::write(&active, [lines(T0 + 1, 20), torn(T0 + 99)].concat()).unwrap();
        dirs.sync(3);
        let mut cursor = DamageCursor::default();
        assert_eq!(
            record_damage(&dirs.store, &mut cursor, Some(T0)).unwrap(),
            []
        );

        let mut grown = fs::read(&active).unwrap();
        grown.extend(lines(T0 + 21, 20));
        fs::write(&active, grown).unwrap();
        let gap = Gap {
            from: T0 + 20,
            to: T0 + 21,
            reason: GapReason::Corrupt,
        };
        assert_eq!(
            record_damage(&dirs.store, &mut cursor, Some(T0)).unwrap(),
            [gap]
        );
        assert_eq!(
            record_damage(&dirs.store, &mut cursor, Some(T0)).unwrap(),
            []
        );

        let mut restarted = DamageCursor::default();
        assert_eq!(
            record_damage(&dirs.store, &mut restarted, Some(T0)).unwrap(),
            []
        );
        assert_eq!(dirs.gaps(), [gap]);
    }

    #[test]
    fn a_torn_end_of_a_rotated_file_is_bounded_by_the_next_file() {
        let dirs = dirs();
        let active = dirs.docker.join("container.log");
        fs::write(&active, lines(T0 + 1, 10)).unwrap();
        dirs.sync(3);
        let mut cursor = DamageCursor::default();
        record_damage(&dirs.store, &mut cursor, Some(T0)).unwrap();

        let mut torn_end = fs::read(&active).unwrap();
        torn_end.extend(torn(T0 + 99));
        fs::write(&active, torn_end).unwrap();
        dirs.rotate(3, T0 + 11);
        dirs.sync(3);
        assert_eq!(
            record_damage(&dirs.store, &mut cursor, Some(T0)).unwrap(),
            [Gap {
                from: T0 + 10,
                to: T0 + 11,
                reason: GapReason::Corrupt,
            }]
        );
    }

    #[test]
    fn a_name_that_no_longer_holds_the_listed_inode_is_not_linked() {
        let dirs = dirs();
        dirs.rotate(3, T0 + 1);
        let source = dirs.docker.join("container.log");
        let wrong = LogFileName {
            seq: 1,
            ino: fs::metadata(&source).unwrap().ino() + 1,
        };
        assert_eq!(
            link_exact(&source, &dirs.store, wrong).unwrap(),
            Link::Rotated
        );
        assert!(dirs.stored().is_empty());
        assert!(!dirs.store.join(LINKING_FILE).exists());
    }

    #[test]
    fn a_container_removed_unseen_gets_one_gap_after_its_last_held_line() {
        let dirs = dirs();
        dirs.rotate(3, T0 + 1);
        dirs.sync(3);
        let meta = |finished_at: Option<&str>| ContainerMeta {
            service: None,
            namespace: None,
            deployment: None,
            replica: "web".to_owned(),
            kind: ContainerKind::Service,
            started_at: None,
            finished_at: finished_at.map(str::to_owned),
            exit_code: None,
            oom_killed: false,
        };
        write_meta(&dirs.store, &meta(Some("2026-01-01T00:00:00Z"))).unwrap();
        assert_eq!(close_removed_container(&dirs.store, T0 + 9).unwrap(), None);

        write_meta(&dirs.store, &meta(None)).unwrap();
        let gap = Gap {
            from: T0 + 1,
            to: T0 + 9,
            reason: GapReason::NotCaptured,
        };
        assert_eq!(
            close_removed_container(&dirs.store, T0 + 9).unwrap(),
            Some(gap)
        );
        assert_eq!(close_removed_container(&dirs.store, T0 + 10).unwrap(), None);
        assert_eq!(dirs.gaps(), [gap]);
    }

    fn logging_with(driver: Option<&str>) -> ContainerInspectResponse {
        ContainerInspectResponse {
            host_config: Some(HostConfig {
                log_config: Some(HostConfigLogConfig {
                    typ: driver.map(str::to_owned),
                    config: None,
                }),
                ..HostConfig::default()
            }),
            ..ContainerInspectResponse::default()
        }
    }

    #[test]
    fn containers_still_on_another_driver_are_skipped() {
        assert_eq!(foreign_log_driver(&logging_with(Some("local"))), None);
        assert_eq!(
            foreign_log_driver(&logging_with(Some("json-file"))),
            Some("json-file")
        );
        assert_eq!(foreign_log_driver(&logging_with(None)), Some(""));
    }

    #[test]
    fn a_compressed_local_set_holds_raw_files_and_marks_the_gz_history_not_captured() {
        let dirs = dirs();
        dirs.rotate(5, T0 + 3);
        fs::write(
            dirs.docker.join("container.log.1.gz"),
            b"\x1f\x8bnot a frame",
        )
        .unwrap();
        fs::write(
            dirs.docker.join("container.log.2.gz"),
            b"\x1f\x8bnot a frame",
        )
        .unwrap();

        let synced = dirs.sync(5);
        assert_eq!(synced.linked, 1);
        assert_eq!(dirs.stored().len(), 1);
        let gap = Gap {
            from: T0,
            to: T0 + 3,
            reason: GapReason::NotCaptured,
        };
        assert_eq!(synced.gap, Some(gap));

        dirs.rotate(5, T0 + 4);
        assert_eq!(
            dirs.sync(5),
            Synced {
                linked: 1,
                gap: None
            }
        );
        assert_eq!(dirs.gaps(), [gap]);
    }
}
