//! `ployz deploy` and `ployz deployment`: ship an Environment from the Config Store
//! and read its Deployments. Cloud's runner runs a Deployment admitted over HTTPS,
//! and `deploy` follows it until it ends. With the hidden in-process Store this CLI
//! is the Deployment's runner: it claims it, prepares and confirms it on the
//! Cluster, and records what happened.

use std::io::Write as _;
use std::time::Duration;

use clap::{ArgAction, ArgMatches, Command, ValueHint};
use ployz_core::{RemoveVolumesRequest, ServiceName};
use ployz_store::{
    Admit, Claimed, ConfigStore, DeploymentId, DeploymentStatus, DeploymentSummary, DeploymentView,
    DeploymentsQuery, PlanQuery, RemovalsQuery, RunEvidence, RunnerId, Trusted, VolumeName,
};

use super::store::{
    Next, Store, environment, failed, mint, next, scoped, store, with_refresh_hint,
};
use super::{Error, connect_client, leaf_matches, required, runtime};
use crate::cli::{base, positional, switch, value};
use crate::cloud_account::StoreCallError;
use crate::connect::Client;
use crate::deploy::ApplyError;
use crate::failure::USAGE_EXIT;
use crate::output::say;

pub(crate) fn deploy_command() -> Command {
    scoped(base(
        "deploy",
        "Publish staged changes if needed, then deploy them and follow the Deployment",
    ))
    .arg(
        positional("service", false)
            .action(ArgAction::Append)
            .help("Deploy only these Services [default: every Service]"),
    )
    .arg(
        switch("plan", None).help("Show what would deploy, from authored state alone; run nothing"),
    )
    .arg(
        value("expect-version", None)
            .value_name("VERSION")
            .help("Refuse unless this is still the latest `ployz diff` version"),
    )
    .arg(
        value("events", None)
            .value_name("FILE")
            .value_hint(ValueHint::FilePath)
            .help("Also write progress to FILE as NDJSON"),
    )
    .arg(
        switch("detach", None)
            .conflicts_with("events")
            .help("Return the queued Deployment at once instead of following it"),
    )
    .arg(crate::cli::volume_acceptance())
}

/// How often `deploy` reads a Deployment Cloud runs while following it.
const FOLLOW_POLL: Duration = Duration::from_secs(1);

pub(crate) fn deployment_command() -> Command {
    Command::new("deployment")
        .about("Read Deployments")
        .arg_required_else_help(true)
        .subcommand(
            scoped(Command::new("ls").about("List Deployments, newest first"))
                .arg(
                    value("limit", None)
                        .value_parser(clap::value_parser!(usize))
                        .help("At most this many, 1-100 [default: 20]"),
                )
                .arg(value("cursor", None).help("The next_cursor of the previous page")),
        )
        .subcommand(
            Command::new("show")
                .about("Show a Deployment with its Deploy Preview and Node Outcomes")
                .arg(positional("id", true)),
        )
}

pub(super) fn deployment_handler(path: &str) -> Option<(super::Handler, super::Json)> {
    use super::Json::Supported;
    Some(match path {
        "ls" => (ls, Supported),
        "show" => (show, Supported),
        _ => return None,
    })
}

