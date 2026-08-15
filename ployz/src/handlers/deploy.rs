use std::{num::NonZeroU32, time::SystemTime};

use clap::ArgMatches;
use ployz_core::{
    ListMachinesRequest, PartialResult, PortPublication, RequestedServiceSpec, RpcError, ServiceId,
    ServiceMode, op, select_service,
};
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{
        BuildOptions, BuildService, ComposeProject, LoadOptions, execute_build, load_project,
        plan_build, plan_compose_deploy,
    },
    connect::Client,
    deploy::{
        DeployOperation, DeployOutcome, DeployPlan, DeploySnapshot, ExecutionError,
        FailedOperation, PlanOptions, ReplacementOperation, execute_operations, plan_deploy,
    },
};

use super::{
    Error,
    build::{push_targets, report_push},
    confirm, connect_client, leaf_matches, required, runtime, string_values,
};

pub(super) fn run(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let requested = run_spec(matches)?;
    let context = matches.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = connect_client(root, context).await?;
        finish(run_connected(&mut client, &requested).await?)
    })
}

pub(super) fn deploy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let (mut project, builds) = prepare_deploy(matches)?;
    let context = project
        .selected_context(
            matches.get_one::<String>("context").map(String::as_str),
            matches.get_one::<String>("connect").map(String::as_str),
        )
        .map(str::to_owned);
    let yes = matches.get_flag("yes");
    let options = plan_options(
        matches.get_flag("recreate"),
        matches.get_flag("skip-health"),
    );
    runtime()?.block_on(async {
        let mut client = connect_client(root, context.as_deref()).await?;
        let outcome = deploy_connected(&mut client, &mut project, &builds, options, yes).await?;
        outcome.map_or(Ok(()), finish)
    })
}

fn prepare_deploy(matches: &ArgMatches) -> Result<(ComposeProject, Vec<BuildService>), Error> {
    let load = LoadOptions {
        command: "deploy".into(),
        files: string_values(matches, "file")
            .into_iter()
            .map(Into::into)
            .collect(),
        profiles: string_values(matches, "profile"),
        ..Default::default()
    };
    let selected = string_values(matches, "service");
    let project = load_project(&load).map_err(|error| error.to_string())?;
    for warning in &project.warnings {
        eprintln!("WARNING: {warning}");
    }
    let build_options = BuildOptions {
        build_args: string_values(matches, "build-arg"),
        deps: true,
        no_cache: matches.get_flag("no-cache"),
        pull: matches.get_flag("build-pull"),
        services: selected.clone(),
        ..Default::default()
    };
    let mut builds = plan_build(&project, &build_options).map_err(|error| error.to_string())?;
    if matches.get_flag("no-build") {
        builds.clear();
    } else {
        execute_build(&builds, &build_options, &load).map_err(|error| error.to_string())?;
    }
    let project = project
        .select_services(&selected)
        .map_err(|error| error.to_string())?;
    Ok((project, builds))
}

pub(super) fn scale(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let replicas = parse_u32(matches, "replicas")?;
    // TODO(EO-012): reject zero before resolving configuration or connecting to a Machine.
    let replicas = NonZeroU32::new(replicas)
        .ok_or_else(|| Error::from("replicas must be greater than zero"))?;
    let selector = required(matches, "service")?;
    let yes = matches.get_flag("yes");
    let context = matches.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = connect_client(root, context).await?;
        let outcome = scale_connected(&mut client, &selector, replicas, yes).await?;
        outcome.map_or(Ok(()), finish)
    })
}

async fn list_machines(client: &mut Client) -> Result<Vec<ployz_core::MachineObservation>, Error> {
    client
        .call::<op::ListMachines>(ListMachinesRequest {}, None)
        .await
        .map(|list| list.machines)
        .map_err(|error| Error::from(error.to_string()))
}

async fn take_snapshot(
    client: &mut Client,
    machines: Vec<ployz_core::MachineObservation>,
) -> Result<DeploySnapshot, Error> {
    let gathered = client
        .deploy_snapshot(machines)
        .await
        .map_err(|error| Error::from(error.to_string()))?;
    report_observation_warnings("container", &gathered.containers);
    report_observation_warnings("volume", &gathered.volumes);
    Ok(gathered.snapshot)
}

fn report_observation_warnings<T>(kind: &str, result: &PartialResult<T, RpcError>) {
    for failure in &result.failures {
        eprintln!(
            "WARNING: {kind} observation failed on {}: {}",
            failure.machine_id, failure.error.message
        );
    }
    for machine in &result.omissions {
        eprintln!("WARNING: {kind} observation omitted {machine}");
    }
}

async fn push_image(
    client: &mut Client,
    service: &BuildService,
    machines: &[ployz_core::MachineObservation],
) -> Result<(), Error> {
    let targets = push_targets(&[], &service.machines);
    let result =
        crate::image::push_using_machines(client, &service.image, None, &targets, machines)
            .await
            .map_err(|error| format!("{}: {error}", service.image))?;
    let failures = report_push(&service.image, result);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; ").into())
    }
}

