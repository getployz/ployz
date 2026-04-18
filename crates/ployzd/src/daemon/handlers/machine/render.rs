use chrono::DateTime;
use ployz_orchestrator::machine_liveness::{MachineLiveness, machine_liveness};
use ployz_types::model::{MachineRecord, MachineStatus, Participation};

use super::types::{MachineAddReport, MachineListReport};

pub(super) fn render_machine_list_report(report: &MachineListReport) -> String {
    let w_id = report
        .rows
        .iter()
        .map(|row| row.id.len())
        .max()
        .unwrap_or(0)
        .max(2);
    let w_ov = report
        .rows
        .iter()
        .map(|row| row.overlay.len())
        .max()
        .unwrap_or(0)
        .max(10);
    let w_sub = report
        .rows
        .iter()
        .map(|row| row.subnet_display.len())
        .max()
        .unwrap_or(0)
        .max(6);
    let w_present = report
        .rows
        .iter()
        .map(|row| if row.reachable { "present" } else { "absent" }.len())
        .max()
        .unwrap_or(0)
        .max("PRESENCE".len());
    let w_ready = report
        .rows
        .iter()
        .map(|row| {
            row.ready
                .map(|ready| if ready { "ready" } else { "not-ready" })
                .unwrap_or("unknown")
                .len()
        })
        .max()
        .unwrap_or(0)
        .max("READY".len());
    let w_draining = report
        .rows
        .iter()
        .map(|row| {
            row.draining
                .map(|draining| if draining { "draining" } else { "no" })
                .unwrap_or("unknown")
                .len()
        })
        .max()
        .unwrap_or(0)
        .max("DRAINING".len());
    let w_phase = report
        .rows
        .iter()
        .map(|row| row.phase.as_deref().unwrap_or("unknown").len())
        .max()
        .unwrap_or(0)
        .max("PHASE".len());
    let w_part = report
        .rows
        .iter()
        .map(|row| row.participation.len())
        .max()
        .unwrap_or(0)
        .max("PARTICIPATION".len());
    let mut lines = Vec::with_capacity(report.rows.len() + 1);
    lines.push(format!(
        "{:<w_id$}  {:<6}  {:<w_part$}  {:<w_present$}  {:<w_ready$}  {:<w_draining$}  {:<w_phase$}  {:<w_ov$}  {:<w_sub$}  {}",
        "ID", "STATUS", "PARTICIPATION", "PRESENCE", "READY", "DRAINING", "PHASE", "OVERLAY IP", "SUBNET", "CREATED",
    ));
    for row in &report.rows {
        let present = if row.reachable { "present" } else { "absent" };
        let ready = row
            .ready
            .map(|value| if value { "ready" } else { "not-ready" })
            .unwrap_or("unknown");
        let draining = row
            .draining
            .map(|value| if value { "draining" } else { "no" })
            .unwrap_or("unknown");
        let phase = row.phase.as_deref().unwrap_or("unknown");
        lines.push(format!(
            "{:<w_id$}  {:<6}  {:<w_part$}  {:<w_present$}  {:<w_ready$}  {:<w_draining$}  {:<w_phase$}  {:<w_ov$}  {:<w_sub$}  {}",
            row.id,
            row.status,
            row.participation,
            present,
            ready,
            draining,
            phase,
            row.overlay,
            row.subnet_display,
            row.created_display,
        ));
    }
    lines.join("\n")
}

pub(super) fn render_machine_add_report(report: &MachineAddReport) -> String {
    let mut lines = Vec::new();
    if !report.warnings.is_empty() {
        lines.extend(report.warnings.iter().cloned());
        lines.push(String::new());
    }

    lines.push("machine add summary".into());
    push_summary_section(
        &mut lines,
        "awaiting_self_publication",
        &report.awaiting_self_publication,
    );
    push_summary_section(&mut lines, "failed_preflight", &report.failed_preflight);
    push_summary_section(&mut lines, "failed_join", &report.failed_join);
    push_summary_section(&mut lines, "failed_self_record", &report.failed_self_record);
    push_summary_section(&mut lines, "failed_ready", &report.failed_ready);
    lines.join("\n")
}

pub(super) fn format_status(machine: &MachineRecord) -> &'static str {
    match machine.status {
        MachineStatus::Up => "up",
        MachineStatus::Down => "down",
        MachineStatus::Unknown => "—",
    }
}

pub(super) fn format_participation(machine: &MachineRecord) -> &'static str {
    match machine.participation {
        Participation::Enabled => "enabled",
        Participation::Draining => "draining",
        Participation::Disabled => "disabled",
    }
}

pub(crate) fn format_liveness(machine: &MachineRecord, now: u64) -> &'static str {
    match machine_liveness(machine, now) {
        MachineLiveness::Fresh => "fresh",
        MachineLiveness::Stale => "stale",
        MachineLiveness::Down => "down",
    }
}

pub(crate) fn format_heartbeat(ts: u64, now: u64) -> String {
    if ts == 0 {
        return "never".into();
    }
    let ago = now.saturating_sub(ts);
    if ago < 60 {
        format!("{ago}s ago")
    } else if ago < 3600 {
        format!("{}m ago", ago / 60)
    } else if ago < 86400 {
        format!("{}h ago", ago / 3600)
    } else {
        format!("{}d ago", ago / 86400)
    }
}

pub(super) fn format_timestamp(ts: u64) -> String {
    if ts == 0 {
        return "—".into();
    }
    DateTime::from_timestamp(ts as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "—".into())
}

fn push_summary_section(lines: &mut Vec<String>, label: &str, values: &[String]) {
    lines.push(format!("{label}: {}", values.len()));
    lines.extend(values.iter().map(|value| format!("  {value}")));
}
