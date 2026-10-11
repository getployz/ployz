//! Closed hours of raw rows folded to 5 minute points, one file per hour,
//! kept 90 days.
//!
//! A fold groups the hour's container rows by Service and hands each group,
//! and the host rows, to `rate`, so a rollup point is the same number a raw
//! read of that 5 minutes would give. The group spans Deployments: a replica
//! replaced mid-point counts once, as it does in a raw read. The groups outlive
//! their containers: once the raw hour is deleted, a removed container's
//! numbers are still in its Service's points.
//!
//! A fold rewrites the hour's whole file from the raw rows, so folding again
//! converges on the same bytes. An hour is folded again whenever its raw
//! segment changed after its rollup was written.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Write as _},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    time::SystemTime,
};

use nix::fcntl::OFlag;
use ployz_core::{
    QualifiedService, ServiceId,
    metrics::{
        HostPoint, MetricMarker, MetricPoints, MetricRange, MetricSample, MetricScope, ServicePoint,
    },
};
use serde::{Deserialize, Serialize};

use super::{
    rate::{self, Buckets},
    segment::{self, Prefilter, Stamped},
};
use crate::observe::layout::{self, METRICS_DIR, SegmentName, StoreRoot};

const NANOS_PER_SEC: i64 = 1_000_000_000;
/// Width of one rollup point.
pub const STEP_NANOS: i64 = 5 * 60 * NANOS_PER_SEC;
const TEMP_SUFFIX: &str = ".tmp";

/// One Service: the group a fold sums.
type ServiceKey = (ServiceId, QualifiedService);

/// One line of a rollup file.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "row", rename_all = "snake_case")]
pub enum Row {
    Service(ServiceRow),
    Host(HostPoint),
    Marker(MetricMarker),
}

/// Every container of a Service, summed over 5 minutes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceRow {
    pub service_id: ServiceId,
    /// The Service's Namespace and name, which is how history selects it.
    pub service: QualifiedService,
    pub point: ServicePoint,
}

impl Stamped for Row {
    fn ts(&self) -> i64 {
        match self {
            Self::Service(service) => service.point.ts,
            Self::Host(point) => point.ts,
            Self::Marker(marker) => marker.ts,
        }
    }
}

/// The rollup points a scope selects, oldest first.
#[derive(Clone, Debug, PartialEq)]
pub enum Rolled {
    Machine(Vec<HostPoint>),
    /// One point per Service id per 5 minutes. A name reused by a new
    /// Service id has a row for each, and its total at a `ts` is their sum.
    Service(Vec<ServiceRow>),
}

