//! `ployz deploy` and `ployz deployment`: ship an Environment from the Config Store,
//! and read, redeploy, start and cancel its Deployments. Cloud's runner runs a
//! Deployment admitted over HTTPS, and `deploy` follows it until it ends. With the
//! hidden in-process Store this CLI is the Deployment's runner: it claims it,
//! prepares and confirms it on the Cluster, and records what happened.

#[cfg(test)]
mod follow_tests;
mod progress;

use crate::ui::progress::{Disposition, Progress};
use crate::ui::{self, Cell, Hint, Table, Tone};
use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{ArgAction, ArgMatches, Command, ValueHint};
use ployz_core::{RpcErrorCode, ServiceName};
use ployz_store::{
    Admit, Cancel, CiVerdict, ConfigStore, Deploy, DeployedNode, DeploymentId, DeploymentStatus,
    DeploymentSummary, DeploymentView, DeploymentsQuery, EnvironmentRef, NodeStatus, PlanQuery,
    Publish, RunnerId, Start, UploadBase, UploadedSource, VolumeName,
};

use super::store::{Next, Store, environment, mint, next, scoped, store};
use super::{Error, leaf_matches, required, runtime};
use crate::cli::{base, positional, switch, value};
use crate::cloud_account::StoreCallError;

pub(super) fn deploy_command(command: Command) -> Command {
    following(
        scoped(base(
            command,
            "Deploy everything staged and follow the Deployment",
        ))
        .arg(
            positional("service", false)
                .action(ArgAction::Append)
                .help("Deploy only these Services [default: every Service]"),
        )
        .arg(
            switch("plan", None)
                .help("Show what would deploy, and any ingress it brings back; run nothing"),
        )
        .arg(
            switch("skip-deploy", None)
                .conflicts_with_all(["service", "plan", "upload", "events", "detach"])
                .help("Save everything staged for the next deploy instead of deploying it"),
        )
        .arg(
            switch("no-prune", None)
                .conflicts_with_all(["service", "skip-deploy", "prune"])
                .help(
                    "Keep Services running in the Environment that it doesn't hold. A deploy of \
                     every Service removes them by default",
                ),
        )
        // Hidden: kept so older scripts still run; a deploy of every Service prunes
        // without them.
        .arg(
            switch("prune", None)
                .conflicts_with_all(["service", "plan", "skip-deploy"])
                .hide(true),
        )
        .arg(crate::cli::yes("Does nothing; kept for older scripts").hide(true))
        .arg(crate::cli::reviewed_version())
        .arg(crate::cli::approval())
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
pub(super) const FOLLOW_POLL: Duration = Duration::from_secs(1);
/// How long a followed Deployment may sit queued before the follow says why it waits.
fn unclaimed_wait() -> Duration {
    crate::cli::env::seconds(crate::cli::env::DEPLOY_UNCLAIMED_SECONDS)
        .unwrap_or(Duration::from_secs(30))
}

pub(super) fn deployment_group(command: Command) -> Command {
    command.about("Read, redeploy, start and cancel Deployments")
}

pub(super) fn ls_command(command: Command) -> Command {
    scoped(command.about("List Deployments, newest first"))
        .arg(
            value("limit", None)
                .value_parser(clap::value_parser!(usize))
                .help("At most this many, 1-100 [default: 20]"),
        )
        .arg(value("cursor", None).help("The next_cursor of the previous page"))
}

pub(super) fn show_command(command: Command) -> Command {
    scoped(command.about("Show a Deployment with its Deploy Preview and Node Outcomes")).arg(id())
}

pub(super) fn redeploy_command(command: Command) -> Command {
    following(scoped(base(
        command,
        "Deploy an ended Deployment's commit and settings again, and follow it. Redeploying \
         one older than a Deployment that ran rolls that back until the next deploy, so it \
         asks first; --yes skips the ask. Outside a terminal without --yes it fails with \
         confirmation_required. A rollback that deletes Volume data needs \
         --accept-volume-loss.",
    )))
    .arg(id())
    .arg(crate::cli::yes("Roll back without asking"))
    .arg(crate::cli::volume_acceptance().help(format!(
        "{}; --yes cannot bypass this",
        crate::cli::VOLUME_ACCEPTANCE
    )))
    .arg(crate::cli::reviewed_version())
    .arg(crate::cli::approval())
}

pub(super) fn start_command(command: Command) -> Command {
    following(scoped(base(
        command,
        "Hand a queued Deployment to a runner now, and follow it",
    )))
    .arg(id())
}

pub(super) fn cancel_command(command: Command) -> Command {
    scoped(command.about("Cancel a Deployment: a queued one never runs, a running one stops"))
        .arg(id())
        .arg(crate::cli::approval())
}

fn id() -> clap::Arg {
    positional("id", true).help("The Deployment's ID, or its number in the Environment")
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
    let skip = matches.get_flag("skip-deploy");
    let no_prune = matches.get_flag("no-prune");
    let store = store(root)?
        .args(skip.then_some("--skip-deploy"))
        .args(no_prune.then_some("--no-prune"))
        .args(names.iter().copied())
        .args(upload.into_iter().flat_map(|source| ["--upload", source]))
        .args(
            message
                .into_iter()
                .flat_map(|message| ["--message", message]),
        )
        .args(following_args(matches));
    if skip {
        return save(matches, &store);
    }
    if matches.get_flag("plan") {
        return plan(matches, &store, services, no_prune);
    }
    let events = open_events(matches)?;
    let request = Request {
        environment: environment(matches)?,
        services,
        version: matches.get_one::<String>("expect-version").cloned(),
        source,
        accept: super::teardown::accepted(matches)?,
        message: matches.get_one::<String>("message").cloned(),
        no_prune,
    };
    upload_and_ship(matches, &store, request, events)?.finish()
}

/// `deploy --skip-deploy`: save everything staged for the next deploy.
fn save(matches: &ArgMatches, store: &Store) -> Result<(), Error> {
    let saving = Publish {
        id: None,
        environment: environment(matches)?,
        version: matches.get_one::<String>("expect-version").cloned(),
        message: matches.get_one::<String>("message").cloned(),
        accept_volume_loss: super::teardown::accepted(matches)?,
    };
    let saved = store
        .publish(&saving)
        .map_err(|error| store.accepting(store.at_review(error, "diff")))?;
    let where_ = format!("{}/{}", saved.environment.project, saved.environment.name);
    let Some(version) = saved.saved else {
        return ui::done(
            &Next::new(&saved, None),
            format_args!("Nothing staged in {where_}."),
        );
    };
    let hint = next(matches, &["deploy"]);
    ui::finish(&Next::new(&saved, Some(hint.clone())), || {
        if saved.created {
            ui::stream(format_args!(
                "Saved version #{version} of {where_}. Not deployed."
            ));
        } else {
            ui::stream(format_args!(
                "Version #{version} already holds everything staged in {where_}. Not deployed."
            ));
        }
        ui::hint(&Hint::Next(hint));
    })
}

/// What a Deploy admits; `source` is the directory it uploads first.
pub(super) struct Request {
    pub(super) environment: EnvironmentRef,
    pub(super) services: Vec<ServiceName>,
    pub(super) version: Option<String>,
    pub(super) source: Option<PathBuf>,
    pub(super) accept: Vec<VolumeName>,
    pub(super) message: Option<String>,
    /// Keep every Service running in the Environment that this Store never held.
    pub(super) no_prune: bool,
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
        no_prune,
    } = request;
    refuse_local_detach(matches, store)?;
    let listing = source
        .as_deref()
        .map(|dir| {
            crate::build::UploadListing::read(dir)
                .map(|listing| (dir, listing))
                .map_err(unreadable(dir))
        })
        .transpose()?;
    let upload = listing
        .as_ref()
        .map(|(dir, listing)| uploaded_source(dir, listing))
        .transpose()?;
    let id = DeploymentId::parse(mint())?;
    if let Some((dir, listing)) = &listing {
        store.upload(&id, dir, listing)?;
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
            prune: false,
            no_prune,
        }))
        .map_err(|error| store.accepting(store.at_review(error, "diff")))?;
    execute(matches, store, &admitted, source.as_deref(), events)
}