async fn run_connected(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<DeployOutcome<ExecutionError>, Error> {
    let machines = list_machines(client).await?;
    let snapshot = take_snapshot(client, machines).await?;
    let mut requested = requested.clone();
    expand_ingress(client, std::iter::once(&mut requested)).await?;
    let plan = plan_deploy(
        &requested,
        &snapshot,
        ServiceId::random(),
        plan_options(false, false),
    )
    .map_err(|error| error.to_string())?;
    render(plan.operations(), client.connection());
    Ok(execute_operations(plan.operations(), client, &CancellationToken::new()).await)
}

pub(super) async fn deploy_requested(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<(), Error> {
    finish(run_connected(client, requested).await?)
}

async fn deploy_connected(
    client: &mut Client,
    project: &mut crate::compose::ComposeProject,
    builds: &[BuildService],
    options: PlanOptions,
    auto_confirm: bool,
) -> Result<Option<DeployOutcome<ExecutionError>>, Error> {
    let machines = list_machines(client).await?;
    let mut failures = Vec::new();
    for service in builds {
        if let Err(error) = push_image(client, service, &machines).await {
            failures.push(error.to_string());
        }
    }
    if !failures.is_empty() {
        return Err(format!("image push failed: {}", failures.join("; ")).into());
    }
    project
        .resolve_secrets()
        .map_err(|error| error.to_string())?;
    let snapshot = take_snapshot(client, machines).await?;
    expand_ingress(client, project.services.values_mut()).await?;
    let compose =
        plan_compose_deploy(project, &snapshot, options).map_err(|error| error.to_string())?;
    // TODO(UT-085): services absent from this finite project are intentionally not removed.
    let operations = compose
        .operations()
        .into_iter()
        .cloned()
        .collect::<Vec<_>>();
    if operations.is_empty() {
        println!("No changes.");
        return Ok(None);
    }
    // TODO(UT-086): this is a best-effort preview over one observer-relative snapshot.
    confirm_and_execute(client, &operations, auto_confirm).await
}

fn scale_plan(
    snapshot: &DeploySnapshot,
    selector: &str,
    replicas: NonZeroU32,
) -> Result<Option<DeployPlan>, Error> {
    let services = ployz_core::derive_services(snapshot.containers.iter().cloned());
    let service = select_service(&services, selector).map_err(|error| error.to_string())?;
    let observed_container = service
        .containers
        .first()
        .ok_or_else(|| Error::from("cannot scale a service without regular containers"))?;
    match observed_container.resolved_spec.mode {
        ServiceMode::Replicated { .. } => {}
        ServiceMode::Global => return Err("global services cannot be scaled".into()),
    }
    if usize::try_from(replicas.get()) == Ok(service.containers.len()) {
        return Ok(None);
    }
    // TODO(UT-046): mixed historical specs use one observed regular container; there is no chooser.
    let mut requested = requested_from_resolved(&observed_container.resolved_spec);
    requested.mode = ServiceMode::Replicated { replicas };
    plan_deploy(
        &requested,
        snapshot,
        service.service_id.clone(),
        plan_options(false, false),
    )
    .map(Some)
    .map_err(|error| Error::from(error.to_string()))
}

async fn scale_connected(
    client: &mut Client,
    selector: &str,
    replicas: NonZeroU32,
    auto_confirm: bool,
) -> Result<Option<DeployOutcome<ExecutionError>>, Error> {
    let machines = list_machines(client).await?;
    let snapshot = take_snapshot(client, machines).await?;
    let Some(plan) = scale_plan(&snapshot, selector, replicas)? else {
        println!("No changes.");
        return Ok(None);
    };
    confirm_and_execute(client, plan.operations(), auto_confirm).await
}

async fn confirm_and_execute(
    client: &mut Client,
    operations: &[DeployOperation],
    auto_confirm: bool,
) -> Result<Option<DeployOutcome<ExecutionError>>, Error> {
    render(operations, client.connection());
    if !auto_confirm && !confirm()? {
        println!("Cancelled. No changes were made.");
        return Ok(None);
    }
    Ok(Some(
        execute_operations(operations, client, &CancellationToken::new()).await,
    ))
}

fn render(operations: &[DeployOperation], connection: &crate::context::Connection) {
    println!("Plan for {connection}:");
    for operation in operations {
        println!("  {}", operation_summary(operation));
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
        DeployOperation::Sequence { operations } => format!("{} operations", operations.len()),
    }
}

fn finish(outcome: DeployOutcome<ExecutionError>) -> Result<(), Error> {
    println!("Completed {} operation(s).", outcome.completed.len());
    let Some(failed) = &outcome.failed else {
        return Ok(());
    };
    Err(format!(
        "Deploy stopped; completed: [{}]; failed: {}; unexecuted: [{}]",
        operation_list(&outcome.completed),
        failed_summary(failed),
        operation_list(&outcome.unexecuted),
    )
    .into())
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

async fn expand_ingress<'a>(
    client: &mut Client,
    specs: impl IntoIterator<Item = &'a mut RequestedServiceSpec>,
) -> Result<(), Error> {
    let specs: Vec<_> = specs.into_iter().collect();
    if !specs.iter().any(|spec| needs_ingress_expansion(spec)) {
        return Ok(());
    }
    let domain = client
        .domain_if_reserved()
        .await
        .map_err(|error| Error::from(error.to_string()))?;
    for spec in specs {
        crate::dns::expand_ingress_ports(spec, domain.as_deref())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn needs_ingress_expansion(requested: &RequestedServiceSpec) -> bool {
    requested
        .ports
        .iter()
        .any(|port| matches!(port, PortPublication::Ingress { .. }))
}

fn plan_options(force_recreate: bool, skip_health_monitor: bool) -> crate::deploy::PlanOptions {
    PlanOptions {
        force_recreate,
        skip_health_monitor,
        placement_seed: SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos() as u64),
    }
}

#[path = "deploy_input.rs"]
mod input;
use input::{parse_u32, requested_from_resolved, run_spec};

#[cfg(test)]
#[path = "deploy_tests.rs"]
mod tests;
