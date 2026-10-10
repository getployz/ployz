//! One page of history from the Log Store.
//!
//! Every row has a key, `(ts, container, seq, n)`: `seq` is the stored
//! file's seq plus one (gaps take 0, the exit takes `u64::MAX`), and `n`
//! ranks rows of equal `ts` within one file. Each file decodes on its own,
//! so a row's key depends only on its file and paging never repeats or skips
//! a row. A line Docker split across a rotation reads as two rows. The
//! unfinished line at the end of a running container's newest file waits
//! until it ends, so a later page reads it whole. A joined line takes the
//! timestamp of its last chunk, so it sorts after every cursor issued while
//! it waited.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BinaryHeap, HashMap},
    fmt, fs,
    io::{self, Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    str::FromStr,
    sync::{
        Mutex, PoisonError,
        atomic::{self, AtomicBool, AtomicUsize},
    },
};

use ployz_core::{
    ContainerId, HistoryContainer, HistoryContainerKind, HistoryGapReason, HistoryRow,
    HistoryStream, LogDirection, LogHistoryRequest,
};

use super::{
    frame::{self, Event, Frames, Reassembler, Stream},
    layout::{
        ContainerKind, ContainerMeta, GAPS_FILE, Gap, GapReason, LogFileName, META_FILE, StoreRoot,
    },
};

/// Enough of a file's head or tail to hold a whole frame of the largest
/// size Docker writes, plus an incomplete one after it.
const BOUND_WINDOW: u64 = 2 << 20;
const BOUND_PROBE: u64 = 32 << 10;
const CANCEL_EVERY: usize = 4096;
/// The text one page holds at most, past its first row. The page ends early,
/// with a cursor, rather than outgrow the observe service's memory.
const PAGE_BYTES: usize = 16 << 20;
/// What a row costs on top of its text.
const ROW_BYTES: usize = 64;
/// How many files' bounds [`Bounds`] keeps before it starts over.
const BOUNDS_KEPT: usize = 1 << 16;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Key {
    ts: i64,
    container: ContainerId,
    seq: u64,
    n: u64,
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}:{}:{}", self.ts, self.container, self.seq, self.n)
    }
}

impl FromStr for Key {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, String> {
        let invalid = || format!("cursor {value:?} did not come from a history page");
        let mut parts = value.split(':');
        let mut next = || parts.next().ok_or_else(invalid);
        let key = Self {
            ts: next()?.parse().map_err(|_| invalid())?,
            container: ContainerId::parse(next()?).map_err(|_| invalid())?,
            seq: next()?.parse().map_err(|_| invalid())?,
            n: next()?.parse().map_err(|_| invalid())?,
        };
        if parts.next().is_some() {
            return Err(invalid());
        }
        Ok(key)
    }
}

pub struct Query {
    namespace: Option<String>,
    service: Option<String>,
    deployment: Option<String>,
    container: Option<ContainerId>,
    /// Inclusive.
    since: i64,
    /// Exclusive.
    until: i64,
    backward: bool,
    limit: usize,
    bytes: usize,
    cursor: Option<Key>,
}

impl Query {
    /// # Errors
    ///
    /// Names the request field outside the contract.
    pub fn new(request: LogHistoryRequest) -> Result<Self, String> {
        request.validate()?;
        let cursor = request.cursor.as_deref().map(str::parse).transpose()?;
        Ok(Self {
            namespace: request.namespace,
            service: request.service,
            deployment: request.deployment,
            container: request.container_id,
            since: request.since_nanos.unwrap_or(i64::MIN),
            until: request.until_nanos.unwrap_or(i64::MAX),
            backward: request.direction == LogDirection::Backward,
            limit: usize::from(request.limit),
            bytes: PAGE_BYTES,
            cursor,
        })
    }

    fn selects(&self, id: &ContainerId, meta: &ContainerMeta) -> bool {
        let matches =
            |wanted: &Option<String>, actual: &Option<String>| wanted.is_none() || wanted == actual;
        self.container.is_none_or(|wanted| wanted == *id)
            && matches(&self.namespace, &meta.namespace)
            && matches(&self.service, &meta.service)
            && matches(&self.deployment, &meta.deployment)
    }
}

/// Rows in the query's direction, each container's [`HistoryRow::Container`]
/// before its first row, and the cursor for the next page.
pub struct Page {
    pub rows: Vec<HistoryRow>,
    pub next: Option<String>,
}

