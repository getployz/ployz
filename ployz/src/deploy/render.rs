//! Tree-shaped stdout for a Deploy Preview and its progress events.
//!
//! No planner lives here. The CLI passes a preview and a recorded event stream.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ployz_core::{
    DeployEvent, DeployOperation, DeployOutcome, DeployPreview, ExecutionError, FailedOperation,
    HttpProtocol, IngressHostname, OperationPhase, OperationRow, OperationStatus, PortPublication,
    ReplacementOperation, UpdateOrder,
};

/// How the live task list is titled.
#[must_use]
pub fn progress_text(event: &DeployEvent, title: &str) -> String {
    match event {
        DeployEvent::Progress {
            completed,
            total,
            rows,
        } => {
            let mut out = format!("[+] {title} {completed}/{total}\n");
            for row in rows {
                out.push_str(&row_line(row));
            }
            out
        }
        DeployEvent::Outcome { outcome } => outcome_text(outcome),
    }
}

/// Tree plan plus footer. Empty operations are "No changes."
#[must_use]
pub fn plan_text(preview: &DeployPreview, context: &str, project_source: Option<&str>) -> String {
    if preview.noop() {
        return "No changes.\n".into();
    }
    let mut out = String::from("Deployment plan\n");
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
    out.push_str("──────────────────────────────────────────\n");
    out.push_str(&plan_footer(preview));
    out.push('\n');
    out
}

/// Confirm prompt targeting the selected context.
#[must_use]
pub fn confirm_prompt(context: &str) -> String {
    format!("Proceed with deployment to {context}? [y/N] ")
}

