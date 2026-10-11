//! Raw metric rows, one JSON line each, in one file per hour.
//!
//! Every container's rows share the hour's file, so disk grows with the hours
//! kept rather than with how many containers came and went. Cleanup deletes
//! a segment once its hour is a day old, or sooner when the Observe Store
//! passes its cap or the disk its free floor.
//!
//! A crash can leave a segment's last line half written. Reads skip any line
//! that does not parse, and the next append first ends the torn line so it
//! cannot swallow the row written after it.

#![cfg_attr(
    not(test),
    expect(dead_code, reason = "segments are read by the metric queries in M9")
)]

use std::{
    collections::BTreeMap,
    fs,
    io::{self, BufRead, BufReader, Write},
    os::unix::fs::{FileExt, OpenOptionsExt},
    path::Path,
};

use nix::fcntl::OFlag;
use ployz_core::metrics::{HostSample, MetricRange, MetricSample, MetricScope};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::observe::layout::{self, METRICS_DIR, SegmentName, StoreRoot};

/// One line of a raw segment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "snake_case")]
pub enum Row {
    Container(MetricSample),
    Host(HostSample),
}

/// A stored metric row, which a range read keeps or drops by its `ts`.
pub trait Stamped {
    fn ts(&self) -> i64;
}

impl Stamped for Row {
    fn ts(&self) -> i64 {
        match self {
            Self::Container(sample) => sample.ts,
            Self::Host(sample) => sample.ts,
        }
    }
}

/// The rows a scope selects, oldest first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Selected {
    Machine(Vec<HostSample>),
    Service(Vec<MetricSample>),
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
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "metric row stamped before 1970",
            )
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

/// Reads the rows `scope` selects with `ts` inside `range`.
///
/// # Errors
///
/// Returns an error when the segment dir exists but cannot be listed. A
/// segment that cannot be read is logged and skipped.
pub fn read(store: &StoreRoot, range: MetricRange, scope: &MetricScope) -> io::Result<Selected> {
    let mut hosts = Vec::new();
    let mut containers = Vec::new();
    scan(&store.raw_metrics(), range, |row| match (scope, row) {
        (MetricScope::Machine, Row::Host(sample)) => hosts.push(sample),
        (MetricScope::Service { service }, Row::Container(sample))
            if sample.service.as_ref() == Some(service) =>
        {
            containers.push(sample);
        }
        _ => {}
    })?;
    hosts.sort_by_key(|sample| sample.ts);
    containers.sort_by_key(|sample| sample.ts);
    Ok(match scope {
        MetricScope::Machine => Selected::Machine(hosts),
        MetricScope::Service { .. } => Selected::Service(containers),
    })
}

/// Calls `visit` with every row in `range` of the hourly files in `dir`,
/// oldest hour first.
///
/// # Errors
///
/// Returns an error when `dir` exists but cannot be listed. A file that
/// cannot be read is logged and skipped.
pub fn scan<R: DeserializeOwned + Stamped>(
    dir: &Path,
    range: MetricRange,
    mut visit: impl FnMut(R),
) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
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
        let skipped = match read_segment(&path, range, &mut visit) {
            Ok(skipped) => skipped,
            Err(error) => {
                tracing::warn!(segment = %path.display(), %error, "skipping a metric segment");
                continue;
            }
        };
        if skipped > 0 {
            tracing::debug!(segment = %path.display(), skipped, "skipped unreadable metric rows");
        }
    }
    Ok(())
}

fn overlaps(name: SegmentName, range: MetricRange) -> bool {
    let nanos =
        |secs: u64| i64::try_from(secs).map_or(i64::MAX, |secs| secs.saturating_mul(1_000_000_000));
    nanos(name.start_secs()) < range.until_nanos && nanos(name.end_secs()) > range.since_nanos
}

