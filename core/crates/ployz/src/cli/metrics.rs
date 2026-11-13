//! How `ployz metrics` prints a metric history: one row per metric with its
//! newest value and the range's average and peak. A metric with no reading
//! prints as not measured, never as zero.

use std::collections::BTreeMap;

use ployz_core::metrics::{MetricMarker, MetricPoints};
use ployz_store::tools::cloud::MetricRow;

use crate::{
    sdk::{
        MetricHistory,
        metrics_rows::{gap_notes, marker_note, rows},
    },
    ui::Table,
};

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
    let heading = format!("{subject} · {}", window.label());
    let mut table = Table::new(
        ["METRIC", "NOW", "AVG", "PEAK"],
        if window.is_duration() {
            format!("No readings for {subject} in the {}.", window.label())
        } else {
            format!("No readings for {subject} {}.", window.label())
        },
    );
    let rows = rows(history, window.until_nanos);
    let mut notes = Vec::new();
    if rows.is_empty() {
        return Report {
            heading,
            table,
            notes,
        };
    }
    for MetricRow {
        metric,
        now,
        avg,
        peak,
    } in rows
    {
        let now = match avg {
            None => "not measured".into(),
            Some(_) => now.unwrap_or_default(),
        };
        table.row([
            metric,
            now,
            avg.unwrap_or_default(),
            peak.unwrap_or_default(),
        ]);
    }
    let markers = history
        .machines
        .iter()
        .flat_map(|machine| &machine.value.markers);
    match &history.points {
        MetricPoints::Service(_) => notes.extend(marker_note(markers, None)),
        MetricPoints::Machine(_) => {
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

#[cfg(test)]
#[path = "metrics_tests.rs"]
mod tests;
