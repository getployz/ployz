//! Turns raw cumulative samples into per-second rates, bucketed to about 300
//! points per range.
//!
//! A rate comes from two consecutive samples of one series and lands in the
//! bucket of the later sample. A counter that went down was reset, so that
//! interval yields no rate rather than a negative one. Within a bucket a
//! Service's rate is every container's deltas summed over the time any of them
//! covered, so a replica replaced mid-bucket counts once while concurrent
//! replicas add up. A bucket no sample landed in is a `not_sampled` gap, never
//! a zero point.

use std::collections::BTreeMap;

use ployz_core::{
    ContainerId,
    metrics::{
        DiskBreakdown, HostMemory, HostPoint, HostSample, InterfaceBytes, InterfaceRate, MetricGap,
        MetricGapReason, MetricHistoryReply, MetricPoints, MetricRange, MetricSample, ServicePoint,
    },
};

const TARGET_BUCKETS: i64 = 300;
/// The sampler's tick. A default bucket narrower than this would be empty
/// between samples.
const SAMPLE_NANOS: i64 = 10_000_000_000;
/// Bounds the bucket table whatever step a caller asks for.
const MAX_BUCKETS: i64 = 4_000;
const NANOS_PER_SEC: f64 = 1e9;

/// The bucket grid of one history query: `[since, until)` cut into `step`
/// wide buckets, the last one possibly shorter.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Buckets {
    since: i64,
    until: i64,
    step: i64,
    len: usize,
}

impl Buckets {
    /// Uses `step_nanos` when given, else the width that yields about 300
    /// buckets, but no less than one sample. Either way the step widens until the grid fits `MAX_BUCKETS`.
    pub(crate) fn new(range: MetricRange, step_nanos: Option<i64>) -> Self {
        let since = range.since_nanos;
        let width = range.until_nanos.saturating_sub(since).max(1);
        let floor = div_ceil(width, MAX_BUCKETS);
        let step = step_nanos
            .unwrap_or_else(|| div_ceil(width, TARGET_BUCKETS).max(SAMPLE_NANOS))
            .clamp(floor.max(1), width);
        Self {
            since,
            until: since.saturating_add(width),
            step,
            len: usize::try_from(div_ceil(width, step)).expect("at most MAX_BUCKETS buckets"),
        }
    }

    pub(super) fn step(&self) -> i64 {
        self.step
    }

    pub(super) fn len(&self) -> usize {
        self.len
    }

    pub(super) fn index(&self, ts: i64) -> Option<usize> {
        if ts < self.since || ts >= self.until {
            return None;
        }
        usize::try_from((ts - self.since) / self.step).ok()
    }

    pub(super) fn start(&self, index: usize) -> i64 {
        self.since + self.step * i64::try_from(index).expect("index is below len")
    }

    fn end(&self, index: usize) -> i64 {
        self.start(index).saturating_add(self.step).min(self.until)
    }

    /// One merged `not_sampled` gap per run of buckets nothing landed in.
    pub(super) fn gaps<T>(&self, buckets: &[Option<T>]) -> Vec<MetricGap> {
        let mut gaps: Vec<MetricGap> = Vec::new();
        for (index, _) in buckets.iter().enumerate().filter(|(_, b)| b.is_none()) {
            let (from, to) = (self.start(index), self.end(index));
            match gaps.last_mut() {
                Some(last) if last.to == from => last.to = to,
                Some(_) | None => gaps.push(MetricGap {
                    from,
                    to,
                    reason: MetricGapReason::NotSampled,
                }),
            }
        }
        gaps
    }
}

/// Ceiling division of positive values; `i64::div_ceil` is unstable.
fn div_ceil(value: i64, by: i64) -> i64 {
    value / by + i64::from(value % by != 0)
}

/// Summed counter deltas and the time their intervals covered.
///
/// Intervals arrive in order of their end. Time an earlier interval already
/// covered counts once, so concurrent replicas share one span of time while
/// their deltas add up.
#[derive(Clone, Copy, Debug)]
struct Delta {
    amount: u64,
    nanos: u64,
    covered_until: i64,
}

