//! Docker event markers, one JSON line each, in one file per hour, which the exit
//! ledger reads back to know a marker is already kept.
//!
//! Cleanup deletes a segment once its hour is a day old, or sooner when the Observe
//! Store passes its cap or the disk its free floor. Each append is one write of whole
//! lines to a file opened for appending, so concurrent lines do not interleave.
//!
//! A crash can leave a segment's last line half written. Reads skip any line
//! that does not parse, and the next append first ends the torn line so it
//! cannot swallow the row written after it. A row kind this reader does not
//! know is skipped the same way, so new kinds can join the file.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Write},
    ops::ControlFlow,
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::Path,
};

use memchr::memmem::Finder;
use nix::fcntl::OFlag;
use ployz_core::metrics::{MetricMarker, MetricRange};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::observe::layout::{self, METRICS_DIR, SegmentName, StoreRoot};

/// One line of a raw segment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "snake_case")]
pub enum Row {
    Marker(MetricMarker),
}

/// A stored metric row, which a range read keeps or drops by its `ts`.
pub trait Stamped {
    fn ts(&self) -> i64;
}

impl Stamped for Row {
    fn ts(&self) -> i64 {
        match self {
            Self::Marker(marker) => marker.ts,
        }
    }
}

/// Appends rows to the segments of the hours their timestamps fall in.
///
/// # Errors
///
/// Returns an error when a row is stamped before 1970, or the segment dir or
/// a segment cannot be created or written.
pub fn append(store: &StoreRoot, rows: &[Row]) -> io::Result<()> {
    let mut lines: BTreeMap<SegmentName, Vec<u8>> = BTreeMap::new();
    for row in rows {
        let name = SegmentName::containing(row.ts()).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "marker stamped before 1970")
        })?;
        let buf = lines.entry(name).or_default();
        serde_json::to_writer(&mut *buf, row)?;
        buf.push(b'\n');
    }
    if lines.is_empty() {
        return Ok(());
    }
    layout::create_store_dir(&store.path().join(METRICS_DIR))?;
    let dir = store.raw_metrics();
    layout::create_store_dir(&dir)?;
    for (name, buf) in lines {
        append_lines(&dir.join(name.to_string()), buf)?;
    }
    Ok(())
}

fn append_lines(path: &Path, mut buf: Vec<u8>) -> io::Result<()> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(OFlag::O_NOFOLLOW.bits())
        .open(path)?;
    let len = file.metadata()?.len();
    if len > 0 {
        let mut last = [0];
        file.read_exact_at(&mut last, len - 1)?;
        if last != *b"\n" {
            buf.insert(0, b'\n');
        }
    }
    file.write_all(&buf)
}