/// Refuse `--detach` on the hidden local Store before anything is queued: it runs
/// Deployments in this process, so nothing would run a detached one.
pub(super) fn refuse_local_detach(matches: &ArgMatches, store: &Store) -> Result<(), Error> {
    if store.local().is_some() && matches.get_flag("detach") {
        return Err(Error::usage(
            "The hidden local Store runs Deployments in this process, so it can't detach",
        ));
    }
    Ok(())
}

/// Open `--events` before queueing anything, so a bad path ships nothing.
pub(super) fn open_events(
    matches: &ArgMatches,
) -> Result<Option<std::io::BufWriter<std::fs::File>>, Error> {
    let Some(path) = matches.get_one::<String>("events") else {
        return Ok(None);
    };
    let file = std::fs::File::create(path).map_err(|error| {
        use std::io::ErrorKind;
        let kind = error.kind();
        let reason = if kind == ErrorKind::NotFound {
            "no such directory"
        } else if kind == ErrorKind::IsADirectory {
            "it is a directory"
        } else if kind == ErrorKind::PermissionDenied {
            "permission denied"
        } else {
            "it can't be created"
        };
        Error::detailed_caused(
            RpcErrorCode::InvalidArgument,
            format!("Can't write --events file {path}: {reason}"),
            serde_json::Value::Null,
            Some(error),
        )
    })?;
    Ok(Some(std::io::BufWriter::new(file)))
}

/// Run or follow a queued Deployment and report it. A failed Deploy exits 1; a failed
/// removal or shutdown, or an applied one missing a Server, exits 3.
fn ship(
    matches: &ArgMatches,
    store: &Store,
    admitted: &DeploymentSummary,
    events: Option<std::io::BufWriter<std::fs::File>>,
) -> Result<(), Error> {
    let shipped = execute(matches, store, admitted, None, events)?;
    let again = progress::removal_retry(&shipped.view, |words| super::rerun(matches, words));
    shipped.finish().map_err(|error| match again {
        Some(again) => error.hint(again),
        None => error,
    })
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
        if let Some(skipped) = prune_skipped(&self.view.deployment, self.view.preview.as_ref()) {
            ui::warn(skipped);
        }
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
    let hint = hint_for(matches, admitted);
    let (signal, _sigterm) = crate::cancellation::interrupted_or_terminated()?;
    let mut runner = match store.local() {
        // Admission can end it at once, with nothing to run.
        Some(local) if admitted.status.in_flight() => {
            let ended = tokio_util::sync::CancellationToken::new();
            Some(OwnedRunner {
                store,
                id: admitted.id.clone(),
                handle: Some(run_here(matches, local, admitted, source, ended.clone())?),
                ended,
            })
        }
        Some(_) | None => None,
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
                successor: None,
            })
    } else {
        follow(matches, store, admitted, events, runner.as_ref(), &signal)
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
            return Err(if signal.is_cancelled() {
                error.interrupted()
            } else {
                error
            });
        }
    };
    if let Some(runner) = runner.as_mut() {
        let settled = runner.settle(signal.is_cancelled());
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
            let interrupted = signal.is_cancelled();
            // A newer Deployment that took over the Environment left this one unknown:
            // name it rather than the store's refusal to record.
            let newer = (!interrupted
                && followed.view.deployment.status == DeploymentStatus::Unknown)
                .then(|| taken_over_by(store, &followed.view))
                .flatten();
            let error = match newer {
                Some(newer) => {
                    progress::replaced(&followed.view, &newer, |args| super::rerun(matches, args))
                }
                None => error,
            };
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
    let interrupted = signal.is_cancelled();
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
        for builder in progress::builders(&followed.view) {
            ui::note(builder);
        }
    }
    for warning in progress::value_warnings(&followed.view)
        .into_iter()
        .chain(progress::ingress_warnings(&followed.view))
    {
        ui::warn(warning);
    }
    let Followed {
        view, successor, ..
    } = followed;
    let ran = if interrupted {
        Err(Error::cancelled())
    } else if detached {
        Ok(())
    } else {
        let named: Vec<&str> = matches
            .try_get_many::<String>("service")
            .ok()
            .flatten()
            .into_iter()
            .flatten()
            .map(String::as_str)
            .collect();
        progress::completion(&view, successor.as_ref(), &named, |args| {
            super::rerun(matches, args)
        })
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
    ended: tokio_util::sync::CancellationToken,
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
        // Locals drop last to first, on a panic too. Dropping the runtime waits out its
        // blocking tasks, so it goes before `ended` wakes following.
        let _ended = ended.drop_guard();
        let runtime = runtime;
        runtime
            .block_on(crate::sdk::run_deployment(
                ployz_core::Operator::Cli,
                local,
                id,
                runner,
                connections,
                Ok(sources),
            ))
            .map_err(Error::from)
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
    /// Cancelled when the runner thread returns, so following wakes at once.
    ended: tokio_util::sync::CancellationToken,
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
    /// The newer Deployment that superseded it without shipping all of its Services.
    successor: Option<DeploymentSummary>,
}

