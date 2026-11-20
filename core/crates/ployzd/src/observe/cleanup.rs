//! Deletes stored metric files to keep the observe store under its limits.
//!
//! Cleanup deletes only what the store itself created: a `<hour>.jsonl`
//! file whose name parses, inside the raw or rollup metrics dir, reached
//! without following a symlink. Every delete is an `unlinkat` relative to a
//! dir it opened with `O_NOFOLLOW`, so nothing in the store can point a delete
//! elsewhere.
//!
//! Closed raw metric segments share the cap and the free floor, ordered by
//! the end of their hour, so under pressure the oldest hours go first. A
//! segment is also deleted once its whole hour is more than a day old. The
//! open hour is never a candidate.
//!
//! Metric rollups join the same budget and order, each by the end of the hour
//! it folds, and go once that hour is more than 90 days old.

use std::{
    ffi::CString,
    io,
    os::fd::{AsRawFd, RawFd},
    time::{Duration, SystemTime},
};

use nix::{
    dir::Dir,
    errno::Errno,
    fcntl::{AtFlags, OFlag},
    sys::stat::{Mode, SFlag, fstatat},
    unistd::{UnlinkatFlags, unlinkat},
};

use super::{
    layout::{METRICS_DIR, RAW_DIR, ROLLUP_DIR, SegmentName, StoreRoot, VERSION_DIR},
    metrics::policy::RETENTION,
};

const GIB: u64 = 1 << 30;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Bytes the store may hold alone.
    pub cap_bytes: u64,
    /// Free bytes the filesystem must keep; below it the store gives way.
    pub free_floor_bytes: u64,
}