pub(super) fn deploy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let services = matches
        .get_many::<String>("service")
        .into_iter()
        .flatten()
        .map(|name| {
            ServiceName::parse(name.as_str()).map_err(|_| {
                Error::usage("Expected Service names: lowercase letters, digits and -")
                    .with_exit(USAGE_EXIT)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let store = store(root)?;
    if matches.get_flag("plan") {
        return plan(matches, &store, services);
    }
    // Open the events file before admitting, so a bad path deploys nothing.
    let events = matches
        .get_one::<String>("events")
        .map(std::fs::File::create)
        .transpose()?
        .map(std::io::BufWriter::new);
    let accept = matches
        .get_many::<String>("accept-volume-loss")
        .into_iter()
        .flatten()
        .map(|name| {
            VolumeName::parse(name.as_str()).map_err(|_| {
                Error::usage("Expected Volume names: lowercase letters, digits and -")
                    .with_exit(USAGE_EXIT)
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let environment = environment(matches)?;
    // The in-process Store trusts this CLI to observe the Servers; Cloud observes
    // them itself.
    let (trusted, client) = match store.local() {
        Some(_) if services.is_empty() => observe(matches, &store, &environment)?,
        Some(_) | None => (Trusted::default(), None),
    };
    let admitted = store
        .admit(
            &Admit {
                id: DeploymentId::parse(mint())?,
                environment,
                services: services.clone(),
                version: matches.get_one::<String>("expect-version").cloned(),
                accept_volume_loss: accept,
            },
            &trusted,
        )
        .map_err(|error| {
            let error = with_retry(
                with_refresh_hint(error, matches, "diff"),
                matches,
                &services,
            );
            failed(matches, &["deploy"])(error)
        })?;
    let hint = shell_words::join(["ployz", "deployment", "show", admitted.id.as_str()]);
    // Only the hidden in-process Store lets this CLI run the Deployment; Cloud's
    // runner runs it otherwise, and this command follows it unless detached.
    let (view, ran) = match store.local() {
        Some(_) if matches.get_flag("detach") => {
            return Err(Error::usage(
                "The hidden local Store runs Deployments in this process, so it can't detach",
            )
            .with_exit(USAGE_EXIT));
        }
        Some(local) => {
            let runner = RunnerId::parse(format!("cli-{}", mint()))?;
            let claimed = local.claim(&admitted.id, &runner)?;
            let ran = runtime()?.block_on(run(matches, local, &claimed, &runner, client, events));
            let view = store
                .deployment(&admitted.id)
                .map_err(failed(matches, &["deploy"]))?;
            (view, ran)
        }
        None if matches.get_flag("detach") => {
            let view = store
                .deployment(&admitted.id)
                .map_err(failed(matches, &["deploy"]))?;
            (view, Ok(()))
        }
        None => {
            let view = follow(matches, &store, &admitted, events)?;
            let ran = if view.deployment.status == DeploymentStatus::Applied {
                Ok(())
            } else {
                Err(Error::partial())
            };
            (view, ran)
        }
    };
    finish_view(&view, Some(hint))?;
    ran
}

/// Follow a Deployment Cloud's runner runs until it ends. Each change of its status
/// or Node Outcomes goes to stderr, and to `events` as NDJSON. Stopping this stops
/// following, never the Deployment.
fn follow(
    matches: &ArgMatches,
    store: &Store,
    admitted: &DeploymentSummary,
    mut events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<DeploymentView, Error> {
    eprintln!(
        "Following Deployment #{}; stopping this leaves it running.",
        admitted.number
    );
    let mut last = None;
    loop {
        let view = store
            .deployment(&admitted.id)
            .map_err(failed(matches, &["deploy"]))?;
        let progress = serde_json::json!({
            "type": "deployment",
            "status": view.deployment.status,
            "nodes": view.nodes,
        });
        if last.as_ref() != Some(&progress) {
            let nodes: Vec<String> = view
                .nodes
                .iter()
                .map(|node| format!("{} {}", node.name, json_word(&node.outcome)))
                .collect();
            eprintln!(
                "{}: {}",
                json_word(&view.deployment.status),
                nodes.join(", ")
            );
            if let Some(file) = events.as_mut() {
                // ponytail: a failed event write never stops following; the file is a tap.
                let _ = writeln!(file, "{progress}");
                let _ = file.flush();
            }
            last = Some(progress);
        }
        if !view.deployment.status.in_flight() {
            return Ok(view);
        }
        std::thread::sleep(FOLLOW_POLL);
    }
}

/// A status as its JSON word, such as `not_applied`.
fn json_word(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn plan(matches: &ArgMatches, store: &Store, services: Vec<ServiceName>) -> Result<(), Error> {
    let plan = store
        .plan(&PlanQuery {
            environment: environment(matches)?,
            services,
        })
        .map_err(failed(matches, &["deploy", "--plan"]))?;
    let mut words = vec!["deploy"];
    words.extend(
        matches
            .get_many::<String>("service")
            .into_iter()
            .flatten()
            .map(String::as_str),
    );
    words.extend(["--expect-version", plan.version.as_str()]);
    let hint = next(matches, &words);
    crate::output::finish(&Next::new(&plan, Some(hint.clone())), || {
        let where_ = format!("{}/{}", plan.environment.project, plan.environment.name);
        if plan.changes.is_empty() {
            say!("No authored changes to deploy in {where_}.");
        }
        for change in &plan.changes {
            say!("{} ({:?})", change.name, change.lifecycle);
            for row in &change.settings {
                say!("  {}: {} -> {}", row.path, row.before, row.after);
            }
        }
        say!(
            "Decided by the Servers when it runs: {}.",
            plan.unresolved.join(", ")
        );
        say!("next: {hint}");
    })
}

/// When a Deploy removes deployed Volumes, observe which Servers hold their data:
/// the evidence the in-process Store reviews it against. A Cluster it can't reach
/// leaves the evidence out, so the Store refuses. Returns the connection to reuse.
fn observe(
    matches: &ArgMatches,
    store: &Store,
    environment: &ployz_store::EnvironmentRef,
) -> Result<(Trusted, Option<Client>), Error> {
    let removals = store
        .removals(&RemovalsQuery {
            environment: environment.clone(),
        })
        .map_err(failed(matches, &["deploy"]))?;
    if removals.volumes.is_empty() {
        return Ok((Trusted::default(), None));
    }
    let sought = removals
        .volumes
        .into_iter()
        .map(|volume| volume.docker_volume)
        .collect();
    let context = matches.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let Ok(mut client) = connect_client(matches, context).await else {
            return Ok((Trusted::default(), None));
        };
        let volumes = client.observe_volumes(sought).await.ok();
        Ok((
            Trusted {
                volumes,
                ..Trusted::default()
            },
            Some(client),
        ))
    })
}

/// A refusal to delete Volume data unaccepted names the exact command that accepts
/// it, bound to the reviewed version so a changed review refuses again.
fn with_retry(
    error: StoreCallError,
    matches: &ArgMatches,
    services: &[ServiceName],
) -> StoreCallError {
    let StoreCallError::Refused(mut error) = error else {
        return error;
    };
    if error.code != ployz_core::RpcErrorCode::ConfirmationRequired {
        return StoreCallError::Refused(error);
    }
    let version = error
        .details
        .get("version")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let accept: Vec<String> = error
        .details
        .get("accept")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().map(str::to_owned))
        .collect();
    let mut words = vec!["deploy"];
    words.extend(services.iter().map(ServiceName::as_str));
    words.extend(["--expect-version", version.as_str()]);
    for name in &accept {
        words.extend(["--accept-volume-loss", name.as_str()]);
    }
    let retry = next(matches, &words);
    error.message = format!("{}.\nRetry: {retry}", error.message);
    if let Some(details) = error.details.as_object_mut() {
        details.insert("next".into(), serde_json::json!(retry));
    }
    StoreCallError::Refused(error)
}

/// Run a claimed Deployment on the Cluster and record each step's evidence. A
/// successful Deploy then deletes exactly the Docker Volumes admission accepted.
async fn run(
    matches: &ArgMatches,
    store: &ConfigStore,
    claimed: &Claimed,
    runner: &RunnerId,
    client: Option<Client>,
    mut events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<(), Error> {
    let id = &claimed.deployment.id;
    let not_executed = |error: Error| -> Error {
        match store.record(id, runner, RunEvidence::NotExecuted(error.report().message)) {
            Ok(_) => error,
            Err(recording) => recording.into(),
        }
    };
    let context = matches.get_one::<String>("context").map(String::as_str);
    let mut client = match client {
        Some(client) => client,
        None => match connect_client(matches, context).await {
            Ok(client) => client,
            Err(error) => return Err(not_executed(error)),
        },
    };
    let plan = match client.preview(claimed.intent.clone()).await {
        Ok(plan) => plan,
        Err(error) => return Err(not_executed(error.into())),
    };
    store.record(id, runner, RunEvidence::Prepared(plan.preview().clone()))?;
    let cancel = crate::cancellation::on_ctrl_c();
    let _stop_listener = cancel.clone().drop_guard();
    let executed = crate::deploy::execute(
        &client,
        &plan,
        claimed.intent.namespace.as_str(),
        &cancel,
        |event| {
            if let Some(file) = events.as_mut() {
                // ponytail: a failed event write never stops a deploy; the file is a tap.
                let _ = serde_json::to_writer(&mut *file, event);
                let _ = writeln!(file);
                let _ = file.flush();
            }
        },
    )
    .await;
    let (outcome, failure) = match executed {
        Ok(outcome) => (outcome, None),
        Err(ApplyError::Execute {
            outcome,
            rows,
            live_shown,
        }) => {
            let failure = Error::from(ApplyError::Execute {
                outcome: outcome.clone(),
                rows,
                live_shown,
            });
            (*outcome, Some(failure))
        }
        Err(error @ ApplyError::Prepare(_)) => return Err(not_executed(error.into())),
    };
    let removed = if failure.is_none() && !claimed.deletes.is_empty() {
        // ponytail: failing to reach the Cluster deletes nothing; the Volume stays
        // deployed, and the next Deploy deletes it.
        client
            .remove_volumes(RemoveVolumesRequest {
                volumes: claimed.deletes.clone(),
                force: false,
            })
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    store.record(
        id,
        runner,
        RunEvidence::Executed {
            outcome: Box::new(outcome),
            removed,
        },
    )?;
    failure.map_or(Ok(()), Err)
}

fn ls(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let page = store(root)?
        .deployments(&DeploymentsQuery {
            environment: environment(matches)?,
            limit: matches.get_one::<usize>("limit").copied(),
            cursor: matches.get_one::<String>("cursor").cloned(),
        })
        .map_err(failed(matches, &["deployment", "ls"]))?;
    let hint = page.next_cursor.as_ref().map(|cursor| {
        let mut words = vec!["deployment", "ls", "--cursor", cursor.as_str()];
        let limit = matches.get_one::<usize>("limit").map(ToString::to_string);
        if let Some(limit) = &limit {
            words.extend(["--limit", limit.as_str()]);
        }
        next(matches, &words)
    });
    crate::output::finish(&Next::new(&page, hint.clone()), || {
        if page.deployments.is_empty() {
            say!(
                "No Deployments in {}/{}.",
                page.environment.project,
                page.environment.name
            );
        }
        for deployment in &page.deployments {
            say!(
                "#{} {:?} Saved revision {} {}",
                deployment.number,
                deployment.status,
                deployment.saved,
                deployment.id
            );
        }
        if let Some(hint) = &hint {
            say!("next: {hint}");
        }
    })
}

fn show(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let id = DeploymentId::parse(required(matches, "id")?)
        .map_err(|_| Error::usage("Expected a Deployment ID (a UUID)").with_exit(USAGE_EXIT))?;
    let view = store(root)?
        .deployment(&id)
        .map_err(failed(matches, &["deployment", "show"]))?;
    finish_view(&view, None)
}

fn finish_view(view: &DeploymentView, hint: Option<String>) -> Result<(), Error> {
    crate::output::finish(&Next::new(view, hint), || {
        say!(
            "Deployment #{} of {}/{}: {:?}",
            view.deployment.number,
            view.environment.project,
            view.environment.name,
            view.deployment.status
        );
        for node in &view.nodes {
            say!("  {}: {:?}", node.name, node.outcome);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refused_volume_loss_names_the_exact_retry() {
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "deploy", "--env", "staging"])
            .unwrap();
        let refused = ployz_core::RpcError {
            code: ployz_core::RpcErrorCode::ConfirmationRequired,
            message: "This Deploy permanently deletes the data of data".into(),
            details: serde_json::json!({ "version": "3:1:0.1", "accept": ["data"] }),
        };
        let StoreCallError::Refused(error) =
            with_retry(StoreCallError::Refused(refused), leaf_matches(&root), &[])
        else {
            panic!("a refusal stays a refusal");
        };
        let retry = "ployz deploy --expect-version 3:1:0.1 --accept-volume-loss data --env staging";
        assert_eq!(error.details.get("next"), Some(&serde_json::json!(retry)));
        assert!(error.message.ends_with(&format!("Retry: {retry}")));
    }
}