fn finish_progress(followed: &mut Followed, disposition: Disposition) {
    let frame = progress::frame(&followed.view);
    let up_to_date = matches!(disposition, Disposition::UpToDate);
    if let Some(progress) = followed.progress.take() {
        progress.finish(frame, disposition);
    } else if !up_to_date {
        Progress::start(frame.clone()).finish(frame, disposition);
    }
    if up_to_date {
        ui::note(progress::up_to_date(&followed.view));
    }
}

fn tap(
    view: &DeploymentView,
    events: &mut Option<std::io::BufWriter<std::fs::File>>,
    last: &mut Option<serde_json::Value>,
) {
    let event = serde_json::json!({
        "type": "deployment",
        "id": view.deployment.id,
        "number": view.deployment.number,
        "status": view.deployment.status,
        "nodes": view.nodes,
    });
    if last.as_ref() != Some(&event) {
        if let Some(file) = events.as_mut() {
            let _ = writeln!(file, "{event}");
            let _ = file.flush();
        }
        *last = Some(event);
    }
}

fn follow(
    matches: &ArgMatches,
    store: &Store,
    admitted: &DeploymentSummary,
    mut events: Option<std::io::BufWriter<std::fs::File>>,
    runner: Option<&OwnedRunner<'_, '_>>,
    signal: &tokio_util::sync::CancellationToken,
) -> Result<Followed, Error> {
    let wait_runtime = runtime()?;
    let mut last = None;
    let mut followed: Option<Followed> = None;
    let mut id = admitted.id.clone();
    let mut tracked = admitted.clone();
    // A Cloud Deployment runs on without this process; a lost read says which one and where to watch it.
    let lost = |error: Error, tracked: &DeploymentSummary| {
        if runner.is_some() || error.report().code != RpcErrorCode::Unavailable {
            return error;
        }
        error
            .context(format!(
                "Stopped following Deployment {}; it keeps running in Ployz Cloud.",
                tracked.label()
            ))
            .hint(Hint::Inspect(hint_for(matches, tracked)))
    };
    let mut unclaimed = Some(std::time::Instant::now() + unclaimed_wait());
    let mut ended = runner
        .map(|runner| runner.ended.clone())
        .unwrap_or_default();
    loop {
        if signal.is_cancelled() {
            return followed.ok_or_else(Error::cancelled);
        }
        let read = store.read(&ployz_store::DeploymentQuery { id: id.clone() });
        if signal.is_cancelled() {
            if let Ok(view) = read {
                tap(&view, &mut events, &mut last);
                if let Some(followed) = followed.as_mut() {
                    followed.view = view;
                } else {
                    followed = Some(Followed {
                        view,
                        progress: None,
                        successor: None,
                    });
                }
            }
            return followed.ok_or_else(Error::cancelled);
        }
        let view = read.map_err(|error| lost(error, &tracked))?;
        let frame = progress::frame(&view);
        if let Some(followed) = followed.as_mut() {
            if let Some(progress) = followed.progress.as_mut() {
                progress.update(frame);
            } else if progress::visible(&view) {
                followed.progress = Some(Progress::start(frame));
            }
            followed.view = view;
        } else {
            let progress = progress::visible(&view).then(|| Progress::start(frame));
            followed = Some(Followed {
                view,
                progress,
                successor: None,
            });
        }
        let current = followed
            .as_ref()
            .expect("a successful read installed its view");
        if runner.is_none() && current.view.deployment.status == DeploymentStatus::Superseded {
            if signal.is_cancelled() {
                return followed.ok_or_else(Error::cancelled);
            }
            let newer = replacement(store, &current.view);
            if signal.is_cancelled() {
                return followed.ok_or_else(Error::cancelled);
            }
            let newer = newer.map_err(|error| {
                if error.report().code != RpcErrorCode::Unavailable {
                    return error;
                }
                error
                    .context(format!(
                        "Deployment {} was superseded, and Ployz Cloud stopped answering before its replacement could be read.",
                        tracked.label()
                    ))
                    .hint(Hint::Inspect(next(matches, &["deployment", "ls"])))
            })?;
            match newer {
                Some(Replacement::Follow(newer)) => {
                    id = newer.id.clone();
                    tracked = newer;
                    last = None;
                    continue;
                }
                Some(Replacement::Name(newer)) => {
                    if let Some(followed) = followed.as_mut() {
                        followed.successor = Some(newer);
                    }
                }
                None => {}
            }
        }
        let current = followed
            .as_ref()
            .expect("a successful read installed its view");
        tap(&current.view, &mut events, &mut last);
        if current.view.deployment.status != DeploymentStatus::Queued {
            unclaimed = None;
        } else if runner.is_none()
            && unclaimed.is_some_and(|after| std::time::Instant::now() >= after)
        {
            unclaimed = None;
            if let Some(ahead) = ahead(store, &current.view) {
                ui::note(format_args!(
                    "Deployment {} waits for Deployment {} to finish.",
                    current.view.deployment.label(),
                    ahead.label()
                ));
                ui::hint(&Hint::Inspect(hint_for(matches, &ahead)));
            } else {
                ui::note(format_args!(
                    "No Server has started Deployment {} yet; it stays queued until one does.",
                    current.view.deployment.label()
                ));
                ui::hint(&Hint::Inspect("ployz server ls".into()));
            }
        }
        if !current.view.deployment.status.in_flight() || runner.is_some_and(OwnedRunner::finished)
        {
            return followed.ok_or_else(Error::cancelled);
        }
        wait_runtime.block_on(async {
            tokio::select! {
                () = signal.cancelled() => {},
                () = ended.cancelled() => {},
                () = tokio::time::sleep(FOLLOW_POLL) => {},
            }
        });
        // The thread may still be exiting; later reads fall back to the poll.
        if ended.is_cancelled() {
            ended = tokio_util::sync::CancellationToken::new();
        }
    }
}

