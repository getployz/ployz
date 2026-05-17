use std::fs;
use std::path::Path;
use std::time::Duration;

use hdrhistogram::Histogram;
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct LatencySummary {
    pub samples: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

pub(crate) struct LatencyRecorder {
    histogram: Histogram<u64>,
}

impl LatencyRecorder {
    pub(crate) fn new() -> Result<Self, String> {
        let histogram =
            Histogram::new(3).map_err(|error| format!("create latency histogram: {error}"))?;
        Ok(Self { histogram })
    }

    pub(crate) fn record(&mut self, duration: Duration) -> Result<(), String> {
        self.histogram
            .record(duration_to_us(duration))
            .map_err(|error| format!("record latency: {error}"))
    }

    pub(crate) fn summary(&self) -> LatencySummary {
        if self.histogram.is_empty() {
            return LatencySummary {
                samples: 0,
                p50_us: 0,
                p95_us: 0,
                p99_us: 0,
                max_us: 0,
            };
        }
        LatencySummary {
            samples: self.histogram.len(),
            p50_us: self.histogram.value_at_quantile(0.50),
            p95_us: self.histogram.value_at_quantile(0.95),
            p99_us: self.histogram.value_at_quantile(0.99),
            max_us: self.histogram.max(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct MemorySnapshot {
    pub physical_bytes: Option<usize>,
    pub virtual_bytes: Option<usize>,
}

pub(crate) fn memory_snapshot() -> MemorySnapshot {
    match memory_stats::memory_stats() {
        Some(stats) => MemorySnapshot {
            physical_bytes: Some(stats.physical_mem),
            virtual_bytes: Some(stats.virtual_mem),
        },
        None => MemorySnapshot {
            physical_bytes: None,
            virtual_bytes: None,
        },
    }
}

pub(crate) fn write_json<T>(path: &Path, report: &T) -> Result<String, String>
where
    T: Serialize,
{
    let Some(parent) = path.parent() else {
        return Err(format!("metrics path has no parent: {}", path.display()));
    };
    fs::create_dir_all(parent)
        .map_err(|error| format!("create metrics dir '{}': {error}", parent.display()))?;
    let contents = serde_json::to_string_pretty(report)
        .map_err(|error| format!("serialize metrics '{}': {error}", path.display()))?;
    fs::write(path, format!("{contents}\n"))
        .map_err(|error| format!("write metrics '{}': {error}", path.display()))?;
    Ok(contents)
}

pub(crate) fn reset_dir(path: &Path) -> Result<(), String> {
    match fs::remove_dir_all(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(format!("remove old dir '{}': {error}", path.display())),
    }
    fs::create_dir_all(path).map_err(|error| format!("create dir '{}': {error}", path.display()))
}

fn duration_to_us(duration: Duration) -> u64 {
    let micros = duration.as_micros();
    if micros > u128::from(u64::MAX) {
        return u64::MAX;
    }
    micros as u64
}