/// Reads one page.
///
/// # Errors
///
/// Returns [`io::ErrorKind::Interrupted`] once `cancel` is set, and an error
/// when the store's containers dir cannot be listed. A file or container
/// cleanup deletes mid-read is skipped.
pub fn page(
    store: &StoreRoot,
    query: &Query,
    known: &Bounds,
    cancel: &AtomicBool,
) -> io::Result<Page> {
    check(cancel)?;
    let mut containers = Vec::new();
    for entry in fs::read_dir(store.containers())? {
        check(cancel)?;
        let entry = entry?;
        let Some(id) = entry
            .file_name()
            .to_str()
            .and_then(|name| ContainerId::parse(name).ok())
        else {
            continue;
        };
        let dir = entry.path();
        let meta = match read_meta(&dir) {
            Ok(Some(meta)) if query.selects(&id, &meta) => meta,
            Ok(_) => continue,
            Err(error) => {
                tracing::warn!(container = %id, %error, "cannot read stored container metadata; leaving it out of history");
                continue;
            }
        };
        let files = match known.files(&dir, cancel) {
            Ok(files) => files,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        containers.push(Stored {
            id,
            dir,
            meta,
            files,
        });
    }
    if query.backward {
        containers.sort_by_key(|stored| std::cmp::Reverse(stored.newest()));
    } else {
        containers.sort_by_key(Stored::oldest);
    }

    let mut best = Best::new(query);
    let mut described = BTreeMap::new();
    for stored in &containers {
        check(cancel)?;
        described.insert(stored.id, describe(&stored.id, &stored.meta));
        stored.read_gaps(query, &mut best);
        stored.read_exit(&mut best);
        stored.read_files(query, &mut best, cancel)?;
    }

    let cut = best.cut;
    let mut ranked = best.heap.into_vec();
    ranked.sort();
    let next = cut.and(ranked.last()).map(|ranked| ranked.key.to_string());
    let mut rows = Vec::with_capacity(ranked.len());
    let mut introduced = std::collections::HashSet::new();
    for ranked in ranked {
        if introduced.insert(ranked.key.container)
            && let Some(container) = described.remove(&ranked.key.container)
        {
            rows.push(HistoryRow::Container(container));
        }
        rows.push(ranked.row);
    }
    Ok(Page { rows, next })
}

fn check(cancel: &AtomicBool) -> io::Result<()> {
    if cancel.load(atomic::Ordering::Relaxed) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "the reader went away",
        ))
    } else {
        Ok(())
    }
}

struct Stored {
    id: ContainerId,
    dir: PathBuf,
    meta: ContainerMeta,
    files: Vec<StoredFile>,
}

struct StoredFile {
    path: PathBuf,
    seq: u64,
    first: i64,
    last: i64,
}

impl Stored {
    fn oldest(&self) -> i64 {
        self.files
            .iter()
            .map(|file| file.first)
            .min()
            .unwrap_or(i64::MAX)
    }

    fn newest(&self) -> i64 {
        self.files
            .iter()
            .map(|file| file.last)
            .max()
            .unwrap_or(i64::MIN)
    }

    fn read_gaps(&self, query: &Query, best: &mut Best<'_>) {
        let recorded = match fs::read_to_string(self.dir.join(GAPS_FILE)) {
            Ok(recorded) => recorded,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return,
            Err(error) => {
                tracing::warn!(container = %self.id, %error, "cannot read stored gaps");
                return;
            }
        };
        for (n, gap) in recorded
            .lines()
            .filter_map(|line| serde_json::from_str::<Gap>(line).ok())
            .enumerate()
        {
            if gap.from >= query.until || gap.to < query.since {
                continue;
            }
            let key = Key {
                ts: gap.from.max(query.since),
                container: self.id,
                seq: 0,
                n: n as u64,
            };
            best.offer(key, || HistoryRow::Gap {
                container_id: self.id,
                from: gap.from,
                to: gap.to,
                reason: match gap.reason {
                    GapReason::NotCaptured => HistoryGapReason::NotCaptured,
                    GapReason::Corrupt => HistoryGapReason::Corrupt,
                },
            });
        }
    }

    fn read_exit(&self, best: &mut Best<'_>) {
        let Some(ts) = self
            .meta
            .finished_at
            .as_deref()
            .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
            .and_then(|at| at.timestamp_nanos_opt())
        else {
            return;
        };
        let key = Key {
            ts,
            container: self.id,
            seq: u64::MAX,
            n: 0,
        };
        best.offer(key, || HistoryRow::Exit {
            container_id: self.id,
            ts,
            exit_code: self.meta.exit_code,
            oom_killed: self.meta.oom_killed,
        });
    }