/// The oldest Deployment in `view`'s Environment still in flight ahead of it, which it waits behind.
/// A failed read names none: the follow's next read reports it.
fn ahead(store: &Store, view: &DeploymentView) -> Option<DeploymentSummary> {
    let page = store
        .read(&DeploymentsQuery {
            environment: EnvironmentRef {
                project: Some(view.environment.project.clone()),
                environment: Some(view.environment.name.clone()),
            },
            limit: None,
            cursor: None,
        })
        .ok()?;
    let ours = view.deployment.number?;
    page.in_flight
        .into_iter()
        .filter(|older| older.number.is_some_and(|number| number < ours))
        .min_by_key(|older| older.number)
}

/// The newer Deployment that took `view`'s Environment over, among its newest page,
/// the latest and those in flight. None when none did or the read fails.
fn taken_over_by(store: &Store, view: &DeploymentView) -> Option<DeploymentSummary> {
    let page = store
        .read(&DeploymentsQuery {
            environment: EnvironmentRef {
                project: Some(view.environment.project.clone()),
                environment: Some(view.environment.name.clone()),
            },
            limit: None,
            cursor: None,
        })
        .ok()?;
    let candidates: Vec<DeploymentSummary> = page
        .deployments
        .into_iter()
        .chain(page.latest)
        .chain(page.in_flight)
        .collect();
    progress::taker(view, &candidates).cloned()
}

/// What took superseded `view`'s place, among the latest and those in flight.
enum Replacement {
    /// The newest newer Deployment that ships every Service `view` targeted: follow it.
    Follow(DeploymentSummary),
    /// Else the newest newer one: name it.
    Name(DeploymentSummary),
}

fn replacement(store: &Store, view: &DeploymentView) -> Result<Option<Replacement>, Error> {
    let page = store.read(&DeploymentsQuery {
        environment: EnvironmentRef {
            project: Some(view.environment.project.clone()),
            environment: Some(view.environment.name.clone()),
        },
        limit: None,
        cursor: None,
    })?;
    let ours = &view.deployment.services;
    // The latest may ship other Services while an older one in flight ships ours:
    // the latest, then the in-flight ones newest first, whatever page they fall on.
    let latest = page.latest.as_ref().map(|latest| latest.id.clone());
    let in_flight = page
        .in_flight
        .into_iter()
        .filter(|newer| latest.as_ref() != Some(&newer.id));
    let mut newest = None;
    for newer in page.latest.into_iter().chain(in_flight) {
        if newer.number <= view.deployment.number {
            continue;
        }
        if newest.is_none() {
            newest = Some(newer.clone());
        }
        // A narrowed one also ships, as Service nodes, the Services its named ones
        // reference; a node it left unchanged or never reached ships nothing.
        let carries = if newer.services.is_empty() || ours.is_empty() {
            ships_all(ours, &newer.services)
        } else {
            let Ok(read) = store.read(&ployz_store::DeploymentQuery {
                id: newer.id.clone(),
            }) else {
                continue;
            };
            let shipped: Vec<ServiceName> = read
                .nodes
                .into_iter()
                .filter_map(|node| match (node.node, node.outcome) {
                    (
                        DeployedNode::Service { name, .. },
                        NodeStatus::Pending
                        | NodeStatus::Deployed
                        | NodeStatus::Failed
                        | NodeStatus::Unknown,
                    ) => Some(name),
                    (
                        DeployedNode::Service { .. },
                        NodeStatus::Unchanged | NodeStatus::NotAttempted | NodeStatus::Removed,
                    )
                    | (DeployedNode::Volume { .. } | DeployedNode::Config { .. }, _) => None,
                })
                .collect();
            ours.iter().all(|service| shipped.contains(service))
        };
        if carries {
            return Ok(Some(Replacement::Follow(newer)));
        }
    }
    Ok(newest.map(Replacement::Name))
}

/// Whether a Deployment targeting `newer` ships every Service one targeting `ours` would; empty targets every Service.
fn ships_all(ours: &[ServiceName], newer: &[ServiceName]) -> bool {
    newer.is_empty() || !ours.is_empty() && ours.iter().all(|service| newer.contains(service))
}

/// `ployz deploy --plan --json`: the plan, plus each domain change as people read it.
#[derive(serde::Serialize)]
struct Planned<'p> {
    #[serde(flatten)]
    plan: &'p ployz_store::PlanView,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    domain_changes: Vec<super::store::DomainLine>,
    /// What the ingress step would do on each Server; `None` when it couldn't check.
    #[serde(skip_serializing_if = "Option::is_none")]
    ingress: Option<Vec<ployz_store::IngressRepair>>,
    /// Why ingress, or an ingress Server, went unchecked.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ingress_unchecked: Vec<String>,
    /// Whether it also removes every other Service running in the Environment,
    /// which only the Servers can name.
    prunes: bool,
}

