//! Tree-shaped stdout for a Deploy Preview and its progress events.
//!
//! No planner lives here. The CLI passes a preview and a recorded event stream.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use ployz_core::{
    DeployOperation, DeployPreview, HttpProtocol, OperationPhase, OperationRow, OperationStatus,
    PortPublication, ReplacementOperation, UpdateOrder,
};

#[cfg(test)]
use ployz_core::{DeployEvent, DeployOutcome, ExecutionError};

use super::report;
#[cfg(test)]
use super::report::Ink;

#[must_use]
#[cfg(test)]
pub fn progress_text(event: &DeployEvent, title: &str) -> String {
    match event {
        DeployEvent::Progress {
            completed,
            total,
            rows,
        } => report::paint_live(title, *completed, *total, rows, &Ink::plain()),
        DeployEvent::Outcome { outcome } => outcome_text(outcome),
    }
}

/// Tree plan plus footer. Empty operations with no listed drift are "No changes."
#[must_use]
pub fn plan_text(preview: &DeployPreview, context: &str, project_source: Option<&str>) -> String {
    titled_plan_text("Deployment plan", preview, context, project_source)
}

/// Same tree as [`plan_text`], titled for Project removal.
#[must_use]
pub fn removal_plan_text(preview: &DeployPreview, context: &str) -> String {
    titled_plan_text("Removal plan", preview, context, None)
}

fn titled_plan_text(
    title: &str,
    preview: &DeployPreview,
    context: &str,
    project_source: Option<&str>,
) -> String {
    if preview.noop()
        && preview.volumes_to_create.is_empty()
        && preview.would_remove.is_empty()
        && preview.preserved_volumes.is_empty()
        && preview.prune_refusal.is_none()
    {
        return "No changes.\n".into();
    }
    let mut out = format!("{title}\n");
    let _ = writeln!(out, "context: {context}");
    match project_source {
        Some(source) => {
            let _ = writeln!(out, "project: {} ({source})", preview.project_name);
        }
        None => {
            let _ = writeln!(out, "project: {}", preview.project_name);
        }
    }
    out.push_str(&service_trees(preview));
    out.push_str(&volumes_to_create_lines(preview));
    for storage in &preview.storage {
        let budget = &storage.budget;
        let gib = ployz_core::STORAGE_GIB as f64;
        let headroom = budget
            .available_bytes
            .saturating_sub(budget.reserve_bytes)
            .saturating_sub(budget.required_growth_bytes);
        let _ = writeln!(
            out,
            "Storage on {}: {:.2} GiB requested, {:.2} GiB estimated pool growth, {:.2} GiB remaining after OS reserve",
            storage.machine_name,
            budget.requested_bytes as f64 / gib,
            budget.required_growth_bytes as f64 / gib,
            headroom as f64 / gib
        );
    }
    if !preview.storage.is_empty() {
        out.push_str("Storage is rechecked and prepared before applications start.\n");
    }
    if !preview.operations.is_empty() {
        out.push_str("──────────────────────────────────────────\n");
        out.push_str(&plan_footer(preview));
        out.push('\n');
    }
    out.push_str(&prune_lines(preview));
    out.push_str(&preserved_lines(preview));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

fn prune_lines(preview: &DeployPreview) -> String {
    let Some(reason) = preview.prune_refusal else {
        return String::new();
    };
    let mut out = String::new();
    for service in &preview.would_remove {
        let _ = writeln!(out, "  would remove {service}");
    }
    let _ = writeln!(out, "{reason}");
    out
}

fn preserved_lines(preview: &DeployPreview) -> String {
    let mut out = String::new();
    for volume in &preview.preserved_volumes {
        let machine = volume
            .machine_name
            .as_ref()
            .map_or_else(|| volume.id.machine_id.to_string(), ToString::to_string);
        let _ = writeln!(
            out,
            "  would preserve volume {} on {machine}",
            volume.id.name
        );
    }
    out
}

fn volumes_to_create_lines(preview: &DeployPreview) -> String {
    if preview.volumes_to_create.is_empty() {
        return String::new();
    }
    let mut out = String::from("Volumes to create\n");
    for item in &preview.volumes_to_create {
        let machine = item
            .machine_name
            .as_ref()
            .map_or_else(|| item.machine_id.to_string(), ToString::to_string);
        match item.maximum_bytes {
            Some(maximum_bytes) => {
                let _ = writeln!(
                    out,
                    "  + provisioned volume {} (maximum {maximum_bytes} bytes) on {machine}",
                    item.name
                );
            }
            None => {
                let _ = writeln!(out, "  + volume {} on {machine}", item.name);
            }
        }
    }
    out
}

/// Confirm prompt targeting the selected context.
#[must_use]
pub fn confirm_prompt(context: &str) -> String {
    format!("Proceed with deployment to {context}? [y/N] ")
}

/// Endpoints on success; synthesized live list plus footer when no printer ran.
#[must_use]
#[cfg(test)]
pub fn outcome_text(outcome: &DeployOutcome<ExecutionError>) -> String {
    match outcome {
        DeployOutcome::Success { completed } => endpoints_footer(completed, None),
        DeployOutcome::Failed { .. } => report::paint_closing(outcome, &[], false, &Ink::plain()),
    }
}

/// Footer using Machine Names already present on live rows.
#[must_use]
#[cfg(test)]
pub fn outcome_text_after(
    outcome: &DeployOutcome<ExecutionError>,
    rows: &[OperationRow],
) -> String {
    report::paint_closing(outcome, rows, true, &Ink::plain())
}

fn service_trees(preview: &DeployPreview) -> String {
    let mut groups: BTreeMap<String, Vec<&OperationRow>> = BTreeMap::new();
    let mut out = String::new();
    for row in &preview.operations {
        if matches!(
            row.operation,
            DeployOperation::RemoveVolume { .. } | DeployOperation::PrepareVolumes { .. }
        ) {
            let _ = writeln!(out, "{}", child_line(row));
            continue;
        }
        let name = row
            .service_name
            .as_ref()
            .map_or_else(|| "service".into(), ToString::to_string);
        groups.entry(name).or_default().push(row);
    }
    for (name, rows) in groups {
        let pruned = preview
            .would_remove
            .iter()
            .any(|service| service.name.as_str() == name);
        out.push_str(&service_block(&name, &rows, pruned));
    }
    out
}

fn service_block(name: &str, rows: &[&OperationRow], pruned: bool) -> String {
    let marker = service_marker(rows, pruned);
    let image = rows.iter().find_map(|row| spec_image(&row.operation));
    let mut out = format!("{marker} service {name}\n");
    if let Some(image) = image {
        let _ = writeln!(out, "  │   image: {image}");
        out.push_str("  │\n");
    }
    for (index, row) in rows.iter().enumerate() {
        let branch = if index + 1 == rows.len() {
            "  ╰── "
        } else {
            "  ├── "
        };
        out.push_str(branch);
        out.push_str(&child_line(row));
        out.push('\n');
    }
    out
}

fn service_marker(rows: &[&OperationRow], pruned: bool) -> &'static str {
    if pruned {
        return "- remove";
    }
    let replacing = rows
        .iter()
        .any(|row| matches!(row.operation, DeployOperation::ReplaceContainer(_)));
    let shrinking = rows.iter().any(|row| {
        matches!(
            row.operation,
            DeployOperation::StopContainer { .. } | DeployOperation::RemoveContainer { .. }
        )
    }) && !rows
        .iter()
        .any(|row| matches!(row.operation, DeployOperation::RunContainer { .. }));
    if replacing || shrinking {
        "~ update"
    } else {
        "+ create"
    }
}