/// Completed-prefix / failed op / unexecuted rest, plus endpoints on success.
#[must_use]
pub fn outcome_text(outcome: &DeployOutcome<ExecutionError>) -> String {
    match outcome {
        DeployOutcome::Success { completed } => endpoints_footer(completed).unwrap_or_default(),
        DeployOutcome::Failed {
            completed,
            failed,
            unexecuted,
        } => {
            let mut out = format!("Completed {} operation(s).\n", completed.len());
            let _ = writeln!(out, "Failed: {}", failed_summary(failed));
            if !unexecuted.is_empty() {
                let _ = writeln!(
                    out,
                    "Unexecuted: {}",
                    unexecuted
                        .iter()
                        .map(operation_label)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            out
        }
    }
}

fn service_trees(preview: &DeployPreview) -> String {
    let mut groups: BTreeMap<String, Vec<&OperationRow>> = BTreeMap::new();
    let mut volumes = Vec::new();
    for row in &preview.operations {
        if matches!(row.operation, DeployOperation::CreateVolume { .. }) {
            volumes.push(row);
            continue;
        }
        let name = row
            .service_name
            .as_ref()
            .map_or_else(|| "service".into(), ToString::to_string);
        groups.entry(name).or_default().push(row);
    }
    let mut out = String::new();
    for row in volumes {
        let _ = writeln!(out, "{}", volume_line(row));
    }
    for (name, rows) in groups {
        out.push_str(&service_block(&name, &rows));
    }
    out
}

fn volume_line(row: &OperationRow) -> String {
    let DeployOperation::CreateVolume { volume, .. } = &row.operation else {
        return String::new();
    };
    format!(
        "+ create volume {} on {}",
        volume.reference,
        machine_label(row)
    )
}

fn service_block(name: &str, rows: &[&OperationRow]) -> String {
    let marker = service_marker(rows);
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

fn service_marker(rows: &[&OperationRow]) -> &'static str {
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
    match operation {
        DeployOperation::RunContainer { spec, .. } | DeployOperation::RunHook { spec, .. } => {
            Some(spec.container.image.as_str())
        }
        DeployOperation::ReplaceContainer(replacement) => {
            Some(replacement.spec.container.image.as_str())
        }
        DeployOperation::CreateVolume { .. }
        | DeployOperation::StopContainer { .. }
        | DeployOperation::RemoveContainer { .. }
        | DeployOperation::StopHook { .. } => None,
    }
}

fn child_line(row: &OperationRow) -> String {
    let machine = machine_label(row);
    match &row.operation {
        DeployOperation::RunContainer { spec, .. } => {
            format!(
                "+ create container {} on {machine}",
                row.display_name.as_deref().unwrap_or(spec.name.as_str())
            )
        }
        DeployOperation::ReplaceContainer(replacement) => format!(
            "+/- replace container {} on {machine}",
            row.display_name
                .as_deref()
                .unwrap_or_else(|| replacement.old_container_id.as_str())
        ),
        DeployOperation::RemoveContainer { container_id, .. } => format!(
            "- remove container {} on {machine}",
            row.display_name.as_deref().unwrap_or(container_id.as_str())
        ),
        DeployOperation::StopContainer { container_id, .. } => format!(
            "- stop container {} on {machine}",
            row.display_name.as_deref().unwrap_or(container_id.as_str())
        ),
        DeployOperation::StopHook { container_id, .. } => {
            format!("- stop hook {container_id} on {machine}")
        }
        DeployOperation::RunHook { spec, .. } => {
            format!("+ run pre-deploy hook for {} on {machine}", spec.name)
        }
        DeployOperation::CreateVolume { volume, .. } => {
            format!("+ create volume {} on {machine}", volume.reference)
        }
    }
}

fn plan_footer(preview: &DeployPreview) -> String {
    let mut creates = 0;
    let mut replaces = 0;
    let mut removes = 0;
    let mut order = None;
    let mut machines = BTreeMap::new();
    for row in &preview.operations {
        machines.insert(row.machine_id, ());
        match &row.operation {
            DeployOperation::RunContainer { .. } | DeployOperation::CreateVolume { .. } => {
                creates += 1;
            }
            DeployOperation::ReplaceContainer(replacement) => {
                replaces += 1;
                order = Some(replacement.spec.update.order);
            }
            DeployOperation::RemoveContainer { .. } | DeployOperation::StopContainer { .. } => {
                removes += 1;
            }
            DeployOperation::StopHook { .. } | DeployOperation::RunHook { .. } => {}
        }
    }
    let mut parts = Vec::new();
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

fn row_line(row: &OperationRow) -> String {
    let name = container_label(row);
    let machine = machine_label(row);
    let (mark, status, elapsed) = status_columns(row);
    format!(" {mark} Container {name} on {machine}  {status}{elapsed}\n")
}

fn container_label(row: &OperationRow) -> String {
    if let Some(name) = &row.display_name {
        return name.clone();
    }
    match &row.operation {
        DeployOperation::RunContainer { spec, .. } | DeployOperation::RunHook { spec, .. } => {
            spec.name.to_string()
        }
        DeployOperation::ReplaceContainer(replacement) => replacement.old_container_id.to_string(),
        DeployOperation::StopContainer { container_id, .. }
        | DeployOperation::RemoveContainer { container_id, .. }
        | DeployOperation::StopHook { container_id, .. } => container_id.to_string(),
        DeployOperation::CreateVolume { volume, .. } => volume.reference.to_string(),
    }
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
            OperationPhase::StoppingContainer | OperationPhase::RemovingContainer => "removing",
            OperationPhase::Compensating => "compensating",
            OperationPhase::Starting
            | OperationPhase::CreatingVolume
            | OperationPhase::CreatingContainer
            | OperationPhase::StartingContainer => "running",
        },
        OperationStatus::Completed => "completed",
        OperationStatus::Failed { .. } => "failed",
        OperationStatus::Unexecuted => "unexecuted",
    }
}

fn status_columns(row: &OperationRow) -> (&'static str, &'static str, String) {
    match &row.status {
        OperationStatus::Pending => ("•", "Pending", String::new()),
        OperationStatus::Running { phase } => match phase {
            OperationPhase::WaitingForHealth { elapsed_ms, .. } => {
                ("…", "Healthy", elapsed(*elapsed_ms))
            }
            OperationPhase::WaitingForHook { elapsed_ms, .. } => {
                ("…", "Hook", elapsed(*elapsed_ms))
            }
            OperationPhase::StoppingContainer | OperationPhase::RemovingContainer => {
                ("…", "Removed", String::new())
            }
            OperationPhase::Compensating => ("…", "Compensating", String::new()),
            OperationPhase::Starting
            | OperationPhase::CreatingVolume
            | OperationPhase::CreatingContainer
            | OperationPhase::StartingContainer => ("…", "Running", String::new()),
        },
        OperationStatus::Completed => ("✔", completed_label(&row.operation), String::new()),
        OperationStatus::Failed { .. } => ("✖", "Failed", String::new()),
        OperationStatus::Unexecuted => ("•", "Unexecuted", String::new()),
    }
}

fn completed_label(operation: &DeployOperation) -> &'static str {
    match operation {
        DeployOperation::RemoveContainer { .. }
        | DeployOperation::StopContainer { .. }
        | DeployOperation::StopHook { .. } => "Removed",
        DeployOperation::CreateVolume { .. } => "Created",
        DeployOperation::RunContainer { .. }
        | DeployOperation::ReplaceContainer(_)
        | DeployOperation::RunHook { .. } => "Healthy",
    }
}

