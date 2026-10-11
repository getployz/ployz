//! Answers one metric history query from the stored rows.
//!
//! A range that starts within the last 23 hours reads raw segments and can
//! take any step. A longer range reads the 5 minute rollups, then raw rows for
//! the hours not folded yet, and averages both into buckets no narrower than
//! 5 minutes. Cleanup deletes a raw hour once it began a day ago, so 23 hours
//! is the oldest start whose first hour is still whole.

use std::{
    collections::{BTreeMap, btree_map::Entry},
    io,
};

use ployz_core::metrics::{
    DiskBreakdown, HostMemory, HostPoint, InterfaceRate, MetricGap, MetricGapReason,
    MetricHistoryReply, MetricHistoryRequest, MetricPoints, MetricRange, MetricScope, ServicePoint,
};

use super::{
    rate::{self, Buckets, Mean},
    rollup::{self, Rolled, STEP_NANOS, ServiceRow},
    segment::{self, Selected},
};
use crate::observe::layout::StoreRoot;

const HOUR_NANOS: i64 = 3_600 * 1_000_000_000;
const RAW_SPAN: i64 = 23 * HOUR_NANOS;
/// Read before a raw range so its first rate has a previous sample.
const LEAD_IN: i64 = STEP_NANOS;

/// The bucket grid `request` is answered on at `now` Unix nanoseconds.
fn grid(request: &MetricHistoryRequest, now: i64) -> Buckets {
    let buckets = Buckets::new(request.range, request.step_nanos);
    if raw_only(request.range, now) || buckets.step() >= STEP_NANOS {
        buckets
    } else {
        Buckets::new(request.range, Some(STEP_NANOS))
    }
}

fn raw_only(range: MetricRange, now: i64) -> bool {
    range.since_nanos >= now.saturating_sub(RAW_SPAN)
}

/// Reads the history `request` asks for, as of `now` Unix nanoseconds.
///
/// Runs to the end once started; the caller bounds how long it waits.
///
/// # Errors
///
/// Returns an error when a metric dir exists but cannot be listed.
pub(crate) fn history(
    store: &StoreRoot,
    request: &MetricHistoryRequest,
    now: i64,
) -> io::Result<MetricHistoryReply> {
    let range = request.range;
    let buckets = grid(request, now);
    if raw_only(range, now) {
        return raw(store, range, &request.scope, buckets);
    }
    let raw_from = rollup::folded_until(store)?.map_or(range.since_nanos, |until| {
        until.clamp(range.since_nanos, range.until_nanos)
    });
    let (rolled, mut markers) = rollup::read(
        store,
        MetricRange {
            until_nanos: raw_from,
            ..range
        },
        &request.scope,
    )?;
    let fresh = if raw_from < range.until_nanos {
        let tail = MetricRange {
            since_nanos: raw_from,
            ..range
        };
        let tail = raw(
            store,
            tail,
            &request.scope,
            Buckets::new(tail, Some(STEP_NANOS)),
        )?;
        markers.extend(tail.markers);
        Some(tail.points)
    } else {
        None
    };
    let mut reply = match rolled {
        Rolled::Machine(mut points) => {
            if let Some(MetricPoints::Machine(fresh)) = fresh {
                points.extend(fresh);
            }
            average_hosts(&points, buckets)
        }
        Rolled::Service(rows) => {
            let mut points = sum_services(rows);
            if let Some(MetricPoints::Service(fresh)) = fresh {
                points.extend(fresh);
            }
            average_services(&points, buckets)
        }
    };
    reply.markers = markers;
    Ok(reply)
}

/// The reply for a read that did not finish in time: no points, and one
/// `timeout` gap over the whole range.
pub(crate) fn timed_out(request: &MetricHistoryRequest, now: i64) -> MetricHistoryReply {
    MetricHistoryReply {
        points: match request.scope {
            MetricScope::Machine => MetricPoints::Machine(Vec::new()),
            MetricScope::Service { .. } => MetricPoints::Service(Vec::new()),
        },
        step_nanos: grid(request, now).step(),
        gaps: vec![MetricGap {
            from: request.range.since_nanos,
            to: request.range.until_nanos,
            reason: MetricGapReason::Timeout,
        }],
        markers: Vec::new(),
    }
}