fn spec_image(operation: &DeployOperation) -> Option<&str> {
    operation.spec().map(|spec| spec.container.image.as_str())
}

fn child_line(row: &OperationRow) -> String {
    let machine = machine_label(row);
    let name = report::visible_row_name(row);
    match &row.operation {
        DeployOperation::PrepareVolumes { .. } => {
            format!("+ prepare provisioned storage on {machine}")
        }
        DeployOperation::WaitHealthy {
            dependent,
            dependency,
            ..
        } => format!("~ wait for {dependency} to be healthy before {dependent}"),
        DeployOperation::RunContainer { .. } => {
            format!("+ create container {name} on {machine}")
        }
        DeployOperation::ReplaceContainer(_) => {
            format!("+/- replace container {name} on {machine}")
        }
        DeployOperation::RemoveContainer { .. } => {
            format!("- remove container {name} on {machine}")
        }
        DeployOperation::StopContainer { .. } => {
            format!("- stop container {name} on {machine}")
        }
        DeployOperation::StopHook { .. } => format!("- stop hook {name} on {machine}"),
        DeployOperation::RunHook { .. } => {
            format!("+ run pre-deploy hook for {name} on {machine}")
        }
        DeployOperation::RemoveVolume { .. } => {
            format!("- remove volume {name} on {machine}")
        }
    }
}

fn plan_footer(preview: &DeployPreview) -> String {
    let mut preparations = 0;
    let mut creates = 0;
    let mut replaces = 0;
    let mut removes = 0;
    let mut order = None;
    let mut machines = BTreeMap::new();
    for row in &preview.operations {
        machines.insert(row.machine_id, ());
        match &row.operation {
            DeployOperation::PrepareVolumes { .. } => preparations += 1,
            DeployOperation::RunContainer { .. } => {
                creates += 1;
            }
            DeployOperation::ReplaceContainer(replacement) => {
                replaces += 1;
                order = Some(replacement.spec.update.order);
            }
            DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveVolume { .. } => {
                removes += 1;
            }
            DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. } => {}
        }
    }
    let mut parts = Vec::new();
    if preparations > 0 {
        parts.push(format!("{preparations} storage preparation"));
    }
    if creates > 0 {
        parts.push(format!("{creates} create"));
    }
    if replaces > 0 {
        let order = match order {
            Some(UpdateOrder::StopFirst) => "stop-first",
            Some(UpdateOrder::StartFirst) | None => "start-first",
        };
        parts.push(format!("{replaces} replace ({order})"));
    }
    if removes > 0 {
        parts.push(format!("{removes} remove"));
    }
    let machine_count = machines.len();
    let machine = if machine_count == 1 {
        "machine"
    } else {
        "machines"
    };
    format!("{} · across {machine_count} {machine}\n", parts.join(" · "))
}

