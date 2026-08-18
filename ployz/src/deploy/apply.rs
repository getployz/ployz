use std::{
    io::{self, IsTerminal, Write},
    num::NonZeroU32,
};

use ployz_core::{PlanOptions, ProjectName, RequestedServiceSpec, ServiceSelector};

use crate::{
    compose::{BuildService, ComposeProject},
    connect::Client,
    context::Connection,
    failure::Failure,
    project::ResolvedProject,
};

use super::{
    DeployOperation, DeployOutcome, DeployPreview, ExecutionError, FailedOperation,
    ReplacementOperation, ServiceAttempt,
    pipeline::{
        PushOutcome, execute_deploy, list_machines, plan_options, plan_project, plan_scale,
        plan_spec, push_project_images,
    },
};

pub(crate) async fn deploy_spec(
    client: &mut Client,
    requested: &RequestedServiceSpec,
    force_recreate: bool,
    skip_health_monitor: bool,
    project_name: &ProjectName,
    header: Option<&ResolvedProject>,
) -> Result<(), Failure> {
    let preview = plan_spec(
        client,
        requested,
        plan_options(force_recreate, skip_health_monitor),
        project_name,
    )
    .await?;
    print_warnings(&preview);
    render(&preview.operations, client.connection(), header);
    finish(execute_deploy(client, &preview.operations, project_name).await)
}

pub(crate) async fn apply_requested(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<(), Failure> {
    deploy_spec(
        client,
        requested,
        false,
        false,
        &ProjectName::system(),
        None,
    )
    .await
}

pub(crate) async fn deploy_project(
    client: &mut Client,
    project: &mut ComposeProject,
    builds: &[BuildService],
    apply: Vec<ServiceAttempt>,
    options: PlanOptions,
    auto_confirm: bool,
    resolved: &ResolvedProject,
) -> Result<(), Failure> {
    let machines = list_machines(client).await?;
    let outcome = push_project_images(client, builds, &machines).await?;
    print_pushed_images(&outcome);
    if !outcome.failures.is_empty() {
        return Err(Failure::usage(format!(
            "image push failed: {}",
            outcome.failures.join("; ")
        )));
    }
    let preview = plan_project(client, project, machines, apply, options, &resolved.name).await?;
    print_warnings(&preview);
    if preview.operations.is_empty() {
        render(&[], client.connection(), Some(resolved));
        println!("No changes.");
        return Ok(());
    }
    // TODO(UT-086): this is a best-effort preview over one observer-relative snapshot.
    confirm_and_execute(client, &preview.operations, auto_confirm, resolved).await
}

pub(crate) async fn deploy_scale(
    client: &mut Client,
    selector: &ServiceSelector,
    replicas: NonZeroU32,
    skip_health_monitor: bool,
    auto_confirm: bool,
    project: &ResolvedProject,
) -> Result<(), Failure> {
    let (preview, project_name) = plan_scale(
        client,
        selector,
        replicas,
        plan_options(false, skip_health_monitor),
    )
    .await?;
    print_warnings(&preview);
    let project = ResolvedProject {
        name: project_name,
        source: project.source,
    };
    if preview.operations.is_empty() {
        render(&[], client.connection(), Some(&project));
        println!("No changes.");
        return Ok(());
    }
    confirm_and_execute(client, &preview.operations, auto_confirm, &project).await
}

async fn confirm_and_execute(
    client: &mut Client,
    operations: &[DeployOperation],
    auto_confirm: bool,
    project: &ResolvedProject,
) -> Result<(), Failure> {
    render(operations, client.connection(), Some(project));
    if !auto_confirm && !confirm()? {
        println!("Cancelled. No changes were made.");
        return Ok(());
    }
    finish(execute_deploy(client, operations, &project.name).await)
}

fn print_pushed_images(outcome: &PushOutcome) {
    for pushed in &outcome.pushed {
        println!("Pushed {} to {}", pushed.image, pushed.machine_id);
    }
}

fn print_warnings(preview: &DeployPreview) {
    for warning in &preview.warnings {
        eprintln!("WARNING: {warning}");
    }
}

fn confirm() -> Result<bool, Failure> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Failure::usage(
            "confirmation requires a terminal; pass --yes to continue",
        ));
    }
    print!("Continue? [y/N] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(matches!(input.trim(), "y" | "Y" | "yes" | "YES"))
}

fn render(
    operations: &[DeployOperation],
    connection: &Connection,
    project: Option<&ResolvedProject>,
) {
    println!("{}", plan_header(connection, project));
    for operation in operations {
        println!("  {}", operation_summary(operation));
    }
}