fn raw(
    store: &StoreRoot,
    range: MetricRange,
    scope: &MetricScope,
    buckets: Buckets,
) -> io::Result<MetricHistoryReply> {
    let lead_in = MetricRange {
        since_nanos: range.since_nanos.saturating_sub(LEAD_IN),
        ..range
    };
    let (selected, mut markers) = segment::read(store, lead_in, scope)?;
    markers.retain(|marker| marker.ts >= range.since_nanos);
    let mut reply = match selected {
        Selected::Machine(samples) => rate::machine_history(&samples, buckets),
        Selected::Service(samples) => rate::service_history(&samples, buckets),
    };
    reply.markers = markers;
    Ok(reply)
}

fn rates(point: &ServicePoint) -> [Option<f64>; 5] {
    [point.cpu_usec, point.rx, point.tx, point.io_r, point.io_w]
}

/// A Service's point at each `ts`: the points of every Service id that held
/// its name, summed. The memory limit is unknown once any id's is.
fn sum_services(rows: Vec<ServiceRow>) -> Vec<ServicePoint> {
    let mut at: BTreeMap<i64, ServicePoint> = BTreeMap::new();
    for ServiceRow { point, .. } in rows {
        let sum = match at.entry(point.ts) {
            Entry::Vacant(entry) => {
                entry.insert(point);
                continue;
            }
            Entry::Occupied(entry) => entry.into_mut(),
        };
        let added = rates(&point);
        for (total, rate) in [
            &mut sum.cpu_usec,
            &mut sum.rx,
            &mut sum.tx,
            &mut sum.io_r,
            &mut sum.io_w,
        ]
        .into_iter()
        .zip(added)
        {
            if let Some(rate) = rate {
                *total = Some(total.unwrap_or(0.0) + rate);
            }
        }
        if let Some(mem) = point.mem_bytes {
            sum.mem_bytes = Some(sum.mem_bytes.unwrap_or(0).saturating_add(mem));
        }
        sum.mem_limit = sum
            .mem_limit
            .zip(point.mem_limit)
            .map(|(total, limit)| total.saturating_add(limit));
    }
    at.into_values().collect()
}

/// Mean of a rate over the points that reported it.
#[derive(Clone, Copy, Debug, Default)]
struct RateMean {
    sum: f64,
    count: u32,
}

impl RateMean {
    fn add(&mut self, value: Option<f64>) {
        if let Some(value) = value {
            self.sum += value;
            self.count += 1;
        }
    }

    fn get(self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / f64::from(self.count))
    }
}

/// Folds each point into the bucket its `ts` falls in. Returns the filled
/// buckets with their start, and a `not_sampled` gap per empty run.
fn fill<P, B: Default>(
    points: &[P],
    buckets: Buckets,
    ts: impl Fn(&P) -> i64,
    mut add: impl FnMut(&mut B, &P),
) -> (Vec<(i64, B)>, Vec<MetricGap>) {
    let mut table: Vec<Option<B>> = Vec::new();
    table.resize_with(buckets.len(), || None);
    for point in points {
        if let Some(slot) = buckets
            .index(ts(point))
            .and_then(|index| table.get_mut(index))
        {
            add(slot.get_or_insert_with(B::default), point);
        }
    }
    let gaps = buckets.gaps(&table);
    let filled = table
        .into_iter()
        .enumerate()
        .filter_map(|(index, bucket)| Some((buckets.start(index), bucket?)))
        .collect();
    (filled, gaps)
}

struct ServiceMean {
    rates: [RateMean; 5],
    mem: Mean,
    /// `None` once a point in the bucket had no limit.
    limit: Option<Mean>,
}

impl Default for ServiceMean {
    fn default() -> Self {
        Self {
            rates: [RateMean::default(); 5],
            mem: Mean::default(),
            limit: Some(Mean::default()),
        }
    }
}

