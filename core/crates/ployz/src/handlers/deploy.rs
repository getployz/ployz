//! `ployz deploy` and `ployz deployment`: ship an Environment from the Config Store,
//! and read, retry, start and cancel its Deployments. Cloud's runner runs a
//! Deployment admitted over HTTPS, and `deploy` follows it until it ends. With the
//! hidden in-process Store this CLI is the Deployment's runner: it claims it,
//! prepares and confirms it on the Cluster, and records what happened.

#[cfg(test)]
mod follow_tests;
mod progress;

use crate::cancellation::CtrlC;
use crate::ui::progress::{Disposition, Progress};
use crate::ui::{self, Cell, Hint, Table, Tone};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{ArgAction, ArgMatches, Command, ValueHint};
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Admit, Cancel, ConfigStore, Deploy, DeploymentId, DeploymentStatus, DeploymentSummary,
    DeploymentView, DeploymentsQuery, EnvironmentRef, PlanQuery, RunnerId, Start, UploadBase,
    UploadedSource, VolumeName,
};

use super::store::{Next, Store, environment, mint, next, scoped, store, with_refresh_hint};
use super::{Error, leaf_matches, required, runtime};
use crate::cli::{base, positional, switch, value};
use crate::cloud_account::StoreCallError;

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
        .arg(crate::cli::reviewed_version())
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

