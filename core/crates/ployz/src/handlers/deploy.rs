//! `ployz deploy` and `ployz deployment`: ship an Environment from the Config Store
//! and read its Deployments. With the hidden in-process Store this CLI is the
//! Deployment's runner: it claims it, prepares and confirms it on the Cluster, and
//! records what happened.

use std::io::Write as _;

use clap::{ArgAction, ArgMatches, Command, ValueHint};
use ployz_core::ServiceName;
use ployz_store::{
    Actor, Admit, Claimed, ConfigStore, DeploymentId, DeploymentView, DeploymentsQuery, PlanQuery,
    RunEvidence, RunnerId, Written,
};

use super::config::{Next, environment, mint, next, scoped, stale, store};
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
}

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
    let (store, actor) = store()?;
    if matches.get_flag("plan") {
        return plan(matches, &store, &actor, services);
    }
    // Open the events file before admitting, so a bad path deploys nothing.
    let events = matches
        .get_one::<String>("events")
        .map(std::fs::File::create)
        .transpose()?
        .map(std::io::BufWriter::new);
    let written = store
        .write(
            &actor,
            ployz_store::Command::Admit(Admit {
                id: DeploymentId::parse(mint())?,
                environment: environment(matches)?,
                services,
                version: matches.get_one::<String>("expect-version").cloned(),
            }),
        )
        .map_err(|error| stale(error, matches, "diff"))?;
    let Written::Deployment(admitted) = written else {
        unreachable!("an admission writes a Deployment");
    };
    let runner = RunnerId::parse(format!("cli-{}", mint()))?;
    let claimed = store.claim(&admitted.id, &runner)?;
    let ran = runtime()?.block_on(run(matches, &store, &claimed, &runner, events));
    let view = store.deployment(&actor, &admitted.id)?;
    let hint = shell_words::join(["ployz", "deployment", "show", view.deployment.id.as_str()]);
    finish_view(&view, Some(hint))?;
    ran
}

fn plan(
    matches: &ArgMatches,
    store: &ConfigStore,
    actor: &Actor,
    services: Vec<ServiceName>,
) -> Result<(), Error> {
    let plan = store.plan(
        actor,
        &PlanQuery {
            environment: environment(matches)?,
            services,
        },
    )?;
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
    let (store, actor) = store()?;
    let page = store.deployments(
        &actor,
        &DeploymentsQuery {
            environment: environment(matches)?,
            limit: matches.get_one::<usize>("limit").copied(),
            cursor: matches.get_one::<String>("cursor").cloned(),
        },
    )?;
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
    let (store, actor) = store()?;
    let view = store.deployment(&actor, &id)?;
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