fn plan(
    matches: &ArgMatches,
    store: &Store,
    services: Vec<ServiceName>,
    no_prune: bool,
) -> Result<(), Error> {
    let prunes = services.is_empty() && !no_prune;
    let plan = store.read(&PlanQuery {
        environment: environment(matches)?,
        services,
    })?;
    let hint = store.again(&["--expect-version", plan.version.as_str()]);
    let (ingress, ingress_unchecked) = match ingress_check(matches) {
        Ok(check) => (Some(check.repairs), check.unchecked),
        Err(Unchecked { cause, silent }) => {
            if let Some(silent) = silent {
                ui::warn(cant_run(&silent));
            }
            (None, vec![cause])
        }
    };
    let shown = Planned {
        domain_changes: super::store::domain_changes(
            plan.changes.iter().flat_map(|change| &change.settings),
        ),
        plan: &plan,
        ingress,
        ingress_unchecked,
        prunes,
    };
    crate::ui::finish(&Next::new(&shown, Some(hint.clone())), || {
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
                for line in super::store::changed(row, &change.settings) {
                    crate::ui::stream(format_args!("  {line}"));
                }
            }
            super::review::restarts(change);
        }
        if prunes {
            crate::ui::stream(format_args!(
                "Also removes any other service running in this environment; --no-prune keeps them."
            ));
        }
        for repair in shown.ingress.iter().flatten() {
            let verb = progress::ingress_verb(repair.action);
            match &repair.failure {
                None => {
                    crate::ui::stream(format_args!("ingress on {}: would {verb}", repair.server))
                }
                Some(
                    ployz_store::IngressFailure::Failed { cause }
                    | ployz_store::IngressFailure::PortTaken { cause },
                ) => crate::ui::stream(format_args!(
                    "ingress on {}: won't {verb}: {cause}",
                    repair.server
                )),
            }
        }
        for cause in &shown.ingress_unchecked {
            crate::ui::stream(format_args!(
                "Couldn't check ingress: {}.",
                cause.trim_end_matches('.')
            ));
        }
        if let Some(undecided) = undecided(&plan.unresolved) {
            ui::stream(format_args!("{undecided}"));
        }
        ui::hint(&Hint::Next(hint.clone()));
    })
}

/// The plan's line for what only the Servers decide, as people read it.
fn undecided(unresolved: &[String]) -> Option<String> {
    if unresolved.is_empty() {
        return None;
    }
    let unresolved: Vec<_> = unresolved
        .iter()
        .map(|item| match item.as_str() {
            "operations" => "which containers start and stop",
            "volume data" => "which volume data is deleted",
            other => other,
        })
        .collect();
    Some(format!(
        "Decided by the Servers when it runs: {}.",
        unresolved.join(", ")
    ))
}

/// The plan's warning when no Server answers: the deploy waits on one.
fn cant_run(silent: &super::server::Silent) -> String {
    let until = if let [_] = silent.names() {
        "it does"
    } else {
        "one does"
    };
    format!(
        "{silent}, so this deploy can't run until {until}. {}",
        silent.next()
    )
}

/// Why ingress went unchecked, and whether that is because no Server answered.
struct Unchecked {
    cause: String,
    silent: Option<super::server::Silent>,
}

fn ingress_check(matches: &ArgMatches) -> Result<crate::ingress::IngressCheck, Unchecked> {
    let context = matches
        .try_get_one::<String>("context")
        .ok()
        .flatten()
        .map(String::as_str);
    let unchecked = |cause| Unchecked {
        cause,
        silent: None,
    };
    runtime()
        .map_err(|error| unchecked(error.to_string()))?
        .block_on(async {
            let mut client = super::server::connect(matches, context)
                .await
                .map_err(|error| Unchecked {
                    cause: crate::ingress::unchecked(&error, None, None),
                    silent: super::server::Silent::of(&error),
                })?;
            let server = client.server();
            crate::ingress::planned_slots(&mut client)
                .await
                .map_err(|error| unchecked(crate::ingress::unchecked(&error, server, None)))
        })
}

/// "uploaded by nick, base abc1234 + changes": where `deployment`'s upload came from,
/// and whether that may still change before it starts.
fn provenance(deployment: &DeploymentSummary) -> Option<String> {
    let upload = deployment.upload.as_ref()?;
    let who = upload
        .uploader
        .as_ref()
        .map_or("this device", ployz_store::Principal::as_str);
    let digest = upload.digest.as_str();
    let digest = digest.get(..12).unwrap_or(digest);
    let source = match &upload.base {
        Some(base) => format!(
            "uploaded by {who}, base {}{}",
            base.commit.as_str().get(..7).unwrap_or_default(),
            if base.changed { " + changes" } else { "" }
        ),
        None => format!("uploaded by {who}, sha256 {digest}"),
    };
    Some(if deployment.chooses_upload_at_start {
        format!("{source} for now; chosen again when it starts")
    } else {
        source
    })
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
fn uploaded_source(
    dir: &Path,
    listing: &crate::build::UploadListing,
) -> Result<UploadedSource, Error> {
    let digest = listing.digest().map_err(unreadable(dir))?;
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

pub(super) fn ls(root: &ArgMatches) -> Result<(), Error> {
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
            Cell::from(deployment.label()),
            Cell::status(status_word(deployment), status_tone(deployment)),
            Cell::from(deployment.saved.to_string()),
        ]);
    }
    ui::list(&Next::new(&page, hint.clone()), &table)?;
    if let Some(hint) = hint {
        ui::hint(&Hint::Next(hint));
    }
    Ok(())
}

/// A Deployment's status in a word or two; a push waiting for CI says how its CI stands,
/// and a running one without a Deploy Preview yet reads preparing.
fn status_word(deployment: &DeploymentSummary) -> String {
    match (deployment.status, deployment.ci) {
        (DeploymentStatus::WaitingForCi, Some(CiVerdict::Failed)) => "CI failed".to_owned(),
        (DeploymentStatus::WaitingForCi, _) => "waiting for CI".to_owned(),
        (DeploymentStatus::Running, _) if deployment.preparing => "preparing".to_owned(),
        (status, _) => super::store::word(&status),
    }
}

/// How a Deployment's status reads at a glance.
const fn status_tone(deployment: &DeploymentSummary) -> Tone {
    match deployment.status {
        DeploymentStatus::Applied => Tone::Good,
        DeploymentStatus::Failed | DeploymentStatus::Unknown => Tone::Bad,
        DeploymentStatus::WaitingForCi => match deployment.ci {
            Some(CiVerdict::Failed) => Tone::Bad,
            Some(CiVerdict::Pending) | None => Tone::Change,
        },
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
        number: Some(number),
    })?;
    Ok(found.deployment.id)
}

/// A refused redeploy, start or cancel points at the Deployment's current state,
/// unless the Store names what to run next, such as `ployz server add`.
fn refused<'a>(
    matches: &'a ArgMatches,
    store: &'a Store,
    id: &'a DeploymentId,
) -> impl FnOnce(StoreCallError) -> Error + 'a {
    move |error| {
        store.fail(super::store::with_next(
            error,
            |refusal| {
                refusal.code == RpcErrorCode::Conflict && refusal.details.get("next").is_none()
            },
            || {
                store
                    .read(&ployz_store::DeploymentQuery { id: id.clone() })
                    .ok()
                    .map_or_else(|| id_hint(id), |view| hint_for(matches, &view.deployment))
            },
        ))
    }
}

