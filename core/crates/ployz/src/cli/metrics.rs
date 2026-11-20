//! How `ployz metrics` prints live numbers: one row per metric, summed over every
//! container that answered. A metric no container reported prints as not measured,
//! never as zero.

use ployz_core::metrics::LiveContainer;

use crate::{sdk::LiveMetrics, ui::Table};

pub(crate) const HISTORY_NOTE: &str = "History is in Ployz Cloud.";
pub(crate) const NOT_ENABLED_NOTE: &str = "Metrics history isn’t enabled on this Cloud.";

pub(crate) fn report(live: &LiveMetrics, subject: &str) -> Table {
    let containers: Vec<&LiveContainer> = live
        .machines
        .iter()
        .flat_map(|machine| &machine.value.containers)
        .collect();
    let mut table = Table::new(
        ["METRIC", "NOW"],
        format!("{subject} has no running containers."),
    );
    if containers.is_empty() {
        return table;
    }
    let sum = |read: fn(&LiveContainer) -> Option<u64>| {
        containers
            .iter()
            .filter_map(|container| read(container))
            .reduce(u64::saturating_add)
    };
    let cpu = containers
        .iter()
        .filter_map(|container| container.cpu)
        .reduce(|total, cpu| total + cpu);
    let shown = |value: Option<String>| value.unwrap_or_else(|| "not measured".to_owned());
    let memory = sum(|container| container.mem_bytes).map(|used| {
        match sum(|container| container.mem_limit) {
            Some(limit) => format!("{} / {}", bytes(used), bytes(limit)),
            None => bytes(used),
        }
    });
    table.row(["Containers".to_owned(), containers.len().to_string()]);
    table.row([
        "CPU".to_owned(),
        shown(cpu.map(|cpu| format!("{cpu:.2} vCPU"))),
    ]);
    table.row(["Memory".to_owned(), shown(memory)]);
    for (metric, read) in [
        (
            "Net in",
            (|container| container.rx) as fn(&LiveContainer) -> Option<u64>,
        ),
        ("Net out", |container| container.tx),
        ("Disk read", |container| container.io_r),
        ("Disk write", |container| container.io_w),
    ] {
        table.row([format!("{metric} (total)"), shown(sum(read).map(bytes))]);
    }
    table
}

/// Decimal units, as storage limits are written.
fn bytes(value: u64) -> String {
    #[expect(clippy::cast_precision_loss, reason = "display rounding only")]
    let mut value = value as f64;
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