/// Folds every closed hour whose raw segment has no rollup yet, or changed
/// after its rollup was written. Returns how many hours it folded.
///
/// # Errors
///
/// Returns an error when the raw segment dir exists but cannot be listed. An
/// hour that fails to fold is logged and tried again on the next call.
pub fn fold_pending(store: &StoreRoot, now: SystemTime) -> io::Result<usize> {
    let now_secs = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let entries = match fs::read_dir(store.raw_metrics()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let rollups = store.rollup_metrics();
    let mut folded = 0;
    for entry in entries.filter_map(Result::ok) {
        let Some(hour) = SegmentName::parse(entry.file_name().as_encoded_bytes()) else {
            continue;
        };
        if hour.end_secs() > now_secs || !stale(&entry, &rollups.join(hour.to_string())) {
            continue;
        }
        match fold_hour(store, hour) {
            Ok(()) => folded += 1,
            Err(error) => tracing::warn!(%hour, %error, "could not fold a metric hour"),
        }
    }
    Ok(folded)
}

/// Whether the rollup at `rollup` is missing or no newer than its raw
/// segment. A rollup written in the same tick as a late append is redone.
fn stale(raw: &fs::DirEntry, rollup: &Path) -> bool {
    let Ok(raw) = raw.metadata().and_then(|meta| meta.modified()) else {
        return false;
    };
    match fs::symlink_metadata(rollup).and_then(|meta| meta.modified()) {
        Ok(rolled) => rolled <= raw,
        Err(_) => true,
    }
}

/// Folds one hour's raw rows into its rollup file, replacing any earlier
/// fold of that hour.
///
/// # Errors
///
/// Returns an error when the raw segments cannot be listed or the rollup
/// cannot be written.
pub fn fold_hour(store: &StoreRoot, hour: SegmentName) -> io::Result<()> {
    let nanos = |secs: u64| {
        i64::try_from(secs)
            .unwrap_or(i64::MAX)
            .saturating_mul(NANOS_PER_SEC)
    };
    let range = MetricRange {
        since_nanos: nanos(hour.start_secs()),
        until_nanos: nanos(hour.end_secs()),
    };
    // One step of lead-in gives each container's first rate of the hour its
    // previous sample.
    let lead_in = MetricRange {
        since_nanos: range.since_nanos.saturating_sub(STEP_NANOS),
        ..range
    };
    let mut hosts = Vec::new();
    let mut markers = Vec::new();
    let mut services: BTreeMap<ServiceKey, Vec<MetricSample>> = BTreeMap::new();
    segment::scan(
        &store.raw_metrics(),
        lead_in,
        &Prefilter::All,
        |row| match row {
            segment::Row::Host(sample) => hosts.push(sample),
            segment::Row::Container(sample) => {
                if let (Some(service_id), Some(service)) =
                    (sample.service_id, sample.service.clone())
                {
                    services
                        .entry((service_id, service))
                        .or_default()
                        .push(sample);
                }
            }
            segment::Row::Marker(marker) => {
                if marker.ts >= range.since_nanos {
                    markers.push(marker);
                }
            }
        },
    )?;

    let buckets = Buckets::new(range, Some(STEP_NANOS));
    let mut rows = Vec::new();
    if let MetricPoints::Machine(points) = rate::machine_history(&hosts, buckets).points {
        rows.extend(points.into_iter().map(Row::Host));
    }
    for ((service_id, service), samples) in services {
        if let MetricPoints::Service(points) = rate::service_history(&samples, buckets).points {
            rows.extend(points.into_iter().map(|point| {
                Row::Service(ServiceRow {
                    service_id,
                    service: service.clone(),
                    point,
                })
            }));
        }
    }
    rows.extend(markers.into_iter().map(Row::Marker));
    write_rollup(store, hour, &rows)
}

fn write_rollup(store: &StoreRoot, hour: SegmentName, rows: &[Row]) -> io::Result<()> {
    let mut buf = Vec::new();
    for row in rows {
        serde_json::to_writer(&mut buf, row)?;
        buf.push(b'\n');
    }
    layout::create_store_dir(&store.path().join(METRICS_DIR))?;
    let dir = store.rollup_metrics();
    layout::create_store_dir(&dir)?;
    let name = hour.to_string();
    let temp = dir.join(format!("{name}{TEMP_SUFFIX}"));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(OFlag::O_NOFOLLOW.bits())
        .open(&temp)?;
    file.write_all(&buf)?;
    file.sync_all()?;
    fs::rename(temp, dir.join(name))
}

/// Reads the rollup points `scope` selects with `ts` inside `range`, and the
/// markers it shows, each oldest first.
///
/// # Errors
///
/// Returns an error when the rollup dir exists but cannot be listed. A file
/// that cannot be read is logged and skipped.
pub fn read(
    store: &StoreRoot,
    range: MetricRange,
    scope: &MetricScope,
) -> io::Result<(Rolled, Vec<MetricMarker>)> {
    let mut hosts = Vec::new();
    let mut services = Vec::new();
    let mut markers = Vec::new();
    segment::scan(
        &store.rollup_metrics(),
        range,
        &Prefilter::of(scope),
        |row| match (scope, row) {
            (MetricScope::Machine, Row::Host(point)) => hosts.push(point),
            (MetricScope::Service { service }, Row::Service(row)) if row.service == *service => {
                services.push(row);
            }
            (_, Row::Marker(marker)) if segment::shows(scope, &marker) => markers.push(marker),
            _ => {}
        },
    )?;
    hosts.sort_by_key(|point| point.ts);
    services.sort_by_key(|row| row.point.ts);
    markers.sort_by_key(|marker| marker.ts);
    let rolled = match scope {
        MetricScope::Machine => Rolled::Machine(hosts),
        MetricScope::Service { .. } => Rolled::Service(services),
    };
    Ok((rolled, markers))
}

/// The end of the newest folded hour in Unix nanoseconds, or `None` before
/// the first fold. Raw segments answer for the time after it.
///
/// # Errors
///
/// Returns an error when the rollup dir exists but cannot be listed.
pub fn folded_until(store: &StoreRoot) -> io::Result<Option<i64>> {
    let entries = match fs::read_dir(store.rollup_metrics()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(entries
        .filter_map(Result::ok)
        .filter_map(|entry| SegmentName::parse(entry.file_name().as_encoded_bytes()))
        .map(SegmentName::end_secs)
        .max()
        .map(|secs| {
            i64::try_from(secs)
                .unwrap_or(i64::MAX)
                .saturating_mul(NANOS_PER_SEC)
        }))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{Duration, SystemTime},
    };

    use ployz_core::{
        ContainerId, MachineId, QualifiedService, ServiceId,
        metrics::{
            CpuJiffies, HostMemory, HostSample, InterfaceBytes, MarkerKind, MetricMarker,
            MetricRange, MetricSample, MetricScope,
        },
    };

    use super::{self as rollup, Rolled, STEP_NANOS, ServiceRow, fold_hour, fold_pending, read};
    use crate::{
        observe::{
            layout::{SegmentName, StoreRoot},
            metrics::segment::{self, Row},
        },
        test_dir::TestDir,
    };

    const SEC: i64 = 1_000_000_000;
    const HOUR: i64 = 3_600 * SEC;
    /// An hour boundary: 2025-10-09T08:00:00Z.
    const T0: i64 = 1_759_996_800 * SEC;

    fn store() -> (TestDir, StoreRoot) {
        let dir = TestDir::new("ployzd-metrics-rollup");
        fs::create_dir_all(&dir.0).unwrap();
        let root = StoreRoot::under(&dir.0);
        root.prepare().unwrap();
        (dir, root)
    }

    fn hour(ts: i64) -> SegmentName {
        SegmentName::containing(ts).unwrap()
    }

    fn service(n: u8) -> ServiceId {
        ServiceId::parse(format!("{n:02x}").repeat(16)).unwrap()
    }

    fn qualified(n: u8) -> QualifiedService {
        QualifiedService::parse(format!("prod/web-{n}")).unwrap()
    }

    fn deployment(n: u8) -> String {
        format!("01JABCDEFGHJKMNPQRSTVWXY{n:02}")
    }

    fn time(ts: i64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_nanos(u64::try_from(ts).unwrap())
    }

    /// A container that has burned `tick` seconds of CPU at one core per
    /// second and holds 100 MiB.
    fn sample(container: u32, service_n: u8, deployment_n: u8, tick: i64) -> MetricSample {
        let ticks = u64::try_from(tick + 1).unwrap();
        MetricSample {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            container_id: ContainerId::parse(format!("{container:08x}").repeat(8)).unwrap(),
            service_id: Some(service(service_n)),
            service: Some(qualified(service_n)),
            deployment_id: Some(deployment(deployment_n)),
            replica: format!("web-{container}"),
            ts: T0 + tick * 10 * SEC,
            cpu_usec: Some(48_213_337_120 + ticks * 10_000_000),
            mem_bytes: Some(100 << 20),
            mem_limit: Some(1 << 30),
            rx: Some(123_456_789_012 + ticks * 81_920),
            tx: Some(98_765_432_109 + ticks * 40_960),
            io_r: Some(5_368_709_120 + ticks * 512),
            io_w: Some(21_474_836_480 + ticks * 1_024),
        }
    }

    fn host(tick: i64) -> HostSample {
        let ticks = u64::try_from(tick + 1).unwrap();
        HostSample {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            ts: T0 + tick * 10 * SEC,
            cpu: Some(CpuJiffies {
                busy: 812_331_456 + ticks * 300,
                idle: 9_123_456_789 + ticks * 15_700,
            }),
            memory: Some(HostMemory {
                total: 137_438_953_472,
                available: 98_765_432_109,
            }),
            interfaces: ["eth0", "ployz-wg", "docker0", "lo"]
                .into_iter()
                .map(|name| {
                    let bytes = InterfaceBytes {
                        rx: 812_345_678_901 + ticks * 1_000_000,
                        tx: 712_345_678_901 + ticks * 500_000,
                    };
                    (name.to_owned(), bytes)
                })
                .collect(),
            disk: None,
        }
    }

    /// One hour at 10 second ticks of `containers` (container, service,
    /// Deployment) and the host, plus the tick before the hour.
    fn write_hour(store: &StoreRoot, containers: &[(u32, u8, u8)]) {
        for tick in -1..360 {
            let mut rows: Vec<Row> = containers
                .iter()
                .map(|&(container, service_n, deployment_n)| {
                    Row::Container(sample(container, service_n, deployment_n, tick))
                })
                .collect();
            rows.push(Row::Host(host(tick)));
            segment::append(store, &rows).unwrap();
        }
    }

    fn all() -> MetricRange {
        MetricRange {
            since_nanos: 0,
            until_nanos: i64::MAX,
        }
    }

    fn service_points(store: &StoreRoot, n: u8) -> Vec<ServiceRow> {
        let scope = MetricScope::Service {
            service: qualified(n),
        };
        let Rolled::Service(points) = read(store, all(), &scope).unwrap().0 else {
            panic!("service scope reads service points");
        };
        points
    }

    #[test]
    fn a_fold_gives_twelve_points_per_service_and_host_and_folding_again_keeps_the_bytes() {
        let (_dir, store) = store();
        write_hour(&store, &[(1, 1, 1), (2, 1, 2), (3, 2, 1)]);
        fold_hour(&store, hour(T0)).unwrap();
        let path = store.rollup_metrics().join("1759996800.jsonl");
        let first = fs::read(&path).unwrap();
        fold_hour(&store, hour(T0)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), first);

        let points = service_points(&store, 1);
        let starts: Vec<i64> = (0..12).map(|i| T0 + i * STEP_NANOS).collect();
        assert_eq!(
            points.iter().map(|p| p.point.ts).collect::<Vec<_>>(),
            starts
        );
        assert!(
            points
                .iter()
                .all(|p| p.service_id == service(1) && p.service == qualified(1)),
            "a point names its Service both ways"
        );
        let first = &points.first().unwrap().point;
        // Two containers of two Deployments at one core each, the first rate
        // seeded by the tick before the hour.
        assert_eq!(first.cpu_usec, Some(2_000_000.0));
        assert_eq!(first.mem_bytes, Some(200 << 20));
        assert_eq!(first.mem_limit, Some(2 << 30));

        let Rolled::Machine(hosts) = read(&store, all(), &MetricScope::Machine).unwrap().0 else {
            panic!("machine scope reads host points");
        };
        assert_eq!(hosts.len(), 12);
        assert_eq!(hosts.first().unwrap().cpu_busy, Some(300.0 / 16_000.0));
    }

    #[test]
    fn a_row_kind_this_reader_does_not_know_is_skipped_in_raw_and_rollup_files() {
        let (_dir, store) = store();
        write_hour(&store, &[(1, 1, 1)]);
        let marker = format!(
            "{{\"row\":\"marker\",\"kind\":\"oom\",\"service_id\":\"{}\",\"service\":\"{}\",\"ts\":{}}}\n",
            service(1),
            qualified(1),
            T0 + SEC
        );
        let append_marker = |path: std::path::PathBuf| {
            let mut bytes = fs::read(&path).unwrap();
            bytes.extend_from_slice(marker.as_bytes());
            fs::write(&path, bytes).unwrap();
        };
        append_marker(store.raw_metrics().join("1759996800.jsonl"));
        fold_hour(&store, hour(T0)).unwrap();
        append_marker(store.rollup_metrics().join("1759996800.jsonl"));

        assert_eq!(service_points(&store, 1).len(), 12);
        let Rolled::Machine(hosts) = read(&store, all(), &MetricScope::Machine).unwrap().0 else {
            panic!("machine scope reads host points");
        };
        assert_eq!(hosts.len(), 12);
    }

    #[test]
    fn a_container_deleted_mid_hour_still_counts_after_its_raw_hour_is_gone() {
        let (_dir, store) = store();
        for tick in -1..360 {
            let mut rows = vec![Row::Container(sample(1, 1, 1, tick))];
            if tick < 120 {
                rows.push(Row::Container(sample(2, 1, 1, tick)));
            }
            segment::append(&store, &rows).unwrap();
        }
        fold_hour(&store, hour(T0)).unwrap();
        fs::remove_dir_all(store.raw_metrics()).unwrap();

        let cpu: Vec<Option<f64>> = service_points(&store, 1)
            .into_iter()
            .map(|p| p.point.cpu_usec)
            .collect();
        let mut want = vec![Some(2_000_000.0); 4];
        want.extend([Some(1_000_000.0); 8]);
        assert_eq!(cpu, want);
    }

    #[test]
    fn a_deploy_mid_point_folds_to_one_replica_not_two() {
        let (_dir, store) = store();
        for tick in -1..360 {
            let (container, deployment_n) = if tick < 15 { (1, 1) } else { (2, 2) };
            segment::append(
                &store,
                &[Row::Container(sample(container, 1, deployment_n, tick))],
            )
            .unwrap();
        }
        fold_hour(&store, hour(T0)).unwrap();

        let points = service_points(&store, 1);
        let first: Vec<_> = points.iter().filter(|p| p.point.ts == T0).collect();
        let cpu: f64 = first.iter().filter_map(|p| p.point.cpu_usec).sum();
        let mem: u64 = first.iter().filter_map(|p| p.point.mem_bytes).sum();
        assert!((cpu - 1_000_000.0).abs() < 1.0, "{cpu} usec/s");
        assert_eq!(mem, 100 << 20);
    }

    #[test]
    fn markers_of_the_hour_outlive_its_raw_rows() {
        let (_dir, store) = store();
        let marker = |ts: i64, kind: MarkerKind| MetricMarker {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            container_id: ContainerId::parse("1".repeat(64)).unwrap(),
            service_id: service(1),
            service: qualified(1),
            deployment_id: Some(deployment(1)),
            replica: "cache-1".into(),
            ts,
            kind,
        };
        let before = marker(T0 - SEC, MarkerKind::Oom);
        let oom = marker(T0 + 600 * SEC, MarkerKind::Oom);
        let exit = marker(
            T0 + 600 * SEC + 1,
            MarkerKind::Exit {
                exit_code: Some(137),
            },
        );
        let rows: Vec<Row> = [&before, &oom, &exit]
            .into_iter()
            .map(|m| Row::Marker(m.clone()))
            .collect();
        segment::append(&store, &rows).unwrap();
        fold_hour(&store, hour(T0)).unwrap();
        fs::remove_dir_all(store.raw_metrics()).unwrap();

        let mut kept = Vec::new();
        segment::scan(
            &store.rollup_metrics(),
            all(),
            &segment::Prefilter::All,
            |row| {
                if let rollup::Row::Marker(m) = row {
                    kept.push(m);
                }
            },
        )
        .unwrap();
        assert_eq!(kept, vec![oom, exit]);
    }

    #[test]
    fn pending_folds_take_closed_hours_once_and_redo_one_that_changed() {
        let (_dir, store) = store();
        write_hour(&store, &[(1, 1, 1)]);
        segment::append(&store, &[Row::Container(sample(1, 1, 1, 360))]).unwrap();
        let touch = |hour_start: i64, at: SystemTime| {
            let name = hour(hour_start).to_string();
            fs::File::options()
                .write(true)
                .open(store.raw_metrics().join(name))
                .unwrap()
                .set_modified(at)
                .unwrap();
        };
        // A rollup written in the same mtime tick as its raw hour is redone,
        // so each hour must have closed visibly earlier.
        touch(T0 - HOUR, time(T0));
        touch(T0, time(T0 + HOUR));

        // The tick before T0 sits in the hour before, already closed.
        assert_eq!(fold_pending(&store, time(T0 + HOUR - SEC)).unwrap(), 1);
        assert_eq!(fold_pending(&store, time(T0 + HOUR)).unwrap(), 1);
        assert_eq!(fold_pending(&store, time(T0 + HOUR)).unwrap(), 0);
        assert_eq!(service_points(&store, 1).len(), 1 + 12);

        let rollup = store.rollup_metrics().join(hour(T0).to_string());
        let rolled = fs::metadata(rollup).unwrap().modified().unwrap();
        touch(T0, rolled + Duration::from_secs(1));
        assert_eq!(fold_pending(&store, time(T0 + 2 * HOUR)).unwrap(), 2);
    }

    #[test]
    fn an_empty_store_folds_and_reads_nothing() {
        let (_dir, store) = store();
        assert_eq!(fold_pending(&store, time(T0)).unwrap(), 0);
        assert_eq!(
            read(&store, all(), &MetricScope::Machine).unwrap().0,
            Rolled::Machine(Vec::new())
        );
        assert!(service_points(&store, 1).is_empty());
    }

    /// Sizes a day of rollups for 50 containers and the host, every field
    /// present, for the PR record.
    #[test]
    fn a_day_of_fifty_containers_stays_under_ten_megabytes() {
        for services in [10_u8, 50] {
            let (_dir, store) = store();
            let containers: Vec<(u32, u8, u8)> = (0..50)
                .map(|n| (n, u8::try_from(n).unwrap() % services, 1))
                .collect();
            write_hour(&store, &containers);
            fold_hour(&store, hour(T0)).unwrap();
            let bytes = fs::metadata(store.rollup_metrics().join("1759996800.jsonl"))
                .unwrap()
                .len();
            let day = bytes * 24;
            println!(
                "rollup, 50 containers in {services} Services + host: {bytes} bytes/hour, {day} bytes/day"
            );
            assert!(day < 10 << 20, "{day} bytes/day");
        }
    }
}