/// `ployz deployment show N` in this command's Project and Environment.
pub(super) fn show_hint(matches: &ArgMatches, number: u64) -> String {
    next(matches, &["deployment", "show", &number.to_string()])
}

/// `ployz deployment show` for `deployment`: by number once it queued, by ID while it waits for CI.
pub(super) fn hint_for(matches: &ArgMatches, deployment: &DeploymentSummary) -> String {
    deployment.number.map_or_else(
        || id_hint(&deployment.id),
        |number| show_hint(matches, number),
    )
}

fn id_hint(id: &DeploymentId) -> String {
    shell_words::join(["ployz", "deployment", "show", id.as_str()])
}

pub(super) fn redeploy(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let yes = matches.get_flag("yes");
    // A refusal to delete data names this command again, with the same answer.
    let mut words: Vec<&str> = matches
        .get_one::<String>("id")
        .map(String::as_str)
        .into_iter()
        .collect();
    if yes {
        words.push("--yes");
    }
    let store = store(root)?.args(words);
    let source = deployment_id(matches, &store, "id")?;
    refuse_local_detach(matches, &store)?;
    // The Store refuses when what it rolls back changed after the ask.
    let rolls_back = match yes {
        true => None,
        false => Some(confirm_rollback(&store, &source)?),
    };
    let events = open_events(matches)?;
    let admitted = store
        .admit(&Admit::Retry(ployz_store::Retry {
            id: DeploymentId::parse(mint())?,
            deployment: source.clone(),
            rolls_back,
            version: matches.get_one::<String>("expect-version").cloned(),
            accept_volume_loss: super::teardown::accepted(matches)?,
        }))
        .map_err(|error| {
            let asks = matches!(&error, StoreCallError::Refused(refusal)
                if refusal.code == RpcErrorCode::ConfirmationRequired);
            match asks {
                true => store.accepting(store.at_review(error, "diff")),
                false => refused(matches, &store, &source)(error),
            }
        })?;
    ship(matches, &store, &admitted, events)
}

/// Ask before redeploying `source` rolls back the Deployments that ran after it.
/// Returns what the person confirmed it rolls back.
fn confirm_rollback(store: &Store, source: &DeploymentId) -> Result<Vec<u64>, Error> {
    let view = store.read(&ployz_store::DeploymentQuery { id: source.clone() })?;
    let Some(undoes) = crate::approval::deployments(&view.rolls_back) else {
        return Ok(Vec::new());
    };
    // Only a Deployment that ran can be rolled back to, and every one that ran is numbered.
    let number = view.deployment.label();
    ui::note(format!("Redeploying {number} undoes {undoes}."));
    ui::confirm(
        &format!("Roll back to {number}?"),
        || {
            Error::detailed(
                RpcErrorCode::ConfirmationRequired,
                format!("Rolling back to {number} needs confirmation. Nothing deployed."),
                serde_json::json!({ "deployment": view.deployment.number, "rolls_back": view.rolls_back }),
            )
            .hint(Hint::Retry(super::typed_with(&["--yes"])))
        },
        "Cancelled. Nothing deployed.",
    )?;
    Ok(view.rolls_back)
}

/// What a Deployment of every Service says of its prune: that it keeps what the
/// Environment lacks, when admitted with `--no-prune`, or why the Servers skipped
/// the prune. Nothing when it prunes, since each Service it removes is named.
fn prune_line(
    deployment: &DeploymentSummary,
    preview: Option<&ployz_core::DeployPreview>,
) -> Option<String> {
    if !deployment.services.is_empty() {
        return None;
    }
    if !deployment.prune {
        return Some("  Keeps services not in this environment.".to_owned());
    }
    prune_skipped(deployment, preview).map(|skipped| format!("  {skipped}"))
}

/// Why a Deployment admitted to prune removed nothing it lacks, said as the deploy
/// ends: a plain deploy prunes, so a skipped prune must not pass in silence.
fn prune_skipped(
    deployment: &DeploymentSummary,
    preview: Option<&ployz_core::DeployPreview>,
) -> Option<String> {
    if !deployment.prune || !deployment.services.is_empty() {
        return None;
    }
    Some(match preview?.prune_refusal.as_ref()? {
        ployz_core::PruneRefusal::IncompleteSnapshot => "Prune skipped: not every Server \
            answered, so services not in this environment stay until a deploy can check \
            them all."
            .to_owned(),
        refusal @ (ployz_core::PruneRefusal::SelectedServices
        | ployz_core::PruneRefusal::RemovalsOnly) => format!("Prune skipped: {refusal}"),
    })
}

pub(super) fn start(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    let store = store(root)?;
    let id = deployment_id(matches, &store, "id")?;
    refuse_local_detach(matches, &store)?;
    let events = open_events(matches)?;
    let queued = store
        .try_write_checked(
            &Start {
                deployment: id.clone(),
            },
            &["deployment", "ls"],
        )
        .map_err(refused(matches, &store, &id))?;
    ship(matches, &store, &queued, events)
}

pub(super) fn cancel(root: &ArgMatches) -> Result<(), Error> {
    let matches = leaf_matches(root);
    // An approval refusal names this command again, with the same Deployment.
    let words: Vec<&str> = matches
        .get_one::<String>("id")
        .map(String::as_str)
        .into_iter()
        .collect();
    let store = store(root)?.args(words);
    let id = deployment_id(matches, &store, "id")?;
    store
        .try_write_checked(
            &Cancel {
                deployment: id.clone(),
            },
            &["deployment", "ls"],
        )
        .map_err(refused(matches, &store, &id))?;
    let view = store.read(&ployz_store::DeploymentQuery { id: id.clone() })?;
    finish_view(&view, Some(hint_for(matches, &view.deployment)))
}

