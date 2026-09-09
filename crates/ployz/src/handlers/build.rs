use std::collections::BTreeSet;

use clap::ArgMatches;

use ployz_build::Output;
use ployz_core::MachineId;

use crate::build_location::{self, Candidate, Evidence, Location, Selection};

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
    let location = Location::requested(
        leaf.get_one::<String>("remote").map(String::as_str),
        leaf.get_flag("local"),
    )
    .map_err(|error| Error::usage(error.to_string()))?;
    if matches!(location, Location::Remote(_)) && leaf.get_flag("push") && !leaf.get_flag("check") {
        return Err(Error::usage(
            "--push is not supported yet for a Build on a Machine; that Build leaves its image on the selected Machine, --push-registry publishes explicitly, and --local builds here",
        ));
    }
    let plan = plan_build(&project, &options)?;
    if plan.is_empty() {
        println!("No buildable services selected.");
        return Ok(());
    }
    if let Location::Remote(selection) = location {
        let context = project
            .selected_context(
                leaf.get_one::<String>("context").map(String::as_str),
                matches.get_one::<String>("connect").map(String::as_str),
            )
            .map(str::to_owned);
        // Capture first: the platforms it fixes decide which Machines can run it.
        let captured = capture_build(&plan, &options, &mut project)?;
        let required = captured.platforms();
        return runtime()?.block_on(async {
            let cancellation = super::cancellation_on_ctrl_c();
            let mut client = connect_client(matches, context.as_deref()).await?;
            let machine = resolve_build_machine(&mut client, &selection, &required).await?;
            let outcome = captured
                .execute_remote(&client, machine, cancellation.clone(), progress)
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

/// One resolved Build Machine, with the evidence this client could not confirm.
struct Resolved {
    name: ployz_core::MachineName,
    id: MachineId,
    /// Required platforms this client could not confirm run natively.
    unconfirmed: Vec<String>,
    /// Visible Machines whose Build capability could not be observed.
    unresolved: Vec<String>,
}

impl Resolved {
    /// Name the Machine before any source leaves this client, then everything
    /// the selection rests on but could not verify.
    fn report(&self) {
        let Self {
            name,
            id,
            unconfirmed,
            unresolved,
        } = self;
        println!("Selected Build Machine {name} ({id})");
        eprintln!("Build Machine: {id}");
        if !unconfirmed.is_empty() {
            // Neither emulation support nor an unreported architecture is
            // observable here, so claim only what was actually established.
            eprintln!(
                "Build Machine {name} is not confirmed to run {} natively; the Machine admits or refuses the Build itself",
                unconfirmed.join(", ")
            );
        }
        if !unresolved.is_empty() {
            // A silent Machine may have been the better candidate. Say so
            // rather than let the choice look better evidenced than it is.
            eprintln!(
                "Build capability was not observed for {}",
                unresolved.join("; ")
            );
        }
    }
}

/// Resolve the Machine this Build runs on and report it before any upload.
///
/// # Errors
///
/// Fails when a named Machine is not visible, ambiguous, or cannot build, and
/// when no observed Machine can run an automatic Build.
pub(super) async fn resolve_build_machine(
    client: &mut crate::connect::Client,
    selection: &Selection,
    required: &BTreeSet<String>,
) -> Result<MachineId, Error> {
    let resolved = match selection {
        Selection::Pinned(target) => {
            let machine = client.build_machine(target).await?;
            let contract = describe(client, machine.id).await?;
            if !contract.supports(ployz_core::BUILD_CAPABILITY) {
                return Err(Error::usage(format!(
                    "Machine {} ({}) does not support remote Builds; name another with --remote=<Machine>, or build here with --local",
                    machine.name, machine.id
                )));
            }
            Resolved {
                unconfirmed: build_location::unconfirmed(&machine.runtime.architecture, required),
                name: machine.name,
                id: machine.id,
                unresolved: Vec::new(),
            }
        }
        Selection::Automatic => {
            let candidates = observe_build_candidates(client).await?;
            let choice = build_location::choose(&candidates, required).map_err(no_machine)?;
            Resolved {
                name: choice.machine.name.clone(),
                id: choice.machine.id,
                unconfirmed: choice.unconfirmed,
                unresolved: choice.unresolved,
            }
        }
    };
    resolved.report();
    Ok(resolved.id)
}

/// The command that fixes an automatic selection with nothing to select.
fn no_machine(error: build_location::NoBuildMachine) -> Error {
    let action = match error {
        build_location::NoBuildMachine::Invisible => {
            "add one with `ployz machine add`, or build here with --local"
        }
        build_location::NoBuildMachine::Inconclusive { .. } => {
            "wait for those Machines and retry, name one with --remote=<Machine>, or build here with --local"
        }
        build_location::NoBuildMachine::Incapable { .. } => {
            "upgrade a Machine, name one with --remote=<Machine>, or build here with --local"
        }
    };
    Error::usage(format!("{error}; {action}"))
}

/// Ask every visible Machine what it can build, keeping unanswered evidence.
///
// ponytail: one concurrent probe per visible Machine, matching the Cluster
// fan-out in `cluster.rs`. Bound the concurrency if a Cluster ever grows past
// the handful of Machines the product targets; sequential probing would cost
// the 5s describe timeout per Machine on every automatic Build.
async fn observe_build_candidates(
    client: &mut crate::connect::Client,
) -> Result<Vec<Candidate>, Error> {
    let mut candidates = Vec::new();
    let mut probes = Vec::new();
    for observation in client.machines().await? {
        let machine = observation.machine;
        if !observation.membership.invites_rpc() {
            candidates.push(Candidate {
                id: machine.id,
                name: machine.name,
                evidence: Evidence::Ineligible(format!(
                    "has membership {} and does not invite an RPC",
                    observation.membership.as_str()
                )),
            });
            continue;
        }
        let probe = client.clone();
        probes.push(async move {
            let evidence = match describe(&probe, machine.id).await {
                Ok(contract) if contract.supports(ployz_core::BUILD_CAPABILITY) => {
                    Evidence::Builds {
                        architecture: machine.runtime.architecture,
                    }
                }
                Ok(_) => Evidence::Refuses,
                Err(error) => Evidence::Unanswered(error.to_string()),
            };
            Candidate {
                id: machine.id,
                name: machine.name,
                evidence,
            }
        });
    }
    candidates.extend(futures_util::future::join_all(probes).await);
    Ok(candidates)
}

/// Read one Machine's advertised contract, refusing an answer from another Machine.
async fn describe(
    client: &crate::connect::Client,
    machine_id: MachineId,
) -> Result<ployz_core::ContractDescription, Error> {
    let contract = client
        .invoke::<ployz_core::op::DescribeContract>(
            ployz_core::DescribeContractRequest {},
            &ployz_core::MachineTarget::from(&machine_id),
            Some(std::time::Duration::from_secs(5)),
        )
        .await?;
    if contract.machine_id != machine_id {
        return Err(Error::usage(format!(
            "Machine {machine_id} was answered by Machine {}",
            contract.machine_id
        )));
    }
    Ok(contract)
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