/// Averages 5 minute Service points into `buckets`.
fn average_services(points: &[ServicePoint], buckets: Buckets) -> MetricHistoryReply {
    let (filled, gaps) = fill(
        points,
        buckets,
        |point| point.ts,
        |mean: &mut ServiceMean, point| {
            for (rate, value) in mean.rates.iter_mut().zip(rates(point)) {
                rate.add(value);
            }
            mean.mem.add(point.mem_bytes);
            mean.limit = mean.limit.zip(point.mem_limit).map(|(mut limit, value)| {
                limit.add(Some(value));
                limit
            });
        },
    );
    let points = filled
        .into_iter()
        .map(|(ts, mean)| {
            let [cpu_usec, rx, tx, io_r, io_w] = mean.rates.map(RateMean::get);
            ServicePoint {
                ts,
                cpu_usec,
                mem_bytes: mean.mem.get(),
                mem_limit: mean.limit.and_then(Mean::get),
                rx,
                tx,
                io_r,
                io_w,
            }
        })
        .collect();
    MetricHistoryReply {
        points: MetricPoints::Service(points),
        step_nanos: buckets.step(),
        gaps,
        markers: Vec::new(),
    }
}

#[derive(Default)]
struct HostMean {
    busy: RateMean,
    total: Mean,
    available: Mean,
    interfaces: BTreeMap<String, (RateMean, RateMean)>,
    disk: Option<DiskBreakdown>,
}