fn elapsed(elapsed_ms: u64) -> String {
    format!("  {:.1}s", elapsed_ms as f64 / 1000.0)
}

fn endpoints_footer(completed: &[DeployOperation]) -> Option<String> {
    let mut by_service: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for operation in completed {
        let spec = match operation {
            DeployOperation::RunContainer { spec, .. } => spec,
            DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. }) => spec,
            DeployOperation::CreateVolume { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. } => continue,
        };
        for port in &spec.ports {
            let PortPublication::Ingress {
                hostname: IngressHostname::Explicit { hostname },
                container_port,
                http_protocol,
                ..
            } = port
            else {
                continue;
            };
            let scheme = match http_protocol {
                HttpProtocol::Https => "https",
                HttpProtocol::Http => "http",
            };
            by_service
                .entry(spec.name.to_string())
                .or_default()
                .push(format!(" • {scheme}://{hostname} → :{container_port}"));
        }
    }
    if by_service.is_empty() {
        return None;
    }
    let mut out = String::new();
    for (service, lines) in by_service {
        let _ = writeln!(out, "\n{service} endpoints:");
        for line in lines {
            let _ = writeln!(out, "{line}");
        }
    }
    Some(out)
}

fn failed_summary(failed: &FailedOperation<ExecutionError>) -> String {
    match failed {
        FailedOperation::Operation { operation, error } => {
            format!("{}: {error}", operation_label(operation))
        }
        FailedOperation::ReplacementHealth {
            operation, error, ..
        } => format!(
            "replace {} for {} on {}: {error}",
            operation.old_container_id, operation.spec.name, operation.machine_id
        ),
    }
}

