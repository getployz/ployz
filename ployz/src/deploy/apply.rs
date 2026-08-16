use std::{
    io::{self, IsTerminal, Write},
    num::NonZeroU32,
};

use ployz_core::RequestedServiceSpec;

use crate::{
    compose::{BuildService, ComposeProject},
    connect::Client,
    context::Connection,
    failure::Failure,
};

use super::{
    DeployOperation, DeployOutcome, ExecutionError, FailedOperation, ReplacementOperation,
    pipeline::{
        DeployPreview, ObservationWarning, PushOutcome, execute_deploy, list_machines,
        plan_options, plan_project, plan_scale, plan_spec, push_project_images,
    },
};

pub(crate) async fn deploy_spec(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<(), Failure> {
    let preview = plan_spec(client, requested).await?;
    print_warnings(&preview);
    render(&preview.operations, client.connection());
    finish(execute_deploy(client, &preview.operations).await)
}

pub(crate) use deploy_spec as apply_requested;

pub(crate) async fn deploy_project(
    client: &mut Client,
    project: &mut ComposeProject,
    builds: &[BuildService],
    force_recreate: bool,
    skip_health_monitor: bool,
    auto_confirm: bool,
) -> Result<(), Failure> {
    let options = plan_options(force_recreate, skip_health_monitor);
    let machines = list_machines(client).await?;
    let outcome = push_project_images(client, builds, &machines).await?;
    print_pushed_images(&outcome);
    if !outcome.failures.is_empty() {
        return Err(Failure::usage(format!(
            "image push failed: {}",
            outcome.failures.join("; ")
        )));
    }
    let preview = plan_project(client, project, machines, options).await?;
    print_warnings(&preview);
    if preview.operations.is_empty() {
        println!("No changes.");
        return Ok(());
    }
    // TODO(UT-086): this is a best-effort preview over one observer-relative snapshot.
    confirm_and_execute(client, &preview.operations, auto_confirm).await
}

pub(crate) async fn deploy_scale(
    client: &mut Client,
    selector: &str,
    replicas: NonZeroU32,
    auto_confirm: bool,
) -> Result<(), Failure> {
    let preview = plan_scale(client, selector, replicas).await?;
    print_warnings(&preview);
    if preview.operations.is_empty() {
        println!("No changes.");
        return Ok(());
    }
    confirm_and_execute(client, &preview.operations, auto_confirm).await
}

async fn confirm_and_execute(
    client: &mut Client,
    operations: &[DeployOperation],
    auto_confirm: bool,
) -> Result<(), Failure> {
    render(operations, client.connection());
    if !auto_confirm && !confirm()? {
        println!("Cancelled. No changes were made.");
        return Ok(());
    }
    finish(execute_deploy(client, operations).await)
}

fn print_pushed_images(outcome: &PushOutcome) {
    for pushed in &outcome.pushed {
        println!("Pushed {} to {}", pushed.image, pushed.machine_id);
    }
}

fn print_warnings(preview: &DeployPreview) {
    for warning in &preview.warnings {
        match warning {
            ObservationWarning::Failed {
                kind,
                machine_id,
                message,
            } => eprintln!("WARNING: {kind} observation failed on {machine_id}: {message}"),
            ObservationWarning::Omitted { kind, machine_id } => {
                eprintln!("WARNING: {kind} observation omitted {machine_id}");
            }
        }
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

fn render(operations: &[DeployOperation], connection: &Connection) {
    println!("Plan for {connection}:");
    for operation in operations {
        println!("  {}", operation_summary(operation));
    }
}

fn finish(outcome: DeployOutcome<ExecutionError>) -> Result<(), Failure> {
    println!("Completed {} operation(s).", outcome.completed.len());
    let Some(failed) = &outcome.failed else {
        return Ok(());
    };
    Err(Failure::usage(format!(
        "Deploy stopped; completed: [{}]; failed: {}; unexecuted: [{}]",
        operation_list(&outcome.completed),
        failed_summary(failed),
        operation_list(&outcome.unexecuted),
    )))
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
