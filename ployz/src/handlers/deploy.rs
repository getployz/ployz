use std::num::NonZeroU32;

use clap::ArgMatches;

use crate::{
    compose::{
        BuildOptions, BuildService, ComposeProject, LoadOptions, execute_build, load_project,
        plan_build,
    },
    deploy::{deploy_project, deploy_scale, deploy_spec},
};

use super::{Error, connect_client, leaf_matches, required, runtime, string_values};

pub(super) use crate::deploy::apply_requested as deploy_requested;

pub(super) fn run(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let requested = run_spec(matches)?;
    let context = matches.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let mut client = connect_client(root, context).await?;
        deploy_spec(&mut client, &requested).await
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
    let force_recreate = matches.get_flag("recreate");
    let skip_health_monitor = matches.get_flag("skip-health");
    runtime()?.block_on(async {
        let mut client = connect_client(root, context.as_deref()).await?;
        deploy_project(
            &mut client,
            &mut project,
            &builds,
            force_recreate,
            skip_health_monitor,
            yes,
        )
        .await
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
        deploy_scale(&mut client, &selector, replicas, yes).await
    })
}

#[path = "deploy_input.rs"]
mod input;
use input::{parse_u32, run_spec};

#[cfg(test)]
#[path = "deploy_tests.rs"]
mod tests;
