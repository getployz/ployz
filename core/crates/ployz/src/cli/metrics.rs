//! How `ployz metrics` prints a metric history: one row per metric with its
//! newest value and the range's average and peak. A metric with no reading
//! prints as not measured, never as zero.

use std::{collections::BTreeMap, time::Duration};

use ployz_core::metrics::{
    DiskBreakdown, HostPoint, MarkerKind, MetricGapReason, MetricMarker, MetricPoints, ServicePoint,
};

use crate::{sdk::MetricHistory, ui::Table};

/// A newest bucket older than this many steps is history, not the current value.
const FRESH_STEPS: i64 = 3;

/// What the human output says about one history.
#[derive(Debug)]
pub(crate) struct Report {
    pub(crate) heading: String,
    pub(crate) table: Table,
    /// Side text: markers and spans with no readings.
    pub(crate) notes: Vec<String>,
}

/// The range the history covers, as the user asked for it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Window<'arg> {
    pub(crate) since: &'arg str,
    pub(crate) until_nanos: i64,
}

impl Window<'_> {
    fn is_duration(&self) -> bool {
        crate::operator::go_duration(self.since).is_some()
    }

    fn label(&self) -> String {
        if self.is_duration() {
            format!("last {}", self.since)
        } else {
            format!("since {}", self.since)
        }
    }
}

/// Lay out `history` for `subject`, the name the user selected it by.
pub(crate) fn report(history: &MetricHistory, subject: &str, window: Window<'_>) -> Report {
    let fresh_since = history.step_nanos.map_or(i64::MAX, |step| {
        window.until_nanos.saturating_sub(FRESH_STEPS * step)
    });
    let heading = format!("{subject} · {}", window.label());
    let mut table = Table::new(
        ["METRIC", "NOW", "AVG", "PEAK"],
        if window.is_duration() {
            format!("No readings for {subject} in the {}.", window.label())
        } else {
            format!("No readings for {subject} {}.", window.label())
        },
    );
    let mut notes = Vec::new();
    let no_readings = match &history.points {
        MetricPoints::Service(points) => points.is_empty(),
        MetricPoints::Machine(points) => points.is_empty(),
    };
    if no_readings {
        return Report {
            heading,
            table,
            notes,
        };
    }
    let markers = history
        .machines
        .iter()
        .flat_map(|machine| &machine.value.markers);
    match &history.points {
        MetricPoints::Service(points) => {
            for metric in service_metrics() {
                table.row(metric.row(points, fresh_since));
            }
            notes.extend(marker_note(markers, None));
        }
        MetricPoints::Machine(points) => {
            for metric in host_metrics(points) {
                table.row(metric.row(points, fresh_since));
            }
            notes.extend(gap_notes(history));
            let mut by_service: BTreeMap<String, Vec<&MetricMarker>> = BTreeMap::new();
            for marker in markers {
                by_service
                    .entry(marker.service.to_string())
                    .or_default()
                    .push(marker);
            }
            for (service, markers) in by_service {
                notes.extend(marker_note(markers, Some(&service)));
            }
        }
    }
    Report {
        heading,
        table,
        notes,
    }
}

trait Bucket {
    fn ts(&self) -> i64;
}

impl Bucket for ServicePoint {
    fn ts(&self) -> i64 {
        self.ts
    }
}

impl Bucket for HostPoint {
    fn ts(&self) -> i64 {
        self.ts
    }
}

type Read<P> = Box<dyn Fn(&P) -> Option<f64>>;

/// One share of a Server's disk.
type DiskPart = fn(&DiskBreakdown) -> Option<u64>;

/// One row of the table: how to read the metric from a bucket, and what it
/// is out of, such as a memory limit.
struct Metric<P> {
    label: String,
    unit: Unit,
    value: Read<P>,
    of: Option<Read<P>>,
}

impl<P: Bucket> Metric<P> {
    fn new(
        label: impl Into<String>,
        unit: Unit,
        value: impl Fn(&P) -> Option<f64> + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            unit,
            value: Box::new(value),
            of: None,
        }
    }

    fn of(mut self, of: impl Fn(&P) -> Option<f64> + 'static) -> Self {
        self.of = Some(Box::new(of));
        self
    }

    fn row(&self, points: &[P], fresh_since: i64) -> [String; 4] {
        let mut values = points.iter().filter_map(|point| (self.value)(point));
        let Some(first) = values.next() else {
            return [
                self.label.clone(),
                "not measured".into(),
                String::new(),
                String::new(),
            ];
        };
        let (mut sum, mut count, mut peak) = (first, 1_u32, first);
        for value in values {
            sum += value;
            count += 1;
            peak = peak.max(value);
        }
        let now = points
            .last()
            .filter(|point| point.ts() >= fresh_since)
            .and_then(|point| {
                let value = self.unit.show((self.value)(point)?);
                Some(match self.of.as_ref().and_then(|of| of(point)) {
                    Some(of) => format!("{value} / {}", self.unit.show(of)),
                    None => value,
                })
            })
            .unwrap_or_default();
        [
            self.label.clone(),
            now,
            self.unit.show(sum / f64::from(count)),
            self.unit.show(peak),
        ]
    }
}

#[derive(Clone, Copy, Debug)]
enum Unit {
    /// CPU microseconds per second.
    Cores,
    /// A fraction from 0 to 1.
    Share,
    Bytes,
    BytesPerSecond,
}

impl Unit {
    fn show(self, value: f64) -> String {
        match self {
            Self::Cores => format!("{:.2} vCPU", value / 1e6),
            Self::Share => format!("{:.0}%", value * 100.0),
            Self::Bytes => bytes(value),
            Self::BytesPerSecond => format!("{}/s", bytes(value)),
        }
    }
}

