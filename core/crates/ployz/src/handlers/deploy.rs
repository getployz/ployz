//! `ployz deploy` and `ployz deployment`: ship an Environment from the Config Store,
//! and read, retry, start and cancel its Deployments. Cloud's runner runs a
//! Deployment admitted over HTTPS, and `deploy` follows it until it ends. With the
//! hidden in-process Store this CLI is the Deployment's runner: it claims it,
//! prepares and confirms it on the Cluster, and records what happened.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{ArgAction, ArgMatches, Command, ValueHint};
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Admit, Cancel, ConfigStore, Deploy, DeploymentId, DeploymentStatus, DeploymentSummary,
    DeploymentView, DeploymentsQuery, EnvironmentRef, PlanQuery, RemovalsQuery, RunnerId, Start,
    UploadBase, UploadedSource, VolumeName, VolumeObservation,
};

use super::store::{
    Next, Store, environment, failed, mint, next, scoped, store, with_refresh_hint,
};
use super::{Error, connect_client, leaf_matches, required, runtime};
use crate::cli::{base, positional, switch, value};
use crate::cloud_account::StoreCallError;
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
        )
        .arg(
            value("message", None)
                .value_name("TEXT")
                .help("Say what this Deploy ships; shown on the Deployment"),
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

pub(super) fn deployment_handler(path: &str) -> Option<super::Handler> {
    Some(match path {
        "ls" => ls,
        "show" => show,
        "retry" => retry,
        "start" => start,
        "cancel" => cancel,
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
    let accept = super::teardown::accepted(matches)?;
    let request = Request {
        environment: environment(matches)?,
        services,
        version: matches.get_one::<String>("expect-version").cloned(),
        source,
        accept,
        message: matches.get_one::<String>("message").cloned(),
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
    pub(super) message: Option<String>,
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
        message,
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
        let archive = crate::build::upload_archive(dir).map_err(unreadable(dir))?;
        store
            .upload(&id, archive)
            .map_err(failed(matches, &["deploy"]))?;
    }
    let admitted = store
        .admit(
            &Admit::Deploy(Deploy {
                id,
                environment,
                services: services.clone(),
                version,
                upload,
                accept_volume_loss: accept,
                message,
            }),
            volumes,
        )
        .map_err(|error| {
            let error = with_retry(
                with_refresh_hint(error, matches, "diff"),
                matches,
                &services,
                source.as_deref(),
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
    // Only the hidden in-process Store lets this CLI run the Deployment, as Cloud's
    // runner does; this command follows it either way, unless detached.
    let runner = match store.local() {
        Some(_) if matches.get_flag("detach") => {
            return Err(Error::usage(
                "The hidden local Store runs Deployments in this process, so it can't detach",
            )
            .with_exit(USAGE_EXIT));
        }
        Some(local) => Some(run_here(matches, local, admitted, source)?),
        None => None,
    };
    let view = if runner.is_none() && matches.get_flag("detach") {
        store
            .read(&ployz_store::DeploymentQuery {
                id: admitted.id.clone(),
            })
            .map_err(failed(matches, words))?
    } else {
        follow(matches, store, admitted, events, runner.as_ref(), words)?
    };
    if let Some(runner) = runner {
        runner
            .join()
            .map_err(|_| Error::coded(RpcErrorCode::Internal, "The Deployment runner stopped"))??;
    }
    let ran = if view.deployment.status == DeploymentStatus::Applied || matches.get_flag("detach") {
        Ok(())
    } else {
        Err(Error::partial())
    };
    // A run that found no upload or usable image for its Services says to upload.
    let needs_upload = matches!(
        &view.outcome,
        Some(ployz_store::Outcome::NotExecuted { needs_upload, .. }) if !needs_upload.is_empty()
    );
    let hint = if needs_upload {
        upload_again(&view.environment)
    } else {
        hint
    };
    Ok(Shipped { view, hint, ran })
}

/// Run the Deployment in this process, as Cloud's runner does, on the Cluster the
/// command line or context names; `source` is its upload.
fn run_here(
    matches: &ArgMatches,
    local: &std::sync::Arc<ConfigStore>,
    admitted: &DeploymentSummary,
    source: Option<&Path>,
) -> Result<std::thread::JoinHandle<Result<DeploymentSummary, Error>>, Error> {
    let connections = crate::connect::resolve_connections(
        &super::config_path(matches)?,
        matches.get_one::<String>("connect").map(String::as_str),
        matches.get_one::<String>("context").map(String::as_str),
        Path::new(crate::connect::DEFAULT_LOCAL_SOCKET),
    )
    .map(|selected| selected.connections)
    // No Cluster to reach: the runner records that nothing ran.
    .unwrap_or_default();
    let runner = RunnerId::parse(format!("cli-{}", mint()))?;
    let sources = crate::sdk::Sources {
        checkouts: BTreeMap::new(),
        upload: source.map(Path::to_path_buf),
    };
    let (local, id) = (std::sync::Arc::clone(local), admitted.id.clone());
    let runtime = runtime()?;
    Ok(std::thread::spawn(move || {
        Ok(runtime.block_on(crate::sdk::run_deployment(
            local,
            id,
            runner,
            connections,
            Ok(sources),
        ))?)
    }))
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
    runner: Option<&std::thread::JoinHandle<Result<DeploymentSummary, Error>>>,
    words: &[&str],
) -> Result<DeploymentView, Error> {
    if runner.is_none() {
        say!(
            "Following Deployment #{}; stopping this leaves it running.",
            admitted.number
        );
    }
    let mut last = None;
    loop {
        let view = store
            .read(&ployz_store::DeploymentQuery {
                id: admitted.id.clone(),
            })
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
                .map(|node| format!("{} {}", node.node.name(), super::store::word(&node.outcome)))
                .collect();
            say!(
                "{}: {}",
                super::store::word(&view.deployment.status),
                nodes.join(", ")
            );
            if let Some(file) = events.as_mut() {
                // ponytail: a failed event write never stops following; the file is a tap.
                let _ = writeln!(file, "{progress}");
                let _ = file.flush();
            }
            last = Some(progress);
        }
        // A runner here that ended leaves nothing more to follow.
        if !view.deployment.status.in_flight() || runner.is_some_and(|runner| runner.is_finished())
        {
            return Ok(view);
        }
        std::thread::sleep(FOLLOW_POLL);
    }
}

fn plan(matches: &ArgMatches, store: &Store, services: Vec<ServiceName>) -> Result<(), Error> {
    let plan = store
        .read(&PlanQuery {
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
            say!(
                "{} ({})",
                change.name,
                super::store::word(&change.lifecycle)
            );
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
            base.commit.as_str().get(..7).unwrap_or_default(),
            if base.changed { " + changes" } else { "" }
        ),
        None => format!("uploaded by {who}, sha256 {digest}"),
    }
}

/// Why `dir` couldn't be read: its ignore rules are the user's input; a missing
/// directory is `not_found`; anything else is the CLI's own failure.
fn unreadable(dir: &Path) -> impl Fn(crate::build::Error) -> Error + '_ {
    move |error| {
        let message = format!("Could not read {}: {error}", dir.display());
        if matches!(error, crate::build::Error::Invalid(_)) {
            Error::usage(message)
        } else if !dir.exists() {
            Error::not_found(message)
        } else {
            Error::coded(RpcErrorCode::Internal, message)
        }
    }
}

/// `dir` as an Uploaded Source: its content digest, and the commit it was checked out
/// at, if any, with whether it holds changes that commit doesn't.
fn uploaded_source(dir: &Path) -> Result<UploadedSource, Error> {
    let digest = crate::build::content_digest(dir).map_err(unreadable(dir))?;
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
        .and_then(|commit| ployz_store::CommitSha::parse(commit).ok())
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
        .read(&RemovalsQuery {
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
    source: Option<&Path>,
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
    // A retry uploads again: `up` does, as does the hidden `deploy --upload`.
    let source = source.and_then(Path::to_str);
    let up = source.is_some() && matches.try_get_one::<String>("upload").is_err();
    if up {
        return accepting(error, matches, &["up"]);
    }
    let mut words = vec!["deploy"];
    words.extend(services.iter().map(ServiceName::as_str));
    if let Some(source) = source {
        words.extend(["--upload", source]);
    }
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
    let accepted = |words: &[&str], name: &str| {
        words
            .windows(2)
            .any(|pair| pair == ["--accept-volume-loss", name])
    };
    for name in &accept {
        if !accepted(&words, name) {
            words.extend(["--accept-volume-loss", name.as_str()]);
        }
    }
    // The acceptance binds to the reviewed version.
    let version = error
        .details
        .get("version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    if let Some(version) = &version
        && !words.contains(&"--expect-version")
    {
        words.extend(["--expect-version", version.as_str()]);
    }
    let retry = next(matches, &words);
    error.message = format!("{}.\nRetry: {retry}", error.message);
    if let Some(details) = error.details.as_object_mut() {
        details.insert("next".into(), serde_json::json!(retry));
    }
    StoreCallError::Refused(error)
}

fn ls(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let page = store(root)?
        .read(&DeploymentsQuery {
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
                "#{} {} Saved revision {} {}",
                deployment.number,
                super::store::word(&deployment.status),
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
        .read(&DeploymentsQuery {
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
    move |error| {
        let error = super::store::with_next(
            error,
            |refusal| refusal.code == RpcErrorCode::Conflict,
            || shell_words::join(["ployz", "deployment", "show", id.as_str()]),
        );
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
            &Admit::Retry(ployz_store::Retry {
                id: DeploymentId::parse(mint())?,
                deployment: source.clone(),
            }),
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
        .write(&Start {
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
        .write(&Cancel {
            deployment: id.clone(),
        })
        .map_err(refused(matches, &id, &["deployment", "cancel"]))?;
    let view = store
        .read(&ployz_store::DeploymentQuery { id: id.clone() })
        .map_err(failed(matches, &["deployment", "cancel"]))?;
    let hint = shell_words::join(["ployz", "deployment", "show", id.as_str()]);
    finish_view(&view, Some(hint))
}

fn show(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let id = deployment_id(matches, &store, "id")?;
    let view = store
        .read(&ployz_store::DeploymentQuery { id: id.clone() })
        .map_err(failed(matches, &["deployment", "show"]))?;
    finish_view(&view, None)
}

pub(super) fn finish_view(view: &DeploymentView, hint: Option<String>) -> Result<(), Error> {
    crate::output::finish(&Next::new(view, hint), || say_view(view))
}

/// A Deployment as human text.
pub(super) fn say_view(view: &DeploymentView) {
    say!(
        "Deployment #{} of {}/{}: {}",
        view.deployment.number,
        view.environment.project,
        view.environment.name,
        super::store::word(&view.deployment.status)
    );
    if let Some(upload) = &view.deployment.upload {
        say!("  {}", provenance(upload));
    }
    if let Some(
        ployz_store::Outcome::NotExecuted { reason, .. }
        | ployz_store::Outcome::Executed {
            reason: Some(reason),
            ..
        },
    ) = &view.outcome
    {
        say!("  {reason}");
    }
    for node in &view.nodes {
        say!(
            "  {}: {}",
            node.node.name(),
            super::store::word(&node.outcome)
        );
    }
    for build in &view.builds {
        let commit = build.commit.as_ref().map_or("the upload", |commit| {
            commit.as_str().get(..7).unwrap_or(commit.as_str())
        });
        let reason = build.message.as_deref().unwrap_or_default();
        say!(
            "  build {} from {commit}: {} {reason}",
            build.service,
            super::store::word(&build.status)
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
        let StoreCallError::Refused(error) = with_retry(
            StoreCallError::Refused(refused.clone()),
            leaf_matches(&root),
            &[],
            None,
        ) else {
            panic!("a refusal stays a refusal");
        };
        let retry = "ployz deploy --expect-version 3:1:0.1 --accept-volume-loss data --env staging";
        assert_eq!(error.details.get("next"), Some(&serde_json::json!(retry)));
        assert!(error.message.ends_with(&format!("Retry: {retry}")));
        // `up` uploads again, accepting the loss.
        let root = crate::cli::command()
            .try_get_matches_from(["ployz", "up", "--env", "staging"])
            .unwrap();
        let StoreCallError::Refused(error) = with_retry(
            StoreCallError::Refused(refused),
            leaf_matches(&root),
            &[],
            Some(Path::new("/src/app")),
        ) else {
            panic!("a refusal stays a refusal");
        };
        assert_eq!(
            error.details.get("next"),
            Some(&serde_json::json!(
                "ployz up --accept-volume-loss data --expect-version 3:1:0.1 --env staging"
            ))
        );
    }
}
