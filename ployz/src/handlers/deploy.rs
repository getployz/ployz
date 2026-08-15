use std::num::NonZeroU32;

use clap::ArgMatches;
#[cfg(test)]
use ployz_core::ServiceId;
use ployz_core::{RequestedServiceSpec, ServiceMode, select_service};
use tokio_util::sync::CancellationToken;

use crate::{
    compose::{
        BuildOptions, BuildService, ComposeProject, LoadOptions, execute_build, load_project,
        plan_build, plan_compose_deploy,
    },
    connect::Client,
    deploy::{
        DeployOperation, DeployOutcome, DeployPlan, DeploySnapshot, ExecutionError, PlanOptions,
        apply::{expand_ingress, finish, list_machines, plan_options, render, take_snapshot},
        apply_requested, execute_operations, plan_deploy,
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
        apply_requested(&mut client, &requested).await
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
    let project = load_project(&load)?;
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
    let mut builds = plan_build(&project, &build_options)?;
    if matches.get_flag("no-build") {
        builds.clear();
    } else {
        execute_build(&builds, &build_options, &load)?;
    }
    let project = project.select_services(&selected)?;
    Ok((project, builds))
}

pub(super) fn scale(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let replicas = parse_u32(matches, "replicas")?;
    // TODO(EO-012): reject zero before resolving configuration or connecting to a Machine.
    let replicas = NonZeroU32::new(replicas)
        .ok_or_else(|| Error::usage("replicas must be greater than zero"))?;
    let selector = required(matches, "service")?;
    let yes = matches.get_flag("yes");
    let context = matches.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = connect_client(root, context).await?;
        let outcome = scale_connected(&mut client, &selector, replicas, yes).await?;
        outcome.map_or(Ok(()), finish)
    })
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
            .map_err(|error| Error::usage(format!("{}: {error}", service.image)))?;
    let failures = report_push(&service.image, result);
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::usage(failures.join("; ")))
    }
}

pub(super) async fn deploy_requested(
    client: &mut Client,
    requested: &RequestedServiceSpec,
) -> Result<(), Error> {
    apply_requested(client, requested).await
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
        return Err(Error::usage(format!(
            "image push failed: {}",
            failures.join("; ")
        )));
    }
    project.resolve_secrets()?;
    let snapshot = take_snapshot(client, machines).await?;
    expand_ingress(client, project.services.values_mut()).await?;
    let compose = plan_compose_deploy(project, &snapshot, options)?;
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
    let service = select_service(&services, selector)?;
    let observed_container = service
        .containers
        .first()
        .ok_or_else(|| Error::usage("cannot scale a service without regular containers"))?;
    match observed_container.resolved_spec.mode {
        ServiceMode::Replicated { .. } => {}
        ServiceMode::Global => return Err(Error::usage("global services cannot be scaled")),
    }
    if usize::try_from(replicas.get()) == Ok(service.containers.len()) {
        return Ok(None);
    }
    // TODO(UT-046): mixed historical specs use one observed regular container; there is no chooser.
    let mut requested = requested_from_resolved(&observed_container.resolved_spec);
    requested.mode = ServiceMode::Replicated { replicas };
    Ok(Some(plan_deploy(
        &requested,
        snapshot,
        service.service_id.clone(),
        plan_options(false, false),
    )?))
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

#[path = "deploy_input.rs"]
mod input;
use input::{parse_u32, requested_from_resolved, run_spec};

#[cfg(test)]
#[path = "deploy_tests.rs"]
mod tests;
