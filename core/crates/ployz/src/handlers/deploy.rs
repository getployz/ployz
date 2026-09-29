//! `ployz deploy` and `ployz deployment`: ship an Environment from the Config Store
//! and read its Deployments. Cloud's runner runs a Deployment admitted over HTTPS,
//! and `deploy` follows it until it ends. With the hidden in-process Store this CLI
//! is the Deployment's runner: it claims it, prepares and confirms it on the
//! Cluster, and records what happened.

use std::io::Write as _;
use std::time::Duration;

use clap::{ArgAction, ArgMatches, Command, ValueHint};
use ployz_core::ServiceName;
use ployz_store::{
    Admit, Claimed, ConfigStore, DeploymentId, DeploymentStatus, DeploymentSummary, DeploymentView,
    DeploymentsQuery, PlanQuery, RunEvidence, RunnerId,
};

use super::store::{
    Next, Store, environment, failed, mint, next, scoped, store, with_refresh_hint,
};
use super::{Error, connect_client, leaf_matches, required, runtime};
use crate::cli::{base, positional, switch, value};
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
    let admitted = store
        .admit(&Admit {
            id: DeploymentId::parse(mint())?,
            environment: environment(matches)?,
            services,
            version: matches.get_one::<String>("expect-version").cloned(),
        })
        .map_err(|error| failed(matches, &["deploy"])(with_refresh_hint(error, matches, "diff")))?;
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
            let ran = runtime()?.block_on(run(matches, local, &claimed, &runner, events));
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

/// Run a claimed Deployment on the Cluster and record each step's evidence.
async fn run(
    matches: &ArgMatches,
    store: &ConfigStore,
    claimed: &Claimed,
    runner: &RunnerId,
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
    let mut client = match connect_client(matches, context).await {
        Ok(client) => client,
        Err(error) => return Err(not_executed(error)),
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
    store.record(id, runner, RunEvidence::Executed(Box::new(outcome)))?;
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
