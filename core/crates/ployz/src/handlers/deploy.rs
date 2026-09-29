//! `ployz deploy` and `ployz deployment`: ship an Environment from the Config Store
//! and read its Deployments. With the hidden in-process Store this CLI is the
//! Deployment's runner: it claims it, prepares and confirms it on the Cluster, and
//! records what happened.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use clap::{ArgAction, ArgMatches, Command, ValueHint};
use ployz_core::config::{ServiceSource, parse_service_config};
use ployz_core::{RpcError, ServiceName};
use ployz_store::{
    Admit, Claimed, ConfigStore, DeploymentId, DeploymentView, DeploymentsQuery, PlanQuery,
    RunEvidence, RunnerId, UploadBase, UploadedSource,
};
use serde_json::{Value, json};

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
        // ponytail: hidden until `ployz up` uploads to Cloud; it composes this path.
        value("upload", None)
            .value_name("DIR")
            .value_hint(ValueHint::DirPath)
            .hide(true)
            .help("Build Services without a source of their own from DIR"),
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
    let source = matches
        .get_one::<String>("upload")
        .map(|dir| Path::new(dir).canonicalize())
        .transpose()?;
    if source.is_some() && store.local().is_none() {
        return Err(Error::coded(
            ployz_core::RpcErrorCode::Unsupported,
            "Uploading to Cloud isn't available yet; deploy from a connected repository",
        ));
    }
    let upload = source.as_deref().map(uploaded_source).transpose()?;
    let admitted = store
        .admit(&Admit {
            id: DeploymentId::parse(mint())?,
            environment: environment(matches)?,
            services,
            version: matches.get_one::<String>("expect-version").cloned(),
            upload,
        })
        .map_err(|error| failed(matches, &["deploy"])(with_refresh_hint(error, matches, "diff")))?;
    // Only the hidden in-process Store lets this CLI run the Deployment; Cloud's
    // runner runs it otherwise, and this command returns it queued.
    let ran = match store.local() {
        Some(local) => {
            let runner = RunnerId::parse(format!("cli-{}", mint()))?;
            let claimed = local.claim(&admitted.id, &runner)?;
            runtime()?.block_on(run(
                matches,
                local,
                &claimed,
                &runner,
                source.as_deref(),
                events,
            ))
        }
        None => Ok(()),
    };
    let view = store
        .deployment(&admitted.id)
        .map_err(failed(matches, &["deploy"]))?;
    // A failed run that names its fix (a new upload) says so; otherwise read the Deployment.
    let hint = ran
        .as_ref()
        .err()
        .and_then(|error| {
            error
                .report()
                .details
                .get("next")?
                .as_str()
                .map(str::to_owned)
        })
        .unwrap_or_else(|| {
            shell_words::join(["ployz", "deployment", "show", view.deployment.id.as_str()])
        });
    finish_view(&view, Some(hint))?;
    ran
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

/// `dir` as an Uploaded Source: its content digest, and the commit it was checked out
/// at, if any, with whether it holds changes that commit doesn't.
fn uploaded_source(dir: &Path) -> Result<UploadedSource, Error> {
    let digest = crate::build::content_digest(dir)
        .map_err(|error| Error::usage(format!("Could not read {}: {error}", dir.display())))?;
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let base = git(&["rev-parse", "--verify", "HEAD"])
        .filter(|commit| ployz_core::is_lower_hex(commit, 40))
        .map(|commit| UploadBase {
            commit,
            // ponytail: files Git ignores count as no change; the base is provenance only.
            changed: git(&["status", "--porcelain", "--", "."])
                .is_none_or(|status| !status.is_empty()),
        });
    Ok(UploadedSource { digest, base })
}

/// The Services this Deployment targets that have no source of their own: they
/// build from its upload.
fn upload_targets(input: &Value) -> Vec<ServiceName> {
    let selected: Vec<&str> = input["selected"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|selected| selected["name"].as_str())
        .collect();
    input["snapshots"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|snapshot| parse_service_config(snapshot["config"].clone()).ok())
        .filter(|config| matches!(config.settings.source, ServiceSource::Empty { .. }))
        .map(|config| config.settings.private_dns)
        .filter(|name| selected.is_empty() || selected.contains(&name.as_str()))
        .collect()
}

