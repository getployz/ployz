use clap::ArgMatches;

use ployz_build::Output;

use crate::compose::{
    BuildOptions, LoadOptions, capture_build, execute_build, load_project, plan_build,
};

use super::{Error, connect_client, leaf_matches, runtime, string_values};

pub(super) fn clear_cache(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
    if leaf.get_one::<String>("connect").is_some() || leaf.get_one::<String>("context").is_some() {
        return Err(Error::usage(
            "cache clearing runs on this execution host; run it there as the builder user without --connect or --context",
        ));
    }
    ployz_build::clear_cache(&ployz_build::HostPolicy::default())
        .map_err(|error| Error::usage(error.to_string()))?;
    println!("Cleared this host user's Ployz build cache.");
    Ok(())
}

pub(super) fn run(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
    let remote = leaf.get_one::<String>("remote");
    if remote.is_some_and(String::is_empty) {
        return Err(Error::usage(
            "select a Build Machine with --remote=<Machine>; automatic selection is not available",
        ));
    }
    if remote.is_some() && leaf.get_flag("push") && !leaf.get_flag("check") {
        return Err(Error::usage(
            "remote build --push is not supported yet; remote build leaves the image on its selected Machine, or --push-registry publishes explicitly",
        ));
    }
    let load = LoadOptions {
        command: "build".into(),
        files: string_values(leaf, "file")
            .into_iter()
            .map(Into::into)
            .collect(),
        profiles: string_values(leaf, "profile"),
        ..Default::default()
    };
    let options = BuildOptions {
        build_args: string_values(leaf, "build-arg"),
        deps: leaf.get_flag("deps"),
        no_cache: leaf.get_flag("no-cache"),
        output: requested_output(leaf),
        pull: leaf.get_flag("pull"),
        services: string_values(leaf, "service"),
    };
    let mut project = load_project(&load)?;
    for warning in &project.warnings {
        eprintln!("WARNING: {warning}");
    }
    let plan = plan_build(&project, &options)?;
    if plan.is_empty() {
        println!("No buildable services selected.");
        return Ok(());
    }
    if let Some(target) = remote {
        let target = ployz_core::MachineTarget::parse(target)?;
        let context = project
            .selected_context(
                leaf.get_one::<String>("context").map(String::as_str),
                matches.get_one::<String>("connect").map(String::as_str),
            )
            .map(str::to_owned);
        return runtime()?.block_on(async {
            let cancellation = super::cancellation_on_ctrl_c();
            let mut client = connect_client(matches, context.as_deref()).await?;
            let machine = select_build_machine(&mut client, &target).await?;
            let captured = capture_build(&plan, &options, &mut project)?;
            let outcome = captured
                .execute_remote(&client, machine.id, cancellation.clone(), progress)
                .await;
            cancellation.cancel();
            report_remote(outcome)
        });
    }
    let cancellation = crate::cancellation::listen()?;
    let built = execute_build(&plan, &options, &load, &mut project, &cancellation)?;
    match options.output {
        Output::Validate => {
            println!("Validated the selected builds. No image was produced.");
            return Ok(());
        }
        Output::Registry => {
            println!("Published the built images to their registries.");
            return Ok(());
        }
        Output::Load => {
            for service in &built {
                println!(
                    "Built {} ({}) as {} in local Docker",
                    service.built.tags.join(", "),
                    service.built.platforms.join(", "),
                    service.built.reference,
                );
            }
        }
    }
    if !leaf.get_flag("push") {
        return Ok(());
    }

    let explicit = string_values(leaf, "machine");
    let context = project.selected_context(
        leaf.get_one::<String>("context").map(String::as_str),
        matches.get_one::<String>("connect").map(String::as_str),
    );
    let runtime = runtime()?;
    let failures = runtime.block_on(async {
        let mut client =
            crate::cancellation::read(&cancellation, connect_client(matches, context)).await?;
        let mut failures = Vec::new();
        for service in &built {
            let targets = push_targets(&explicit, &service.machines);
            match crate::image::push(
                &mut client,
                service.content(),
                None,
                &targets,
                &cancellation,
            )
            .await
            {
                Ok(result) => failures.extend(report_push(&service.image, result)),
                Err(error) => failures.push(push_failure(&service.image, error)?),
            }
        }
        Ok::<_, Error>(failures)
    })?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(Error::usage(failures.join("; ")))
    }
}