    fn read_files(
        &self,
        query: &Query,
        best: &mut Best<'_>,
        cancel: &AtomicBool,
    ) -> io::Result<()> {
        let order: Box<dyn Iterator<Item = &StoredFile>> = if query.backward {
            Box::new(self.files.iter().rev())
        } else {
            Box::new(self.files.iter())
        };
        let newest = self.files.last().map(|file| file.seq);
        for file in order {
            check(cancel)?;
            // Nothing more lands in a rotated file or a stopped container's.
            let ended = Some(file.seq) != newest || self.meta.finished_at.is_some();
            if best.skips(&self.id, file) {
                continue;
            }
            let bytes = match fs::read(&file.path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for (index, (n, line)) in lines(&bytes, ended).into_iter().enumerate() {
                if index % CANCEL_EVERY == 0 {
                    check(cancel)?;
                }
                let key = Key {
                    ts: line.ts,
                    container: self.id,
                    seq: file.seq + 1,
                    n,
                };
                best.offer(key, || HistoryRow::Line {
                    container_id: self.id,
                    ts: line.ts,
                    stream: match line.stream {
                        Stream::Stdout => HistoryStream::Stdout,
                        Stream::Stderr => HistoryStream::Stderr,
                    },
                    text: line.text,
                });
            }
        }
        Ok(())
    }
}

/// A file's lines by timestamp, each with its rank among equal timestamps.
/// The unfinished line at its end counts only once the file has `ended`.
/// A line takes the timestamp of its last chunk.
fn lines(bytes: &[u8], ended: bool) -> Vec<(u64, frame::Line)> {
    let mut reassembler = Reassembler::default();
    let mut lines = Vec::new();
    let (mut stdout, mut stderr) = (i64::MIN, i64::MIN);
    for event in Frames::new(bytes) {
        if let Event::Entry(entry) = event {
            match entry.stream {
                Stream::Stdout => stdout = entry.ts,
                Stream::Stderr => stderr = entry.ts,
            }
            // Only a chunk of its own stream ends a line.
            reassembler.push(&entry, |line| {
                lines.push(frame::Line {
                    ts: entry.ts,
                    ..line
                });
            });
        }
    }
    if ended {
        lines.extend(reassembler.finish().map(|line| frame::Line {
            ts: match line.stream {
                Stream::Stdout => stdout,
                Stream::Stderr => stderr,
            },
            ..line
        }));
    }
    lines.sort_by_key(|line| line.ts);
    let mut previous = None;
    let mut n = 0;
    lines
        .into_iter()
        .map(|line| {
            n = if previous == Some(line.ts) { n + 1 } else { 0 };
            previous = Some(line.ts);
            (n, line)
        })
        .collect()
}

/// Each stored file's first and last frame timestamps, kept between pages
/// so a page opens only the files it reads. A file only grows, so a bound
/// stays good while its length does.
#[derive(Default)]
pub struct Bounds {
    known: Mutex<HashMap<PathBuf, Known>>,
    reads: AtomicUsize,
}

#[derive(Clone, Copy)]
struct Known {
    len: u64,
    first: i64,
    last: i64,
}

impl Bounds {
    fn files(&self, dir: &Path, cancel: &AtomicBool) -> io::Result<Vec<StoredFile>> {
        let mut files = Vec::new();
        for entry in fs::read_dir(dir)? {
            check(cancel)?;
            let entry = entry?;
            let Some(name) = LogFileName::parse(entry.file_name().as_encoded_bytes()) else {
                continue;
            };
            let path = entry.path();
            let (first, last) = match self.of(&path) {
                Ok(bounds) => bounds,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            files.push(StoredFile {
                path,
                seq: name.seq,
                first,
                last,
            });
        }
        files.sort_by_key(|file| file.seq);
        Ok(files)
    }

    fn of(&self, path: &Path) -> io::Result<(i64, i64)> {
        let len = fs::metadata(path)?.len();
        let known = self
            .known
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(path)
            .copied();
        if let Some(known) = known
            && known.len == len
        {
            return Ok((known.first, known.last));
        }
        self.reads.fetch_add(1, atomic::Ordering::Relaxed);
        let (first, last) = bounds(path)?;
        let mut kept = self.known.lock().unwrap_or_else(PoisonError::into_inner);
        if kept.len() >= BOUNDS_KEPT {
            kept.clear();
        }
        kept.insert(path.to_owned(), Known { len, first, last });
        Ok((first, last))
    }
}

/// The first and last frame timestamps, read from the file's ends. A bound
/// the window cannot show is unbounded, so the file is never skipped wrongly.
fn bounds(path: &Path) -> io::Result<(i64, i64)> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut head = Vec::new();
    (&mut file).take(BOUND_PROBE).read_to_end(&mut head)?;
    // A probe inside a large frame can contain another valid frame in its text.
    let first = match Frames::new(&head).next() {
        Some(Event::Entry(entry)) => entry.ts,
        _ => {
            (&mut file)
                .take(BOUND_WINDOW - head.len() as u64)
                .read_to_end(&mut head)?;
            frame::first_ts(&head).unwrap_or(i64::MIN)
        }
    };
    if len <= head.len() as u64 {
        return Ok((first, frame::last_ts(&head).unwrap_or(i64::MAX)));
    }
    file.seek(SeekFrom::Start(len.saturating_sub(BOUND_PROBE)))?;
    let mut tail = Vec::new();
    (&mut file).take(BOUND_PROBE).read_to_end(&mut tail)?;
    let last = tail.last_chunk::<4>().and_then(|footer| {
        let size = usize::try_from(u32::from_be_bytes(*footer)).ok()?;
        let start = tail.len().checked_sub(size.checked_add(8)?)?;
        let bytes = tail.get(start..)?;
        if bytes.get(..4)? != footer {
            return None;
        }
        match Frames::new(bytes).next() {
            Some(Event::Entry(entry)) => Some(entry.ts),
            _ => None,
        }
    });
    if let Some(last) = last {
        return Ok((first, last));
    }
    file.seek(SeekFrom::Start(len.saturating_sub(BOUND_WINDOW)))?;
    tail.clear();
    file.take(BOUND_WINDOW).read_to_end(&mut tail)?;
    Ok((first, frame::last_ts(&tail).unwrap_or(i64::MAX)))
}

fn read_meta(dir: &Path) -> io::Result<Option<ContainerMeta>> {
    match fs::read(dir.join(META_FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn describe(id: &ContainerId, meta: &ContainerMeta) -> HistoryContainer {
    HistoryContainer {
        container_id: *id,
        namespace: meta.namespace.clone(),
        service: meta.service.clone(),
        deployment: meta.deployment.clone(),
        replica: meta.replica.clone(),
        kind: match meta.kind {
            ContainerKind::Service => HistoryContainerKind::Service,
            ContainerKind::PreDeployHook => HistoryContainerKind::PreDeployHook,
            ContainerKind::System => HistoryContainerKind::System,
        },
    }
}

/// The best rows seen so far, at most `limit` of them and `bytes` of text
/// past the first; the heap's top is the worst of them.
struct Best<'q> {
    query: &'q Query,
    heap: BinaryHeap<Ranked>,
    bytes: usize,
    /// The best row left out for room, or a file's first row where a whole
    /// file was. Every row the page keeps is better.
    cut: Option<Key>,
}

struct Ranked {
    key: Key,
    backward: bool,
    row: HistoryRow,
}

impl Ranked {
    fn bytes(&self) -> usize {
        ROW_BYTES
            + match &self.row {
                HistoryRow::Line { text, .. } => text.len(),
                HistoryRow::Container(_)
                | HistoryRow::Gap { .. }
                | HistoryRow::Exit { .. }
                | HistoryRow::End { .. }
                | HistoryRow::Heartbeat
                | HistoryRow::Error(_) => 0,
            }
    }
}

impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.backward {
            other.key.cmp(&self.key)
        } else {
            self.key.cmp(&other.key)
        }
    }
}

impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}

