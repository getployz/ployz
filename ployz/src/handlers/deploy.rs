use std::num::NonZeroU32;

use clap::ArgMatches;
use ployz_core::ServiceSelector;

use crate::{
    compose::{
        BuildOptions, BuildService, ComposeError, ComposeProject, LoadOptions, compose_identity,
        execute_build, load_project, plan_build,
    },
    deploy::{ServiceAttempt, deploy_project, deploy_scale, deploy_spec, plan_options},
    project::{
        ProjectNameInput, ResolvedProject, resolve_compose_command, resolve_explicit,
        resolve_from_matches,
    },
};

use super::{Error, connect_client, leaf_matches, required, runtime, string_values};

pub(super) fn run(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let requested = run_spec(matches)?;
    let project = resolve_from_matches(
        matches,
        ProjectNameInput {
            implicit_default: true,
            ..ProjectNameInput::default()
        },
    )?;
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
            Some(&project),
        )
        .await
    })
}

pub(super) fn deploy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let load = deploy_load(matches);
    let resolved = resolve_from_compose_load(matches, &load)?;
    let (mut project, builds, apply) = prepare_deploy(matches, &load)?;
    let context = project
        .selected_context(
            matches.get_one::<String>("context").map(String::as_str),
            matches.get_one::<String>("connect").map(String::as_str),
        )
        .map(str::to_owned);
    let yes = matches.get_flag("yes");
    let force_recreate = matches.get_flag("recreate");
    let skip_health_monitor = matches.get_flag("skip-health");
    runtime()?.block_on(async {
        let mut client = connect_client(root, context.as_deref()).await?;
        deploy_project(
            &mut client,
            &mut project,
            &builds,
            apply,
            plan_options(force_recreate, skip_health_monitor),
            yes,
            &resolved,
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
        ..Default::default()
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
        execute_build(&builds, &build_options, load)?;
    }
    let apply = apply_attempts(&project, &selected)?;
    Ok((project, builds, apply))
}

fn apply_attempts(
    project: &ComposeProject,
    selected: &[String],
) -> Result<Vec<ServiceAttempt>, Error> {
    if selected.is_empty() {
        return Ok(project
            .services
            .values()
            .map(|spec| ServiceAttempt {
                name: spec.name.clone(),
            })
            .collect());
    }
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
    // TODO(EO-012): reject zero before resolving configuration or connecting to a Machine.
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
            yes,
            &project,
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
