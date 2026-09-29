//! `ployz deploy` and `ployz deployment`: ship an Environment from the Config Store,
//! and read, retry, start and cancel its Deployments. Cloud's runner runs a
//! Deployment admitted over HTTPS, and `deploy` follows it until it ends. With the
//! hidden in-process Store this CLI is the Deployment's runner: it claims it,
//! prepares and confirms it on the Cluster, and records what happened.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{ArgAction, ArgMatches, Command, ValueHint};
use ployz_core::{RemoveVolumesRequest, RpcErrorCode, ServiceName};
use ployz_store::{
    Admit, Cancel, Claimed, ConfigStore, DeploymentId, DeploymentStatus, DeploymentSummary,
    DeploymentView, DeploymentsQuery, EnvironmentRef, PlanQuery, RemovalsQuery, RunEvidence,
    RunnerId, Start, UploadBase, UploadedSource, VolumeName, VolumeObservation,
};
use serde_json::{Value, json};

use super::store::{
    Next, Store, environment, failed, mint, next, scoped, store, with_refresh_hint,
};
use super::{Error, connect_client, leaf_matches, required, runtime};
use crate::cli::{base, positional, switch, value};
use crate::cloud_account::StoreCallError;
use crate::deploy::ApplyError;
use crate::failure::USAGE_EXIT;
use crate::output::say;

pub(crate) fn deploy_command() -> Command {
    following(
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
            switch("plan", None)
                .help("Show what would deploy, from authored state alone; run nothing"),
        )
        .arg(
            value("expect-version", None)
                .value_name("VERSION")
                .help("Refuse unless this is still the latest `ployz diff` version"),
        ),
    )
    .arg(
        // Hidden: `ployz up` is how users upload; tests upload any directory.
        value("upload", None)
            .value_name("DIR")
            .value_hint(ValueHint::DirPath)
            .hide(true)
            .help("Build Services without a source of their own from DIR"),
    )
    .arg(crate::cli::volume_acceptance())
}

/// `--events` and `--detach`, for every command that queues a Deployment and follows it.
pub(super) fn following(command: Command) -> Command {
    command
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
        .about("Read, retry, start and cancel Deployments")
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
            scoped(
                Command::new("show")
                    .about("Show a Deployment with its Deploy Preview and Node Outcomes"),
            )
            .arg(id()),
        )
        .subcommand(
            following(scoped(base(
                "retry",
                "Deploy again exactly what a failed, unknown or cancelled Deployment froze, and follow it",
            )))
            .arg(id()),
        )
        .subcommand(
            following(scoped(base(
                "start",
                "Hand a queued Deployment to a runner now, and follow it",
            )))
            .arg(id()),
        )
        .subcommand(
            scoped(
                Command::new("cancel")
                    .about("Cancel a Deployment: a queued one never runs, a running one stops"),
            )
            .arg(id()),
        )
}

fn id() -> clap::Arg {
    positional("id", true).help("The Deployment's ID, or its number in the Environment")
}