/// Averages 5 minute host points into `buckets`. Disk is the bucket's last
/// reading.
fn average_hosts(points: &[HostPoint], buckets: Buckets) -> MetricHistoryReply {
    let (filled, gaps) = fill(
        points,
        buckets,
        |point| point.ts,
        |mean: &mut HostMean, point| {
            mean.busy.add(point.cpu_busy);
            if let Some(memory) = point.memory {
                mean.total.add(Some(memory.total));
                mean.available.add(Some(memory.available));
            }
            for (name, rate) in &point.interfaces {
                let (rx, tx) = mean.interfaces.entry(name.clone()).or_default();
                rx.add(Some(rate.rx));
                tx.add(Some(rate.tx));
            }
            if point.disk.is_some() {
                mean.disk = point.disk;
            }
        },
    );
    let points = filled
        .into_iter()
        .map(|(ts, mean)| HostPoint {
            ts,
            cpu_busy: mean.busy.get(),
            memory: mean
                .total
                .get()
                .zip(mean.available.get())
                .map(|(total, available)| HostMemory { total, available }),
            interfaces: mean
                .interfaces
                .into_iter()
                .filter_map(|(name, (rx, tx))| {
                    Some((
                        name,
                        InterfaceRate {
                            rx: rx.get()?,
                            tx: tx.get()?,
                        },
                    ))
                })
                .collect(),
            disk: mean.disk,
        })
        .collect();
    MetricHistoryReply {
        points: MetricPoints::Machine(points),
        step_nanos: buckets.step(),
        gaps,
        markers: Vec::new(),
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
pub(crate) mod tests {
    use std::{fs, time::Instant};

    use ployz_core::{
        ContainerId, MachineId, QualifiedService, ServiceId,
        metrics::{
            CpuJiffies, HostMemory, HostPoint, HostSample, InterfaceBytes, InterfaceRate,
            MarkerKind, MetricGap, MetricGapReason, MetricHistoryReply, MetricHistoryRequest,
            MetricMarker, MetricPoints, MetricRange, MetricSample, MetricScope, ServicePoint,
        },
    };

    use super::history;
    use crate::{
        observe::{
            layout::{SegmentName, StoreRoot},
            metrics::{
                rollup::{self, STEP_NANOS, ServiceRow},
                segment::{self, Row},
            },
        },
        test_dir::TestDir,
    };

    pub(crate) const SEC: i64 = 1_000_000_000;
    const MIN: i64 = 60 * SEC;
    const HOUR: i64 = 60 * MIN;
    const DAY: i64 = 24 * HOUR;
    /// An hour boundary: 2025-10-09T08:00:00Z.
    const T0: i64 = 1_759_996_800 * SEC;

    fn store() -> (TestDir, StoreRoot) {
        let dir = TestDir::new("ployzd-metrics-history");
        fs::create_dir_all(&dir.0).unwrap();
        let root = StoreRoot::under(&dir.0);
        root.prepare().unwrap();
        (dir, root)
    }

    fn service(n: u8) -> ServiceId {
        ServiceId::parse(format!("{n:02x}").repeat(16)).unwrap()
    }

    fn qualified(n: u8) -> QualifiedService {
        QualifiedService::parse(format!("prod/web-{n}")).unwrap()
    }

    fn machine() -> MachineId {
        MachineId::parse("a".repeat(32)).unwrap()
    }

    fn secs(ts: i64) -> u64 {
        u64::try_from(ts / SEC).unwrap()
    }

    /// Container `n` of Service `service_n`, or a system container without
    /// one, whose CPU, net and IO counters grow `per_sec` each second.
    fn container(n: u8, service_n: Option<u8>, deployment: u8, ts: i64, per_sec: u64) -> Row {
        let total = Some(per_sec * secs(ts));
        Row::Container(MetricSample {
            machine_id: machine(),
            container_id: ContainerId::parse(format!("{n:02x}").repeat(32)).unwrap(),
            service_id: service_n.map(service),
            service: service_n.map(qualified),
            deployment_id: Some(format!("deployment-{deployment}")),
            replica: format!("web-{n}"),
            ts,
            cpu_usec: total,
            mem_bytes: Some(100),
            mem_limit: Some(1_000),
            rx: total,
            tx: total,
            io_r: total,
            io_w: total,
        })
    }

    /// A Machine a quarter busy, receiving 1,000 B/s and sending 500 B/s.
    pub(crate) fn host(ts: i64) -> Row {
        let secs = secs(ts);
        Row::Host(HostSample {
            machine_id: machine(),
            ts,
            cpu: Some(CpuJiffies {
                busy: secs,
                idle: 3 * secs,
            }),
            memory: Some(HostMemory {
                total: 4_000,
                available: 1_000,
            }),
            interfaces: [(
                "eth0".to_owned(),
                InterfaceBytes {
                    rx: 1_000 * secs,
                    tx: 500 * secs,
                },
            )]
            .into(),
            disk: None,
        })
    }

    fn marker(service_n: u8, ts: i64, kind: MarkerKind) -> MetricMarker {
        MetricMarker {
            machine_id: machine(),
            container_id: ContainerId::parse(format!("{service_n:02x}").repeat(32)).unwrap(),
            service_id: service(service_n),
            service: qualified(service_n),
            deployment_id: Some("deployment-1".into()),
            replica: format!("web-{service_n}"),
            ts,
            kind,
        }
    }

    fn append_markers(store: &StoreRoot, markers: &[MetricMarker]) {
        let rows: Vec<Row> = markers.iter().cloned().map(Row::Marker).collect();
        segment::append(store, &rows).unwrap();
    }

    /// Appends `rows(ts)` every 10 seconds of `[from, to)`.
    pub(crate) fn write(store: &StoreRoot, from: i64, to: i64, rows: impl Fn(i64) -> Vec<Row>) {
        let all: Vec<Row> = (from..to)
            .step_by(usize::try_from(10 * SEC).unwrap())
            .flat_map(rows)
            .collect();
        segment::append(store, &all).unwrap();
    }

    fn request(
        scope: MetricScope,
        since: i64,
        until: i64,
        step: Option<i64>,
    ) -> MetricHistoryRequest {
        MetricHistoryRequest {
            scope,
            range: MetricRange {
                since_nanos: since,
                until_nanos: until,
            },
            step_nanos: step,
        }
    }

    fn service_points(reply: &MetricHistoryReply) -> &[ServicePoint] {
        match &reply.points {
            MetricPoints::Service(points) => points,
            MetricPoints::Machine(_) => panic!("expected Service points"),
        }
    }

    fn host_points(reply: &MetricHistoryReply) -> &[HostPoint] {
        match &reply.points {
            MetricPoints::Machine(points) => points,
            MetricPoints::Service(_) => panic!("expected Machine points"),
        }
    }

    pub(crate) fn near(value: Option<f64>, expected: f64) -> bool {
        value.is_some_and(|value| (value - expected).abs() < expected * 1e-6)
    }

    fn assert_host_rates(points: &[HostPoint]) {
        for point in points {
            assert!(near(point.cpu_busy, 0.25), "{point:?}");
            let eth0 = point.interfaces["eth0"];
            assert!(near(Some(eth0.rx), 1_000.0), "{point:?}");
            assert!(near(Some(eth0.tx), 500.0), "{point:?}");
        }
    }

    #[test]
    fn a_recent_machine_range_reads_raw_rates() {
        let (_dir, store) = store();
        write(&store, T0, T0 + HOUR, |ts| vec![host(ts)]);

        let asked = request(MetricScope::Machine, T0 + 30 * MIN, T0 + HOUR, Some(MIN));
        let reply = history(&store, &asked, T0 + HOUR).unwrap();

        assert_eq!(reply.step_nanos, MIN);
        assert_eq!(reply.gaps, []);
        let points = host_points(&reply);
        assert_eq!(points.len(), 30);
        assert_eq!(points[0].ts, T0 + 30 * MIN);
        assert_host_rates(points);
    }

    #[test]
    fn a_service_leaves_out_other_services_and_system_containers() {
        let (_dir, store) = store();
        write(&store, T0, T0 + HOUR, |ts| {
            vec![
                container(1, Some(1), 1, ts, 10),
                container(2, Some(1), 1, ts, 20),
                container(3, None, 0, ts, 1_000),
                container(4, Some(2), 1, ts, 5_000),
            ]
        });

        let scope = MetricScope::Service {
            service: qualified(1),
        };
        let asked = request(scope, T0 + 30 * MIN, T0 + HOUR, Some(STEP_NANOS));
        let reply = history(&store, &asked, T0 + HOUR).unwrap();

        let points = service_points(&reply);
        assert_eq!(points.len(), 6);
        for point in points {
            assert!(near(point.cpu_usec, 30.0), "{point:?}");
            assert!(near(point.rx, 30.0), "{point:?}");
            assert_eq!(point.mem_bytes, Some(200));
            assert_eq!(point.mem_limit, Some(2_000));
        }
    }

    #[test]
    fn a_week_reads_rollups_then_the_unfolded_hours_from_raw() {
        let (_dir, store) = store();
        write(&store, T0, T0 + 3 * HOUR, |ts| {
            vec![
                host(ts),
                container(1, Some(1), 1, ts, 10),
                container(2, Some(1), 2, ts, 20),
            ]
        });
        for hour in [T0, T0 + HOUR] {
            rollup::fold_hour(&store, SegmentName::containing(hour).unwrap()).unwrap();
        }
        // Cleanup took the oldest raw hour; only its rollup is left.
        let oldest = SegmentName::containing(T0).unwrap().to_string();
        fs::remove_file(store.raw_metrics().join(oldest)).unwrap();
        let now = T0 + 3 * HOUR;
        let since = now - 7 * DAY;
        let step = 7 * DAY / 300;

        let scope = MetricScope::Service {
            service: qualified(1),
        };
        let reply = history(&store, &request(scope, since, now, None), now).unwrap();
        assert_eq!(reply.step_nanos, step);
        let points = service_points(&reply);
        assert!(points.first().unwrap().ts <= T0, "{points:?}");
        assert!(points.last().unwrap().ts > T0 + 2 * HOUR, "{points:?}");
        for point in points {
            // Both Deployments, summed.
            assert!(near(point.cpu_usec, 30.0), "{point:?}");
            assert_eq!(point.mem_bytes, Some(200));
            assert_eq!(point.mem_limit, Some(2_000));
        }
        assert_eq!(
            reply.gaps.first(),
            Some(&MetricGap {
                from: since,
                to: points[0].ts,
                reason: MetricGapReason::NotSampled
            })
        );

        let asked = request(MetricScope::Machine, since, now, None);
        let reply = history(&store, &asked, now).unwrap();
        let points = host_points(&reply);
        assert!(points.first().unwrap().ts <= T0, "{points:?}");
        assert!(points.last().unwrap().ts > T0 + 2 * HOUR, "{points:?}");
        assert_host_rates(points);
    }

    #[test]
    fn a_recent_range_returns_the_markers_inside_it_for_its_scope() {
        let (_dir, store) = store();
        let since = T0 + 30 * MIN;
        let until = T0 + HOUR;
        let restart = marker(1, T0 + 50 * MIN, MarkerKind::Restart);
        let oom = marker(1, T0 + 40 * MIN, MarkerKind::Oom);
        let other = marker(
            2,
            T0 + 45 * MIN,
            MarkerKind::Exit {
                exit_code: Some(137),
            },
        );
        append_markers(
            &store,
            &[
                marker(1, T0 + 10 * MIN, MarkerKind::Oom),
                // Inside the lead-in read for the first rate, not the range.
                marker(1, since - MIN, MarkerKind::Restart),
                restart.clone(),
                oom.clone(),
                other.clone(),
                marker(1, until, MarkerKind::Oom),
            ],
        );

        let scope = MetricScope::Service {
            service: qualified(1),
        };
        let reply = history(&store, &request(scope, since, until, None), until).unwrap();
        assert_eq!(reply.markers, [oom.clone(), restart.clone()]);

        let asked = request(MetricScope::Machine, since, until, None);
        let reply = history(&store, &asked, until).unwrap();
        assert_eq!(reply.markers, [oom, other, restart]);
    }

    #[test]
    fn a_wide_range_returns_rolled_and_raw_tail_markers_once() {
        let (_dir, store) = store();
        write(&store, T0, T0 + 3 * HOUR, |ts| {
            vec![host(ts), container(1, Some(1), 1, ts, 10)]
        });
        let rolled = marker(1, T0 + 20 * MIN, MarkerKind::Oom);
        // Folded, and also inside the raw tail's lead-in.
        let seam = marker(1, T0 + 2 * HOUR - 2 * MIN, MarkerKind::Restart);
        let other = marker(2, T0 + HOUR + 30 * MIN, MarkerKind::Oom);
        let tail = marker(
            1,
            T0 + 2 * HOUR + 20 * MIN,
            MarkerKind::Exit { exit_code: Some(0) },
        );
        append_markers(
            &store,
            &[rolled.clone(), seam.clone(), other.clone(), tail.clone()],
        );
        for hour in [T0, T0 + HOUR] {
            rollup::fold_hour(&store, SegmentName::containing(hour).unwrap()).unwrap();
        }
        let oldest = SegmentName::containing(T0).unwrap().to_string();
        fs::remove_file(store.raw_metrics().join(oldest)).unwrap();
        let now = T0 + 3 * HOUR;
        let since = now - 7 * DAY;

        let scope = MetricScope::Service {
            service: qualified(1),
        };
        let reply = history(&store, &request(scope, since, now, None), now).unwrap();
        assert_eq!(reply.markers, [rolled.clone(), seam.clone(), tail.clone()]);

        let asked = request(MetricScope::Machine, since, now, None);
        let reply = history(&store, &asked, now).unwrap();
        assert_eq!(reply.markers, [rolled, other, seam, tail]);
    }

    #[test]
    fn an_empty_store_is_one_not_sampled_gap() {
        let (_dir, store) = store();
        let now = T0;
        for scope in [
            MetricScope::Machine,
            MetricScope::Service {
                service: qualified(1),
            },
        ] {
            for since in [now - HOUR, now - 30 * DAY] {
                let asked = request(scope.clone(), since, now, None);
                let reply = history(&store, &asked, now).unwrap();
                assert_eq!(
                    reply.gaps,
                    [MetricGap {
                        from: since,
                        to: now,
                        reason: MetricGapReason::NotSampled
                    }]
                );
                let empty = match scope {
                    MetricScope::Machine => MetricPoints::Machine(Vec::new()),
                    MetricScope::Service { .. } => MetricPoints::Service(Vec::new()),
                };
                assert_eq!(reply.points, empty);
            }
        }
    }

    #[test]
    fn a_step_widens_to_4000_buckets_and_to_rollups_past_a_day() {
        let (_dir, store) = store();
        let now = T0;
        let step = |since: i64| {
            let asked = request(MetricScope::Machine, since, now, Some(SEC));
            history(&store, &asked, now).unwrap().step_nanos
        };
        assert_eq!(step(now - HOUR), SEC);
        assert_eq!(step(now - 7 * DAY), STEP_NANOS);
        assert_eq!(step(now - 90 * DAY), 90 * DAY / 4_000);
    }

    /// 90 days of rollups for 50 Services, two Deployments each.
    #[test]
    #[ignore = "perf: writes about 300 MB of rollups"]
    fn ninety_days_of_fifty_services_answers_in_500_ms() {
        let (_dir, store) = store();
        let dir = store.rollup_metrics();
        fs::create_dir_all(&dir).unwrap();
        let now = T0;
        let since = now - 90 * DAY;
        let line = |buf: &mut Vec<u8>, row: &rollup::Row| {
            serde_json::to_writer(&mut *buf, row).unwrap();
            buf.push(b'\n');
        };
        for (index, hour) in (since..now)
            .step_by(usize::try_from(HOUR).unwrap())
            .enumerate()
        {
            let mut buf = Vec::new();
            let restarted = u8::try_from(index % 50).unwrap();
            line(
                &mut buf,
                &rollup::Row::Marker(marker(restarted, hour, MarkerKind::Restart)),
            );
            for ts in (hour..hour + HOUR).step_by(usize::try_from(STEP_NANOS).unwrap()) {
                let host = HostPoint {
                    ts,
                    cpu_busy: Some(0.25),
                    memory: Some(HostMemory {
                        total: 4_000,
                        available: 1_000,
                    }),
                    interfaces: [("eth0".to_owned(), InterfaceRate { rx: 1e3, tx: 5e2 })].into(),
                    disk: None,
                };
                line(&mut buf, &rollup::Row::Host(host));
                for n in 0..50 {
                    // Two Service ids that held the name, summed on read.
                    for id in [n, n + 100] {
                        let point = ServicePoint {
                            ts,
                            cpu_usec: Some(1e5),
                            mem_bytes: Some(1 << 28),
                            mem_limit: Some(1 << 30),
                            rx: Some(8e4),
                            tx: Some(4e4),
                            io_r: Some(512.0),
                            io_w: Some(1_024.0),
                        };
                        let row = rollup::Row::Service(ServiceRow {
                            service_id: service(id),
                            service: qualified(n),
                            point,
                        });
                        line(&mut buf, &row);
                    }
                }
            }
            let name = SegmentName::containing(hour).unwrap().to_string();
            fs::write(dir.join(name), buf).unwrap();
        }

        for scope in [
            MetricScope::Machine,
            MetricScope::Service {
                service: qualified(7),
            },
        ] {
            let started = Instant::now();
            let reply = history(&store, &request(scope.clone(), since, now, None), now).unwrap();
            let took = started.elapsed();
            eprintln!("{scope:?}: {took:?}");
            assert_eq!(reply.gaps, []);
            match &reply.points {
                MetricPoints::Machine(points) => {
                    assert_eq!(points.len(), 300);
                    assert_eq!(reply.markers.len(), 90 * 24);
                }
                MetricPoints::Service(points) => {
                    assert_eq!(points.len(), 300);
                    // Hours 7, 57, ... 2157 of 2160.
                    assert_eq!(reply.markers.len(), 44);
                    assert!(near(points[0].cpu_usec, 2e5), "{:?}", points[0]);
                }
            }
            assert!(took.as_millis() < 500, "{scope:?} took {took:?}");
        }
    }
}