impl Limits {
    /// The limits for a filesystem of `total_bytes`: a cap of 5% held between
    /// 512 MiB and 5 GiB, and a floor of 10% free but never under 2 GiB.
    #[must_use]
    pub fn for_filesystem(total_bytes: u64) -> Self {
        Self {
            cap_bytes: (total_bytes / 20).clamp(GIB / 2, 5 * GIB),
            free_floor_bytes: (total_bytes / 10).max(2 * GIB),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Disk {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl Disk {
    /// Measures the filesystem holding `store`.
    ///
    /// # Errors
    ///
    /// Returns the `statvfs` error.
    pub fn measure(store: &StoreRoot) -> io::Result<Self> {
        let stat = nix::sys::statvfs::statvfs(store.path())?;
        let fragment = stat.fragment_size();
        Ok(Self {
            total_bytes: stat.blocks().saturating_mul(fragment),
            free_bytes: stat.blocks_available().saturating_mul(fragment),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Report {
    pub bytes_freed: u64,
    pub segments_deleted: usize,
    pub rollups_deleted: usize,
}

enum Place {
    /// A segment in the raw metrics dir open as this fd.
    Segment(RawFd),
    /// A rollup in the rollup dir open as this fd.
    Rollup(RawFd),
}

struct Candidate {
    place: Place,
    name: CString,
    /// The end of the file's hour, then its start, both in Unix seconds.
    order: (i64, u64),
    size: u64,
    allocated_bytes: u64,
    dev: u64,
    ino: u64,
}

const DIR_FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_DIRECTORY)
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_CLOEXEC);

/// Runs one cleanup pass. An error on one file is logged and skipped.
#[must_use]
pub fn run(store: &StoreRoot, limits: &Limits, disk: Disk, now: SystemTime) -> Report {
    let mut report = Report::default();
    let mut candidates = Vec::new();

    let now_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut raw = open_metrics(store, RAW_DIR);
    if let Some(raw) = raw.as_mut() {
        scan_metric_hours(raw, now_secs, Place::Segment, &mut candidates);
    }
    let mut rollups = open_metrics(store, ROLLUP_DIR);
    if let Some(rollups) = rollups.as_mut() {
        scan_metric_hours(rollups, now_secs, Place::Rollup, &mut candidates);
    }

    let hour_age_limit =
        |age: Duration| i64::try_from(now_secs.saturating_sub(age.as_secs())).unwrap_or(i64::MAX);
    let raw_age_limit = hour_age_limit(RETENTION.raw);
    let rollup_age_limit = hour_age_limit(RETENTION.rollup);
    candidates.sort_by(|left, right| left.order.cmp(&right.order));
    let mut held: u64 = candidates.iter().map(|candidate| candidate.size).sum();
    let mut released: u64 = 0;
    for candidate in &candidates {
        let too_old = match candidate.place {
            Place::Segment(_) => candidate.order.0 <= raw_age_limit,
            Place::Rollup(_) => candidate.order.0 <= rollup_age_limit,
        };
        let over_cap = held > limits.cap_bytes;
        let under_floor = disk.free_bytes.saturating_add(released) < limits.free_floor_bytes;
        if !(too_old || over_cap || under_floor) {
            continue;
        }
        let (Place::Segment(dir_fd) | Place::Rollup(dir_fd)) = candidate.place;
        match unlink_unchanged(dir_fd, candidate) {
            Ok(()) => {
                held -= candidate.size;
                released += candidate.allocated_bytes;
                report.bytes_freed += candidate.size;
                match candidate.place {
                    Place::Segment(_) => report.segments_deleted += 1,
                    Place::Rollup(_) => report.rollups_deleted += 1,
                }
            }
            Err(error) => {
                tracing::warn!(?candidate.name, %error, "could not delete a stored file")
            }
        }
    }

    report
}

/// Opens a metrics dir one component at a time from the store's own path, so
/// a symlink anywhere in it stops cleanup instead of redirecting it, or `None`
/// before anything was written there or when it cannot be opened.
fn open_metrics(store: &StoreRoot, name: &str) -> Option<Dir> {
    match open_metrics_dir(store, name) {
        Ok(dir) => dir,
        Err(error) => {
            tracing::warn!(dir = name, %error, "could not open a metrics dir");
            None
        }
    }
}

fn open_metrics_dir(store: &StoreRoot, name: &str) -> nix::Result<Option<Dir>> {
    let base = Dir::open(store.base(), DIR_FLAGS, Mode::empty())?;
    let version = Dir::openat(
        Some(base.as_raw_fd()),
        VERSION_DIR,
        DIR_FLAGS,
        Mode::empty(),
    )?;
    let metrics = match Dir::openat(
        Some(version.as_raw_fd()),
        METRICS_DIR,
        DIR_FLAGS,
        Mode::empty(),
    ) {
        Ok(metrics) => metrics,
        Err(Errno::ENOENT) => return Ok(None),
        Err(error) => return Err(error),
    };
    match Dir::openat(Some(metrics.as_raw_fd()), name, DIR_FLAGS, Mode::empty()) {
        Ok(dir) => Ok(Some(dir)),
        Err(Errno::ENOENT) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Adds every file of a closed hour in `dir` to `candidates`, ordered by the
/// end of its hour.
fn scan_metric_hours(
    dir: &mut Dir,
    now_secs: u64,
    place: fn(RawFd) -> Place,
    candidates: &mut Vec<Candidate>,
) {
    let dir_fd = dir.as_raw_fd();
    let closed: Vec<(CString, SegmentName)> = dir
        .iter()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let segment = SegmentName::parse(entry.file_name().to_bytes())?;
            (segment.end_secs() <= now_secs).then(|| (entry.file_name().to_owned(), segment))
        })
        .collect();
    for (name, segment) in closed {
        let stat = match fstatat(Some(dir_fd), name.as_c_str(), AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(stat) => stat,
            Err(error) => {
                tracing::warn!(?name, %error, "skipping a metric hour");
                continue;
            }
        };
        if SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT != SFlag::S_IFREG
            || stat.st_nlink != 1
        {
            continue;
        }
        candidates.push(Candidate {
            place: place(dir_fd),
            name,
            order: (
                i64::try_from(segment.end_secs()).unwrap_or(i64::MAX),
                segment.start_secs(),
            ),
            size: u64::try_from(stat.st_size).unwrap_or(0),
            allocated_bytes: u64::try_from(stat.st_blocks)
                .unwrap_or(0)
                .saturating_mul(512),
            dev: stat.st_dev,
            ino: stat.st_ino,
        });
    }
}

/// Unlinks the candidate only if it is still the file it was when scanned.
fn unlink_unchanged(dir_fd: RawFd, candidate: &Candidate) -> nix::Result<()> {
    let stat = fstatat(
        Some(dir_fd),
        candidate.name.as_c_str(),
        AtFlags::AT_SYMLINK_NOFOLLOW,
    )?;
    let kind = SFlag::from_bits_truncate(stat.st_mode) & SFlag::S_IFMT;
    if kind != SFlag::S_IFREG
        || stat.st_dev != candidate.dev
        || stat.st_ino != candidate.ino
        || stat.st_nlink != 1
    {
        return Err(Errno::ESTALE);
    }
    unlinkat(
        Some(dir_fd),
        candidate.name.as_c_str(),
        UnlinkatFlags::NoRemoveDir,
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt as _, symlink},
        time::{Duration, SystemTime},
    };

    use super::{Disk, Limits, Report, run};
    use crate::{observe::layout::StoreRoot, test_dir::TestDir};

    const T0: u64 = 1_760_000_000;

    struct Store {
        _dir: TestDir,
        root: StoreRoot,
    }

    fn store() -> Store {
        let dir = TestDir::new("ployzd-observe-cleanup");
        fs::create_dir_all(&dir.0).unwrap();
        let root = StoreRoot::under(&dir.0);
        root.prepare().unwrap();
        Store { _dir: dir, root }
    }

    fn segment_name(hours_back: u64) -> String {
        let hour = T0 - T0 % 3_600;
        format!("{}.jsonl", hour - hours_back * 3_600)
    }

    impl Store {
        fn clean(&self, cap_bytes: u64, free_bytes: u64) -> Report {
            let limits = Limits {
                cap_bytes,
                free_floor_bytes: 0,
            };
            let disk = Disk {
                total_bytes: u64::MAX,
                free_bytes,
            };
            let now = SystemTime::UNIX_EPOCH + Duration::from_secs(T0);
            run(&self.root, &limits, disk, now)
        }

        fn segment(&self, hours_back: u64, size: usize) -> u64 {
            let raw = self.root.raw_metrics();
            fs::create_dir_all(&raw).unwrap();
            let path = raw.join(segment_name(hours_back));
            fs::write(&path, vec![b'x'; size]).unwrap();
            fs::metadata(path).unwrap().blocks() * 512
        }

        fn segments(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.root.raw_metrics())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }

        fn rollup(&self, hours_back: u64) {
            let dir = self.root.rollup_metrics();
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(segment_name(hours_back)), vec![b'x'; 100]).unwrap();
        }

        fn rollups(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(self.root.rollup_metrics())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect();
            names.sort();
            names
        }
    }

    #[test]
    fn limits_scale_with_the_filesystem_within_bounds() {
        let gib = 1_u64 << 30;
        let small = Limits::for_filesystem(4 * gib);
        assert_eq!(
            (small.cap_bytes, small.free_floor_bytes),
            (gib / 2, 2 * gib)
        );
        let mid = Limits::for_filesystem(40 * gib);
        assert_eq!((mid.cap_bytes, mid.free_floor_bytes), (2 * gib, 4 * gib));
        let large = Limits::for_filesystem(4000 * gib);
        assert_eq!(
            (large.cap_bytes, large.free_floor_bytes),
            (5 * gib, 400 * gib)
        );
    }

    #[test]
    fn a_symlink_in_the_store_path_stops_cleanup() {
        let store = store();
        store.segment(30, 100);
        let real = store.root.base().join("real");
        fs::rename(store.root.path(), &real).unwrap();
        symlink(&real, store.root.path()).unwrap();
        let limits = Limits::for_filesystem(u64::MAX);
        let disk = Disk {
            total_bytes: u64::MAX,
            free_bytes: u64::MAX,
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(T0);
        let report = run(&store.root, &limits, disk, now);
        assert_eq!(report, Report::default());
        assert_eq!(fs::read_dir(real.join("metrics/raw")).unwrap().count(), 1);
    }

    #[test]
    fn raw_segments_go_once_their_hour_is_a_day_old_and_other_entries_stay() {
        let store = store();
        let raw = store.root.raw_metrics();
        fs::create_dir_all(&raw).unwrap();
        let expired = segment_name(25);
        let kept = segment_name(24);
        let current = segment_name(0);
        for name in [&expired, &kept, &current] {
            fs::write(raw.join(name), b"{}\n").unwrap();
        }
        let old_dir = segment_name(72);
        fs::create_dir(raw.join(&old_dir)).unwrap();
        fs::write(raw.join("notes.txt"), b"keep").unwrap();

        let report = store.clean(u64::MAX, u64::MAX);
        assert_eq!(report.segments_deleted, 1);
        let mut want = vec![kept, current, old_dir, "notes.txt".to_owned()];
        want.sort();
        assert_eq!(store.segments(), want);
        assert_eq!(store.clean(u64::MAX, u64::MAX).segments_deleted, 0);
    }

    #[test]
    fn below_the_free_floor_the_oldest_metric_hours_go_first_and_the_open_hour_stays() {
        let store = store();
        let oldest = store.segment(3, 10_000);
        let next = store.segment(2, 10_000);
        store.segment(1, 10_000);
        store.segment(0, 10_000);
        let limits = Limits {
            cap_bytes: u64::MAX,
            free_floor_bytes: 1_000 + oldest + next,
        };
        let disk = Disk {
            total_bytes: u64::MAX,
            free_bytes: 1_000,
        };
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(T0);
        let report = run(&store.root, &limits, disk, now);
        assert_eq!((report.segments_deleted, report.bytes_freed), (2, 20_000));
        let mut want = vec![segment_name(1), segment_name(0)];
        want.sort();
        assert_eq!(store.segments(), want);

        let disk = Disk {
            total_bytes: u64::MAX,
            free_bytes: 0,
        };
        let report = run(&store.root, &limits, disk, now);
        assert_eq!(report.segments_deleted, 1);
        assert_eq!(store.segments(), [segment_name(0)]);
    }

    #[test]
    fn rollups_go_once_their_hour_is_ninety_days_old() {
        let store = store();
        store.rollup(91 * 24);
        store.rollup(89 * 24);
        fs::write(
            store.root.rollup_metrics().join("1759996800.jsonl.tmp"),
            b"x",
        )
        .unwrap();

        let report = store.clean(u64::MAX, u64::MAX);
        assert_eq!((report.rollups_deleted, report.segments_deleted), (1, 0));
        assert_eq!(
            store.rollups(),
            [segment_name(89 * 24), "1759996800.jsonl.tmp".to_owned()]
        );
    }

    #[test]
    fn rollups_share_the_cap_with_segments_in_age_order() {
        let store = store();
        store.rollup(30 * 24);
        store.segment(2, 100);
        store.rollup(1);

        let report = store.clean(200, u64::MAX);
        assert_eq!((report.rollups_deleted, report.segments_deleted), (1, 0));
        assert_eq!(store.rollups(), [segment_name(1)]);
        assert_eq!(store.segments(), [segment_name(2)]);
    }

    #[test]
    fn the_hour_a_23_hour_range_starts_in_is_never_deleted_while_that_range_is_served_raw() {
        let store = store();
        let raw = store.root.raw_metrics();
        fs::create_dir_all(&raw).unwrap();
        let hour = T0 - T0 % 3_600;
        for hours_back in 0..30 {
            fs::write(raw.join(segment_name(hours_back)), b"{}\n").unwrap();
        }
        for minutes_into_hour in [0, 1, 30, 59] {
            let now_secs = hour + minutes_into_hour * 60;
            let start = now_secs - 23 * 3_600;
            let first = format!("{}.jsonl", start - start % 3_600);
            let report = run(
                &store.root,
                &Limits::for_filesystem(u64::MAX),
                Disk {
                    total_bytes: u64::MAX,
                    free_bytes: u64::MAX,
                },
                SystemTime::UNIX_EPOCH + Duration::from_secs(now_secs),
            );
            assert!(
                store.segments().contains(&first),
                "{first} after {report:?} at +{minutes_into_hour} min"
            );
        }
    }
}