fn machine_label(row: &OperationRow) -> String {
    row.machine_name
        .as_ref()
        .map_or_else(|| row.machine_id.to_string(), ToString::to_string)
}

pub(super) fn status_kind(status: &OperationStatus) -> &'static str {
    match status {
        OperationStatus::Pending => "pending",
        OperationStatus::Running { phase } => match phase {
            OperationPhase::WaitingForHealth { .. } => "health",
            OperationPhase::WaitingForHook { .. } => "hook",
            OperationPhase::StoppingContainer
            | OperationPhase::RemovingContainer
            | OperationPhase::RemovingVolume => "removing",
            OperationPhase::Compensating => "compensating",
            OperationPhase::Starting
            | OperationPhase::CreatingContainer
            | OperationPhase::StartingContainer => "running",
        },
        OperationStatus::Completed => "completed",
        OperationStatus::Failed { .. } => "failed",
        OperationStatus::Unexecuted => "unexecuted",
    }
}

/// Compact success report using only completed operations.
#[must_use]
pub(super) fn success_text(
    completed: &[DeployOperation],
    title: &str,
    cluster_domain: Option<&str>,
) -> String {
    let mut counts = BTreeMap::new();
    let mut ready = 0;
    let mut unchecked = 0;
    let mut machines = BTreeSet::new();
    for operation in completed {
        machines.insert(operation.machine_id());
        let (word, skip_health_monitor) = match operation {
            DeployOperation::RunContainer {
                skip_health_monitor,
                ..
            } => ("created", Some(*skip_health_monitor)),
            DeployOperation::ReplaceContainer(replacement) => {
                ("replaced", Some(replacement.skip_health_monitor))
            }
            DeployOperation::RemoveContainer { .. } => ("removed", None),
            DeployOperation::StopContainer { .. } => ("stopped", None),
            DeployOperation::RemoveVolume { .. } => ("removed", None),
            DeployOperation::PrepareVolumes { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::StopHook { .. } => continue,
        };
        *counts.entry(word).or_insert(0) += 1;
        if let Some(skip) = skip_health_monitor {
            ready += 1;
            unchecked += usize::from(skip);
        }
    }
    let mut parts = Vec::new();
    if ready > 0 {
        parts.push(format!("{ready} ready"));
    }
    if unchecked > 0 {
        parts.push(format!("health checks skipped: {unchecked}"));
    }
    // Keep creation and replacement counts ahead of cleanup operations.
    for word in ["created", "replaced", "stopped", "removed"] {
        if let Some(count) = counts.get(word) {
            parts.push(format!("{count} {word}"));
        }
    }
    let count = machines.len();
    let noun = if count == 1 { "machine" } else { "machines" };
    parts.push(format!("{count} {noun}"));
    let mut out = format!("✓ {title}\n  {}\n", parts.join(" · "));
    out.push_str(&endpoints_footer(completed, cluster_domain));
    out
}

fn endpoints_footer(completed: &[DeployOperation], cluster_domain: Option<&str>) -> String {
    let mut by_service = BTreeMap::<_, BTreeSet<_>>::new();
    for operation in completed {
        let spec = match operation {
            DeployOperation::RunContainer { spec, .. } => spec,
            DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. }) => spec,
            DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::PrepareVolumes { .. }
            | DeployOperation::RemoveVolume { .. } => continue,
        };
        for port in &spec.ports {
            let PortPublication::Ingress {
                hostname,
                container_port,
                load_balancer_port,
                http_protocol,
            } = port
            else {
                continue;
            };
            let Some(hostname) = hostname.as_explicit_host() else {
                continue;
            };
            let (scheme, default_port) = match http_protocol {
                HttpProtocol::Https => ("https", 443),
                HttpProtocol::Http => ("http", 80),
            };
            let mut url = format!("{scheme}://{hostname}");
            if load_balancer_port.get() != default_port {
                let _ = write!(url, ":{load_balancer_port}");
            }
            by_service
                .entry((&spec.name, container_port))
                .or_default()
                .insert((hostname.under_cluster_domain(cluster_domain), url));
        }
    }
    let mut out = String::new();
    for ((service, port), endpoints) in by_service {
        let _ = writeln!(out, "\n{service} → :{port}");
        for (_, url) in endpoints {
            let _ = writeln!(out, "  {url}");
        }
    }
    out
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "render_success_tests.rs"]
mod success_tests;