impl Eq for Ranked {}

impl<'q> Best<'q> {
    fn new(query: &'q Query) -> Self {
        Self {
            query,
            heap: BinaryHeap::with_capacity(query.limit + 1),
            bytes: 0,
            cut: None,
        }
    }

    /// The key a row must beat to enter the page.
    fn edge(&self) -> Option<Key> {
        (self.heap.len() >= self.query.limit)
            .then(|| self.heap.peek().map(|ranked| ranked.key))
            .flatten()
            .or(self.cut)
    }

    fn better(&self, a: &Key, b: &Key) -> bool {
        if self.query.backward { a > b } else { a < b }
    }

    fn in_range(&self, key: &Key) -> bool {
        (self.query.since..self.query.until).contains(&key.ts)
            && self
                .query
                .cursor
                .is_none_or(|cursor| self.better(&cursor, key))
    }

    fn cut(&mut self, key: Key) {
        if self.cut.is_none_or(|cut| self.better(&key, &cut)) {
            self.cut = Some(key);
        }
    }

    fn offer(&mut self, key: Key, row: impl FnOnce() -> HistoryRow) {
        if !self.in_range(&key) {
            return;
        }
        if self.edge().is_some_and(|edge| !self.better(&key, &edge)) {
            self.cut(key);
            return;
        }
        let ranked = Ranked {
            key,
            backward: self.query.backward,
            row: row(),
        };
        self.bytes += ranked.bytes();
        self.heap.push(ranked);
        while self.heap.len() > self.query.limit
            || (self.bytes > self.query.bytes && self.heap.len() > 1)
        {
            let Some(worst) = self.heap.pop() else { break };
            self.bytes -= worst.bytes();
            self.cut(worst.key);
        }
    }