pub(super) fn show(root: &ArgMatches) -> Result<(), Error> {
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
        "Deployment {} of {}/{}: {}",
        view.deployment.label(),
        view.environment.project,
        view.environment.name,
        status_word(&view.deployment)
    ));
    if let Some(provenance) = provenance(&view.deployment) {
        crate::ui::stream(format_args!("  {provenance}"));
    }
    if let Some(line) = prune_line(&view.deployment, view.preview.as_ref()) {
        crate::ui::stream(format_args!("{line}"));
    }
    for warning in view.preview.iter().flat_map(|preview| &preview.warnings) {
        if let ployz_core::DeployWarning::NotInStore { .. } = warning {
            crate::ui::stream(format_args!("  ! {warning}"));
        }
    }
    if let Some(
        ployz_store::Outcome::NotExecuted { reason, cause, .. }
        | ployz_store::Outcome::Executed {
            reason: Some(reason),
            cause,
            cancelled: false,
            ..
        },
    ) = &view.deployment.outcome
    {
        crate::ui::stream(format_args!("  {reason}"));
        if let Some(cause) = cause.last() {
            crate::ui::stream(format_args!("    cause: {cause}"));
        }
    }
    if let Some(ployz_store::Outcome::Executed { services, .. }) = &view.deployment.outcome {
        for line in service_outcome_lines(services) {
            crate::ui::stream(format_args!("{line}"));
        }
    }
    if super::teardown::left_on_old_servers(&view.deployment) {
        crate::ui::stream(format_args!(
            "  Left on old servers: no Server was left to take it off, so whatever ran there still runs"
        ));
    }
    for node in &view.nodes {
        let outcome = if node.outcome == ployz_store::NodeStatus::Failed && node.cancelled {
            "cancelled".to_owned()
        } else {
            super::store::word(&node.outcome)
        };
        crate::ui::stream(format_args!("  {}: {outcome}", node.node.name()));
    }
    for line in untargeted_lines(&view.untargeted) {
        crate::ui::stream(format_args!("{line}"));
    }
    for build in &view.builds {
        let commit = build.commit.as_ref().map_or("the upload", |commit| {
            commit.as_str().get(..7).unwrap_or(commit.as_str())
        });
        let reason = build.message.as_deref().unwrap_or_default();
        let on = build
            .server
            .as_deref()
            .map_or_else(String::new, |server| format!(" on {server}"));
        crate::ui::stream(format_args!(
            "  build {} from {commit}: {}{on} {reason}",
            build.service,
            super::store::word(&build.status)
        ));
        if let Some(why) = &build.github_skipped {
            crate::ui::stream(format_args!("    GitHub skipped it: {why}"));
        }
    }
}

/// How many Servers removed each Service the Deployment's Store doesn't hold.
fn untargeted_lines(
    untargeted: &BTreeMap<ServiceName, Vec<ployz_store::ServerRow>>,
) -> Vec<String> {
    untargeted
        .iter()
        .map(|(service, rows)| {
            let removed = rows
                .iter()
                .filter(|row| row.state == ployz_store::RowState::Completed)
                .count();
            format!(
                "  {service}: removed {removed}/{} (not in this environment)",
                rows.len()
            )
        })
        .collect()
}