/// A byte test every stored line a read keeps passes. Checking it first skips parsing
/// the other rows.
pub enum Prefilter {
    /// Lines containing these bytes.
    Contains(Finder<'static>),
}

impl Prefilter {
    fn keeps(&self, line: &[u8]) -> bool {
        match self {
            Self::Contains(finder) => finder.find(line).is_some(),
        }
    }
}

/// Whether a row in `range` that `prefilter` keeps satisfies `found`,
/// reading no further than the first that does.
///
/// # Errors
///
/// Returns an error when `dir` exists but cannot be listed. A file that cannot be read
/// is logged and skipped.
pub fn find<R: DeserializeOwned + Stamped>(
    dir: &Path,
    range: MetricRange,
    prefilter: &Prefilter,
    mut found: impl FnMut(&R) -> bool,
) -> io::Result<bool> {
    scan_until(dir, range, prefilter, |row| {
        if found(&row) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
}

/// Whether `visit` broke off the scan.
fn scan_until<R: DeserializeOwned + Stamped>(
    dir: &Path,
    range: MetricRange,
    prefilter: &Prefilter,
    mut visit: impl FnMut(R) -> ControlFlow<()>,
) -> io::Result<bool> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut segments: Vec<SegmentName> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| SegmentName::parse(entry.file_name().as_encoded_bytes()))
        .filter(|name| overlaps(*name, range))
        .collect();
    segments.sort();
    for name in segments {
        let path = dir.join(name.to_string());
        let (skipped, stopped) = match read_segment(&path, range, prefilter, &mut visit) {
            Ok(read) => read,
            Err(error) => {
                tracing::warn!(segment = %path.display(), %error, "skipping a metric segment");
                continue;
            }
        };
        if skipped > 0 {
            tracing::debug!(segment = %path.display(), skipped, "skipped unreadable metric rows");
        }
        if stopped {
            return Ok(true);
        }
    }
    Ok(false)
}

fn overlaps(name: SegmentName, range: MetricRange) -> bool {
    name.start_nanos() < range.until_nanos && name.end_nanos() > range.since_nanos
}

/// Visits the rows of one segment in `range`. Returns how many lines did not parse, a
/// torn last line included, and whether `visit` broke off.
fn read_segment<R: DeserializeOwned + Stamped>(
    path: &Path,
    range: MetricRange,
    prefilter: &Prefilter,
    visit: &mut impl FnMut(R) -> ControlFlow<()>,
) -> io::Result<(usize, bool)> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(OFlag::O_NOFOLLOW.bits())
        .open(path)?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut skipped = 0;
    loop {
        line.clear();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Ok((skipped, false));
        }
        let Some(complete) = line.strip_suffix(b"\n") else {
            return Ok((skipped + 1, false));
        };
        if !prefilter.keeps(complete) {
            continue;
        }
        match serde_json::from_slice::<R>(complete) {
            Ok(row) if (range.since_nanos..range.until_nanos).contains(&row.ts()) => {
                if visit(row).is_break() {
                    return Ok((skipped, true));
                }
            }
            Ok(_) => {}
            Err(_) => skipped += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, io::Write as _};

    use memchr::memmem::Finder;
    use ployz_core::{
        ContainerId, MachineId, QualifiedService, ServiceId,
        metrics::{MarkerKind, MetricMarker, MetricRange},
    };

    use super::{Prefilter, Row, append, find};
    use crate::{observe::layout::StoreRoot, test_dir::TestDir};

    const SEC: i64 = 1_000_000_000;
    /// An hour boundary: 2025-10-09T08:00:00Z.
    const T0: i64 = 1_759_996_800 * SEC;

    fn marker(ts: i64) -> MetricMarker {
        MetricMarker {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            container_id: ContainerId::parse("b".repeat(64)).unwrap(),
            service_id: ServiceId::parse("c".repeat(32)).unwrap(),
            service: QualifiedService::parse("prod/web").unwrap(),
            deployment_id: None,
            replica: "web-1".into(),
            ts,
            kind: MarkerKind::Oom,
        }
    }

    fn stored(store: &StoreRoot, wanted: &MetricMarker) -> bool {
        find(
            &store.raw_metrics(),
            MetricRange {
                since_nanos: wanted.ts,
                until_nanos: wanted.ts + 1,
            },
            &Prefilter::Contains(Finder::new(b"bbbb").into_owned()),
            |row: &Row| matches!(row, Row::Marker(found) if found == wanted),
        )
        .unwrap()
    }

    #[test]
    fn a_torn_last_line_is_skipped_and_the_next_append_is_kept() {
        let dir = TestDir::new("ployzd-metrics-segment");
        fs::create_dir_all(&dir.0).unwrap();
        let store = StoreRoot::under(&dir.0);
        store.prepare().unwrap();
        append(&store, &[Row::Marker(marker(T0))]).unwrap();
        let path = store.raw_metrics().join("1759996800.jsonl");
        let mut torn = serde_json::to_vec(&Row::Marker(marker(T0 + SEC))).unwrap();
        torn.truncate(torn.len() / 2);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&torn)
            .unwrap();
        assert!(stored(&store, &marker(T0)));
        assert!(!stored(&store, &marker(T0 + SEC)));

        append(&store, &[Row::Marker(marker(T0 + 2 * SEC))]).unwrap();
        assert!(stored(&store, &marker(T0 + 2 * SEC)));
    }
}