/// Build the Deployment's uploaded Services from `source`, or, without it, reuse their
/// latest images; record the receipts, and return the plan with the images it keeps
/// alive until execution.
#[expect(
    clippy::too_many_arguments,
    reason = "the runner's store, identity, connection and upload are separate caller-owned inputs"
)]
async fn build(
    matches: &ArgMatches,
    store: &ConfigStore,
    claimed: &Claimed,
    runner: &RunnerId,
    client: &mut crate::connect::Client,
    services: Vec<ServiceName>,
    source: Option<&Path>,
    tap: &impl Fn(Value),
) -> Result<(crate::deploy::DeployPlan, Vec<crate::build::BuiltService>), Error> {
    use crate::sdk::preparation::{self, BuildReceipt, PreparationInput};
    let digest = claimed
        .deployment
        .upload
        .as_ref()
        .map(|upload| upload.digest.clone())
        .unwrap_or_default();
    let input = PreparationInput {
        deployment: claimed.input.clone(),
        sources: source.map_or_else(BTreeMap::new, |dir| {
            services
                .iter()
                .map(|name| (name.clone(), PathBuf::from(dir)))
                .collect()
        }),
        source_commits: BTreeMap::new(),
        uploads: services
            .iter()
            .map(|name| (name.clone(), digest.clone()))
            .collect(),
        // A receipt this ployz can't read is only a hint lost.
        build_receipts: claimed
            .receipts
            .iter()
            .filter_map(|(name, receipt)| {
                let receipt = serde_json::from_value::<BuildReceipt>(receipt.clone()).ok()?;
                Some((name.clone(), receipt))
            })
            .collect(),
        build_index: 0,
        preferred_machine: None,
    };
    let upload_again = |mut error: RpcError| -> Error {
        if let Some(details) = error.details.as_object_mut() {
            let dir = source.map_or_else(|| ".".into(), |dir| dir.display().to_string());
            details.insert(
                "next".into(),
                json!(super::store::next(matches, &["deploy", "--upload", &dir])),
            );
        }
        error.into()
    };
    let captured = tokio::task::spawn_blocking(move || preparation::capture(input))
        .await
        .map_err(std::io::Error::other)?
        .map_err(upload_again)?;
    let cancel = crate::cancellation::on_ctrl_c();
    let _stop_listener = cancel.clone().drop_guard();
    let prepared = crate::sdk::prepare::prepare(
        client,
        captured.intent,
        captured.build,
        &captured.reusable,
        captured.preference,
        &cancel,
        |progress| {
            if let crate::sdk::prepare::Progress::Build(ployz_build::Progress::StepOutput {
                text,
                ..
            }) = &progress
            {
                eprint!("{text}");
            }
            tap(json!({ "Preparation": progress }));
        },
    )
    .await
    .map_err(|error| upload_again(crate::sdk::preparation_error(error, cancel.is_cancelled())))?;
    let (plan, retained) = prepared.into_parts();
    let receipts = preparation::receipts(&captured.fingerprints, &retained)
        .into_iter()
        .map(|(name, receipt)| {
            (
                name,
                serde_json::to_value(receipt).expect("a build receipt is JSON"),
            )
        })
        .collect();
    store.record(&claimed.deployment.id, runner, RunEvidence::Built(receipts))?;
    Ok((plan, retained))
}

/// Run a claimed Deployment on the Cluster and record each step's evidence.
async fn run(
    matches: &ArgMatches,
    store: &ConfigStore,
    claimed: &Claimed,
    runner: &RunnerId,
    source: Option<&Path>,
    events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<(), Error> {
    let events = RefCell::new(events);
    let tap = |event: Value| {
        if let Some(file) = events.borrow_mut().as_mut() {
            // ponytail: a failed event write never stops a deploy; the file is a tap.
            let _ = serde_json::to_writer(&mut *file, &event);
            let _ = writeln!(file);
            let _ = file.flush();
        }
    };
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
    let services = claimed
        .deployment
        .upload
        .as_ref()
        .map(|_| upload_targets(&claimed.input))
        .unwrap_or_default();
    // Built images stay retained on their Machines until execution ends.
    let (plan, _retained) = if services.is_empty() {
        match client.preview(claimed.intent.clone()).await {
            Ok(plan) => (plan, Vec::new()),
            Err(error) => return Err(not_executed(error.into())),
        }
    } else {
        match build(
            matches,
            store,
            claimed,
            runner,
            &mut client,
            services,
            source,
            &tap,
        )
        .await
        {
            Ok(built) => built,
            Err(error) => return Err(not_executed(error)),
        }
    };
    store.record(id, runner, RunEvidence::Prepared(plan.preview().clone()))?;
    let cancel = crate::cancellation::on_ctrl_c();
    let _stop_listener = cancel.clone().drop_guard();
    let executed = crate::deploy::execute(
        &client,
        &plan,
        claimed.intent.namespace.as_str(),
        &cancel,
        |event| tap(serde_json::to_value(event).expect("a deploy event is JSON")),
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
