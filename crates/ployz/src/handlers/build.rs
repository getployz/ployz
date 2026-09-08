use clap::ArgMatches;

use ployz_build::Output;

use crate::compose::{BuildOptions, LoadOptions, execute_build, load_project, plan_build};

use super::{Error, connect_client, leaf_matches, runtime, string_values};

pub(super) fn run(matches: &ArgMatches) -> Result<(), Error> {
    let leaf = leaf_matches(matches);
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
    let built = execute_build(&plan, &options, &load, &mut project)?;
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
                    service.built.platform,
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
        let mut client = connect_client(matches, context).await?;
        let mut failures = Vec::new();
        for service in &built {
            let targets = push_targets(&explicit, &service.machines);
            match crate::image::push(&mut client, service.content(), None, &targets).await {
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