impl Default for Delta {
    fn default() -> Self {
        Self {
            amount: 0,
            nanos: 0,
            covered_until: i64::MIN,
        }
    }
}

impl Delta {
    /// Adds the interval `from..to` unless either reading is missing or the
    /// counter went down, which means it was reset.
    fn add(&mut self, prev: Option<u64>, next: Option<u64>, from: i64, to: i64) {
        if let (Some(prev), Some(next)) = (prev, next)
            && next >= prev
            && to > from
        {
            self.amount = self.amount.saturating_add(next - prev);
            let from = from.max(self.covered_until);
            if let Ok(nanos @ 1..) = u64::try_from(to.saturating_sub(from)) {
                self.nanos = self.nanos.saturating_add(nanos);
                self.covered_until = to;
            }
        }
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "rates are approximate; f64 holds counters to 2^53 exactly"
    )]
    fn per_sec(self) -> Option<f64> {
        (self.nanos > 0).then(|| self.amount as f64 * NANOS_PER_SEC / self.nanos as f64)
    }
}

/// Mean of a gauge over the samples that reported it.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Mean {
    sum: u128,
    count: u64,
}

impl Mean {
    pub(super) fn add(&mut self, value: Option<u64>) {
        if let Some(value) = value {
            self.sum += u128::from(value);
            self.count += 1;
        }
    }

    pub(super) fn get(self) -> Option<u64> {
        (self.count > 0).then(|| {
            u64::try_from(self.sum / u128::from(self.count)).expect("mean of u64 fits u64")
        })
    }
}

/// The container counters that become rates, in `ServicePoint` order.
const COUNTERS: usize = 5;

fn counters(sample: &MetricSample) -> [Option<u64>; COUNTERS] {
    [
        sample.cpu_usec,
        sample.rx,
        sample.tx,
        sample.io_r,
        sample.io_w,
    ]
}

/// Every container of a Service at one sampling tick, summed.
#[derive(Clone, Copy, Debug)]
struct Tick {
    ts: i64,
    mem: Option<u64>,
    limit: Option<u64>,
}

/// Every container of a Service summed over one bucket.
///
/// Memory sums the containers at each tick and averages those sums, so a
/// replica replaced mid-bucket counts once, not twice.
struct ServiceBucket {
    rates: [Delta; COUNTERS],
    tick: Option<Tick>,
    mem: Mean,
    /// `None` once a sample in the bucket reported no limit.
    limit: Option<Mean>,
}

impl Default for ServiceBucket {
    fn default() -> Self {
        Self {
            rates: [Delta::default(); COUNTERS],
            tick: None,
            mem: Mean::default(),
            limit: Some(Mean::default()),
        }
    }
}

impl ServiceBucket {
    /// Adds `sample`, which is no older than any sample added before, and the
    /// interval from its container's previous reading.
    fn add(&mut self, prev: Reading, sample: &MetricSample) {
        if self.tick.is_some_and(|tick| tick.ts != sample.ts) {
            self.close_tick();
        }
        let tick = self.tick.get_or_insert(Tick {
            ts: sample.ts,
            mem: None,
            limit: Some(0),
        });
        if let Some(mem) = sample.mem_bytes {
            tick.mem = Some(tick.mem.unwrap_or(0).saturating_add(mem));
        }
        tick.limit = tick
            .limit
            .zip(sample.mem_limit)
            .map(|(sum, limit)| sum.saturating_add(limit));
        if tick.limit.is_none() {
            self.limit = None;
        }
        for ((delta, before), after) in self
            .rates
            .iter_mut()
            .zip(prev.counters)
            .zip(counters(sample))
        {
            delta.add(before, after, prev.ts, sample.ts);
        }
    }

    fn close_tick(&mut self) {
        if let Some(tick) = self.tick.take() {
            self.mem.add(tick.mem);
            if let Some(limit) = &mut self.limit {
                limit.add(tick.limit);
            }
        }
    }