fn service_outcome_lines(services: &[ployz_core::ServiceOutcome]) -> Vec<String> {
    services
        .iter()
        .map(|ployz_core::ServiceOutcome { service, result }| {
            let words = match result {
                ployz_core::ServiceResult::New => "serves its new copies",
                ployz_core::ServiceResult::Old => "not switched to its new copies",
                ployz_core::ServiceResult::Updated => "serves its copies with their new limits",
                ployz_core::ServiceResult::NotStarted => "not started",
                ployz_core::ServiceResult::Unconfirmed => {
                    "may serve its new copies: the switch was not confirmed"
                }
                ployz_core::ServiceResult::Newer => "a newer deploy serves it",
            };
            format!("  {service}: {words}")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{
        Tone, open_events, provenance, prune_line, prune_skipped, service_outcome_lines, ships_all,
        status_tone, status_word, undecided, untargeted_lines,
    };
    use ployz_core::{RpcErrorCode, ServiceName};

    #[test]
    fn a_deployment_speaks_of_its_prune_only_when_it_kept_services_or_the_servers_refused() {
        let deployment = |services: &[&str], prune: bool| -> ployz_store::DeploymentSummary {
            serde_json::from_value(serde_json::json!({
                "id": "00000000-0000-4000-8000-000000000101",
                "environment_id": "00000000-0000-4000-8000-000000000002",
                "number": 1, "status": "applied", "saved": 1, "services": services,
                "runner": null, "upload": null, "remove": false, "prune": prune,
                "admitted_by": null, "admitted_at": 0, "started_at": null, "ended_at": null,
                "message": null, "push": null, "ci": null, "in_flight": false, "outcome": null,
            }))
            .unwrap()
        };
        let preview = |refusal: Option<&str>| -> ployz_core::DeployPreview {
            serde_json::from_value(serde_json::json!({
                "namespace": "shop-production", "operations": [], "warnings": [],
                "would_remove": [], "preserved_volumes": [], "prune_refusal": refusal
            }))
            .unwrap()
        };
        let refused = preview(Some("incomplete_snapshot"));
        let skipped = "Prune skipped: not every Server answered, so services not in this \
                       environment stay until a deploy can check them all.";
        assert_eq!(
            prune_line(&deployment(&[], true), Some(&refused)),
            Some(format!("  {skipped}"))
        );
        assert_eq!(
            prune_skipped(&deployment(&[], true), Some(&refused)).as_deref(),
            Some(skipped)
        );
        assert_eq!(prune_skipped(&deployment(&[], false), Some(&refused)), None);
        assert_eq!(
            prune_skipped(&deployment(&["web"], true), Some(&refused)),
            None
        );
        // What it removed is listed by name, so a prune that ran says nothing more.
        assert_eq!(
            prune_line(&deployment(&[], true), Some(&preview(None))),
            None
        );
        assert_eq!(prune_line(&deployment(&[], true), None), None);
        assert_eq!(
            prune_line(&deployment(&[], false), None).as_deref(),
            Some("  Keeps services not in this environment.")
        );
        assert_eq!(
            prune_line(&deployment(&["web"], false), Some(&refused)),
            None
        );
    }

    #[test]
    fn the_plan_says_what_the_servers_decide_in_plain_words() {
        assert_eq!(undecided(&[]), None);
        assert_eq!(
            undecided(&["operations".to_owned(), "volume data".to_owned()]).as_deref(),
            Some(
                "Decided by the Servers when it runs: which containers start and stop, \
                 which volume data is deleted."
            )
        );
    }

    #[test]
    fn an_events_path_that_cannot_be_written_names_the_flag_and_the_path() {
        let refused = |path: &str| {
            let root = crate::cli::command()
                .try_get_matches_from(["ployz", "deploy", "--events", path])
                .unwrap();
            open_events(super::leaf_matches(&root))
                .unwrap_err()
                .report()
        };
        let missing = refused("/nonexistent/dir/ev.ndjson");
        assert_eq!(missing.code, RpcErrorCode::InvalidArgument);
        assert_eq!(missing.cause.len(), 1, "{missing:?}");
        assert_eq!(
            missing.message,
            "Can't write --events file /nonexistent/dir/ev.ndjson: no such directory"
        );
        let directory = std::env::temp_dir();
        let directory = directory.to_str().unwrap();
        assert_eq!(
            refused(directory).message,
            format!("Can't write --events file {directory}: it is a directory")
        );
    }

    #[test]
    fn a_service_this_store_doesnt_hold_reads_how_many_copies_it_removed() {
        let rows: std::collections::BTreeMap<ServiceName, Vec<ployz_store::ServerRow>> =
            serde_json::from_value(serde_json::json!({
                "cache": [
                    {"machine_id": "a".repeat(32), "server": "alpha", "state": "completed", "started_at": 1, "finished_at": 2},
                    {"machine_id": "b".repeat(32), "server": "beta", "state": "not_attempted", "started_at": null, "finished_at": 2}
                ],
                "db": [
                    {"machine_id": "a".repeat(32), "server": "alpha", "state": "completed", "started_at": 1, "finished_at": 2}
                ]
            }))
            .unwrap();
        assert_eq!(
            untargeted_lines(&rows),
            [
                "  cache: removed 1/2 (not in this environment)",
                "  db: removed 1/1 (not in this environment)",
            ]
        );
    }

    #[test]
    fn each_service_reads_where_it_ended() {
        let services: Vec<ployz_core::ServiceOutcome> = serde_json::from_value(serde_json::json!([
            {"service": "web", "result": "new"},
            {"service": "api", "result": "old"},
            {"service": "cache", "result": "updated"},
            {"service": "db", "result": "not_started"},
            {"service": "worker", "result": "unconfirmed"},
            {"service": "cron", "result": "newer"},
        ]))
        .unwrap();
        assert_eq!(
            service_outcome_lines(&services),
            [
                "  web: serves its new copies",
                "  api: not switched to its new copies",
                "  cache: serves its copies with their new limits",
                "  db: not started",
                "  worker: may serve its new copies: the switch was not confirmed",
                "  cron: a newer deploy serves it",
            ]
        );
    }

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

    #[test]
    fn a_push_waiting_for_ci_reads_how_its_ci_stands() {
        let deployment = |status: &str, ci: Option<&str>| -> ployz_store::DeploymentSummary {
            serde_json::from_value(serde_json::json!({
                "id": "00000000-0000-4000-8000-000000000101",
                "environment_id": "00000000-0000-4000-8000-000000000002",
                "number": 1, "status": status, "saved": 1, "services": [],
                "runner": null, "upload": null, "remove": false, "admitted_by": null,
                "admitted_at": 0, "started_at": null, "ended_at": null, "message": null,
                "push": null, "ci": ci, "in_flight": false, "outcome": null,
            }))
            .unwrap()
        };
        let read = |status: &str, ci: Option<&str>| {
            let deployment = deployment(status, ci);
            (status_word(&deployment), status_tone(&deployment))
        };
        assert_eq!(
            read("waiting_for_ci", Some("pending")),
            ("waiting for CI".to_owned(), Tone::Change)
        );
        assert_eq!(
            read("waiting_for_ci", Some("failed")),
            ("CI failed".to_owned(), Tone::Bad)
        );
        assert_eq!(read("queued", None), ("queued".to_owned(), Tone::Change));
    }

    #[test]
    fn a_queued_deploy_without_an_upload_of_its_own_says_its_upload_may_change() {
        let deployment = |status: &str, at_start: bool| -> ployz_store::DeploymentSummary {
            serde_json::from_value(serde_json::json!({
                "id": "00000000-0000-4000-8000-000000000101",
                "environment_id": "00000000-0000-4000-8000-000000000002",
                "number": 1, "status": status, "saved": 1, "services": [], "runner": null,
                "upload": {"digest": "a".repeat(64), "base": null, "uploader": "nick"},
                "chooses_upload_at_start": at_start, "remove": false, "admitted_by": null,
                "admitted_at": 0, "started_at": null, "ended_at": null, "message": null,
                "push": null, "ci": null, "in_flight": true, "outcome": null,
            }))
            .unwrap()
        };
        assert_eq!(
            provenance(&deployment("queued", true)).as_deref(),
            Some("uploaded by nick, sha256 aaaaaaaaaaaa for now; chosen again when it starts")
        );
        assert_eq!(
            provenance(&deployment("running", false)).as_deref(),
            Some("uploaded by nick, sha256 aaaaaaaaaaaa")
        );
    }

    #[test]
    fn a_running_deployment_reads_preparing_until_it_has_a_deploy_preview() {
        let read = |status: &str, preparing: bool| {
            let deployment: ployz_store::DeploymentSummary =
                serde_json::from_value(serde_json::json!({
                    "id": "00000000-0000-4000-8000-000000000101",
                    "environment_id": "00000000-0000-4000-8000-000000000002",
                    "number": 1, "status": status, "saved": 1, "services": [],
                    "runner": "cloud-run", "upload": null, "preparing": preparing,
                    "remove": false, "admitted_by": null, "admitted_at": 0, "started_at": 0,
                    "ended_at": null, "message": null, "push": null, "ci": null,
                    "in_flight": true, "outcome": null,
                }))
                .unwrap();
            (status_word(&deployment), status_tone(&deployment))
        };
        assert_eq!(
            read("running", true),
            ("preparing".to_owned(), Tone::Change)
        );
        assert_eq!(read("running", false), ("running".to_owned(), Tone::Change));
    }
}
