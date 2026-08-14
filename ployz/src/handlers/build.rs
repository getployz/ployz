use clap::ArgMatches;

use crate::compose::{BuildOptions, LoadOptions, execute_build, load_project, plan_build};

use super::{Error, image, leaf_matches, string_values};

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
        check: leaf.get_flag("check"),
        deps: leaf.get_flag("deps"),
        no_cache: leaf.get_flag("no-cache"),
        pull: leaf.get_flag("pull"),
        push_registry: leaf.get_flag("push-registry"),
        services: string_values(leaf, "service"),
    };
    let project = load_project(&load).map_err(|error| error.to_string())?;
    for warning in &project.warnings {
        eprintln!("WARNING: {warning}");
    }
    let plan = plan_build(&project, &options).map_err(|error| error.to_string())?;
    if plan.services.is_empty() {
        println!("No buildable services selected.");
        return Ok(());
    }
    execute_build(&plan, &options, &load).map_err(|error| error.to_string())?;
    if !leaf.get_flag("push") {
        return Ok(());
    }

    let explicit = string_values(leaf, "machine");
    let context = project.selected_context(
        leaf.get_one::<String>("context").map(String::as_str),
        matches.get_one::<String>("connect").map(String::as_str),
    );
    let runtime = image::runtime()?;
    let failures = runtime.block_on(async {
        let mut client = image::connect_client(matches, context).await?;
        let mut failures = Vec::new();
        for service in &plan.services {
            let targets = push_targets(&explicit, &service.machines);
            match crate::image::push(&mut client, &service.image, None, &targets).await {
                Ok(result) => {
                    for success in result.successes {
                        println!("Pushed {} to {}", service.image, success.machine_id);
                    }
                    for failure in result.failures {
                        failures.push(format!(
                            "{} on {}: {}",
                            service.image, failure.machine_id, failure.error
                        ));
                    }
                    for omission in result.omissions {
                        failures.push(format!(
                            "{} on {}: no terminal response",
                            service.image, omission
                        ));
                    }
                }
                Err(error) => failures.push(format!("{}: {error}", service.image)),
            }
        }
        Ok::<_, Error>(failures)
    })?;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

fn push_targets(explicit: &[String], configured: &[ployz_core::MachineSelector]) -> Vec<String> {
    if explicit.is_empty() {
        configured.iter().map(ToString::to_string).collect()
    } else {
        explicit.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::MachineSelector;

    use super::*;

    #[test]
    fn explicit_push_targets_override_service_targets_and_empty_means_all() {
        let configured = [MachineSelector::parse("service-machine").unwrap()];
        assert_eq!(
            push_targets(&["explicit-machine".into()], &configured),
            ["explicit-machine"]
        );
        assert_eq!(push_targets(&[], &configured), ["service-machine"]);
        assert!(push_targets(&[], &[]).is_empty());
    }
}