fn plan_header(connection: &Connection, project: Option<&ResolvedProject>) -> String {
    match project {
        Some(project) => format!(
            "Plan for {connection}:\nProject: {} ({})",
            project.name, project.source
        ),
        None => format!("Plan for {connection}:"),
    }
}

fn finish(outcome: DeployOutcome<ExecutionError>) -> Result<(), Failure> {
    match outcome {
        DeployOutcome::Success { completed } => {
            println!("Completed {} operation(s).", completed.len());
            Ok(())
        }
        DeployOutcome::Failed {
            completed,
            failed,
            unexecuted,
        } => {
            println!("Completed {} operation(s).", completed.len());
            Err(Failure::usage(format!(
                "Deploy stopped; completed: [{}]; failed: {}; unexecuted: [{}]",
                operation_list(&completed),
                failed_summary(&failed),
                operation_list(&unexecuted),
            )))
        }
    }
}

fn failed_summary(failed: &FailedOperation<ExecutionError>) -> String {
    match failed {
        FailedOperation::Operation { operation, error } => {
            format!("{}: {error}", operation_summary(operation))
        }
        FailedOperation::ReplacementHealth {
            operation,
            error,
            compensation,
        } => format!(
            "{}: {error}; compensation: {compensation:?}",
            replacement_summary(operation),
        ),
    }
}

fn operation_summary(operation: &DeployOperation) -> String {
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
        DeployOperation::ReplaceContainer(operation) => replacement_summary(operation),
        DeployOperation::StopHook {
            machine_id,
            container_id,
        } => format!("stop hook {container_id} on {machine_id}"),
        DeployOperation::RunHook {
            machine_id, spec, ..
        } => format!("run pre-deploy hook for {} on {machine_id}", spec.name),
    }
}

fn replacement_summary(operation: &ReplacementOperation) -> String {
    format!(
        "replace {} for {} on {}",
        operation.old_container_id, operation.spec.name, operation.machine_id
    )
}

fn operation_list(operations: &[DeployOperation]) -> String {
    operations
        .iter()
        .map(operation_summary)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deploy::DeployWarning;
    use crate::dns::ingress_dns_warnings;

    #[test]
    fn deploy_prints_ingress_misses_as_warning_lines_without_failing() {
        let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
            "name": "web",
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "nginx", "pull_policy": "missing" },
            "ports": [
                {
                    "mode": "ingress",
                    "hostname": { "kind": "explicit", "hostname": "app.example.com" },
                    "load_balancer_port": 443,
                    "container_port": 8080,
                    "http_protocol": "https"
                },
                {
                    "mode": "ingress",
                    "hostname": { "kind": "explicit", "hostname": "plain.example.com" },
                    "load_balancer_port": 80,
                    "container_port": 8080,
                    "http_protocol": "http"
                }
            ]
        }))
        .unwrap();
        let cluster = ["192.0.2.1".parse().unwrap()];
        let preview = DeployPreview {
            operations: Vec::new(),
            warnings: ingress_dns_warnings([&spec], &cluster, |hostname| match hostname.as_str() {
                "app.example.com" => vec!["198.51.100.10".parse().unwrap()],
                "plain.example.com" => Vec::new(),
                other => panic!("unexpected {other}"),
            })
            .into_iter()
            .map(DeployWarning::from)
            .collect(),
        };
        assert_eq!(
            preview
                .warnings
                .iter()
                .map(|warning| format!("WARNING: {warning}"))
                .collect::<Vec<_>>(),
            [
                "WARNING: Ingress Hostname app.example.com resolves to 198.51.100.10; it should resolve to 192.0.2.1. A certificate cannot be issued until it points at this Cluster.",
                "WARNING: Ingress Hostname plain.example.com does not resolve; it should resolve to 192.0.2.1.",
            ]
        );
        assert!(
            !preview
                .warnings
                .iter()
                .map(|warning| format!("WARNING: {warning}"))
                .any(|line| line.contains("plain.example.com")
                    && line.to_ascii_lowercase().contains("certificate"))
        );
    }

    #[test]
    fn plan_header_states_the_project_and_precedence_level() {
        let connection = Connection::tcp("127.0.0.1:1".parse().unwrap());
        let project = crate::project::ResolvedProject {
            name: ployz_core::ProjectName::parse("shop").unwrap(),
            source: crate::project::ProjectNameSource::ComposeName,
        };
        assert_eq!(
            plan_header(&connection, Some(&project)),
            "Plan for tcp://127.0.0.1:1:\nProject: shop (top-level Compose name)"
        );
        assert_eq!(
            plan_header(&connection, None),
            "Plan for tcp://127.0.0.1:1:"
        );
    }
}