pub(super) async fn select_build_machine(
    client: &mut crate::connect::Client,
    target: &ployz_core::MachineTarget,
) -> Result<ployz_core::Machine, Error> {
    let machine = client.build_machine(target).await?;
    println!("Selected Build Machine {} ({})", machine.name, machine.id);
    eprintln!("Build Machine: {}", machine.id);
    let contract = client
        .invoke::<ployz_core::op::DescribeContract>(
            ployz_core::DescribeContractRequest {},
            &ployz_core::MachineTarget::from(&machine.id),
            Some(std::time::Duration::from_secs(5)),
        )
        .await?;
    if contract.machine_id != machine.id || !contract.supports(ployz_core::BUILD_CAPABILITY) {
        return Err(Error::usage(format!(
            "Machine {} does not support remote Builds",
            machine.id
        )));
    }
    Ok(machine)
}

pub(super) fn progress(event: ployz_build::Progress) {
    use std::io::Write as _;
    match event {
        ployz_build::Progress::Stage(stage) => eprintln!("Build: {stage:?}"),
        ployz_build::Progress::Target { .. } => {}
        ployz_build::Progress::Timing {
            queue_wait,
            execution,
        } => eprintln!(
            "Build queue wait: {:.2}s; execution: {:.2}s",
            queue_wait.as_secs_f64(),
            execution.as_secs_f64()
        ),
        ployz_build::Progress::Output(bytes) => {
            let _ = std::io::stderr().write_all(&bytes);
        }
    }
}

/// Validation supersedes publication: a checked recipe produces no image.
fn requested_output(leaf: &ArgMatches) -> Output {
    if leaf.get_flag("check") {
        Output::Validate
    } else if leaf.get_flag("push-registry") {
        Output::Registry
    } else {
        Output::Load
    }
}

pub(super) fn report_push(
    image: &str,
    result: ployz_core::PartialResult<(), crate::image::PushError>,
) -> Vec<String> {
    for success in result.successes {
        println!("Pushed {image} to {}", success.machine_id);
    }
    result
        .failures
        .into_iter()
        .map(|failure| format!("{image} on {}: {}", failure.machine_id, failure.error))
        .chain(
            result
                .omissions
                .into_iter()
                .map(|machine| format!("{image} on {machine}: no terminal response")),
        )
        .collect()
}

pub(super) fn push_targets(
    explicit: &[String],
    configured: &[ployz_core::MachineTarget],
) -> Vec<String> {
    if explicit.is_empty() {
        configured.iter().map(ToString::to_string).collect()
    } else {
        explicit.to_vec()
    }
}

fn push_failure(image: &str, error: crate::image::PushError) -> Result<String, Error> {
    let message = format!("{image}: {error}");
    if error.is_cancellation() {
        Err(Error::usage(message))
    } else {
        Ok(message)
    }
}

fn report_remote(outcome: ployz_build::remote::Outcome) -> Result<(), Error> {
    use ployz_build::remote::Outcome;
    match outcome {
        Outcome::Images { machine_id, images } => {
            for image in images {
                println!(
                    "Built {} ({}) as {} on Machine {machine_id}",
                    image.tags.join(", "),
                    image.platforms.join(", "),
                    image.reference
                );
            }
            Ok(())
        }
        Outcome::Validated { machine_id } => {
            println!(
                "Validated the selected builds on Machine {machine_id}. No image was produced."
            );
            Ok(())
        }
        Outcome::Published { machine_id } => {
            println!("Published the built images from Machine {machine_id} to their registries.");
            Ok(())
        }
        Outcome::Failed {
            stage,
            message,
            work,
        } => Err(Error::usage(format!(
            "Build failed during {stage:?}: {message}; target evidence: {work:?}; no deployment or implicit image transfer was attempted"
        ))),
        Outcome::Unknown {
            stage,
            message,
            work,
        } => Err(Error::usage(format!(
            "Build outcome unknown during {stage:?}: {message}; target evidence: {work:?}; no deployment or implicit image transfer was attempted"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::MachineTarget;

    use super::*;

    #[test]
    fn explicit_push_targets_override_service_targets_and_empty_means_all() {
        let configured = [MachineTarget::parse("service-machine").unwrap()];
        assert_eq!(
            push_targets(&["explicit-machine".into()], &configured),
            ["explicit-machine"]
        );
        assert_eq!(push_targets(&[], &configured), ["service-machine"]);
        assert!(push_targets(&[], &[]).is_empty());
    }

    #[test]
    fn cancellation_is_terminal_while_other_push_errors_accumulate() {
        assert!(push_failure("example.test/api", crate::image::PushError::Cancelled).is_err());
        assert_eq!(
            push_failure(
                "example.test/api",
                crate::image::PushError::ImageNotFound("example.test/api".into()),
            )
            .unwrap(),
            "example.test/api: image 'example.test/api' not found locally"
        );
    }
}
