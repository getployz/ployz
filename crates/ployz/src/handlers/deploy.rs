use std::num::NonZeroU32;

use clap::ArgMatches;
use ployz_core::{ComposePruneRefusal, ServiceSelector};

use crate::{
    compose::{
        BuildOptions, BuiltService, CapturedCompose, ComposeError, ComposeProject, LoadOptions,
        capture_build, compose_identity, has_explicit_nondefault_compose_file, load_project,
        plan_build,
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
    let project = load_project(&load)?;
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
    options.selected = selected_attempts(&project, &string_values(matches, "service"))?;
    let cancellation = crate::cancellation::listen()?;
    let (candidate, builds) =
        prepare_deploy(matches, &load, project, &resolved, options, &cancellation)?;
    runtime()?.block_on(async {
        let mut client =
            crate::cancellation::read(&cancellation, connect_client(root, context.as_deref()))
                .await?;
        deploy_project(
            &mut client,
            &candidate,
            &builds,
            &cancellation,
            crate::deploy::ConfirmGate {
                auto_confirm: yes,
                context: context.as_deref().unwrap_or("default"),
                project: &resolved,
            },
        )
        .await
    })
}

/// Render the captured Compose candidate against fresh, read-only Cluster evidence.
///
/// # Errors
/// Propagates source loading, selection, connection, comparison, and output encoding failures.
pub(super) fn changes(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let load = deploy_load(matches);
    let resolved = resolve_from_compose_load(matches, &load)?;
    let project = load_project(&load)?;
    let context = project
        .selected_context(
            matches.get_one::<String>("context").map(String::as_str),
            matches.get_one::<String>("connect").map(String::as_str),
        )
        .map(str::to_owned);
    for warning in &project.warnings {
        eprintln!("WARNING: {warning}");
    }
    let options = ployz_core::PlanOptions {
        selected: selected_attempts(&project, &string_values(matches, "service"))?,
        ..Default::default()
    };
    let hints = reconciliation_hints(&load, &resolved);
    let candidate = project.capture(
        resolved.name,
        options,
        hints.requested_profiles,
        hints.compose_refusal,
        load.files,
    );
    let review = runtime()?.block_on(async {
        let mut client = connect_client(root, context.as_deref()).await?;
        Ok::<_, Error>(client.changes(&candidate).await?)
    })?;
    if matches.get_one::<String>("output").map(String::as_str) == Some("json") {
        println!("{}", serde_json::to_string_pretty(&review)?);
    } else {
        println!(
            "Project {} · candidate {} · observer {}",
            review.project_name, review.candidate_id, review.observer_machine_id
        );
        println!(
            "Review coverage: service settings, attachments, and redacted live environment. Attachment contents and credentials are redacted. Image tags are not content evidence. Live Observation is observer-relative."
        );
        if review.selection.is_empty() {
            println!("Selection: Services enabled by the requested profiles");
        } else {
            println!(
                "Selection: {} (including dependencies)",
                review
                    .selection
                    .iter()
                    .map(|selected| selected.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        for service in review.services {
            println!(
                "{} command: {}",
                service.name,
                serde_json::to_string(&service.command)?
            );
            if service.observations.is_empty() && service.missing_on.is_empty() {
                println!("  No Container observation available");
            }
            for observed in service.observations {
                print!(
                    "  Machine {} Container {}: ",
                    observed.machine_id, observed.container_id
                );
                if observed.changes.is_empty() {
                    println!(
                        "{} (compared settings unchanged)",
                        serde_json::to_string(&observed.command)?
                    );
                } else {
                    for change in observed.changes {
                        let ployz_core::SettingChange {
                            setting,
                            before,
                            after,
                        } = change;
                        println!(
                            "{setting}: {} -> {}",
                            serde_json::to_string(&before)?,
                            serde_json::to_string(&after)?
                        );
                    }
                }
                if let Some(failure) = observed.environment_failure {
                    println!("    environment observation failed ({})", failure);
                }
                for row in observed.environment {
                    println!(
                        "    environment {}: {}",
                        row.key,
                        serde_json::to_string(&row.evidence)?
                    );
                }
            }
            for machine in service.missing_on {
                println!("  Machine {machine}: no observed Container");
            }
        }
        for service in review.would_remove {
            if let Some(reason) = review.prune_refusal {
                println!("{}: preserved. {reason}", service.name);
            } else {
                println!(
                    "{}: remove observed Service (absent from captured target)",
                    service.name
                );
            }
        }
        for failure in review.failures {
            println!(
                "Machine {}: observation failed ({})",
                failure.machine_id, failure.error
            );
        }
        for machine in review.omissions {
            println!("Machine {machine}: observation omitted");
        }
    }
    Ok(())
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
    mut project: ComposeProject,
    resolved: &ResolvedProject,
    options: ployz_core::PlanOptions,
    cancellation: &tokio_util::sync::CancellationToken,
) -> Result<(CapturedCompose, Vec<BuiltService>), Error> {
    let selected = string_values(matches, "service");
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
        // A Deploy needs the image on this host, so it always loads it.
        output: ployz_build::Output::Load,
        pull: matches.get_flag("build-pull"),
        services: build_names,
    };
    let builds = plan_build(&project, &build_options)?;
    let captured_build = if matches.get_flag("no-build") {
        None
    } else {
        Some(capture_build(&builds, &build_options, &mut project)?)
    };
    project.resolve_secrets()?;
    let hints = reconciliation_hints(load, resolved);
    let candidate = project.capture(
        resolved.name.clone(),
        options,
        hints.requested_profiles,
        hints.compose_refusal,
        load.files.clone(),
    );
    // Every required Build finishes before any application change begins.
    let built = match captured_build {
        Some(build) => build
            .execute(load.docker.as_deref(), cancellation)
            .map_err(crate::deploy::DeployError::from)?,
        None => Vec::new(),
    };
    Ok((candidate, built))
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