    fn into_point(mut self, ts: i64) -> ServicePoint {
        self.close_tick();
        let [cpu_usec, rx, tx, io_r, io_w] = self.rates.map(Delta::per_sec);
        ServicePoint {
            ts,
            cpu_usec,
            mem_bytes: self.mem.get(),
            mem_limit: self.limit.and_then(Mean::get),
            rx,
            tx,
            io_r,
            io_w,
        }
    }
}

/// The counters of a container's last sample, all a rate needs from it.
#[derive(Clone, Copy)]
struct Reading {
    ts: i64,
    counters: [Option<u64>; COUNTERS],
}

impl Reading {
    fn of(sample: &MetricSample) -> Self {
        Self {
            ts: sample.ts,
            counters: counters(sample),
        }
    }
}

/// Sums one Service's containers into one point per bucket, fed a batch at a
/// time so a caller holds one batch of samples, not the whole range.
pub(crate) struct ServiceFold {
    buckets: Buckets,
    table: Vec<Option<ServiceBucket>>,
    prevs: BTreeMap<ContainerId, Reading>,
}

impl ServiceFold {
    pub(crate) fn new(buckets: Buckets) -> Self {
        let mut table = Vec::new();
        table.resize_with(buckets.len, || None);
        Self {
            buckets,
            table,
            prevs: BTreeMap::new(),
        }
    }

    /// Adds `samples`, in any order, each no older than every sample of the
    /// batches added before. A sample before the range only seeds the first
    /// rate of its container.
    pub(crate) fn add(&mut self, samples: &[MetricSample]) {
        let mut sorted: Vec<&MetricSample> = samples.iter().collect();
        // Stable sort is linear on the time-ordered rows a store returns.
        sorted.sort_by_key(|sample| sample.ts);
        for sample in sorted {
            // A container's first sample is its own predecessor: an empty interval.
            let prev = self
                .prevs
                .insert(sample.container_id, Reading::of(sample))
                .unwrap_or_else(|| Reading::of(sample));
            if let Some(slot) = self
                .buckets
                .index(sample.ts)
                .and_then(|index| self.table.get_mut(index))
            {
                slot.get_or_insert_with(ServiceBucket::default)
                    .add(prev, sample);
            }
        }
    }

    pub(crate) fn finish(self) -> MetricHistoryReply {
        let Self { buckets, table, .. } = self;
        let gaps = buckets.gaps(&table);
        let points = table
            .into_iter()
            .enumerate()
            .filter_map(|(index, bucket)| Some(bucket?.into_point(buckets.start(index))))
            .collect();
        MetricHistoryReply {
            points: MetricPoints::Service(points),
            step_nanos: buckets.step,
            gaps,
            markers: Vec::new(),
        }
    }
}

/// Sums every container in `samples`, one Service's in any order, into one
/// point per bucket.
pub(crate) fn service_history(samples: &[MetricSample], buckets: Buckets) -> MetricHistoryReply {
    let mut fold = ServiceFold::new(buckets);
    fold.add(samples);
    fold.finish()
}

/// One bucket of host totals.
#[derive(Default)]
struct HostBucket {
    busy: Delta,
    idle: Delta,
    total: Mean,
    available: Mean,
    /// Receive and transmit deltas per interface, over the same intervals.
    interfaces: BTreeMap<String, (Delta, Delta)>,
    disk: Option<DiskBreakdown>,
}

impl HostBucket {
    fn into_point(self, ts: i64) -> HostPoint {
        let busy = self.busy.amount;
        let total = busy.saturating_add(self.idle.amount);
        #[expect(
            clippy::cast_precision_loss,
            reason = "a busy fraction needs no more than f64 precision"
        )]
        let cpu_busy = (total > 0).then(|| busy as f64 / total as f64);
        let interfaces = self
            .interfaces
            .into_iter()
            .filter_map(|(name, (rx, tx))| {
                let rate = InterfaceRate {
                    rx: rx.per_sec()?,
                    tx: tx.per_sec()?,
                };
                Some((name, rate))
            })
            .collect();
        HostPoint {
            ts,
            cpu_busy,
            memory: self
                .total
                .get()
                .zip(self.available.get())
                .map(|(total, available)| HostMemory { total, available }),
            interfaces,
            disk: self.disk,
        }
    }
}

