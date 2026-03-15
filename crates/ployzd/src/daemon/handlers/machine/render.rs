use crate::machine_liveness::{MachineLiveness, machine_liveness};
use crate::model::{MachineRecord, MachineStatus, Participation};
use crate::time::now_unix_secs;
use chrono::DateTime;

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
    let w_hb = report
        .rows
        .iter()
        .map(|row| row.heartbeat_display.len())
        .max()
        .unwrap_or(0)
        .max(9);
    let w_part = report
        .rows
        .iter()
        .map(|row| row.participation.len())
        .max()
        .unwrap_or(0)
        .max("PARTICIPATION".len());
    let w_live = report
        .rows
        .iter()
        .map(|row| row.liveness.len())
        .max()
        .unwrap_or(0)
        .max("LIVENESS".len());

    let mut lines = Vec::with_capacity(report.rows.len() + 1);
    lines.push(format!(
        "{:<w_id$}  {:<6}  {:<w_part$}  {:<w_live$}  {:<w_ov$}  {:<w_sub$}  {:<w_hb$}  {}",
        "ID", "STATUS", "PARTICIPATION", "LIVENESS", "OVERLAY IP", "SUBNET", "HEARTBEAT", "CREATED",
    ));
    for row in &report.rows {
        lines.push(format!(
            "{:<w_id$}  {:<6}  {:<w_part$}  {:<w_live$}  {:<w_ov$}  {:<w_sub$}  {:<w_hb$}  {}",
            row.id,
            row.status,
            row.participation,
            row.liveness,
            row.overlay,
            row.subnet_display,
            row.heartbeat_display,
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

pub(super) fn format_liveness(machine: &MachineRecord, now: u64) -> &'static str {
    match machine_liveness(machine, now) {
        MachineLiveness::Fresh => "fresh",
        MachineLiveness::Stale => "stale",
        MachineLiveness::Down => "down",
    }
}

pub(super) fn format_heartbeat(ts: u64, now: u64) -> String {
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

pub(super) fn degraded_mesh_warning(machine: &MachineRecord) -> String {
    let now = now_unix_secs();
    let role = match machine.participation {
        Participation::Disabled => "disabled",
        Participation::Enabled => "enabled",
        Participation::Draining => "draining",
    };
    let heartbeat = format_heartbeat(machine.last_heartbeat, now);
    format!(
        "warning: {role} peer '{}' has a stale heartbeat ({heartbeat})",
        machine.id
    )
}

fn push_summary_section(lines: &mut Vec<String>, label: &str, values: &[String]) {
    lines.push(format!("{label}: {}", values.len()));
    lines.extend(values.iter().map(|value| format!("  {value}")));
}
