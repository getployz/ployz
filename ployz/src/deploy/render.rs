//! Tree-shaped stdout for a Deploy Preview and its progress events.
//!
//! No planner lives here. The CLI passes a preview and a recorded event stream.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use ployz_core::{
    DeployEvent, DeployOperation, DeployOutcome, DeployPreview, ExecutionError, FailedOperation,
    HttpProtocol, OperationPhase, OperationRow, OperationStatus, PortPublication,
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

/// Confirm prompt for removing one observer-derived Project.
#[must_use]
pub fn confirm_removal_prompt(project: &ployz_core::ProjectName, context: &str) -> String {
    format!("Proceed with removal of Project {project} from {context}? [y/N] ")
}

/// Completed operations / failed op / unexecuted operations, plus endpoints on success.
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
    let mut out = String::new();
    for row in &preview.operations {
        if matches!(row.operation, DeployOperation::RemoveVolume { .. }) {
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
    match &row.operation {
        DeployOperation::WaitHealthy {
            dependent,
            dependency,
            ..
        } => format!("~ wait for {dependency} to be healthy before {dependent}"),
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
        DeployOperation::RemoveVolume { id } => {
            format!("- remove volume {} on {machine}", id.name)
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
    let (mark, status, elapsed) = status_columns(row);
    if let DeployOperation::WaitHealthy { dependency, .. } = &row.operation {
        return format!(" {mark} Dependency {dependency}  {status}{elapsed}\n");
    }
    let name = container_label(row);
    let machine = machine_label(row);
    format!(" {mark} Container {name} on {machine}  {status}{elapsed}\n")
}

fn container_label(row: &OperationRow) -> String {
    if let Some(name) = &row.display_name {
        return name.clone();
    }
    match &row.operation {
        DeployOperation::WaitHealthy { dependency, .. } => dependency.to_string(),
        DeployOperation::RunContainer { spec, .. } | DeployOperation::RunHook { spec, .. } => {
            spec.name.to_string()
        }
        DeployOperation::ReplaceContainer(replacement) => replacement.old_container_id.to_string(),
        DeployOperation::StopContainer { container_id, .. }
        | DeployOperation::RemoveContainer { container_id, .. }
        | DeployOperation::StopHook { container_id, .. } => container_id.to_string(),
        DeployOperation::RemoveVolume { id } => id.name.to_string(),
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
            OperationPhase::StoppingContainer
            | OperationPhase::RemovingContainer
            | OperationPhase::RemovingVolume => ("…", "Removed", String::new()),
            OperationPhase::Compensating => ("…", "Compensating", String::new()),
            OperationPhase::Starting
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
        | DeployOperation::StopHook { .. }
        | DeployOperation::RemoveVolume { .. } => "Removed",
        DeployOperation::WaitHealthy { .. }
        | DeployOperation::RunContainer { .. }
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
            DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. } => continue,
        };
        for port in &spec.ports {
            let PortPublication::Ingress {
                hostname,
                container_port,
                http_protocol,
                ..
            } = port
            else {
                continue;
            };
            let Some(hostname) = hostname.as_explicit_host() else {
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
        DeployOperation::WaitHealthy {
            dependent,
            dependency,
            ..
        } => format!("wait for {dependency} to be healthy before {dependent}"),
        DeployOperation::RunContainer {
            machine_id, spec, ..
        } => format!("run {} on {machine_id}", spec.name),
        DeployOperation::StopContainer {
            machine_id,
            container_id,
            ..
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
        DeployOperation::RemoveVolume { id } => {
            format!("remove volume {} on {}", id.name, id.machine_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU64;

    use ployz_core::{
        ContainerId, DeployOperation, DockerVolumeId, DockerVolumeName, MachineId, MachineName,
        OperationRow, OperationStatus, PreservedVolume, ProjectName, ProvisionedVolumeMaximumBytes,
        PruneRefusal, QualifiedService, ReplacementOperation, RequestedServiceSpec,
        ResolvedServiceSpec, ServiceName, UpdateOrder, VolumeToCreate,
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
        assert_eq!(
            confirm_removal_prompt(&ProjectName::parse("shop").unwrap(), "prod"),
            "Proceed with removal of Project shop from prod? [y/N] "
        );
    }

    #[test]
    fn removal_plan_lists_container_and_volume_removes() {
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let container_id = ContainerId::parse("f".repeat(64)).unwrap();
        let volume_id = DockerVolumeId {
            machine_id,
            name: DockerVolumeName::parse("shop_data").unwrap(),
        };
        let rows = vec![
            OperationRow::pending(
                0,
                DeployOperation::RemoveContainer {
                    machine_id,
                    container_id,
                },
                Some(MachineName::parse("edge").unwrap()),
                Some("web-1".into()),
                Some(ServiceName::parse("web").unwrap()),
            ),
            OperationRow::pending(
                1,
                DeployOperation::RemoveVolume {
                    id: volume_id.clone(),
                },
                Some(MachineName::parse("edge").unwrap()),
                None,
                None,
            ),
        ];
        let mut preview = DeployPreview::new(rows, Vec::new(), ProjectName::parse("shop").unwrap());
        preview.would_remove = vec![QualifiedService::parse("shop/web").unwrap()];
        let text = removal_plan_text(&preview, "default");
        assert!(text.starts_with("Removal plan\n"), "{text}");
        assert!(text.contains("- remove container web-1 on edge"), "{text}");
        assert!(text.contains("- remove volume shop_data on edge"), "{text}");
        preview.operations.clear();
        preview.preserved_volumes = vec![PreservedVolume {
            id: volume_id,
            machine_name: Some(MachineName::parse("edge").unwrap()),
        }];
        let preserved = removal_plan_text(&preview, "default");
        assert!(
            preserved.contains("would preserve volume shop_data on edge"),
            "{preserved}"
        );
    }

    #[test]
    fn service_tree_keeps_volume_and_service_ordering() {
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let machine = Some(MachineName::parse("edge").unwrap());
        let container = |index, service: &str, display: &str, id: char| {
            OperationRow::pending(
                index,
                DeployOperation::StopContainer {
                    machine_id,
                    container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
                    purpose: ployz_core::StopContainerPurpose::Lifecycle,
                },
                machine.clone(),
                Some(display.into()),
                Some(ServiceName::parse(service).unwrap()),
            )
        };
        let volume = |index, name: &str| {
            OperationRow::pending(
                index,
                DeployOperation::RemoveVolume {
                    id: DockerVolumeId {
                        machine_id,
                        name: DockerVolumeName::parse(name).unwrap(),
                    },
                },
                machine.clone(),
                None,
                None,
            )
        };
        let rows = vec![
            container(0, "web", "web-old", '1'),
            volume(1, "z"),
            container(2, "api", "api-old", '2'),
            volume(3, "a"),
            container(4, "web", "web-new", '3'),
        ];
        let preview = DeployPreview::new(rows, Vec::new(), ProjectName::parse("app").unwrap());

        assert_eq!(
            service_trees(&preview),
            concat!(
                "- remove volume z on edge\n",
                "- remove volume a on edge\n",
                "~ update service api\n",
                "  ╰── - stop container api-old on edge\n",
                "~ update service web\n",
                "  ├── - stop container web-old on edge\n",
                "  ╰── - stop container web-new on edge\n",
            )
        );
    }

    #[test]
    fn plan_identifies_a_provisioned_volume_and_its_bound() {
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let mut preview =
            DeployPreview::new(Vec::new(), Vec::new(), ProjectName::parse("shop").unwrap());
        preview.volumes_to_create = vec![VolumeToCreate {
            machine_id,
            machine_name: Some(MachineName::parse("edge").unwrap()),
            name: DockerVolumeName::parse("data").unwrap(),
            maximum_bytes: Some(ProvisionedVolumeMaximumBytes::new(
                NonZeroU64::new(1_073_741_824).unwrap(),
            )),
        }];

        let text = plan_text(&preview, "default", None);

        assert!(text.contains("Volumes to create\n"), "{text}");
        assert!(
            text.contains("+ provisioned volume data (maximum 1073741824 bytes) on edge"),
            "{text}"
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
    fn plan_shows_dependency_health_wait() {
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let row = OperationRow::pending(
            0,
            DeployOperation::WaitHealthy {
                machine_id,
                dependent: QualifiedService::parse("app/web").unwrap(),
                dependency: QualifiedService::parse("app/db").unwrap(),
            },
            Some(MachineName::parse("edge").unwrap()),
            None,
            Some(ServiceName::parse("web").unwrap()),
        );
        let preview = DeployPreview::new(vec![row], Vec::new(), ProjectName::parse("app").unwrap());

        assert!(
            plan_text(&preview, "default", None)
                .contains("~ wait for app/db to be healthy before app/web")
        );
    }

    #[test]
    fn plan_lists_would_remove_with_observer_relative_refusal() {
        let mut preview =
            DeployPreview::new(Vec::new(), Vec::new(), ProjectName::parse("shop").unwrap());
        preview.would_remove = vec![QualifiedService::parse("shop/debug").unwrap()];
        preview.prune_refusal = Some(PruneRefusal::IncompleteSnapshot);
        let text = plan_text(&preview, "default", Some("top-level Compose name"));
        assert!(
            text.contains("project: shop (top-level Compose name)"),
            "{text}"
        );
        assert!(text.contains("would remove shop/debug"), "{text}");
        assert!(
            text.contains("incomplete relative to this Machine's current visible fan-out"),
            "{text}"
        );
        assert!(
            !text.to_ascii_lowercase().contains("cluster completeness")
                || text.contains("not Cluster completeness"),
            "{text}"
        );
        assert!(!text.contains("authoritative"));
        assert!(!text.contains("No changes."));
    }

    #[test]
    fn plan_lists_preserved_volumes_instead_of_no_changes() {
        let mut preview =
            DeployPreview::new(Vec::new(), Vec::new(), ProjectName::parse("shop").unwrap());
        preview.preserved_volumes = vec![ployz_core::PreservedVolume {
            id: ployz_core::DockerVolumeId {
                machine_id: MachineId::parse("d".repeat(32)).unwrap(),
                name: ployz_core::DockerVolumeName::parse("shop_data").unwrap(),
            },
            machine_name: Some(MachineName::parse("edge").unwrap()),
        }];
        let text = plan_text(&preview, "default", None);
        assert!(
            text.contains("would preserve volume shop_data on edge"),
            "{text}"
        );
        assert!(!text.contains("No changes."));
    }

    #[test]
    fn plan_shows_prune_as_remove_operations_before_confirm() {
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let row = OperationRow::pending(
            0,
            DeployOperation::RemoveContainer {
                machine_id,
                container_id: ContainerId::parse("f".repeat(64)).unwrap(),
            },
            Some(MachineName::parse("machine-dc3c").unwrap()),
            Some("debug/fde7ac7f11ad".into()),
            Some(ServiceName::parse("debug").unwrap()),
        );
        let mut preview =
            DeployPreview::new(vec![row], Vec::new(), ProjectName::parse("shop").unwrap());
        preview.would_remove = vec![QualifiedService::parse("shop/debug").unwrap()];
        let text = plan_text(&preview, "default", None);
        assert!(text.contains("- remove service debug\n"), "{text}");
        assert!(
            text.contains("- remove container debug/fde7ac7f11ad on machine-dc3c"),
            "{text}"
        );
        assert!(text.contains("1 remove · across 1 machine"), "{text}");
        assert!(!text.contains("~ update service debug"), "{text}");
        assert!(!text.contains("would remove"), "{text}");
        assert!(!text.contains("will not remove"), "{text}");
        assert_eq!(
            confirm_prompt("default"),
            "Proceed with deployment to default? [y/N] "
        );
        assert!(!preview.noop());
    }

    #[test]
    fn replica_shrink_still_prints_update_not_service_remove() {
        let machine_id = MachineId::parse("d".repeat(32)).unwrap();
        let row = OperationRow::pending(
            0,
            DeployOperation::RemoveContainer {
                machine_id,
                container_id: ContainerId::parse("f".repeat(64)).unwrap(),
            },
            Some(MachineName::parse("machine-dc3c").unwrap()),
            Some("web/fde7ac7f11ad".into()),
            Some(ServiceName::parse("web").unwrap()),
        );
        let preview =
            DeployPreview::new(vec![row], Vec::new(), ProjectName::parse("shop").unwrap());
        let text = plan_text(&preview, "default", None);
        assert!(text.contains("~ update service web\n"), "{text}");
        assert!(
            text.contains("- remove container web/fde7ac7f11ad on machine-dc3c"),
            "{text}"
        );
        assert!(!text.contains("- remove service web"), "{text}");
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
                spec,
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
        requested
            .to_resolved(
                ployz_core::ServiceId::parse("a".repeat(32)).unwrap(),
                ployz_core::ResolvedUpdateConfig {
                    order: UpdateOrder::StartFirst,
                    monitor_millis: None,
                },
            )
            .expect("volume graph is scoped")
    }
}
