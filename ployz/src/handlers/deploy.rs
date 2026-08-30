use std::num::NonZeroU32;

use clap::ArgMatches;
use ployz_core::{ComposePruneRefusal, ServiceSelector};

use crate::{
    compose::{
        BuildOptions, BuildService, ComposeError, ComposeProject, LoadOptions, compose_identity,
        execute_build, has_explicit_nondefault_compose_file, load_project, plan_build,
    },
    deploy::{
        ReconciliationHints, ServiceAttempt, deploy_project, deploy_scale, deploy_spec,
        plan_options,
    },
    project::{ResolvedProject, resolve_compose_command, resolve_explicit, resolve_run_command},
};

use super::{Error, connect_client, leaf_matches, required, runtime, string_values};

pub(super) fn run(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let requested = run_spec(matches)?;
    let project = resolve_run_command(matches)?;
    let context = matches.get_one::<String>("context").map(String::as_str);
    let force_recreate = matches.get_flag("recreate");
    let skip_health_monitor = matches.get_flag("skip-health");
    runtime()?.block_on(async {
        let mut client = connect_client(root, context).await?;
        deploy_spec(
            &mut client,
            &requested,
            force_recreate,
            skip_health_monitor,
            &project.name,
            context.unwrap_or("default"),
            Some(&project),
        )
        .await
    })
}

pub(super) fn deploy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let load = deploy_load(matches);
    let resolved = resolve_from_compose_load(matches, &load)?;
    let (mut project, builds, selected) = prepare_deploy(matches, &load)?;
    let context = project
        .selected_context(
            matches.get_one::<String>("context").map(String::as_str),
            matches.get_one::<String>("connect").map(String::as_str),
        )
        .map(str::to_owned);
    let yes = matches.get_flag("yes");
    let force_recreate = matches.get_flag("recreate");
    let skip_health_monitor = matches.get_flag("skip-health");
    let mut options = plan_options(force_recreate, skip_health_monitor);
    options.selected = selected;
    let hints = reconciliation_hints(&load, &resolved);
    runtime()?.block_on(async {
        let mut client = connect_client(root, context.as_deref()).await?;
        deploy_project(
            &mut client,
            &mut project,
            &builds,
            options,
            hints,
            crate::deploy::ConfirmGate {
                auto_confirm: yes,
                context: context.as_deref().unwrap_or("default"),
                project: &resolved,
            },
        )
        .await
    })
}

fn deploy_load(matches: &ArgMatches) -> LoadOptions {
    LoadOptions {
        command: "deploy".into(),
        files: string_values(matches, "file")
            .into_iter()
            .map(Into::into)
            .collect(),
        profiles: string_values(matches, "profile"),
        all_profiles: true,
        ..Default::default()
    }
}

fn reconciliation_hints(load: &LoadOptions, resolved: &ResolvedProject) -> ReconciliationHints {
    ReconciliationHints {
        requested_profiles: load.profiles.clone(),
        compose_refusal: compose_prune_refusal(load, resolved),
    }
}

fn compose_prune_refusal(
    load: &LoadOptions,
    resolved: &ResolvedProject,
) -> Option<ComposePruneRefusal> {
    if !load.all_profiles {
        Some(ComposePruneRefusal::FilteredProfiles)
    } else if resolved.source.is_directory_guess() && has_explicit_nondefault_compose_file(load) {
        Some(ComposePruneRefusal::GuessedProjectName)
    } else {
        None
    }
}

fn resolve_from_compose_load(
    matches: &ArgMatches,
    load: &LoadOptions,
) -> Result<ResolvedProject, Error> {
    match resolve_explicit(matches)? {
        Some(resolved) => Ok(resolved),
        None => {
            let identity = compose_identity(load);
            Ok(resolve_compose_command(
                matches,
                identity.name.as_deref(),
                identity.directory.as_deref(),
            )?)
        }
    }
}

fn prepare_deploy(
    matches: &ArgMatches,
    load: &LoadOptions,
) -> Result<(ComposeProject, Vec<BuildService>, Vec<ServiceAttempt>), Error> {
    let selected = string_values(matches, "service");
    let project = load_project(load)?;
    for warning in &project.warnings {
        eprintln!("WARNING: {warning}");
    }
    let build_names = if selected.is_empty() {
        project.enabled_service_names(&load.profiles)
    } else {
        selected.clone()
    };
    let build_options = BuildOptions {
        build_args: string_values(matches, "build-arg"),
        deps: true,
        no_cache: matches.get_flag("no-cache"),
        pull: matches.get_flag("build-pull"),
        services: build_names,
        ..Default::default()
    };
    let mut builds = plan_build(&project, &build_options)?;
    if matches.get_flag("no-build") {
        builds.clear();
    } else {
        execute_build(&builds, &build_options, load)?;
    }
    let selected = selected_attempts(&project, &selected)?;
    Ok((project, builds, selected))
}

fn selected_attempts(
    project: &ComposeProject,
    selected: &[String],
) -> Result<Vec<ServiceAttempt>, Error> {
    selected
        .iter()
        .map(|name| {
            project
                .services
                .get(name)
                .map(|spec| ServiceAttempt {
                    name: spec.name.clone(),
                })
                .ok_or_else(|| ComposeError::Invalid(format!("undefined service '{name}'")).into())
        })
        .collect()
}

pub(super) fn scale(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let replicas = parse_u32(matches, "replicas")?;
    // TODO: reject zero before resolving configuration or connecting to a Machine.
    let replicas = NonZeroU32::new(replicas)
        .ok_or_else(|| Error::usage("replicas must be greater than zero"))?;
    let selector = ServiceSelector::parse(required(matches, "service")?)?;
    let yes = matches.get_flag("yes");
    let skip_health_monitor = matches.get_flag("skip-health");
    let load = LoadOptions {
        command: "scale".into(),
        ..Default::default()
    };
    let project = resolve_from_compose_load(matches, &load)?;
    let context = matches.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = connect_client(root, context).await?;
        deploy_scale(
            &mut client,
            &selector,
            replicas,
            skip_health_monitor,
            crate::deploy::ConfirmGate {
                auto_confirm: yes,
                context: context.unwrap_or("default"),
                project: &project,
            },
        )
        .await
    })
}

#[path = "deploy_input.rs"]
mod input;
use input::{parse_u32, run_spec};

#[cfg(test)]
#[path = "deploy_tests.rs"]
mod tests;