fn operation_label(operation: &DeployOperation) -> String {
    match operation {
        DeployOperation::CreateVolume { machine_id, volume } => {
            format!("create volume {} on {machine_id}", volume.reference)
        }
        DeployOperation::RunContainer {
            machine_id, spec, ..
        } => format!("run {} on {machine_id}", spec.name),
        DeployOperation::StopContainer {
            machine_id,
            container_id,
        } => format!("stop {container_id} on {machine_id}"),
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
        } => format!("remove {container_id} on {machine_id}"),
        DeployOperation::ReplaceContainer(operation) => format!(
            "replace {} for {} on {}",
            operation.old_container_id, operation.spec.name, operation.machine_id
        ),
        DeployOperation::StopHook {
            machine_id,
            container_id,
        } => format!("stop hook {container_id} on {machine_id}"),
        DeployOperation::RunHook {
            machine_id, spec, ..
        } => format!("run pre-deploy hook for {} on {machine_id}", spec.name),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{
        ContainerId, DeployOperation, MachineId, MachineName, OperationRow, OperationStatus,
        ProjectName, ReplacementOperation, RequestedServiceSpec, ResolvedServiceSpec, ServiceName,
        UpdateOrder,
    };

    use super::*;

    #[test]
    fn empty_preview_prints_no_changes_without_a_prompt_body() {
        let preview =
            DeployPreview::new(Vec::new(), Vec::new(), ProjectName::parse("app").unwrap());
        assert_eq!(plan_text(&preview, "default", None), "No changes.\n");
        assert_eq!(
            confirm_prompt("default"),
            "Proceed with deployment to default? [y/N] "
        );
    }

    #[test]
    fn replace_plan_matches_tree_shape() {
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let old = ContainerId::parse("f".repeat(64)).unwrap();
        let spec = resolved("excalidraw", "excalidraw/excalidraw:latest");
        let row = OperationRow::pending(
            0,
            DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id,
                old_container_id: old,
                spec,
                skip_health_monitor: false,
            }),
            Some(MachineName::parse("machine-dc3c").unwrap()),
            Some("excalidraw/fde7ac7f11ad".into()),
            Some(ServiceName::parse("excalidraw").unwrap()),
        );
        let preview = DeployPreview::new(vec![row], Vec::new(), ProjectName::parse("app").unwrap());
        let text = plan_text(&preview, "default", None);
        assert!(text.contains("Deployment plan\ncontext: default\nproject: app\n"));
        assert!(text.contains("~ update service excalidraw\n"));
        assert!(text.contains("  │   image: excalidraw/excalidraw:latest\n"));
        assert!(
            text.contains("  ╰── +/- replace container excalidraw/fde7ac7f11ad on machine-dc3c\n")
        );
        assert!(text.contains("1 replace (start-first) · across 1 machine\n"));
    }

    #[test]
    fn progress_snapshot_prints_healthy_elapsed_and_removed() {
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let machine = Some(MachineName::parse("machine-dc3c").unwrap());
        let spec = resolved("excalidraw", "excalidraw/excalidraw:latest");
        let healthy = OperationRow {
            index: 0,
            machine_id,
            machine_name: machine.clone(),
            operation: DeployOperation::RunContainer {
                machine_id,
                spec: spec.clone(),
                skip_health_monitor: false,
            },
            display_name: Some("excalidraw-0z12".into()),
            service_name: Some(ServiceName::parse("excalidraw").unwrap()),
            status: OperationStatus::Running {
                phase: OperationPhase::WaitingForHealth {
                    container_id: ContainerId::parse("a".repeat(64)).unwrap(),
                    health: None,
                    elapsed_ms: 30_600,
                    deadline_ms: 60_000,
                },
            },
        };
        let removed = OperationRow {
            index: 1,
            machine_id,
            machine_name: machine,
            operation: DeployOperation::RemoveContainer {
                machine_id,
                container_id: ContainerId::parse("f".repeat(64)).unwrap(),
            },
            display_name: Some("excalidraw/fde7ac7f11ad".into()),
            service_name: Some(ServiceName::parse("excalidraw").unwrap()),
            status: OperationStatus::Completed,
        };
        let event = DeployEvent::Progress {
            completed: 2,
            total: 2,
            rows: vec![healthy, removed],
        };
        let text = progress_text(&event, "Deploying to default");
        assert!(text.contains("[+] Deploying to default 2/2\n"));
        assert!(text.contains("Container excalidraw-0z12 on machine-dc3c"));
        assert!(text.contains("Healthy"));
        assert!(text.contains("30.6s"));
        assert!(text.contains("Container excalidraw/fde7ac7f11ad on machine-dc3c"));
        assert!(text.contains("Removed"));
    }

    #[test]
    fn run_style_titles_the_task_list_for_an_ad_hoc_service() {
        let event = DeployEvent::Progress {
            completed: 0,
            total: 1,
            rows: Vec::new(),
        };
        let text = progress_text(&event, "Running service api");
        assert_eq!(text, "[+] Running service api 0/1\n");
    }

    #[test]
    fn success_with_ingress_prints_endpoints_footer() {
        let spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
            "service_id": "a".repeat(32),
            "name": "excalidraw",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "excalidraw/excalidraw:latest", "pull_policy": "missing" },
            "ports": [{
                "mode": "ingress",
                "hostname": { "kind": "explicit", "hostname": "excalidraw.acme.ployz.app" },
                "load_balancer_port": 443,
                "container_port": 80,
                "http_protocol": "https"
            }]
        }))
        .unwrap();
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let outcome = DeployOutcome::Success {
            completed: vec![DeployOperation::RunContainer {
                machine_id,
                spec,
                skip_health_monitor: true,
            }],
        };
        let text = outcome_text(&outcome);
        assert!(text.contains("excalidraw endpoints:"));
        assert!(text.contains("https://excalidraw.acme.ployz.app → :80"));
    }

    fn resolved(name: &str, image: &str) -> ResolvedServiceSpec {
        let requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
            "name": name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": image, "pull_policy": "missing" }
        }))
        .unwrap();
        requested.to_resolved(
            ployz_core::ServiceId::parse("a".repeat(32)).unwrap(),
            ployz_core::ResolvedUpdateConfig {
                order: UpdateOrder::StartFirst,
                monitor_millis: None,
            },
        )
    }
}
