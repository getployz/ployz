use chrono::DateTime;
use ployz_types::model::{MachineLifecycle, MachineMembership};

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
    let w_lifecycle = report
        .rows
        .iter()
        .map(|row| row.lifecycle.len())
        .max()
        .unwrap_or(0)
        .max("LIFECYCLE".len());
    let w_authority = report
        .rows
        .iter()
        .map(|row| row.authority.role.to_string().len())
        .max()
        .unwrap_or(0)
        .max("AUTHORITY".len());
    let w_bucket = report
        .rows
        .iter()
        .map(|row| row.authority.data_bucket.to_string().len())
        .max()
        .unwrap_or(0)
        .max("BUCKET".len());
    let w_loss = report
        .rows
        .iter()
        .map(|row| row.authority.loss_impact.to_string().len())
        .max()
        .unwrap_or(0)
        .max("LOSS".len());
    let w_region = report
        .rows
        .iter()
        .map(|row| row.region.len())
        .max()
        .unwrap_or(0)
        .max("REGION".len());
    let w_az = report
        .rows
        .iter()
        .map(|row| row.availability_zone_display.len())
        .max()
        .unwrap_or(0)
        .max("AZ".len());

    let mut lines = Vec::with_capacity(report.rows.len() + 1);
    lines.push(format!(
        "{:<w_id$}  {:<w_lifecycle$}  {:<w_authority$}  {:<w_bucket$}  {:<w_loss$}  {:<w_region$}  {:<w_az$}  {:<w_ov$}  {:<w_sub$}  {}",
        "ID", "LIFECYCLE", "AUTHORITY", "BUCKET", "LOSS", "REGION", "AZ", "OVERLAY IP", "SUBNET", "CREATED",
    ));
    for row in &report.rows {
        lines.push(format!(
            "{:<w_id$}  {:<w_lifecycle$}  {:<w_authority$}  {:<w_bucket$}  {:<w_loss$}  {:<w_region$}  {:<w_az$}  {:<w_ov$}  {:<w_sub$}  {}",
            row.id,
            row.lifecycle,
            row.authority.role,
            row.authority.data_bucket,
            row.authority.loss_impact,
            row.region,
            row.availability_zone_display,
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
    push_summary_section(&mut lines, "failed_enable", &report.failed_enable);
    lines.join("\n")
}

pub(crate) fn format_lifecycle(machine: &MachineMembership) -> &'static str {
    match machine.lifecycle {
        MachineLifecycle::Standby => "standby",
        MachineLifecycle::Active => "active",
        MachineLifecycle::Draining => "draining",
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