    /// Whether no row of `file` can enter the page. A file left out for
    /// room counts as cut, so the page still gets a cursor.
    fn skips(&mut self, container: &ContainerId, file: &StoredFile) -> bool {
        let query = self.query;
        let (mut low, mut high) = (query.since, query.until);
        if let Some(cursor) = query.cursor {
            if query.backward {
                high = high.min(cursor.ts.saturating_add(1));
            } else {
                low = low.max(cursor.ts);
            }
        }
        if file.last < low || file.first >= high {
            return true;
        }
        let Some(edge) = self.edge() else {
            return false;
        };
        let (beyond, ts, n) = if query.backward {
            (file.last < edge.ts, file.last, u64::MAX)
        } else {
            (file.first > edge.ts, file.first, 0)
        };
        if beyond {
            self.cut(Key {
                ts,
                container: *container,
                seq: file.seq + 1,
                n,
            });
        }
        beyond
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::AtomicBool;

    use ployz_core::{
        ContainerId, HistoryGapReason, HistoryRow, HistoryStream, LogDirection, LogHistoryRequest,
    };

    use super::{Bounds, Query, ROW_BYTES, page};
    use crate::{
        observe::{
            frame::{Piece, Stream, tests::frame},
            layout::{ContainerKind, ContainerMeta, Gap, GapReason, LogFileName, StoreRoot},
        },
        test_dir::TestDir,
    };

    pub(crate) const T0: i64 = 1_760_000_000_000_000_000;

    pub(crate) struct Store {
        _dir: TestDir,
        pub(crate) root: StoreRoot,
        bounds: Bounds,
    }

    impl Store {
        pub(crate) fn new() -> Self {
            let dir = TestDir::new("ployzd-observe-query");
            std::fs::create_dir_all(&dir.0).unwrap();
            let root = StoreRoot::under(&dir.0);
            root.prepare().unwrap();
            Self {
                _dir: dir,
                root,
                bounds: Bounds::default(),
            }
        }

        pub(crate) fn container(&self, hex: char, service: &str) -> ContainerId {
            let id = ContainerId::parse(hex.to_string().repeat(64)).unwrap();
            let dir = self.root.container(&id);
            std::fs::create_dir(&dir).unwrap();
            let meta = ContainerMeta {
                service: Some(service.into()),
                namespace: Some("prod".into()),
                deployment: Some(format!("dep-{service}")),
                replica: format!("{service}-1"),
                kind: ContainerKind::Service,
                started_at: None,
                finished_at: None,
                exit_code: None,
                oom_killed: false,
            };
            std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
            id
        }

        pub(crate) fn file(&self, id: &ContainerId, seq: u64, frames: &[Vec<u8>]) {
            std::fs::write(self.path(id, seq), frames.concat()).unwrap();
        }

        fn path(&self, id: &ContainerId, seq: u64) -> std::path::PathBuf {
            let name = LogFileName {
                seq,
                ino: seq + 100,
            };
            self.root.container(id).join(name.to_string())
        }

        fn append(&self, id: &ContainerId, seq: u64, bytes: &[u8]) {
            use std::io::Write as _;
            std::fs::OpenOptions::new()
                .append(true)
                .open(self.path(id, seq))
                .unwrap()
                .write_all(bytes)
                .unwrap();
        }

        fn page(&self, request: LogHistoryRequest) -> super::Page {
            self.query(&Query::new(request).unwrap())
        }

        fn query(&self, query: &Query) -> super::Page {
            page(&self.root, query, &self.bounds, &AtomicBool::new(false)).unwrap()
        }
    }

    pub(crate) fn line(ts: i64, text: &str) -> Vec<u8> {
        frame(ts, Stream::Stdout, text.as_bytes(), Piece::Whole)
    }

    fn request(direction: LogDirection, limit: u16) -> LogHistoryRequest {
        LogHistoryRequest {
            namespace: Some("prod".into()),
            direction,
            limit,
            ..LogHistoryRequest::default()
        }
    }

    fn texts(rows: &[HistoryRow]) -> Vec<String> {
        rows.iter()
            .filter_map(|row| match row {
                HistoryRow::Line { text, .. } => Some(String::from_utf8(text.clone()).unwrap()),
                HistoryRow::Gap { .. } => Some("gap".into()),
                HistoryRow::Exit { .. } => Some("exit".into()),
                HistoryRow::Container(_)
                | HistoryRow::End { .. }
                | HistoryRow::Heartbeat
                | HistoryRow::Error(_) => None,
            })
            .collect()
    }

    #[test]
    fn equal_timestamps_merge_by_container_then_file_order() {
        let store = Store::new();
        let b = store.container('b', "web");
        let a = store.container('a', "web");
        store.file(&b, 0, &[line(T0, "b0"), line(T0, "b1"), line(T0 + 2, "b2")]);
        store.file(&a, 0, &[line(T0, "a0"), line(T0 + 1, "a1")]);
        store.file(&a, 1, &[line(T0, "a-late")]);

        let page = store.page(request(LogDirection::Forward, 100));
        assert_eq!(texts(&page.rows), ["a0", "a-late", "b0", "b1", "a1", "b2"]);
        let introduced: Vec<_> = page
            .rows
            .iter()
            .filter_map(|row| {
                if let HistoryRow::Container(container) = row {
                    Some(container.container_id)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(introduced, [a, b]);
        assert!(matches!(page.rows.first(), Some(HistoryRow::Container(_))));
        assert_eq!(page.next, None);

        let backward = store.page(request(LogDirection::Backward, 100));
        assert_eq!(
            texts(&backward.rows),
            ["b2", "a1", "b1", "b0", "a-late", "a0"]
        );
    }

    #[test]
    fn gaps_and_the_exit_in_range_come_back_as_rows() {
        let store = Store::new();
        let id = store.container('c', "web");
        store.file(&id, 0, &[line(T0, "first"), line(T0 + 10, "last")]);
        let dir = store.root.container(&id);
        let gaps = [
            Gap {
                from: T0 + 2,
                to: T0 + 5,
                reason: GapReason::Corrupt,
            },
            Gap {
                from: T0 - 100,
                to: T0 - 50,
                reason: GapReason::NotCaptured,
            },
        ];
        let jsonl: String = gaps
            .iter()
            .map(|gap| serde_json::to_string(gap).unwrap() + "\n")
            .collect();
        std::fs::write(dir.join("gaps.jsonl"), jsonl).unwrap();
        let mut meta: ContainerMeta =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap();
        meta.finished_at = Some("2025-10-09T08:53:20.000000020Z".into());
        meta.exit_code = Some(137);
        meta.oom_killed = true;
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();

        let page = store.page(LogHistoryRequest {
            since_nanos: Some(T0),
            ..request(LogDirection::Forward, 100)
        });
        assert_eq!(texts(&page.rows), ["first", "gap", "last", "exit"]);
        assert!(page.rows.iter().any(|row| matches!(
            row,
            HistoryRow::Gap {
                from,
                reason: HistoryGapReason::Corrupt,
                ..
            } if *from == T0 + 2
        )));
        assert!(page.rows.iter().any(|row| matches!(
            row,
            HistoryRow::Exit {
                exit_code: Some(137),
                oom_killed: true,
                ..
            }
        )));
    }

    #[test]
    fn a_running_containers_unfinished_line_waits_until_it_ends() {
        let store = Store::new();
        let id = store.container('d', "web");
        let truncated = line(T0 + 2, "half written");
        let (written, rest) = truncated.split_at(truncated.len() - 3);
        store.file(&id, 0, &[line(T0 - 1, "rotated")]);
        store.file(
            &id,
            1,
            &[
                line(T0, "done"),
                frame(T0 + 1, Stream::Stderr, b"still ", Piece::Continues),
                written.to_vec(),
            ],
        );
        let first = store.page(LogHistoryRequest {
            since_nanos: Some(T0),
            ..request(LogDirection::Forward, 1)
        });
        assert_eq!(texts(&first.rows), ["done"]);
        assert_eq!(first.next, None);

        store.append(&id, 1, rest);
        store.append(
            &id,
            1,
            &frame(T0 + 3, Stream::Stderr, b"going", Piece::Last),
        );
        let second = store.page(LogHistoryRequest {
            cursor: Some(format!("{T0}:{id}:2:0")),
            ..request(LogDirection::Forward, 100)
        });
        assert_eq!(texts(&second.rows), ["half written", "still going"]);
        assert!(second.rows.iter().any(|row| matches!(
            row,
            HistoryRow::Line {
                stream: HistoryStream::Stderr,
                ..
            }
        )));
    }

    #[test]
    fn a_stopped_containers_unfinished_line_is_read() {
        let store = Store::new();
        let id = store.container('d', "web");
        store.file(
            &id,
            0,
            &[frame(T0, Stream::Stdout, b"cut off", Piece::Continues)],
        );
        assert!(texts(&store.page(request(LogDirection::Forward, 10)).rows).is_empty());
        let dir = store.root.container(&id);
        let mut meta: ContainerMeta =
            serde_json::from_slice(&std::fs::read(dir.join("meta.json")).unwrap()).unwrap();
        meta.finished_at = Some("2025-10-09T08:53:20Z".into());
        std::fs::write(dir.join("meta.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
        assert_eq!(
            texts(&store.page(request(LogDirection::Forward, 10)).rows),
            ["cut off", "exit"]
        );
    }

    #[test]
    fn a_page_ends_early_at_its_byte_budget() {
        let store = Store::new();
        let id = store.container('a', "web");
        store.file(
            &id,
            0,
            &[line(T0, "aaaa"), line(T0 + 1, "bbbb"), line(T0 + 2, "cccc")],
        );
        for (budget, pages) in [
            (
                2 * (ROW_BYTES + 4),
                vec![vec!["aaaa", "bbbb"], vec!["cccc"]],
            ),
            (1, vec![vec!["aaaa"], vec!["bbbb"], vec!["cccc"]]),
        ] {
            let mut seen = Vec::new();
            let mut cursor = None;
            loop {
                let mut query = Query::new(LogHistoryRequest {
                    cursor: cursor.clone(),
                    ..request(LogDirection::Forward, 100)
                })
                .unwrap();
                query.bytes = budget;
                let page = store.query(&query);
                seen.push(texts(&page.rows));
                match page.next {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            assert_eq!(seen, pages, "budget {budget}");
        }
    }

    #[test]
    fn a_page_reads_an_unchanged_files_bounds_once() {
        let store = Store::new();
        let id = store.container('b', "web");
        store.file(&id, 0, &[line(T0, "old")]);
        store.file(&id, 1, &[line(T0 + 1, "new")]);
        let reads = || {
            store
                .bounds
                .reads
                .load(std::sync::atomic::Ordering::Relaxed)
        };
        store.page(request(LogDirection::Forward, 10));
        assert_eq!(reads(), 2);
        store.page(request(LogDirection::Backward, 10));
        assert_eq!(reads(), 2);
        store.append(&id, 1, &line(T0 + 2, "newer"));
        let page = store.page(LogHistoryRequest {
            since_nanos: Some(T0 + 2),
            ..request(LogDirection::Forward, 10)
        });
        assert_eq!(texts(&page.rows), ["newer"]);
        assert_eq!(reads(), 3);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_cold_small_page_does_not_read_every_segment() {
        let store = Store::new();
        let id = store.container('b', "web");
        for seq in 0..16_u64 {
            let frames: Vec<_> = (0..10_000)
                .map(|n| {
                    line(
                        T0 + i64::try_from(seq * 10_000).unwrap() + n,
                        &format!("{seq}-{n:05} {}", "x".repeat(224)),
                    )
                })
                .collect();
            store.file(&id, seq, &frames);
        }
        let read_bytes = || {
            std::fs::read_to_string("/proc/thread-self/io")
                .unwrap()
                .lines()
                .find_map(|line| line.strip_prefix("rchar: "))
                .unwrap()
                .parse::<u64>()
                .unwrap()
        };
        for (direction, seq, first, limit, last) in [
            (LogDirection::Forward, 0, 0, 200, 199),
            (LogDirection::Forward, 0, 0, 5_000, 4_999),
            (LogDirection::Backward, 15, 9_999, 200, 9_800),
            (LogDirection::Backward, 15, 9_999, 5_000, 5_000),
        ] {
            let before = read_bytes();
            let page = page(
                &store.root,
                &Query::new(request(direction, limit)).unwrap(),
                &Bounds::default(),
                &AtomicBool::new(false),
            )
            .unwrap();
            let bytes = read_bytes() - before;
            let rows = texts(&page.rows);
            assert_eq!(rows.len(), usize::from(limit));
            assert!(
                rows.first()
                    .unwrap()
                    .starts_with(&format!("{seq}-{first:05} "))
            );
            assert!(
                rows.last()
                    .unwrap()
                    .starts_with(&format!("{seq}-{last:05} "))
            );
            assert_eq!(
                page.next,
                Some(format!("{}:{id}:{}:0", T0 + seq * 10_000 + last, seq + 1))
            );
            assert!(
                bytes < 8 << 20,
                "a cold {limit}-row page read {bytes} bytes"
            );
        }
    }

    #[test]
    fn large_or_damaged_boundary_frames_never_hide_a_row() {
        let store = Store::new();
        let id = store.container('b', "web");
        let inner = line(T0 + 9_999, "a frame inside the outer line");
        let mut text = vec![b'x'; 999_978];
        text.get_mut(64..64 + inner.len())
            .unwrap()
            .copy_from_slice(&inner);
        let first = frame(T0, Stream::Stdout, &text, Piece::Whole);
        assert_eq!(first.get(..4).unwrap(), 1_000_000_u32.to_be_bytes());
        let last = frame(T0 + 20, Stream::Stdout, &text, Piece::Whole);
        let middle = frame(
            T0 + 10,
            Stream::Stdout,
            &vec![b'y'; 128 << 10],
            Piece::Whole,
        );
        let torn = line(T0 + 30, "being written");
        for (prefix, suffix) in [
            (Vec::new(), Vec::new()),
            (Vec::new(), torn.get(..9).unwrap().to_vec()),
            (vec![0xee; 33 << 10], vec![0xee; 33 << 10]),
            (vec![0xee; 2 << 20], vec![0xee; 2 << 20]),
        ] {
            store.file(
                &id,
                0,
                &[prefix, first.clone(), middle.clone(), last.clone(), suffix],
            );
            for (direction, since, until, ts) in [
                (LogDirection::Forward, T0, T0 + 1, T0),
                (LogDirection::Backward, T0 + 20, T0 + 21, T0 + 20),
            ] {
                let page = page(
                    &store.root,
                    &Query::new(LogHistoryRequest {
                        since_nanos: Some(since),
                        until_nanos: Some(until),
                        ..request(direction, 10)
                    })
                    .unwrap(),
                    &Bounds::default(),
                    &AtomicBool::new(false),
                )
                .unwrap();
                let lines: Vec<_> = page
                    .rows
                    .iter()
                    .filter_map(|row| {
                        if let HistoryRow::Line { ts, text, .. } = row {
                            Some((*ts, text))
                        } else {
                            None
                        }
                    })
                    .collect();
                assert_eq!(lines, [(ts, &text)]);
            }
        }
    }

    #[test]
    fn paging_either_way_neither_repeats_nor_skips_a_row() {
        let store = Store::new();
        let mut everything = Vec::new();
        for (hex, offset) in [('1', 0), ('2', 1)] {
            let id = store.container(hex, "web");
            for seq in 0..3_u64 {
                let frames: Vec<_> = (0..7)
                    .map(|i| {
                        let text = format!("{hex}-{seq}-{i}");
                        everything.push(text.clone());
                        line(T0 + i64::try_from(seq * 3).unwrap() + i / 2 + offset, &text)
                    })
                    .collect();
                store.file(&id, seq, &frames);
            }
        }
        let full = texts(&store.page(request(LogDirection::Forward, 5_000)).rows);
        assert_eq!(full.len(), everything.len());

        for (direction, expected) in [
            (LogDirection::Forward, full.clone()),
            (
                LogDirection::Backward,
                full.iter().rev().cloned().collect::<Vec<_>>(),
            ),
        ] {
            let mut seen = Vec::new();
            let mut cursor = None;
            loop {
                let page = store.page(LogHistoryRequest {
                    cursor: cursor.clone(),
                    ..request(direction, 4)
                });
                seen.extend(texts(&page.rows));
                match page.next {
                    Some(next) => cursor = Some(next),
                    None => break,
                }
            }
            assert_eq!(seen, expected, "{direction:?}");
        }
    }

    /// Every page from `request` on: each page's texts and its cursor.
    fn walk(store: &Store, request: &LogHistoryRequest) -> Vec<(Vec<String>, Option<String>)> {
        let mut pages = Vec::new();
        let mut cursor = request.cursor.clone();
        loop {
            let page = store.page(LogHistoryRequest {
                cursor: cursor.clone(),
                ..request.clone()
            });
            pages.push((texts(&page.rows), page.next.clone()));
            match page.next {
                Some(next) => cursor = Some(next),
                None => return pages,
            }
        }
    }

    #[test]
    fn a_page_that_fills_before_a_file_still_has_a_cursor() {
        let store = Store::new();
        let id = store.container('a', "web");
        store.file(&id, 0, &[line(T0, "one"), line(T0 + 1, "two")]);
        store.file(&id, 1, &[line(T0 + 2, "three"), line(T0 + 3, "four")]);
        for (direction, expected) in [
            (LogDirection::Forward, [["one", "two"], ["three", "four"]]),
            (LogDirection::Backward, [["four", "three"], ["two", "one"]]),
        ] {
            let pages: Vec<_> = walk(&store, &request(direction, 2))
                .into_iter()
                .map(|(texts, _)| texts)
                .filter(|texts| !texts.is_empty())
                .collect();
            assert_eq!(pages, expected, "{direction:?}");
        }
    }

    #[test]
    fn a_line_still_being_written_hides_nothing_and_reads_once_it_ends() {
        let store = Store::new();
        let id = store.container('a', "web");
        let err = |ts, text: &str| frame(ts, Stream::Stderr, text.as_bytes(), Piece::Whole);
        store.file(
            &id,
            0,
            &[
                err(T0 - 2, "a"),
                err(T0 - 1, "b"),
                frame(T0, Stream::Stdout, b"long ", Piece::Continues),
                err(T0 + 1, "c"),
                err(T0 + 2, "d"),
            ],
        );
        let other = store.container('b', "web");
        store.file(&other, 0, &[line(T0 + 5, "x"), line(T0 + 6, "y")]);
        let read = |request: LogHistoryRequest| -> Vec<String> {
            walk(&store, &request)
                .into_iter()
                .flat_map(|(texts, _)| texts)
                .collect()
        };

        let forward = walk(&store, &request(LogDirection::Forward, 1));
        let walked: Vec<_> = forward
            .iter()
            .flat_map(|(texts, _)| texts.clone())
            .collect();
        assert_eq!(walked, ["a", "b", "c", "d", "x", "y"]);
        assert_eq!(
            read(request(LogDirection::Backward, 1)),
            ["y", "x", "d", "c", "b", "a"]
        );
        for (direction, expected) in [
            (LogDirection::Forward, ["c", "d", "x", "y"]),
            (LogDirection::Backward, ["y", "x", "d", "c"]),
        ] {
            let page = store.page(LogHistoryRequest {
                since_nanos: Some(T0 + 1),
                ..request(direction, 100)
            });
            assert_eq!(texts(&page.rows), expected, "{direction:?}");
        }
        assert_eq!(
            texts(&store.page(request(LogDirection::Backward, 5)).rows),
            ["y", "x", "d", "c", "b"]
        );

        // Every cursor issued while it waited is below the line once it ends.
        store.append(&id, 0, &frame(T0 + 7, Stream::Stdout, b"line", Piece::Last));
        let cursors: Vec<_> = forward.into_iter().filter_map(|(_, next)| next).collect();
        assert_eq!(
            read(LogHistoryRequest {
                cursor: cursors.last().cloned(),
                ..request(LogDirection::Forward, 1)
            }),
            ["y", "long line"]
        );
        assert_eq!(
            read(LogHistoryRequest {
                cursor: cursors.first().cloned(),
                ..request(LogDirection::Forward, 1)
            }),
            ["b", "c", "d", "x", "y", "long line"]
        );
        assert_eq!(
            read(request(LogDirection::Backward, 1)),
            ["long line", "y", "x", "d", "c", "b", "a"]
        );
    }

    #[test]
    fn the_selector_and_time_range_narrow_the_page() {
        let store = Store::new();
        let web = store.container('e', "web");
        let api = store.container('f', "api");
        store.file(&web, 0, &[line(T0, "web-early"), line(T0 + 5, "web-late")]);
        store.file(&api, 0, &[line(T0 + 1, "api")]);
        let page = store.page(LogHistoryRequest {
            service: Some("web".into()),
            since_nanos: Some(T0 + 1),
            until_nanos: Some(T0 + 10),
            ..request(LogDirection::Forward, 100)
        });
        assert_eq!(texts(&page.rows), ["web-late"]);
        let page = store.page(LogHistoryRequest {
            namespace: None,
            deployment: Some("dep-api".into()),
            ..request(LogDirection::Forward, 100)
        });
        assert_eq!(texts(&page.rows), ["api"]);
    }

    #[test]
    fn a_reader_that_went_away_stops_the_read() {
        let store = Store::new();
        let id = store.container('9', "web");
        store.file(&id, 0, &[line(T0, "x")]);
        let error = page(
            &store.root,
            &Query::new(request(LogDirection::Forward, 10)).unwrap(),
            &store.bounds,
            &AtomicBool::new(true),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(store.bounds.known.lock().unwrap().is_empty());
    }

    #[test]
    fn a_cancelled_page_never_opens_the_store() {
        let store = Store::new();
        std::fs::remove_dir_all(store.root.containers()).unwrap();
        let error = page(
            &store.root,
            &Query::new(request(LogDirection::Forward, 10)).unwrap(),
            &store.bounds,
            &AtomicBool::new(true),
        )
        .err()
        .unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    }

    #[test]
    fn cancelled_file_discovery_never_reads_a_segment() {
        let store = Store::new();
        let id = store.container('9', "web");
        store.file(&id, 0, &[line(T0, "x")]);
        let error = store
            .bounds
            .files(&store.root.container(&id), &AtomicBool::new(true))
            .err()
            .unwrap();
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
        assert!(store.bounds.known.lock().unwrap().is_empty());
    }

    #[test]
    fn a_cursor_from_elsewhere_is_refused() {
        for cursor in [
            "",
            "1:2:3",
            "x:aa:1:1",
            &format!("1:{}:1:1:9", "a".repeat(64)),
        ] {
            assert!(
                Query::new(LogHistoryRequest {
                    cursor: Some(cursor.into()),
                    ..request(LogDirection::Forward, 10)
                })
                .is_err(),
                "{cursor}"
            );
        }
    }
}