pub(super) fn deployment_handler(path: &str) -> Option<(super::Handler, super::Json)> {
    use super::Json::Supported;
    Some(match path {
        "ls" => (ls, Supported),
        "show" => (show, Supported),
        "retry" => (retry, Supported),
        "start" => (start, Supported),
        "cancel" => (cancel, Supported),
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
    let events = open_events(matches)?;
    let source = matches
        .get_one::<String>("upload")
        .map(|dir| Path::new(dir).canonicalize())
        .transpose()?;
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
    let request = Request {
        environment: environment(matches)?,
        services,
        version: matches.get_one::<String>("expect-version").cloned(),
        source,
        accept,
    };
    upload_and_ship(matches, &store, request, events)?.finish()
}

/// What a Deploy admits; `source` is the directory it uploads first.
pub(super) struct Request {
    pub(super) environment: EnvironmentRef,
    pub(super) services: Vec<ServiceName>,
    pub(super) version: Option<String>,
    pub(super) source: Option<PathBuf>,
    pub(super) accept: Vec<VolumeName>,
}

/// Upload `request.source`, admit the Deployment, then run or follow it.
pub(super) fn upload_and_ship(
    matches: &ArgMatches,
    store: &Store,
    request: Request,
    events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<Shipped, Error> {
    let Request {
        environment,
        services,
        version,
        source,
        accept,
    } = request;
    let upload = source.as_deref().map(uploaded_source).transpose()?;
    // The in-process Store trusts this CLI to observe the Servers; Cloud observes
    // them itself.
    let volumes = match store.local() {
        Some(_) if services.is_empty() => observe(matches, store, &environment, false)?,
        Some(_) | None => None,
    };
    let id = DeploymentId::parse(mint())?;
    if let Some(dir) = source.as_deref().filter(|_| store.local().is_none()) {
        let archive = crate::build::upload_archive(dir)
            .map_err(|error| Error::usage(format!("Could not read {}: {error}", dir.display())))?;
        store
            .upload(&id, archive)
            .map_err(failed(matches, &["deploy"]))?;
    }
    let admitted = store
        .admit(
            &Admit {
                id,
                environment,
                services: services.clone(),
                version,
                upload,
                retry: None,
                remove: false,
                accept_volume_loss: accept,
            },
            volumes,
        )
        .map_err(|error| {
            let error = with_retry(
                with_refresh_hint(error, matches, "diff"),
                matches,
                &services,
            );
            failed(matches, &["deploy"])(error)
        })?;
    execute(
        matches,
        store,
        &admitted,
        source.as_deref(),
        events,
        &["deploy"],
    )
}

/// Open `--events` before queueing anything, so a bad path ships nothing.
pub(super) fn open_events(
    matches: &ArgMatches,
) -> Result<Option<std::io::BufWriter<std::fs::File>>, Error> {
    Ok(matches
        .get_one::<String>("events")
        .map(std::fs::File::create)
        .transpose()?
        .map(std::io::BufWriter::new))
}

/// Run or follow a queued Deployment and report it; `source` is the directory
/// uploaded for it, and `words` name the command for its failures. Exits 3 unless
/// it applied.
fn ship(
    matches: &ArgMatches,
    store: &Store,
    admitted: &DeploymentSummary,
    events: Option<std::io::BufWriter<std::fs::File>>,
    words: &[&str],
) -> Result<(), Error> {
    execute(matches, store, admitted, None, events, words)?.finish()
}

/// A Deployment that ended, or was left to run: its view, the command to run next,
/// and whether it applied.
pub(super) struct Shipped {
    pub(super) view: DeploymentView,
    pub(super) hint: String,
    pub(super) ran: Result<(), Error>,
}

impl Shipped {
    fn finish(self) -> Result<(), Error> {
        finish_view(&self.view, Some(self.hint))?;
        self.ran
    }
}

pub(super) fn execute(
    matches: &ArgMatches,
    store: &Store,
    admitted: &DeploymentSummary,
    source: Option<&Path>,
    events: Option<std::io::BufWriter<std::fs::File>>,
    words: &[&str],
) -> Result<Shipped, Error> {
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
            let ran = runtime()?.block_on(run(matches, local, &claimed, &runner, source, events));
            let view = store
                .deployment(&admitted.id)
                .map_err(failed(matches, words))?;
            (view, ran)
        }
        None if matches.get_flag("detach") => {
            let view = store
                .deployment(&admitted.id)
                .map_err(failed(matches, words))?;
            (view, Ok(()))
        }
        None => {
            let view = follow(matches, store, admitted, events, words)?;
            let ran = if view.deployment.status == DeploymentStatus::Applied {
                Ok(())
            } else {
                Err(Error::partial())
            };
            (view, ran)
        }
    };
    // A run that found no upload or usable image for its Services says to upload;
    // one that names another fix says that; otherwise read the Deployment.
    let cloud_needs_upload = matches!(
        &view.outcome,
        Some(ployz_store::Outcome::NotExecuted { needs_upload, .. }) if !needs_upload.is_empty()
    );
    let details = ran.as_ref().err().map(|error| error.report().details);
    let local_needs_upload = details.as_ref().is_some_and(|details| {
        details.pointer("/preparation/kind") == Some(&json!("upload_needed"))
    });
    let hint = if cloud_needs_upload || local_needs_upload {
        upload_again(&view.environment)
    } else {
        details
            .as_ref()
            .and_then(|details| details.get("next")?.as_str().map(str::to_owned))
            .unwrap_or(hint)
    };
    Ok(Shipped { view, hint, ran })
}

/// `ployz up` in this Environment, which uploads the directory it runs in. It names
/// the scope, so an unlinked directory never founds a new Project.
fn upload_again(environment: &ployz_store::EnvironmentSummary) -> String {
    shell_words::join([
        "ployz",
        "up",
        "--project",
        environment.project.as_str(),
        "--env",
        environment.name.as_str(),
    ])
}

/// Follow a Deployment Cloud's runner runs until it ends. Each change of its status
/// or Node Outcomes goes to stderr, and to `events` as NDJSON. Stopping this stops
/// following, never the Deployment.
fn follow(
    matches: &ArgMatches,
    store: &Store,
    admitted: &DeploymentSummary,
    mut events: Option<std::io::BufWriter<std::fs::File>>,
    words: &[&str],
) -> Result<DeploymentView, Error> {
    eprintln!(
        "Following Deployment #{}; stopping this leaves it running.",
        admitted.number
    );
    let mut last = None;
    loop {
        let view = store
            .deployment(&admitted.id)
            .map_err(failed(matches, words))?;
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

/// "uploaded by nick, base abc1234 + changes": where an upload came from.
fn provenance(upload: &UploadedSource) -> String {
    let who = upload
        .uploader
        .as_ref()
        .map_or("this device", ployz_store::Principal::as_str);
    let digest = upload.digest.get(..12).unwrap_or(&upload.digest);
    match &upload.base {
        Some(base) => format!(
            "uploaded by {who}, base {}{}",
            base.commit.get(..7).unwrap_or(&base.commit),
            if base.changed { " + changes" } else { "" }
        ),
        None => format!("uploaded by {who}, sha256 {digest}"),
    }
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
    Ok(UploadedSource {
        digest,
        base,
        uploader: None,
    })
}

/// Build the Deployment's uploaded Services from `source`, or, without it, reuse their
/// latest images; record the receipts, and return the plan with the images it keeps
/// alive until execution.
async fn build(
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
    let captured = tokio::task::spawn_blocking(move || preparation::capture(input))
        .await
        .map_err(std::io::Error::other)??;
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
    .map_err(|error| crate::sdk::preparation_error(error, cancel.is_cancelled()))?;
    let (plan, retained) = prepared.into_parts();
    let receipts = preparation::receipts(&captured.fingerprints, &captured.contents, &retained)
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

/// When a Deploy removes deployed Volumes, observe which Servers hold their data:
/// the evidence the in-process Store reviews it against. A Cluster it can't reach
/// leaves the evidence out, so the Store refuses.
pub(super) fn observe(
    matches: &ArgMatches,
    store: &Store,
    environment: &ployz_store::EnvironmentRef,
    remove: bool,
) -> Result<Option<VolumeObservation>, Error> {
    let removals = store
        .removals(&RemovalsQuery {
            environment: environment.clone(),
            remove,
        })
        .map_err(failed(matches, &["deploy"]))?;
    if removals.volumes.is_empty() {
        return Ok(None);
    }
    let sought = removals
        .volumes
        .into_iter()
        .map(|volume| volume.docker_volume)
        .collect();
    let context = matches.get_one::<String>("context").map(String::as_str);
    runtime()?.block_on(async {
        let Ok(mut client) = connect_client(matches, context).await else {
            return Ok(None);
        };
        Ok(client.observe_volumes(sought).await.ok())
    })
}

/// A refusal to delete Volume data unaccepted names the exact command that accepts
/// it, bound to the reviewed version so a changed review refuses again.
fn with_retry(
    error: StoreCallError,
    matches: &ArgMatches,
    services: &[ServiceName],
) -> StoreCallError {
    let version = match &error {
        StoreCallError::Refused(error) => error
            .details
            .get("version")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        StoreCallError::Cloud(_) => String::new(),
    };
    let mut words = vec!["deploy"];
    words.extend(services.iter().map(ServiceName::as_str));
    words.extend(["--expect-version", version.as_str()]);
    accepting(error, matches, &words)
}

/// A refusal to delete Volume data names `words` again, accepting each Volume the
/// refusal lists.
pub(super) fn accepting(
    error: StoreCallError,
    matches: &ArgMatches,
    words: &[&str],
) -> StoreCallError {
    let StoreCallError::Refused(mut error) = error else {
        return error;
    };
    if error.code != ployz_core::RpcErrorCode::ConfirmationRequired {
        return StoreCallError::Refused(error);
    }
    let accept: Vec<String> = error
        .details
        .get("accept")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|name| name.as_str().map(str::to_owned))
        .collect();
    let mut words = words.to_vec();
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
    // Its containers carry its ID, which `logs --deployment` reads.
    client.deployment_id = id.as_str().parse().ok();
    let services = claimed
        .deployment
        .upload
        .as_ref()
        .map(|_| claimed.uploads.clone())
        .unwrap_or_default();
    // Built images stay retained on their Machines until execution ends.
    let (plan, _retained) = if services.is_empty() {
        match client.preview(claimed.intent.clone()).await {
            Ok(plan) => (plan, Vec::new()),
            Err(error) => return Err(not_executed(error.into())),
        }
    } else {
        match build(store, claimed, runner, &mut client, services, source, &tap).await {
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

/// Argument `arg`: a Deployment ID, or its number (`#N`) in the scoped Environment.
pub(super) fn deployment_id(
    matches: &ArgMatches,
    store: &Store,
    arg: &str,
) -> Result<DeploymentId, Error> {
    let given = required(matches, arg)?;
    let Ok(number) = given.trim_start_matches('#').parse::<u64>() else {
        return DeploymentId::parse(given)
            .map_err(|_| Error::usage("Expected a Deployment ID or number").with_exit(USAGE_EXIT));
    };
    // Pages hold Deployments numbered below the cursor, newest first.
    let page = store
        .deployments(&DeploymentsQuery {
            environment: environment(matches)?,
            limit: Some(1),
            cursor: number.checked_add(1).map(|cursor| cursor.to_string()),
        })
        .map_err(failed(matches, &["deployment", "ls"]))?;
    page.deployments
        .into_iter()
        .find(|deployment| deployment.number == number)
        .map(|deployment| deployment.id)
        .ok_or_else(|| {
            Error::not_found(format!(
                "{}/{} has no Deployment #{number}",
                page.environment.project, page.environment.name
            ))
        })
}

/// A refused retry, start or cancel points at the Deployment's current state.
fn refused<'a>(
    matches: &'a ArgMatches,
    id: &'a DeploymentId,
    words: &'a [&'a str],
) -> impl FnOnce(StoreCallError) -> Error + 'a {
    move |mut error| {
        if let StoreCallError::Refused(refusal) = &mut error
            && refusal.code == RpcErrorCode::Conflict
            && let Some(details) = refusal.details.as_object_mut()
        {
            details.insert(
                "next".into(),
                shell_words::join(["ployz", "deployment", "show", id.as_str()]).into(),
            );
        }
        failed(matches, words)(error)
    }
}

fn retry(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let source = deployment_id(matches, &store, "id")?;
    let events = open_events(matches)?;
    let words = ["deployment", "retry"];
    let admitted = store
        .admit(
            &Admit {
                id: DeploymentId::parse(mint())?,
                environment: ployz_store::EnvironmentRef::default(),
                services: Vec::new(),
                version: None,
                upload: None,
                retry: Some(source.clone()),
                remove: false,
                accept_volume_loss: Vec::new(),
            },
            None,
        )
        .map_err(refused(matches, &source, &words))?;
    ship(matches, &store, &admitted, events, &words)
}

fn start(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let id = deployment_id(matches, &store, "id")?;
    let events = open_events(matches)?;
    let words = ["deployment", "start"];
    let queued = store
        .start(&Start {
            deployment: id.clone(),
        })
        .map_err(refused(matches, &id, &words))?;
    ship(matches, &store, &queued, events, &words)
}

fn cancel(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let id = deployment_id(matches, &store, "id")?;
    store
        .cancel(&Cancel {
            deployment: id.clone(),
        })
        .map_err(refused(matches, &id, &["deployment", "cancel"]))?;
    let view = store
        .deployment(&id)
        .map_err(failed(matches, &["deployment", "cancel"]))?;
    let hint = shell_words::join(["ployz", "deployment", "show", id.as_str()]);
    finish_view(&view, Some(hint))
}

fn show(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let id = deployment_id(matches, &store, "id")?;
    let view = store
        .deployment(&id)
        .map_err(failed(matches, &["deployment", "show"]))?;
    finish_view(&view, None)
}

pub(super) fn finish_view(view: &DeploymentView, hint: Option<String>) -> Result<(), Error> {
    crate::output::finish(&Next::new(view, hint), || say_view(view))
}

/// A Deployment as human text.
pub(super) fn say_view(view: &DeploymentView) {
    say!(
        "Deployment #{} of {}/{}: {:?}",
        view.deployment.number,
        view.environment.project,
        view.environment.name,
        view.deployment.status
    );
    if let Some(upload) = &view.deployment.upload {
        say!("  {}", provenance(upload));
    }
    for node in &view.nodes {
        say!("  {}: {:?}", node.name, node.outcome);
    }
    for build in &view.builds {
        let commit = build
            .commit
            .as_deref()
            .map_or("the upload", |commit| commit.get(..7).unwrap_or(commit));
        let reason = build.message.as_deref().unwrap_or_default();
        say!(
            "  build {} from {commit}: {:?} {reason}",
            build.service,
            build.status
        );
    }
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