/// Visits the rows of one segment in `range` and returns how many lines did
/// not parse, a torn last line included.
fn read_segment<R: DeserializeOwned + Stamped>(
    path: &Path,
    range: MetricRange,
    visit: &mut impl FnMut(R),
) -> io::Result<usize> {
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
            return Ok(skipped);
        }
        let Some(complete) = line.strip_suffix(b"\n") else {
            return Ok(skipped + 1);
        };
        match serde_json::from_slice::<R>(complete) {
            Ok(row) if (range.since_nanos..range.until_nanos).contains(&row.ts()) => visit(row),
            Ok(_) => {}
            Err(_) => skipped += 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs, io::Write as _};

    use ployz_core::{
        ContainerId, MachineId, QualifiedService, ServiceId,
        metrics::{
            CpuJiffies, DiskBreakdown, HostMemory, HostSample, InterfaceBytes, MetricRange,
            MetricSample, MetricScope,
        },
    };

    use super::{Row, Selected, append, read};
    use crate::{observe::layout::StoreRoot, test_dir::TestDir};

    const SEC: i64 = 1_000_000_000;
    const HOUR: i64 = 3_600 * SEC;
    /// An hour boundary: 2025-10-09T08:00:00Z.
    const T0: i64 = 1_759_996_800 * SEC;

    fn store() -> (TestDir, StoreRoot) {
        let dir = TestDir::new("ployzd-metrics-segment");
        fs::create_dir_all(&dir.0).unwrap();
        let root = StoreRoot::under(&dir.0);
        root.prepare().unwrap();
        (dir, root)
    }

    fn machine() -> MachineId {
        MachineId::parse("a".repeat(32)).unwrap()
    }

    fn service(n: u8) -> ServiceId {
        ServiceId::parse(format!("{n:02x}").repeat(16)).unwrap()
    }

    fn qualified(n: u8) -> QualifiedService {
        QualifiedService::parse(format!("prod/web-{n}")).unwrap()
    }

    fn container(n: u32) -> ContainerId {
        ContainerId::parse(format!("{n:08x}").repeat(8)).unwrap()
    }

    fn sample(container_n: u32, service_n: u8, ts: i64) -> MetricSample {
        MetricSample {
            machine_id: machine(),
            container_id: container(container_n),
            service_id: Some(service(service_n)),
            service: Some(qualified(service_n)),
            deployment_id: Some("01JABCDEFGHJKMNPQRSTVWXYZ0".into()),
            replica: format!("web-{container_n}"),
            ts,
            cpu_usec: Some(48_213_337_120),
            mem_bytes: Some(412_331_008),
            mem_limit: Some(1_073_741_824),
            rx: Some(123_456_789_012),
            tx: Some(98_765_432_109),
            io_r: Some(5_368_709_120),
            io_w: Some(21_474_836_480),
        }
    }

    fn host(ts: i64) -> HostSample {
        HostSample {
            machine_id: machine(),
            ts,
            cpu: Some(CpuJiffies {
                busy: 812_331_456,
                idle: 9_123_456_789,
            }),
            memory: Some(HostMemory {
                total: 137_438_953_472,
                available: 98_765_432_109,
            }),
            interfaces: ["eth0", "ployz0", "docker0", "lo"]
                .into_iter()
                .map(|name| {
                    let bytes = InterfaceBytes {
                        rx: 812_345_678_901,
                        tx: 712_345_678_901,
                    };
                    (name.to_owned(), bytes)
                })
                .collect::<BTreeMap<_, _>>(),
            disk: Some(DiskBreakdown {
                total: 3_840_000_000_000,
                free: 2_123_456_789_012,
                volumes: Some(812_345_678_901),
                mirror_slots: Some(123_456_789_012),
                observe_store: Some(12_345_678_901),
                other: Some(768_197_530_074),
            }),
        }
    }

    fn range(since: i64, until: i64) -> MetricRange {
        MetricRange {
            since_nanos: since,
            until_nanos: until,
        }
    }

    fn segments(store: &StoreRoot) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(store.raw_metrics())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_range_read_returns_only_its_scope_inside_the_range_across_hours() {
        let (_dir, store) = store();
        let rows = [
            Row::Container(sample(1, 1, T0 - SEC)),
            Row::Host(host(T0 - SEC)),
            Row::Container(sample(1, 1, T0 + 10 * SEC)),
            Row::Container(sample(2, 2, T0 + 10 * SEC)),
            Row::Host(host(T0 + 10 * SEC)),
            Row::Container(sample(1, 1, T0 + HOUR + 10 * SEC)),
            Row::Host(host(T0 + HOUR + 10 * SEC)),
            Row::Container(sample(1, 1, T0 + 2 * HOUR)),
        ];
        append(&store, &rows[..4]).unwrap();
        append(&store, &rows[4..]).unwrap();
        assert_eq!(
            segments(&store),
            [
                "1759993200.jsonl",
                "1759996800.jsonl",
                "1760000400.jsonl",
                "1760004000.jsonl"
            ]
        );

        let window = range(T0, T0 + 2 * HOUR);
        let scope = MetricScope::Service {
            service: qualified(1),
        };
        assert_eq!(
            read(&store, window, &scope).unwrap(),
            Selected::Service(vec![
                sample(1, 1, T0 + 10 * SEC),
                sample(1, 1, T0 + HOUR + 10 * SEC),
            ])
        );
        assert_eq!(
            read(&store, window, &MetricScope::Machine).unwrap(),
            Selected::Machine(vec![host(T0 + 10 * SEC), host(T0 + HOUR + 10 * SEC)])
        );
    }

    #[test]
    fn an_empty_store_reads_nothing() {
        let (_dir, store) = store();
        assert_eq!(
            read(&store, range(0, i64::MAX), &MetricScope::Machine).unwrap(),
            Selected::Machine(Vec::new())
        );
    }

    #[test]
    fn a_torn_last_line_is_skipped_and_the_next_append_is_kept() {
        let (_dir, store) = store();
        append(&store, &[Row::Host(host(T0))]).unwrap();
        let path = store.raw_metrics().join("1759996800.jsonl");
        let mut torn = serde_json::to_vec(&Row::Host(host(T0 + 10 * SEC))).unwrap();
        torn.truncate(torn.len() / 2);
        fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(&torn)
            .unwrap();
        let everything = range(T0, T0 + HOUR);
        assert_eq!(
            read(&store, everything, &MetricScope::Machine).unwrap(),
            Selected::Machine(vec![host(T0)])
        );

        append(&store, &[Row::Host(host(T0 + 20 * SEC))]).unwrap();
        assert_eq!(
            read(&store, everything, &MetricScope::Machine).unwrap(),
            Selected::Machine(vec![host(T0), host(T0 + 20 * SEC)])
        );
    }

    #[test]
    fn five_hundred_short_lived_containers_in_one_hour_share_one_file() {
        let (_dir, store) = store();
        for n in 0..500 {
            let ts = T0 + i64::from(n) * 7 * SEC;
            append(&store, &[Row::Container(sample(n, 1, ts))]).unwrap();
        }
        assert_eq!(segments(&store), ["1759996800.jsonl"]);
        let scope = MetricScope::Service {
            service: qualified(1),
        };
        let Selected::Service(samples) = read(&store, range(T0, T0 + HOUR), &scope).unwrap() else {
            panic!("service scope reads service samples");
        };
        assert_eq!(samples.len(), 500);
    }

    /// One hour of 50 containers and the host at 10 second ticks, every field
    /// present, to size a segment. Prints the bytes for the PR record.
    #[test]
    fn one_hour_of_fifty_containers_stays_under_ten_megabytes() {
        let (_dir, store) = store();
        for tick in 0..360 {
            let ts = T0 + tick * 10 * SEC;
            let mut rows: Vec<Row> = (0..50)
                .map(|n| Row::Container(sample(n, u8::try_from(n % 10).unwrap(), ts)))
                .collect();
            rows.push(Row::Host(host(ts)));
            append(&store, &rows).unwrap();
        }
        let bytes = fs::metadata(store.raw_metrics().join("1759996800.jsonl"))
            .unwrap()
            .len();
        println!("one hour, 50 containers + host at 10 s: {bytes} bytes");
        assert!(bytes < 10 << 20, "{bytes} bytes");
    }
}