/// Decimal units, as storage limits are written.
fn bytes(value: f64) -> String {
    let mut value = value;
    let mut units = ["B", "KB", "MB", "GB", "TB"].into_iter();
    let mut unit = units.next().unwrap_or_default();
    while value >= 1000.0
        && let Some(next) = units.next()
    {
        value /= 1000.0;
        unit = next;
    }
    if unit == "B" || value >= 10.0 {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

#[expect(clippy::cast_precision_loss, reason = "display rounding only")]
fn float(value: u64) -> f64 {
    value as f64
}

fn service_metrics() -> Vec<Metric<ServicePoint>> {
    vec![
        Metric::new("CPU", Unit::Cores, |point: &ServicePoint| point.cpu_usec),
        Metric::new("Memory", Unit::Bytes, |point: &ServicePoint| {
            point.mem_bytes.map(float)
        })
        .of(|point| point.mem_limit.map(float)),
        Metric::new("Net in", Unit::BytesPerSecond, |point: &ServicePoint| {
            point.rx
        }),
        Metric::new("Net out", Unit::BytesPerSecond, |point: &ServicePoint| {
            point.tx
        }),
        Metric::new("Disk read", Unit::BytesPerSecond, |point: &ServicePoint| {
            point.io_r
        }),
        Metric::new(
            "Disk write",
            Unit::BytesPerSecond,
            |point: &ServicePoint| point.io_w,
        ),
    ]
}

/// Interfaces that carry the Server's own traffic. Loopback, Docker bridges
/// and container veths repeat bytes already counted on another interface.
fn own_interface(name: &str) -> bool {
    name != "lo"
        && !["veth", "docker", "br-"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
}

fn host_metrics(points: &[HostPoint]) -> Vec<Metric<HostPoint>> {
    let mut metrics = vec![
        Metric::new("CPU", Unit::Share, |point: &HostPoint| point.cpu_busy),
        Metric::new("Memory", Unit::Bytes, |point: &HostPoint| {
            point
                .memory
                .map(|memory| float(memory.total.saturating_sub(memory.available)))
        })
        .of(|point| point.memory.map(|memory| float(memory.total))),
    ];
    let interfaces: std::collections::BTreeSet<&String> = points
        .iter()
        .flat_map(|point| point.interfaces.keys())
        .filter(|name| own_interface(name))
        .collect();
    for name in interfaces {
        let rx = name.clone();
        metrics.push(Metric::new(
            format!("Net {name} in"),
            Unit::BytesPerSecond,
            move |point: &HostPoint| point.interfaces.get(&rx).map(|rate| rate.rx),
        ));
        let tx = name.clone();
        metrics.push(Metric::new(
            format!("Net {name} out"),
            Unit::BytesPerSecond,
            move |point: &HostPoint| point.interfaces.get(&tx).map(|rate| rate.tx),
        ));
    }
    metrics.push(
        Metric::new("Disk", Unit::Bytes, |point: &HostPoint| {
            point
                .disk
                .map(|disk| float(disk.total.saturating_sub(disk.free)))
        })
        .of(|point| point.disk.map(|disk| float(disk.total))),
    );
    let parts: [(&str, DiskPart); 4] = [
        ("Volumes", |disk| disk.volumes),
        ("Mirrors", |disk| disk.mirror_slots),
        ("Logs and metrics", |disk| disk.observe_store),
        ("Other", |disk| disk.other),
    ];
    for (label, part) in parts {
        metrics.push(Metric::new(label, Unit::Bytes, move |point: &HostPoint| {
            point.disk.as_ref().and_then(part).map(float)
        }));
    }
    metrics
}

/// One note per gap reason a Server reported, with the time it covers.
fn gap_notes(history: &MetricHistory) -> Vec<String> {
    let mut spans: BTreeMap<&'static str, i64> = BTreeMap::new();
    for gap in history
        .machines
        .iter()
        .flat_map(|machine| &machine.value.gaps)
    {
        let reason = match gap.reason {
            MetricGapReason::NotSampled => "No readings",
            MetricGapReason::Timeout => "The store timed out",
        };
        *spans.entry(reason).or_default() += gap.to.saturating_sub(gap.from);
    }
    spans
        .into_iter()
        .map(|(reason, nanos)| {
            format!(
                "{reason} for {} of the range.",
                span(Duration::from_nanos(
                    u64::try_from(nanos).unwrap_or_default()
                ))
            )
        })
        .collect()
}

fn span(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// `service` names whose markers these are, when the subject is a Server.
fn marker_note<'marker>(
    markers: impl IntoIterator<Item = &'marker MetricMarker>,
    service: Option<&str>,
) -> Option<String> {
    let (mut ooms, mut exits, mut restarts) = (0_usize, 0_usize, 0_usize);
    for marker in markers {
        match marker.kind {
            MarkerKind::Oom => ooms += 1,
            MarkerKind::Exit { .. } => exits += 1,
            MarkerKind::Restart => restarts += 1,
        }
    }
    let counts: Vec<String> = [
        (ooms, "OOM kill", "OOM kills"),
        (exits, "exit", "exits"),
        (restarts, "restart", "restarts"),
    ]
    .into_iter()
    .filter(|(count, _, _)| *count > 0)
    .map(|(count, one, many)| format!("{count} {}", if count == 1 { one } else { many }))
    .collect();
    if counts.is_empty() {
        return None;
    }
    let counts = counts.join(", ");
    Some(match service {
        Some(service) => format!("{service}: {counts} in the range."),
        None => format!("{counts} in the range."),
    })
}

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