/// One point per bucket of one Machine's host totals.
///
/// A sample before the range only seeds the first rate. Disk is the bucket's
/// last reading, since it moves slowly and is sampled less often.
pub(crate) fn machine_history(samples: &[HostSample], buckets: Buckets) -> MetricHistoryReply {
    let mut sorted: Vec<&HostSample> = samples.iter().collect();
    sorted.sort_unstable_by_key(|sample| sample.ts);

    let mut table: Vec<Option<HostBucket>> = Vec::new();
    table.resize_with(buckets.len, || None);
    let mut prev: Option<&HostSample> = None;
    for sample in sorted {
        let before = prev.replace(sample);
        let Some(bucket) = buckets
            .index(sample.ts)
            .and_then(|index| table.get_mut(index))
        else {
            continue;
        };
        let bucket = bucket.get_or_insert_with(HostBucket::default);
        bucket.total.add(sample.memory.map(|memory| memory.total));
        bucket
            .available
            .add(sample.memory.map(|memory| memory.available));
        if sample.disk.is_some() {
            bucket.disk = sample.disk;
        }
        let Some(before) = before else {
            continue;
        };
        if let (Some(a), Some(b)) = (before.cpu, sample.cpu)
            && b.busy >= a.busy
            && b.idle >= a.idle
        {
            bucket
                .busy
                .add(Some(a.busy), Some(b.busy), before.ts, sample.ts);
            bucket
                .idle
                .add(Some(a.idle), Some(b.idle), before.ts, sample.ts);
        }
        for (name, now) in &sample.interfaces {
            let then = before.interfaces.get(name);
            // A reset in either direction drops both, so rx and tx always
            // cover the same intervals.
            let Some(InterfaceBytes { rx, tx }) =
                then.filter(|then| now.rx >= then.rx && now.tx >= then.tx)
            else {
                continue;
            };
            let (rx_delta, tx_delta) = bucket.interfaces.entry(name.clone()).or_default();
            rx_delta.add(Some(*rx), Some(now.rx), before.ts, sample.ts);
            tx_delta.add(Some(*tx), Some(now.tx), before.ts, sample.ts);
        }
    }

    let gaps = buckets.gaps(&table);
    let points = table
        .into_iter()
        .enumerate()
        .filter_map(|(index, bucket)| Some(bucket?.into_point(buckets.start(index))))
        .collect();
    MetricHistoryReply {
        points: MetricPoints::Machine(points),
        step_nanos: buckets.step,
        gaps,
        markers: Vec::new(),
    }
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
mod tests {
    use std::time::Instant;

    use ployz_core::{ContainerId, MachineId, QualifiedService, ServiceId, metrics::CpuJiffies};

    use super::*;

    const SEC: i64 = 1_000_000_000;

    fn container(n: u8) -> ContainerId {
        ContainerId::parse(format!("{n:02x}").repeat(32)).unwrap()
    }

    fn machine() -> MachineId {
        MachineId::parse("a".repeat(32)).unwrap()
    }

    /// A container whose every counter grows by `per_sec` each second.
    fn at(n: u8, secs: i64, per_sec: u64) -> MetricSample {
        let total = Some(per_sec * u64::try_from(secs).unwrap());
        MetricSample {
            machine_id: machine(),
            container_id: container(n),
            service_id: Some(ServiceId::parse("c".repeat(32)).unwrap()),
            service: Some(QualifiedService::parse("prod/web").unwrap()),
            deployment_id: None,
            replica: format!("web-{n}"),
            ts: secs * SEC,
            cpu_usec: total,
            mem_bytes: Some(100),
            mem_limit: Some(1_000),
            rx: total,
            tx: total,
            io_r: total,
            io_w: total,
        }
    }

    fn grid(since_secs: i64, until_secs: i64, step_secs: i64) -> Buckets {
        Buckets::new(
            MetricRange {
                since_nanos: since_secs * SEC,
                until_nanos: until_secs * SEC,
            },
            Some(step_secs * SEC),
        )
    }

    fn service(reply: MetricHistoryReply) -> (Vec<ServicePoint>, Vec<MetricGap>) {
        match reply.points {
            MetricPoints::Service(points) => (points, reply.gaps),
            MetricPoints::Machine(_) => panic!("expected Service points"),
        }
    }

    fn machine_points(reply: MetricHistoryReply) -> (Vec<HostPoint>, Vec<MetricGap>) {
        match reply.points {
            MetricPoints::Machine(points) => (points, reply.gaps),
            MetricPoints::Service(_) => panic!("expected Machine points"),
        }
    }

    fn rates(point: &ServicePoint) -> [Option<f64>; COUNTERS] {
        [point.cpu_usec, point.rx, point.tx, point.io_r, point.io_w]
    }

    #[test]
    fn a_default_step_is_never_finer_than_one_sample() {
        let range = MetricRange {
            since_nanos: 0,
            until_nanos: 600 * SEC,
        };
        assert_eq!(Buckets::new(range, None).step(), 10 * SEC);
        assert_eq!(Buckets::new(range, Some(SEC)).step(), SEC);
    }

    #[test]
    fn steady_growth_is_a_constant_rate() {
        let samples: Vec<_> = (0..6).map(|i| at(1, i * 10, 250)).collect();
        let (points, gaps) = service(service_history(&samples, grid(0, 60, 30)));

        assert_eq!(gaps, []);
        assert_eq!(
            points.iter().map(|p| p.ts).collect::<Vec<_>>(),
            [0, 30 * SEC]
        );
        for point in &points {
            assert_eq!(rates(point), [Some(250.0); COUNTERS]);
            assert_eq!(point.mem_bytes, Some(100));
            assert_eq!(point.mem_limit, Some(1_000));
        }
    }

    #[test]
    fn a_counter_decrease_is_a_reset_not_a_negative_rate() {
        let cpu = [1_000, 2_000, 500, 1_500, 100];
        let samples: Vec<_> = cpu
            .iter()
            .zip(0..)
            .map(|(&cpu_usec, i)| MetricSample {
                cpu_usec: Some(cpu_usec),
                ..at(1, i * 10, 1)
            })
            .collect();
        // Buckets [0,30) and [30,50): each holds +1000 over 10s and a reset.
        let (points, _) = service(service_history(&samples, grid(0, 50, 30)));
        assert_eq!(points[0].cpu_usec, Some(100.0));
        assert_eq!(points[1].cpu_usec, Some(100.0));

        let only_reset = [samples[1].clone(), samples[2].clone()];
        let (points, gaps) = service(service_history(&only_reset, grid(10, 30, 20)));
        assert_eq!(gaps, []);
        assert_eq!(points[0].cpu_usec, None, "a reset interval yields no rate");
        assert_eq!(points[0].rx, Some(1.0), "other counters keep theirs");
    }

    #[test]
    fn a_missing_field_is_absent_not_zero() {
        let samples: Vec<_> = (0..4)
            .map(|i| MetricSample {
                io_w: None,
                rx: (i != 2).then(|| u64::try_from(i).unwrap() * 70),
                mem_limit: None,
                ..at(1, i * 10, 7)
            })
            .collect();
        let (points, _) = service(service_history(&samples, grid(0, 40, 40)));
        let point = &points[0];
        assert_eq!(point.io_w, None);
        assert_eq!(point.mem_limit, None);
        // Only 0s to 10s has both ends; the next two intervals each miss one.
        assert_eq!(point.rx, Some(7.0));
        assert_eq!(point.cpu_usec, Some(7.0));
    }

    #[test]
    fn a_container_that_appears_mid_range_adds_from_its_second_sample() {
        let mut samples: Vec<_> = (0..9).map(|i| at(1, i * 10, 100)).collect();
        samples.extend((3..9).map(|i| at(2, i * 10, 40)));
        samples.reverse();
        let (points, gaps) = service(service_history(&samples, grid(0, 90, 30)));

        assert_eq!(gaps, []);
        let cpu: Vec<_> = points.iter().map(|p| p.cpu_usec).collect();
        // Container 2's first sample at 30s has no predecessor, so it adds
        // only memory; its rate counts from 40s on, so it adds 40 for two
        // thirds of the 30s the Service covered in that bucket.
        assert_eq!(cpu, [Some(100.0), Some(3_800.0 / 30.0), Some(140.0)]);
        let mem: Vec<_> = points.iter().map(|p| p.mem_bytes).collect();
        assert_eq!(mem, [Some(100), Some(200), Some(200)]);
        assert_eq!(points[1].mem_limit, Some(2_000));
    }

    #[test]
    fn a_replacement_mid_bucket_keeps_the_rate_of_one_replica() {
        // A rolling deploy: the old replica runs 0s to 150s, the new one from
        // 160s, both at the same speed and memory.
        let mut samples: Vec<_> = (0..=15).map(|i| at(1, i * 10, 1_000)).collect();
        samples.extend((16..29).map(|i| at(2, i * 10, 1_000)));
        let (points, gaps) = service(service_history(&samples, grid(0, 288, 288)));

        assert_eq!(gaps, []);
        assert_eq!(rates(&points[0]), [Some(1_000.0); COUNTERS]);
        assert_eq!(points[0].mem_bytes, Some(100));
        assert_eq!(points[0].mem_limit, Some(1_000));
    }

    #[test]
    fn concurrent_replicas_sum() {
        let mut samples: Vec<_> = (0..=6).map(|i| at(1, i * 10, 300)).collect();
        samples.extend((0..=6).map(|i| at(2, i * 10, 200)));
        let (points, _) = service(service_history(&samples, grid(0, 61, 61)));

        assert_eq!(rates(&points[0]), [Some(500.0); COUNTERS]);
        assert_eq!(points[0].mem_bytes, Some(200));
        assert_eq!(points[0].mem_limit, Some(2_000));
    }

    #[test]
    fn one_unlimited_replica_hides_the_service_limit() {
        let samples = [
            at(1, 0, 1),
            MetricSample {
                mem_limit: None,
                ..at(2, 0, 1)
            },
        ];
        let (points, _) = service(service_history(&samples, grid(0, 10, 10)));
        assert_eq!(points[0].mem_bytes, Some(200));
        assert_eq!(points[0].mem_limit, None);
    }

    #[test]
    fn empty_buckets_are_one_merged_gap_and_no_points() {
        let samples: Vec<_> = [0, 10, 70, 80].into_iter().map(|s| at(1, s, 5)).collect();
        let (points, gaps) = service(service_history(&samples, grid(0, 90, 15)));

        assert_eq!(
            points.iter().map(|p| p.ts / SEC).collect::<Vec<_>>(),
            [0, 60, 75]
        );
        assert_eq!(
            gaps,
            [MetricGap {
                from: 15 * SEC,
                to: 60 * SEC,
                reason: MetricGapReason::NotSampled,
            }]
        );

        let (points, gaps) = service(service_history(&[], grid(0, 90, 15)));
        assert_eq!(points, []);
        assert_eq!(
            gaps,
            [MetricGap {
                from: 0,
                to: 90 * SEC,
                reason: MetricGapReason::NotSampled,
            }]
        );
    }

    #[test]
    fn a_sample_before_the_range_seeds_the_first_rate() {
        let samples = [at(1, 0, 3), at(1, 10, 3)];
        let (points, gaps) = service(service_history(&samples, grid(5, 15, 10)));
        assert_eq!(gaps, []);
        assert_eq!(points.len(), 1);
        assert_eq!(points[0].ts, 5 * SEC);
        assert_eq!(points[0].cpu_usec, Some(3.0));
    }

    #[test]
    fn a_day_at_ten_seconds_reduces_to_300_buckets() {
        let day = 24 * 60 * 60;
        let samples: Vec<_> = (0..day / 10).map(|i| at(1, i * 10, 9)).collect();
        assert_eq!(samples.len(), 8_640);
        let range = MetricRange {
            since_nanos: 0,
            until_nanos: day * SEC,
        };
        let (points, gaps) = service(service_history(&samples, Buckets::new(range, None)));

        assert_eq!(gaps, []);
        assert_eq!(points.len(), 300);
        assert_eq!(points[1].ts, 288 * SEC);
        assert!(points.iter().skip(1).all(|p| p.cpu_usec == Some(9.0)));
    }

    #[test]
    fn a_tiny_step_widens_to_bound_the_grid() {
        let range = MetricRange {
            since_nanos: 0,
            until_nanos: 90 * 24 * 60 * 60 * SEC,
        };
        let buckets = Buckets::new(range, Some(1));
        assert!(buckets.len <= 4_000);
        let (_, gaps) = service(service_history(&[], buckets));
        assert_eq!(gaps.last().map(|g| g.to), Some(range.until_nanos));
    }

    fn host(secs: i64, busy: u64, idle: u64, eth0: u64) -> HostSample {
        HostSample {
            machine_id: machine(),
            ts: secs * SEC,
            cpu: Some(CpuJiffies { busy, idle }),
            memory: Some(HostMemory {
                total: 1_000,
                available: u64::try_from(secs).unwrap() * 10,
            }),
            interfaces: BTreeMap::from([(
                "eth0".to_owned(),
                InterfaceBytes {
                    rx: eth0,
                    tx: eth0 * 2,
                },
            )]),
            disk: None,
        }
    }

    #[test]
    fn host_totals_become_a_busy_fraction_and_interface_rates() {
        let samples = [
            host(0, 0, 0, 0),
            host(10, 25, 75, 1_000),
            // The interface counter went down: a reset, skipped.
            host(20, 50, 150, 10),
            host(30, 75, 225, 1_010),
        ];
        let (points, gaps) = machine_points(machine_history(&samples, grid(0, 40, 40)));
        assert_eq!(gaps, []);
        let point = &points[0];
        assert_eq!(point.cpu_busy, Some(0.25));
        assert_eq!(
            point.interfaces.get("eth0"),
            Some(&InterfaceRate {
                rx: 100.0,
                tx: 200.0
            })
        );
        assert_eq!(
            point.memory,
            Some(HostMemory {
                total: 1_000,
                available: 150
            })
        );
        assert_eq!(point.disk, None);
    }

    #[test]
    fn a_host_without_cpu_reports_no_busy_fraction() {
        let samples = [
            HostSample {
                cpu: None,
                ..host(0, 0, 0, 0)
            },
            host(10, 5, 5, 10),
        ];
        let (points, _) = machine_points(machine_history(&samples, grid(0, 20, 20)));
        assert_eq!(points[0].cpu_busy, None);
        assert_eq!(points[0].interfaces["eth0"].rx, 1.0);
    }

    /// Run with `cargo nextest run -p ployzd --release --run-ignored only observe::metrics::rate`.
    #[test]
    #[ignore = "timing; meaningful only in a release build"]
    fn a_day_of_fifty_containers_reduces_under_50ms() {
        let day = 24 * 60 * 60;
        // Time-major, the order a store returns them in.
        let samples: Vec<_> = (0..day / 10)
            .flat_map(|i| (0..50).map(move |n| at(n, i * 10, 9)))
            .collect();
        assert_eq!(samples.len(), 432_000);
        let range = MetricRange {
            since_nanos: 0,
            until_nanos: day * SEC,
        };

        let started = Instant::now();
        let (points, _) = service(service_history(&samples, Buckets::new(range, None)));
        let elapsed = started.elapsed();

        eprintln!("reduced {} samples in {elapsed:?}", samples.len());
        assert_eq!(points.len(), 300);
        assert_eq!(points[1].cpu_usec, Some(450.0));
        assert!(elapsed.as_millis() < 50, "took {elapsed:?}");
    }
}