/// The [`following`] flags as typed, so a rerun follows (or detaches) the same way.
pub(super) fn following_args(matches: &ArgMatches) -> Vec<&str> {
    let mut args = Vec::new();
    if let Some(events) = matches.get_one::<String>("events") {
        args.extend(["--events", events.as_str()]);
    }
    if matches.get_flag("detach") {
        args.push("--detach");
    }
    args
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
    let services = super::store::service_names(matches, "service")?;
    let names: Vec<&str> = services.iter().map(ServiceName::as_str).collect();
    let source = matches
        .get_one::<String>("upload")
        .map(|dir| Path::new(dir).canonicalize())
        .transpose()?;
    // A retry uploads again, as the hidden `deploy --upload` does.
    let upload = source.as_deref().and_then(Path::to_str);
    let message = matches.get_one::<String>("message").map(String::as_str);
    let store = store(root)?
        .args(names.iter().copied())
        .args(upload.into_iter().flat_map(|source| ["--upload", source]))
        .args(
            message
                .into_iter()
                .flat_map(|message| ["--message", message]),
        )
        .args(following_args(matches));
    if matches.get_flag("plan") {
        return plan(matches, &store, services);
    }
    let events = open_events(matches)?;
    let request = Request {
        environment: environment(matches)?,
        services,
        version: matches.get_one::<String>("expect-version").cloned(),
        source,
        accept: super::teardown::accepted(matches)?,
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
    let id = DeploymentId::parse(mint())?;
    if let Some(dir) = source.as_deref() {
        store.upload(&id, dir)?;
    }
    let admitted = store
        .admit(&Admit::Deploy(Deploy {
            id,
            environment,
            services,
            version,
            upload,
            accept_volume_loss: accept,
            message,
        }))
        .map_err(|error| store.accepting(with_refresh_hint(error, matches, "diff")))?;
    execute(matches, store, &admitted, source.as_deref(), events)
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

/// Run or follow a queued Deployment and report it. Exits 3 unless it applied.
fn ship(
    matches: &ArgMatches,
    store: &Store,
    admitted: &DeploymentSummary,
    events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<(), Error> {
    execute(matches, store, admitted, None, events)?.finish()
}

/// A Deployment that ended, or was left to run: its view, the command to run next,
/// and whether it applied.
pub(super) struct Shipped {
    pub(super) view: DeploymentView,
    pub(super) hint: String,
    pub(super) ran: Result<(), Error>,
    /// The run found no upload or usable image, so `hint` uploads one.
    pub(super) needs_upload: bool,
    pub(super) presented: bool,
}

impl Shipped {
    fn finish(self) -> Result<(), Error> {
        if self.presented {
            ui::emit(&Next::new(&self.view, Some(self.hint.clone())))?;
        } else {
            finish_view(&self.view, Some(self.hint.clone()))?;
        }
        if self.needs_upload {
            ui::hint(&Hint::Next(self.hint));
        }
        self.ran
    }
}

pub(super) fn execute(
    matches: &ArgMatches,
    store: &Store,
    admitted: &DeploymentSummary,
    source: Option<&Path>,
    events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<Shipped, Error> {
    let hint = show_hint(matches, admitted.number);
    let signal = CtrlC::subscribe()?;
    let mut runner = match store.local() {
        Some(_) if matches.get_flag("detach") => {
            return Err(Error::usage(
                "The hidden local Store runs Deployments in this process, so it can't detach",
            ));
        }
        Some(local) => Some(OwnedRunner {
            store,
            id: admitted.id.clone(),
            handle: Some(run_here(matches, local, admitted, source)?),
        }),
        None => None,
    };
    let detached = runner.is_none() && matches.get_flag("detach");
    let followed = if detached {
        store
            .read(&ployz_store::DeploymentQuery {
                id: admitted.id.clone(),
            })
            .map(|view| Followed {
                view,
                progress: None,
            })
    } else {
        follow(store, admitted, events, runner.as_ref(), &signal)
    };
    let mut followed = match followed {
        Ok(followed) => followed,
        Err(error) => {
            let cleanup = runner
                .as_mut()
                .map(|runner| runner.settle(true))
                .transpose();
            let error = match cleanup {
                Ok(_) => error,
                Err(cleanup) => error.with_diagnostic(ui::progress::Diagnostic {
                    failures: vec![ui::progress::Detail {
                        message: cleanup.to_string(),
                        causes: cleanup.causes(),
                    }],
                    ..Default::default()
                }),
            };
            return Err(if signal.token().is_cancelled() {
                error.interrupted()
            } else {
                error
            });
        }
    };
    if let Some(runner) = runner.as_mut() {
        let settled = runner.settle(signal.token().is_cancelled());
        let final_view = store.read(&ployz_store::DeploymentQuery {
            id: admitted.id.clone(),
        });
        match final_view {
            Ok(view) => followed.view = view,
            Err(_) => {
                if let Ok(Some(summary)) = &settled {
                    followed.view.deployment = summary.clone();
                }
            }
        }
        if let Err(error) = settled {
            let interrupted = signal.token().is_cancelled();
            finish_progress(
                &mut followed,
                if interrupted {
                    Disposition::LocalInterrupted
                } else {
                    Disposition::Settled
                },
            );
            ui::emit(&Next::new(&followed.view, Some(hint)))?;
            return Err(if interrupted {
                error.interrupted()
            } else {
                error
            });
        }
    }
    let interrupted = signal.token().is_cancelled();
    let disposition = if interrupted {
        if runner.is_some() {
            Disposition::LocalInterrupted
        } else if followed.view.deployment.status.in_flight() {
            Disposition::CloudContinues
        } else {
            Disposition::Settled
        }
    } else if progress::noop(&followed.view) {
        Disposition::UpToDate
    } else {
        Disposition::Settled
    };
    if !detached {
        finish_progress(&mut followed, disposition);
    }
    let view = followed.view;
    let ran = if interrupted {
        Err(Error::cancelled())
    } else if detached {
        Ok(())
    } else {
        progress::completion(&view)
    };
    // A run that found no upload or usable image for its Services says to upload.
    let needs_upload = matches!(
        &view.deployment.outcome,
        Some(ployz_store::Outcome::NotExecuted { needs_upload, .. }) if !needs_upload.is_empty()
    );
    let hint = if needs_upload {
        upload_again(&view.environment)
    } else {
        hint
    };
    Ok(Shipped {
        view,
        hint,
        ran,
        needs_upload,
        presented: !detached,
    })
}

/// Run the Deployment in this process, as Cloud's runner does, on the Cluster the
/// command line or context names; `source` is its upload.
fn run_here(
    matches: &ArgMatches,
    local: &std::sync::Arc<ConfigStore>,
    admitted: &DeploymentSummary,
    source: Option<&Path>,
) -> Result<std::thread::JoinHandle<Result<DeploymentSummary, Error>>, Error> {
    let direct = matches.get_one::<String>("connect").map(String::as_str);
    let context = matches.get_one::<String>("context").map(String::as_str);
    let connections = match crate::connect::resolve_connections(
        &super::config_path(matches)?,
        direct,
        context,
        Path::new(crate::connect::DEFAULT_LOCAL_SOCKET),
    ) {
        Ok(selected) => selected.connections,
        // Nothing named and no Cluster to reach: the runner records that nothing ran.
        Err(_) if direct.is_none() && context.is_none() => Vec::new(),
        Err(error) => return Err(error.into()),
    };
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

struct OwnedRunner<'store, 'matches> {
    store: &'store Store<'matches>,
    id: DeploymentId,
    handle: Option<std::thread::JoinHandle<Result<DeploymentSummary, Error>>>,
}

impl OwnedRunner<'_, '_> {
    fn finished(&self) -> bool {
        self.handle
            .as_ref()
            .is_none_or(|handle| handle.is_finished())
    }

    fn settle(&mut self, cancel: bool) -> Result<Option<DeploymentSummary>, Error> {
        let cancellation = if cancel && !self.finished() {
            self.store
                .write(&Cancel {
                    deployment: self.id.clone(),
                })
                .map(|_| ())
        } else {
            Ok(())
        };
        let ended = self
            .handle
            .take()
            .map(|handle| {
                handle.join().map_err(|_| {
                    Error::coded(RpcErrorCode::Internal, "The Deployment runner stopped")
                })
            })
            .transpose()?;
        let summary = ended.transpose()?;
        if let Err(error) = cancellation {
            let view = self.store.read(&ployz_store::DeploymentQuery {
                id: self.id.clone(),
            });
            if !view.is_ok_and(|view| !view.deployment.status.in_flight()) {
                return Err(error.context("Could not confirm that local Deployment work stopped."));
            }
        }
        Ok(summary)
    }
}

impl Drop for OwnedRunner<'_, '_> {
    fn drop(&mut self) {
        if self.handle.is_some() {
            let _ = self.settle(true);
        }
    }
}

struct Followed {
    view: DeploymentView,
    progress: Option<Progress>,
}

fn finish_progress(followed: &mut Followed, disposition: Disposition) {
    let frame = progress::frame(&followed.view);
    if let Some(progress) = followed.progress.take() {
        progress.finish(frame, disposition);
    } else if matches!(disposition, Disposition::UpToDate) {
        ui::note("Everything is up to date.");
    } else {
        Progress::start(frame.clone()).finish(frame, disposition);
    }
}

fn tap(
    view: &DeploymentView,
    events: &mut Option<std::io::BufWriter<std::fs::File>>,
    last: &mut Option<serde_json::Value>,
) {
    let event = serde_json::json!({ "type": "deployment", "status": view.deployment.status, "nodes": view.nodes });
    if last.as_ref() != Some(&event) {
        if let Some(file) = events.as_mut() {
            let _ = writeln!(file, "{event}");
            let _ = file.flush();
        }
        *last = Some(event);
    }
}

/// Follow only the owned local run, or Cloud's eligible replacement, retaining real last-known evidence.
fn follow(
    store: &Store,
    admitted: &DeploymentSummary,
    mut events: Option<std::io::BufWriter<std::fs::File>>,
    runner: Option<&OwnedRunner<'_, '_>>,
    signal: &CtrlC,
) -> Result<Followed, Error> {
    let mut last = None;
    let mut followed: Option<Followed> = None;
    let mut id = admitted.id.clone();
    loop {
        if signal.token().is_cancelled() {
            return followed.ok_or_else(Error::cancelled);
        }
        let read = store.read(&ployz_store::DeploymentQuery { id: id.clone() });
        if signal.token().is_cancelled() {
            if let Ok(view) = read {
                tap(&view, &mut events, &mut last);
                if let Some(followed) = followed.as_mut() {
                    followed.view = view;
                } else {
                    followed = Some(Followed {
                        view,
                        progress: None,
                    });
                }
            }
            return followed.ok_or_else(Error::cancelled);
        }
        let view = read?;
        let frame = progress::frame(&view);
        if let Some(followed) = followed.as_mut() {
            if let Some(progress) = followed.progress.as_mut() {
                progress.update(frame);
            } else if view.preview.as_ref().is_some_and(|preview| !preview.noop()) {
                followed.progress = Some(Progress::start(frame));
            }
            followed.view = view;
        } else {
            let progress = view
                .preview
                .as_ref()
                .filter(|preview| !preview.noop())
                .map(|_| Progress::start(frame));
            followed = Some(Followed { view, progress });
        }
        let current = followed
            .as_ref()
            .expect("a successful read installed its view");
        if runner.is_none() && current.view.deployment.status == DeploymentStatus::Superseded {
            if signal.token().is_cancelled() {
                return followed.ok_or_else(Error::cancelled);
            }
            let newer = replacement(store, &current.view);
            if signal.token().is_cancelled() {
                return followed.ok_or_else(Error::cancelled);
            }
            if let Some(newer) = newer? {
                id = newer.id;
                last = None;
                continue;
            }
        }
        tap(&current.view, &mut events, &mut last);
        if !current.view.deployment.status.in_flight() || runner.is_some_and(OwnedRunner::finished)
        {
            return followed.ok_or_else(Error::cancelled);
        }
        let deadline = std::time::Instant::now() + FOLLOW_POLL;
        while !signal.token().is_cancelled() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

/// The Environment's newest Deployment when it is newer than superseded `view` and targets every Service `view` did.
fn replacement(store: &Store, view: &DeploymentView) -> Result<Option<DeploymentSummary>, Error> {
    let page = store.read(&DeploymentsQuery {
        environment: EnvironmentRef {
            project: Some(view.environment.project.clone()),
            environment: Some(view.environment.name.clone()),
        },
        limit: Some(1),
        cursor: None,
    })?;
    Ok(page.deployments.into_iter().next().filter(|newer| {
        newer.number > view.deployment.number
            && ships_all(&view.deployment.services, &newer.services)
    }))
}

/// Whether a Deployment targeting `newer` ships every Service one targeting `ours` would; empty targets every Service.
fn ships_all(ours: &[ServiceName], newer: &[ServiceName]) -> bool {
    newer.is_empty() || !ours.is_empty() && ours.iter().all(|service| newer.contains(service))
}

fn plan(matches: &ArgMatches, store: &Store, services: Vec<ServiceName>) -> Result<(), Error> {
    let plan = store.read(&PlanQuery {
        environment: environment(matches)?,
        services,
    })?;
    let hint = store.again(&["--expect-version", plan.version.as_str()]);
    crate::ui::finish(&Next::new(&plan, Some(hint.clone())), || {
        let where_ = format!("{}/{}", plan.environment.project, plan.environment.name);
        if plan.changes.is_empty() {
            crate::ui::stream(format_args!("No authored changes to deploy in {where_}."));
        }
        for change in &plan.changes {
            crate::ui::stream(format_args!(
                "{} ({})",
                change.name,
                super::store::word(&change.lifecycle)
            ));
            for row in &change.settings {
                crate::ui::stream(format_args!(
                    "  {}: {} → {}",
                    row.path,
                    super::store::shown(&row.before),
                    super::store::shown(&row.after)
                ));
            }
        }
        if !plan.unresolved.is_empty() {
            ui::stream(format_args!(
                "Decided by the Servers when it runs: {}.",
                plan.unresolved.join(", ")
            ));
        }
        ui::hint(&Hint::Next(hint.clone()));
    })
}

/// "uploaded by nick, base abc1234 + changes": where an upload came from.
fn provenance(upload: &UploadedSource) -> String {
    let who = upload
        .uploader
        .as_ref()
        .map_or("this device", ployz_store::Principal::as_str);
    let digest = upload.digest.as_str();
    let digest = digest.get(..12).unwrap_or(digest);
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
pub(super) fn unreadable(dir: &Path) -> impl Fn(crate::build::Error) -> Error + '_ {
    move |error| {
        let message = format!("Could not read {}.", dir.display());
        let code = if matches!(error, crate::build::Error::Invalid(_)) {
            RpcErrorCode::InvalidArgument
        } else if !dir.exists() {
            RpcErrorCode::NotFound
        } else {
            RpcErrorCode::Internal
        };
        Error::caused(code, message, error)
    }
}

/// `dir` as an Uploaded Source: its content digest, and the commit it was checked out
/// at, if any, with whether it holds changes that commit doesn't.
fn uploaded_source(dir: &Path) -> Result<UploadedSource, Error> {
    let digest = crate::build::content_digest(dir).map_err(unreadable(dir))?;
    let digest =
        ployz_core::UploadDigest::parse(digest).expect("a content digest is a lowercase sha256");
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

fn ls(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let page = store(root)?.read(&DeploymentsQuery {
        environment: environment(matches)?,
        limit: matches.get_one::<usize>("limit").copied(),
        cursor: matches.get_one::<String>("cursor").cloned(),
    })?;
    let hint = page.next_cursor.as_ref().map(|cursor| {
        let mut words = vec!["deployment", "ls", "--cursor", cursor.as_str()];
        let limit = matches.get_one::<usize>("limit").map(ToString::to_string);
        if let Some(limit) = &limit {
            words.extend(["--limit", limit.as_str()]);
        }
        next(matches, &words)
    });
    let mut table = Table::new(
        ["DEPLOYMENT", "STATUS", "SAVED REVISION"],
        format!(
            "No Deployments in {}/{} yet.",
            page.environment.project, page.environment.name
        ),
    );
    for deployment in &page.deployments {
        table.row([
            Cell::from(format!("#{}", deployment.number)),
            Cell::status(
                super::store::word(&deployment.status),
                status_tone(deployment.status),
            ),
            Cell::from(deployment.saved.to_string()),
        ]);
    }
    ui::list(&Next::new(&page, hint.clone()), &table)?;
    if let Some(hint) = hint {
        ui::hint(&Hint::Next(hint));
    }
    Ok(())
}

/// How a Deployment's status reads at a glance.
const fn status_tone(status: DeploymentStatus) -> Tone {
    match status {
        DeploymentStatus::Applied => Tone::Good,
        DeploymentStatus::Failed | DeploymentStatus::Unknown => Tone::Bad,
        DeploymentStatus::Queued
        | DeploymentStatus::Superseded
        | DeploymentStatus::Running
        | DeploymentStatus::Cancelling
        | DeploymentStatus::Cancelled => Tone::Change,
    }
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
            .map_err(|_| Error::usage("Expected a Deployment ID or number"));
    };
    let found = store.read(&ployz_store::NumberedDeploymentQuery {
        environment: environment(matches)?,
        number,
    })?;
    Ok(found.deployment.id)
}

/// A refused retry, start or cancel points at the Deployment's current state.
fn refused<'a>(
    matches: &'a ArgMatches,
    store: &'a Store,
    id: &'a DeploymentId,
) -> impl FnOnce(StoreCallError) -> Error + 'a {
    move |error| {
        store.fail(super::store::with_next(
            error,
            |refusal| refusal.code == RpcErrorCode::Conflict,
            || {
                numbered(store, id)
                    .map_or_else(|| id_hint(id), |(number, _)| show_hint(matches, number))
            },
        ))
    }
}

/// `ployz deployment show N` in this command's Project and Environment.
pub(super) fn show_hint(matches: &ArgMatches, number: u64) -> String {
    next(matches, &["deployment", "show", &number.to_string()])
}

/// `ployz deployment show N` for Deployment `id` of `project`, in its own Environment.
pub(super) fn show_hint_in(store: &Store, project: &str, id: &DeploymentId) -> String {
    numbered(store, id).map_or_else(
        || id_hint(id),
        |(number, environment)| {
            shell_words::join([
                "ployz",
                "deployment",
                "show",
                &number.to_string(),
                "--project",
                project,
                "--env",
                &environment,
            ])
        },
    )
}

/// Deployment `id`'s number and Environment, when the Store can still say them.
fn numbered(store: &Store, id: &DeploymentId) -> Option<(u64, String)> {
    store
        .read(&ployz_store::DeploymentQuery { id: id.clone() })
        .ok()
        .map(|view| (view.deployment.number, view.environment.name.to_string()))
}

fn id_hint(id: &DeploymentId) -> String {
    shell_words::join(["ployz", "deployment", "show", id.as_str()])
}

fn retry(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let source = deployment_id(matches, &store, "id")?;
    let events = open_events(matches)?;
    let admitted = store
        .admit(&Admit::Retry(ployz_store::Retry {
            id: DeploymentId::parse(mint())?,
            deployment: source.clone(),
        }))
        .map_err(refused(matches, &store, &source))?;
    ship(matches, &store, &admitted, events)
}

fn start(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let id = deployment_id(matches, &store, "id")?;
    let events = open_events(matches)?;
    let queued = store
        .try_write(&Start {
            deployment: id.clone(),
        })
        .map_err(refused(matches, &store, &id))?;
    ship(matches, &store, &queued, events)
}

fn cancel(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let id = deployment_id(matches, &store, "id")?;
    store
        .try_write(&Cancel {
            deployment: id.clone(),
        })
        .map_err(refused(matches, &store, &id))?;
    let view = store.read(&ployz_store::DeploymentQuery { id: id.clone() })?;
    finish_view(&view, Some(show_hint(matches, view.deployment.number)))
}

fn show(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let id = deployment_id(matches, &store, "id")?;
    let view = store.read(&ployz_store::DeploymentQuery { id: id.clone() })?;
    finish_view(&view, None)
}

pub(super) fn finish_view(view: &DeploymentView, hint: Option<String>) -> Result<(), Error> {
    crate::ui::finish(&Next::new(view, hint), || say_view(view))
}

/// A Deployment as human text.
pub(super) fn say_view(view: &DeploymentView) {
    crate::ui::stream(format_args!(
        "Deployment #{} of {}/{}: {}",
        view.deployment.number,
        view.environment.project,
        view.environment.name,
        super::store::word(&view.deployment.status)
    ));
    if let Some(upload) = &view.deployment.upload {
        crate::ui::stream(format_args!("  {}", provenance(upload)));
    }
    if let Some(
        ployz_store::Outcome::NotExecuted { reason, cause, .. }
        | ployz_store::Outcome::Executed {
            reason: Some(reason),
            cause,
            ..
        },
    ) = &view.deployment.outcome
    {
        crate::ui::stream(format_args!("  {reason}"));
        if let Some(cause) = cause.last() {
            crate::ui::stream(format_args!("    cause: {cause}"));
        }
    }
    if super::teardown::left_on_old_servers(&view.deployment) {
        crate::ui::stream(format_args!(
            "  Left on old servers: no Server was left to take it off, so whatever ran there still runs"
        ));
    }
    for node in &view.nodes {
        crate::ui::stream(format_args!(
            "  {}: {}",
            node.node.name(),
            super::store::word(&node.outcome)
        ));
    }
    for build in &view.builds {
        let commit = build.commit.as_ref().map_or("the upload", |commit| {
            commit.as_str().get(..7).unwrap_or(commit.as_str())
        });
        let reason = build.message.as_deref().unwrap_or_default();
        crate::ui::stream(format_args!(
            "  build {} from {commit}: {} {reason}",
            build.service,
            super::store::word(&build.status)
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::ships_all;
    use ployz_core::ServiceName;

    #[test]
    fn a_newer_deployment_carries_a_superseded_one_only_when_it_ships_its_services() {
        let names = |names: &[&str]| -> Vec<ServiceName> {
            names
                .iter()
                .map(|name| ServiceName::parse(*name).unwrap())
                .collect()
        };
        assert!(ships_all(&names(&[]), &names(&[])));
        assert!(ships_all(&names(&["web"]), &names(&[])));
        assert!(ships_all(&names(&["web"]), &names(&["web", "db"])));
        assert!(!ships_all(&names(&["web", "db"]), &names(&["web"])));
        assert!(!ships_all(&names(&[]), &names(&["web"])));
    }
}
