//! Present the Store's recorded Deployment evidence without new Cluster reads.

use crate::deploy::{Replica, service_row, starts, updates};
use crate::failure::Failure;
use crate::ui::{
    Hint,
    progress::{Detail, Diagnostic, Frame, LogTail, Row, Run, State, Subject, Timing, Work},
};
use ployz_core::config::{RuntimeFailureKind, RuntimeOutcomeSummary};
use ployz_core::{
    DeployOperation, DeployWarning, PullPolicy, QualifiedService, RpcError, RpcErrorCode,
    ServiceName, config::MEM_LIMIT_MAX,
};
use ployz_store::{
    BuildStatus, DeployedNode, DeploymentStatus, DeploymentSummary, DeploymentView,
    DeploymentWarning, FailedAt, IngressAction, IngressFailure, NodeOutcome, NodeStatus, RowState,
    ServerRow,
};
use serde::Deserialize;
use std::time::{Duration, UNIX_EPOCH};

pub(super) fn frame(view: &DeploymentView) -> Frame {
    let mut rows = Vec::new();
    let pulling = pulling(view);
    for (index, node) in view.nodes.iter().enumerate() {
        let DeployedNode::Service { name, .. } = &node.node else {
            rows.push(Row {
                subject: Subject::Node {
                    index,
                    name: match &node.node {
                        DeployedNode::Config { name, .. } => format!("Config {name}"),
                        DeployedNode::Volume { name, .. } => format!("Volume {name}"),
                        DeployedNode::Service { name, .. } => name.to_string(),
                    },
                },
                state: node_state(node, false),
                detail: None,
                timing: Timing::Unavailable,
            });
            continue;
        };
        let runtime = view.runtime_names.get(name).unwrap_or(name);
        let mut row = runtime_row(view, name, runtime, &node.rows);
        if let Some(volumes) = pulling
            && row.state == State::Pending
        {
            row.detail = pulled(view, runtime).map(|image| format!("pulling {image}{volumes}"));
        }
        if node.rows.is_empty() {
            let build = view.builds.iter().find(|build| build.service == *name);
            row.state = node_state(
                node,
                build.is_some_and(|build| build.status == BuildStatus::Failed),
            );
            // A removal with nothing to plan found no copy on the Servers it could
            // check. It is still a removal, not a switch.
            if node.outcome == NodeStatus::Removed
                && let Subject::Service(graph) = &mut row.subject
            {
                graph.work = Work::Remove;
            }
            let blocked = view.preview.as_ref().and_then(|preview| {
                preview
                    .blocked
                    .iter()
                    .find(|entry| entry.service == *runtime)
                    .map(|entry| crate::ui::escaped(&entry.error.message))
            });
            row.detail = blocked.or_else(|| {
                build.map(|build| {
                    let word = match build.status {
                        BuildStatus::Pending => "build pending",
                        BuildStatus::Building => "building",
                        BuildStatus::Built => "built",
                        BuildStatus::Reused => "build reused",
                        BuildStatus::Failed => "build failed",
                    };
                    match (&build.server, build.status) {
                        (
                            Some(server),
                            BuildStatus::Building | BuildStatus::Built | BuildStatus::Failed,
                        ) => format!("{word} on {server}"),
                        _ => word.to_owned(),
                    }
                })
            });
        }
        rows.push(row);
    }
    // A full reconciliation removes what runs in the Namespace and this Store doesn't hold.
    for (runtime, server_rows) in &view.untargeted {
        let mut row = runtime_row(view, runtime, runtime, server_rows);
        row.detail
            .get_or_insert_with(|| "not in this environment".to_owned());
        rows.push(row);
    }
    rows.extend(view.deployment.ingress.iter().map(|repair| Row {
        subject: Subject::ServiceOnServer {
            service: QualifiedService::system_ingress(),
            machine: repair.machine,
            server: repair.server.clone(),
        },
        state: if repair.failure.is_some() {
            State::Warned
        } else {
            State::Completed
        },
        detail: Some(ingress_verb(repair.action).to_owned()),
        timing: Timing::Unavailable,
    }));
    if ingress_unchecked(view).next().is_some() {
        rows.push(Row {
            subject: Subject::Unlocated(QualifiedService::system_ingress()),
            state: State::Unknown,
            detail: Some("not checked".to_owned()),
            timing: Timing::Unavailable,
        });
    }
    Frame {
        run: Run::Stored(view.deployment.id.clone()),
        title: format!(
            "Deploying {} of {}/{}",
            view.deployment.label(),
            view.environment.project,
            view.environment.name
        ),
        rows,
        notices: view
            .preview
            .iter()
            .flat_map(|preview| &preview.warnings)
            .chain(
                view.deployment
                    .warnings
                    .iter()
                    .filter_map(|warning| match warning {
                        DeploymentWarning::Deploy { warning } => Some(warning),
                        DeploymentWarning::Fragile { .. }
                        | DeploymentWarning::NotSet { .. }
                        | DeploymentWarning::NotSetInFile { .. }
                        | DeploymentWarning::IngressNotServing { .. }
                        | DeploymentWarning::IngressUnchecked { .. } => None,
                    }),
            )
            .map(|warning| by_service_name(view, warning.clone()))
            .fold(Vec::new(), |mut notices, warning| {
                if !notices.contains(&warning) {
                    notices.push(warning);
                }
                notices
            }),
    }
}

/// `warning` naming its Service by its name, not the runtime name its containers run under.
fn by_service_name(view: &DeploymentView, mut warning: DeployWarning) -> DeployWarning {
    if let DeployWarning::ImagePortMismatch { service, .. } = &mut warning
        && let Some((name, _)) = view
            .runtime_names
            .iter()
            .find(|(_, runtime)| *runtime == service)
    {
        *service = name.clone();
    }
    warning
}

/// While the run is pulling its images, what it may be doing beside the pull. A
/// Deploy pulls every image it runs on each Server, then prepares the plan's
/// Volumes, before any Server starts work on a Service. The Store records neither
/// step, so a running Deploy whose builds are done and whose every row still waits
/// is pulling, or preparing Volumes when its plan does.
fn pulling(view: &DeploymentView) -> Option<&'static str> {
    let preview = view.preview.as_ref()?;
    let waiting = view.deployment.status == DeploymentStatus::Running
        && view
            .builds
            .iter()
            .all(|build| !matches!(build.status, BuildStatus::Pending | BuildStatus::Building))
        && view
            .nodes
            .iter()
            .flat_map(|node| &node.rows)
            .chain(view.untargeted.values().flatten())
            .all(|row| matches!(row.state, RowState::Pending));
    let volumes = preview
        .operations
        .iter()
        .any(|row| matches!(row.operation, DeployOperation::PrepareVolumes { .. }));
    waiting.then_some(if volumes { " or preparing Volumes" } else { "" })
}

/// The image the plan pulls for Service `runtime`: none under `Never`.
fn pulled<'a>(view: &'a DeploymentView, runtime: &ServiceName) -> Option<&'a str> {
    view.preview
        .as_ref()?
        .operations
        .iter()
        .filter(|row| row.service_name() == Some(runtime))
        .find_map(|row| row.operation.spec())
        .filter(|spec| spec.container.pull_policy != PullPolicy::Never)
        .map(|spec| spec.container.image.as_str())
}

/// Service `name`'s row from its recorded work on each Server, read against the plan
/// for `runtime`.
fn runtime_row(
    view: &DeploymentView,
    name: &ServiceName,
    runtime: &ServiceName,
    rows: &[ServerRow],
) -> Row {
    let preview = view.preview.as_ref();
    service_row(
        QualifiedService::new(view.namespace.clone(), name.clone()),
        preview
            .and_then(|preview| preview.depths.get(runtime))
            .copied(),
        preview.and_then(|preview| preview.placed_on.get(runtime)),
        rows.iter().map(|row| Replica {
            machine: row.machine_id,
            state: &row.state,
            starts: preview
                .is_none_or(|preview| starts(&preview.operations, runtime, row.machine_id)),
            updates: preview
                .is_some_and(|preview| updates(&preview.operations, runtime, row.machine_id)),
            timing: match (row.started_at, row.finished_at) {
                (Some(start), Some(end)) => Timing::Finished(Duration::from_secs(
                    u64::try_from(end.saturating_sub(start)).unwrap_or(0),
                )),
                (Some(start), None) => u64::try_from(start)
                    .ok()
                    .and_then(|secs| UNIX_EPOCH.checked_add(Duration::from_secs(secs)))
                    .map_or(Timing::Unavailable, Timing::Started),
                (None, _) => Timing::Unavailable,
            },
        }),
    )
}

fn node_state(node: &NodeOutcome, build_failed: bool) -> State {
    match node.outcome {
        NodeStatus::Pending if build_failed => State::Failed,
        NodeStatus::Pending => State::Pending,
        NodeStatus::Deployed | NodeStatus::Removed => State::Completed,
        NodeStatus::Failed if node.cancelled => State::Cancelled,
        NodeStatus::Failed => State::Failed,
        NodeStatus::NotAttempted => State::NotAttempted,
        NodeStatus::Unchanged => State::Unchanged,
        NodeStatus::Unknown => State::Unknown,
    }
}

/// Start once there is observed work, preserving one caption for a clean no-op.
pub(super) fn visible(view: &DeploymentView) -> bool {
    !view.deployment.ingress.is_empty()
        || ingress_unchecked(view).next().is_some()
        || view
            .preview
            .as_ref()
            .is_some_and(|preview| !preview.noop() || preview.has_observation_gaps())
        || view.builds.iter().any(|build| {
            matches!(
                build.status,
                BuildStatus::Building | BuildStatus::Built | BuildStatus::Failed
            )
        })
}

pub(super) fn noop(view: &DeploymentView) -> bool {
    view.deployment.status == DeploymentStatus::Applied
        && view.deployment.ingress.is_empty()
        && ingress_unchecked(view).next().is_none()
        && view
            .nodes
            .iter()
            .all(|node| node.outcome == NodeStatus::Unchanged)
        && view
            .preview
            .as_ref()
            .is_some_and(|preview| preview.noop() && !preview.has_observation_gaps())
}

/// Said for a Deployment that changed nothing on the Servers: it still recorded the
/// saved version it shipped.
pub(super) fn up_to_date(view: &DeploymentView) -> String {
    format!(
        "Nothing changed on the Servers; Deployment {} shipped version #{}.",
        view.deployment.label(),
        view.deployment.saved
    )
}

pub(super) fn ingress_verb(action: IngressAction) -> &'static str {
    match action {
        IngressAction::Create => "create",
        IngressAction::Start => "start",
    }
}

fn ingress_unchecked(view: &DeploymentView) -> impl Iterator<Item = &str> {
    view.deployment
        .warnings
        .iter()
        .filter_map(|warning| match warning {
            DeploymentWarning::IngressUnchecked { cause } => Some(cause.as_str()),
            DeploymentWarning::Fragile { .. }
            | DeploymentWarning::NotSet { .. }
            | DeploymentWarning::NotSetInFile { .. }
            | DeploymentWarning::IngressNotServing { .. }
            | DeploymentWarning::Deploy { .. } => None,
        })
}

/// Where each build ran, and why GitHub passed one on to the Servers.
pub(super) fn builders(view: &DeploymentView) -> Vec<String> {
    view.builds
        .iter()
        .filter_map(|build| {
            let built = build
                .server
                .as_ref()
                .filter(|_| build.status == BuildStatus::Built)
                .map(|server| format!("{} built on {server}.", build.service));
            let skipped = build
                .github_skipped
                .as_deref()
                .map(|why| why.trim_end_matches('.'));
            match (built, skipped) {
                (Some(built), Some(why)) => Some(format!("{built} GitHub skipped it: {why}.")),
                (Some(built), None) => Some(built),
                (None, Some(why)) => Some(format!("GitHub skipped {}: {why}.", build.service)),
                (None, None) => None,
            }
        })
        .collect()
}

/// What the Deployment left out of a Service's variables or a Config's files.
pub(super) fn value_warnings(view: &DeploymentView) -> Vec<String> {
    view.deployment
        .warnings
        .iter()
        .filter_map(|warning| {
            Some(match warning {
                DeploymentWarning::NotSet {
                    service,
                    variable,
                    broken: true,
                    ..
                } => format!("Left {variable} out of {service}: it has a broken reference."),
                DeploymentWarning::NotSetInFile {
                    config,
                    file,
                    secret,
                    broken: true,
                } => format!("Left {secret} empty in {config}/{file}: it is a broken reference."),
                DeploymentWarning::NotSet {
                    service,
                    variable,
                    reads: Some(reads),
                    sealed_elsewhere: true,
                    ..
                } => {
                    let (owner, key) = reads.split_once('.').unwrap_or((reads, ""));
                    format!(
                        "Left {variable} out of {service}: it reads {reads}, a sealed value this branch doesn't get from its parent. Set {service}.env.{variable} in this branch, or run `ployz env copy {owner} --env {}` and set {owner}.env.{key} in the copy, whose volumes start empty.",
                        view.environment.name
                    )
                }
                DeploymentWarning::NotSet {
                    service,
                    variable,
                    reads: Some(reads),
                    ..
                } => format!("Left {variable} out of {service}: it reads {reads}, which is not set."),
                DeploymentWarning::NotSet {
                    service, variable, ..
                } => format!("Left {variable} out of {service}: it is not set."),
                DeploymentWarning::NotSetInFile {
                    config,
                    file,
                    secret,
                    ..
                } => format!("Left {secret} empty in {config}/{file}: it is not set."),
                DeploymentWarning::Fragile {
                    config,
                    file,
                    variable,
                } => format!("{variable} may break {config}/{file}."),
                DeploymentWarning::IngressNotServing { .. }
                | DeploymentWarning::IngressUnchecked { .. }
                | DeploymentWarning::Deploy { .. } => return None,
            })
        })
        .collect()
}

pub(super) fn ingress_warnings(view: &DeploymentView) -> Vec<String> {
    let unchecked = ingress_unchecked(view).map(|cause| {
        crate::ui::then_sentence(
            &crate::ui::with_reason("Couldn't check ingress", cause),
            "Deploy again to retry.",
        )
    });
    view.deployment
        .ingress
        .iter()
        .filter_map(|repair| {
            let server = &repair.server;
            Some(match repair.failure.as_ref()? {
                IngressFailure::PortTaken { cause } => format!(
                    "Ingress isn't running on {server}: something else holds port 80 or 443 ({cause}). \
                     See what holds it with `sudo ss -ltnp '( sport = :80 or sport = :443 )'`. \
                     Ployz starts ingress on its own once the port is free."
                ),
                IngressFailure::Failed { cause } => crate::ui::then_sentence(
                    &crate::ui::with_reason(
                        format_args!("Ingress isn't running on {server}"),
                        cause,
                    ),
                    "Deploy again to retry.",
                ),
            })
        })
        .chain(unchecked)
        .collect()
}

/// `named` are the Services the command line named, which its retry repeats.
pub(super) fn completion(
    view: &DeploymentView,
    successor: Option<&DeploymentSummary>,
    named: &[&str],
    command: impl Fn(&[&str]) -> String,
) -> Result<(), Failure> {
    if view.deployment.status == DeploymentStatus::Superseded {
        return Err(superseded(view, successor, command));
    }
    if view.deployment.status != DeploymentStatus::Applied {
        return Err(failure(view, named, command));
    }
    if view
        .preview
        .as_ref()
        .is_some_and(|preview| preview.has_observation_gaps())
        || left_a_gap(view)
        || left_a_copy_running(view)
        || ingress_unchecked(view).next().is_some()
    {
        return Err(Failure::partial());
    }
    Ok(())
}

/// The run found a Server it could not fully observe after the plan was made.
fn left_a_gap(view: &DeploymentView) -> bool {
    view.deployment.warnings.iter().any(|warning| {
        matches!(warning, DeploymentWarning::Deploy { warning } if warning.observation_gap().is_some())
    })
}

fn left_a_copy_running(view: &DeploymentView) -> bool {
    view.deployment.warnings.iter().any(|warning| {
        matches!(warning, DeploymentWarning::Deploy { warning } if warning.leaves_a_copy_running())
    })
}

/// How a command names a Deployment: its number once it queued, else its ID.
fn target(deployment: &DeploymentSummary) -> String {
    deployment
        .number
        .map_or_else(|| deployment.id.to_string(), |number| number.to_string())
}

/// How to retry a removal or shutdown that ended without applying when the command that
/// ran it is `deployment redeploy` or `start`: redeploy it in its own Environment.
/// A Deploy can't.
pub(super) fn removal_retry(
    view: &DeploymentView,
    command: impl Fn(&[&str]) -> String,
) -> Option<Hint> {
    // The ended statuses `deployment redeploy` accepts for a removal.
    let ended = matches!(
        view.deployment.status,
        DeploymentStatus::Failed | DeploymentStatus::Unknown | DeploymentStatus::Cancelled
    );
    (view.deployment.remove && ended).then(|| {
        Hint::Retry(command(&[
            "deployment",
            "redeploy",
            &target(&view.deployment),
            "--project",
            view.environment.project.as_str(),
            "--env",
            view.environment.name.as_str(),
        ]))
    })
}

/// A newer Deployment took this one's place. Deploying again would take the newer one's.
fn superseded(
    view: &DeploymentView,
    successor: Option<&DeploymentSummary>,
    command: impl Fn(&[&str]) -> String,
) -> Failure {
    let scope = [
        "--project",
        view.environment.project.as_str(),
        "--env",
        view.environment.name.as_str(),
    ];
    let (message, inspect) = match successor {
        Some(newer) => {
            let from = newer.push.as_ref().map_or(String::new(), |push| {
                format!(
                    " (push {})",
                    push.head.as_str().get(..7).unwrap_or_default()
                )
            });
            let shown = target(newer);
            (
                format!(
                    "Deployment {} was superseded by {}{from}.",
                    view.deployment.label(),
                    newer.label()
                ),
                command(&[&["deployment", "show", shown.as_str()][..], &scope].concat()),
            )
        }
        None => (
            format!("Deployment {} was superseded.", view.deployment.label()),
            command(&[&["deployment", "ls"][..], &scope].concat()),
        ),
    };
    Failure::from(RpcError {
        code: RpcErrorCode::Internal,
        message,
        cause: Vec::new(),
        details: serde_json::Value::Null,
    })
    .hint(Hint::Inspect(inspect))
}

/// The Deployment among `candidates` that took `view`'s Environment over: the first
/// newer one to start after it, when it executed and `candidates` holds every number
/// up to it. None when that is not established, as when `view`'s claim lapsed while
/// a newer one waited, or the first to start never executed: a later one is never
/// named in its place.
pub(super) fn taker<'a>(
    view: &DeploymentView,
    candidates: &'a [DeploymentSummary],
) -> Option<&'a DeploymentSummary> {
    let ours = &view.deployment;
    let number = ours.number?;
    let first = candidates
        .iter()
        .filter(|newer| newer.number > ours.number && newer.started_at.is_some())
        .filter(|newer| newer.started_at >= ours.started_at)
        .min_by_key(|newer| newer.number)?;
    let read_all = (number + 1..first.number?).all(|between| {
        candidates
            .iter()
            .any(|candidate| candidate.number == Some(between))
    });
    let executed = !matches!(
        first.outcome,
        Some(ployz_store::Outcome::NotExecuted { .. })
    );
    (read_all && executed).then_some(first)
}

/// A newer Deployment took over this Environment while this one ran, so what it did
/// stays unknown. Deploying again would only redo the newer one.
pub(super) fn replaced(
    view: &DeploymentView,
    newer: &DeploymentSummary,
    command: impl Fn(&[&str]) -> String,
) -> Failure {
    let shown = target(newer);
    Failure::detailed(
        RpcErrorCode::Conflict,
        format!(
            "Deployment {} was replaced by {} before it finished.",
            view.deployment.label(),
            newer.label()
        ),
        serde_json::json!({ "deployment": view.deployment.id, "replaced_by": newer.id }),
    )
    .hint(Hint::Inspect(command(&[
        "deployment",
        "show",
        shown.as_str(),
        "--project",
        view.environment.project.as_str(),
        "--env",
        view.environment.name.as_str(),
    ])))
}

fn failure(view: &DeploymentView, named: &[&str], command: impl Fn(&[&str]) -> String) -> Failure {
    let (reason, cause) = match &view.deployment.outcome {
        Some(
            ployz_store::Outcome::Executed {
                reason: Some(reason),
                cause,
                cancelled: false,
                ..
            }
            | ployz_store::Outcome::NotExecuted { reason, cause, .. },
        ) => (reason.clone(), cause.clone()),
        _ if view.deployment.status == DeploymentStatus::Cancelled => (
            format!("Deployment {} was cancelled.", view.deployment.label()),
            Vec::new(),
        ),
        _ => (
            format!("Deployment {} did not complete.", view.deployment.label()),
            Vec::new(),
        ),
    };
    let scope = [
        "--project",
        view.environment.project.as_str(),
        "--env",
        view.environment.name.as_str(),
    ];
    let deployment = target(&view.deployment);
    // A Service the plan blocked is blocked again until its cause changes.
    let blocked = view
        .preview
        .as_ref()
        .is_some_and(|preview| !preview.blocked.is_empty());
    let lost_to_newer = matches!(
        &view.deployment.outcome,
        Some(ployz_store::Outcome::Executed { summary, .. })
            if matches!(
                RuntimeOutcomeSummary::deserialize(summary),
                Ok(RuntimeOutcomeSummary::Failed { reason: RuntimeFailureKind::NewerServing, .. })
            )
    );
    let mut diagnostic = Diagnostic::default();
    if let Some(ployz_store::Outcome::Executed { compensation, .. }) = &view.deployment.outcome {
        diagnostic.compensation = compensation
            .iter()
            .map(|line| Detail {
                message: line.clone(),
                causes: Vec::new(),
            })
            .collect();
    }
    let mut hints = Vec::new();
    // A failed build, or an image that won't pull, fails the same way again.
    let mut same_again = false;
    let mut bad_image = false;
    let mut primary_seen = false;
    for node in &view.nodes {
        let DeployedNode::Service { name, .. } = &node.node else {
            continue;
        };
        let service = QualifiedService::new(
            view.namespace.clone(),
            view.runtime_names.get(name).unwrap_or(name).clone(),
        );
        let built = view.builds.iter().find(|build| build.service == *name);
        let mut build_failed = built.is_some_and(|build| build.status == BuildStatus::Failed);
        let mut logs = false;
        let mut pulled = false;
        let mut memory_limit = None;
        for row in &node.rows {
            let RowState::Failed {
                reason: row_reason,
                cause: row_cause,
                step,
                log,
                out_of_memory,
                unanswered,
                ..
            } = &row.state
            else {
                continue;
            };
            memory_limit = memory_limit.or(out_of_memory.and_then(|oom| oom.memory_bytes));
            if !primary_seen && reason == format!("{name}: {row_reason}") && cause == *row_cause {
                primary_seen = true;
            } else {
                diagnostic.failures.push(Detail {
                    message: format!("{name} on {}: {row_reason}.", row.server),
                    causes: row_cause.clone(),
                });
            }
            if lost_to_newer {
                continue;
            }
            diagnostic.logs.push(LogTail {
                service: service.clone(),
                name: name.clone(),
                machine: row.machine_id,
                server: row.server.clone(),
                log: log.clone(),
            });
            match step {
                Some(
                    FailedAt::Start
                    | FailedAt::Health
                    | FailedAt::Hook
                    | FailedAt::Stop
                    | FailedAt::Remove,
                ) => logs = true,
                // A pull whose answer was lost may have worked: nothing says the image is bad.
                Some(FailedAt::Pull) => pulled |= !unanswered,
                Some(FailedAt::Build) => build_failed = true,
                Some(FailedAt::Create | FailedAt::Volume) => {}
                None => logs |= log.is_some(),
            }
        }
        // Twice the limit the Container ran out of memory under, in the unit it reads in,
        // up to the most memLimit allows. At the most, only using less memory fixes it.
        if let Some(bytes) = memory_limit {
            let gb = bytes.get() as f64 / 1e9;
            if gb < MEM_LIMIT_MAX {
                let raised = ployz_store::memory_size((gb * 2.0).min(MEM_LIMIT_MAX));
                let setting = format!("{name}.memLimit={}", raised.replace(' ', ""));
                hints.push(Hint::Next(command(
                    &[&["set", setting.as_str()][..], &scope].concat(),
                )));
            } else {
                diagnostic.failures.push(Detail {
                    message: format!(
                        "{name} already has the most memory memLimit allows, {}: it needs to use less.",
                        ployz_store::memory_size(MEM_LIMIT_MAX)
                    ),
                    causes: Vec::new(),
                });
            }
        }
        // One hint per Service, by its config name: it reads every Server's logs, and a
        // token that reads through Cloud can serve it too. Before a Container exists it
        // would show only the old one's lines, so a failed build names its build log and
        // a failed pull names the image, unless a build made it.
        if build_failed {
            hints.push(Hint::Inspect(command(
                &[
                    &[
                        "logs",
                        "--deployment",
                        deployment.as_str(),
                        "--build",
                        name.as_str(),
                    ][..],
                    &scope,
                ]
                .concat(),
            )));
            same_again = true;
        } else if logs {
            hints.push(Hint::Inspect(command(
                &[&["logs", name.as_str()][..], &scope].concat(),
            )));
        } else if pulled && built.is_none() {
            let image = format!("{name}.image");
            hints.push(Hint::Inspect(command(
                &[&["get", image.as_str()][..], &scope].concat(),
            )));
        }
        // An image it didn't build that won't pull fails again on any Server, whichever
        // failure the inspect hint above reads.
        bad_image |= pulled && !build_failed && built.is_none();
    }
    let mut failure = Failure::from(RpcError {
        code: if lost_to_newer {
            RpcErrorCode::Conflict
        } else {
            RpcErrorCode::Internal
        },
        message: crate::ui::sentence(reason),
        cause,
        details: serde_json::Value::Null,
    })
    .with_diagnostic(diagnostic);
    for hint in hints {
        failure = failure.hint(hint);
    }
    // A Deploy can't retry a removal or shutdown: the command that ran it names its own retry.
    if view.deployment.remove {
        return failure;
    }
    // Only a Deploy that failed is its own result (exit 1). One replaced, cancelled or
    // whose runner was lost didn't fail: part of what was asked just didn't happen (3).
    let mut failure = if view.deployment.status == DeploymentStatus::Failed {
        failure.outcome()
    } else {
        failure
    };
    if bad_image {
        let revision = view.deployment.saved.to_string();
        failure = failure.hint(Hint::Undo(command(
            &[&["history", "undo", revision.as_str()][..], &scope].concat(),
        )));
    }
    // A refusal that holds until something staged changes gets no retry, not even
    // `up`: its message says what to change, and a build that failed is the one case
    // where a fresh upload is that change.
    if matches!(
        &view.deployment.outcome,
        Some(ployz_store::Outcome::NotExecuted {
            same_again: true,
            ..
        })
    ) {
        return failure;
    }
    // An upload deploys again only from `up`: a Deploy ships the last one that built.
    if view.deployment.upload.is_some() {
        return failure.hint(Hint::Retry(command(&[&["up"][..], &scope].concat())));
    }
    if same_again || bad_image || blocked {
        return failure;
    }
    // Not `deployment.services`: admission adds the Services it retires, which no deploy can name.
    let words = ["deploy"]
        .into_iter()
        .chain(named.iter().copied())
        .chain(scope);
    failure.hint(Hint::Retry(command(&words.collect::<Vec<_>>())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn command(args: &[&str]) -> String {
        shell_words::join(std::iter::once("ployz").chain(args.iter().copied()))
    }

    pub(super) fn view() -> DeploymentView {
        serde_json::from_value(json!({
            "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "environment_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "number": 7,
            "status": "failed", "saved": 1, "services": [], "remove": false, "admitted_at": 1, "in_flight": false,
            "outcome": {"type": "executed", "summary": {}, "reason": "web: CreateContainer failed", "cause": ["pull failed", "denied"]},
            "environment": {"id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "project": "shop", "name": "production", "revision": 1},
            "namespace": "shop-production", "preview": null, "builds": [], "runtime_names": {"web": "private-web"},
            "nodes": [
                {"type": "service", "id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "name": "web", "outcome": "failed", "rows": [
                    {"machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "server": "edge", "state": "failed", "reason": "CreateContainer failed", "cause": ["pull failed", "denied"], "log": {"status": "shown", "lines": ["startup failed"], "cut": false, "redacted": 0}, "started_at": 1, "finished_at": 2},
                    {"machine_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "server": "edge", "state": "failed", "reason": "CreateContainer failed", "cause": ["pull failed", "denied"], "log": null, "started_at": 1, "finished_at": 2}
                ]},
                {"type": "service", "id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd", "name": "api", "outcome": "unchanged", "rows": []},
                {"type": "volume", "id": "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee", "name": "data", "outcome": "not_attempted", "rows": []}
            ]
        })).unwrap()
    }

    #[test]
    fn a_variable_left_out_names_the_variable_without_a_value_it_reads() {
        let mut view = view();
        view.environment.name = ployz_store::EnvironmentName::parse("b4b").unwrap();
        view.deployment.warnings = serde_json::from_value(json!([
            {"service": "app", "variable": "DBURL", "reads": "db.TOK3", "sealed_elsewhere": true},
            {"service": "app", "variable": "CACHE", "reads": "app.TOKEN"},
            {"service": "app", "variable": "TOKEN"},
            {"service": "app", "variable": "LINK", "broken": true}
        ]))
        .unwrap();

        assert_eq!(
            value_warnings(&view),
            [
                "Left DBURL out of app: it reads db.TOK3, a sealed value this branch doesn't get from its parent. Set app.env.DBURL in this branch, or run `ployz env copy db --env b4b` and set db.env.TOK3 in the copy, whose volumes start empty.",
                "Left CACHE out of app: it reads app.TOKEN, which is not set.",
                "Left TOKEN out of app: it is not set.",
                "Left LINK out of app: it has a broken reference.",
            ]
        );
    }

    #[test]
    fn a_not_set_warning_reads_the_same_with_or_without_what_it_reads() {
        let old: DeploymentWarning =
            serde_json::from_value(json!({"service": "app", "variable": "TOKEN"})).unwrap();
        assert_eq!(
            serde_json::to_value(&old).unwrap(),
            json!({"service": "app", "variable": "TOKEN"})
        );
        // A client before 0.3.13 reads the new fields as nothing and says Not set.
        #[derive(serde::Deserialize)]
        #[serde(untagged)]
        enum Before {
            NotSet {
                service: String,
                variable: String,
                #[serde(default)]
                broken: bool,
            },
        }
        let new = serde_json::to_value(DeploymentWarning::NotSet {
            service: "app".parse().unwrap(),
            variable: "DBURL".into(),
            broken: false,
            reads: Some("db.TOK3".into()),
            sealed_elsewhere: true,
        })
        .unwrap();
        assert_eq!(
            new,
            json!({"service": "app", "variable": "DBURL", "reads": "db.TOK3", "sealed_elsewhere": true})
        );
        let Before::NotSet {
            service,
            variable,
            broken,
        } = serde_json::from_value(new).unwrap();
        assert_eq!(
            (service.as_str(), variable.as_str(), broken),
            ("app", "DBURL", false)
        );
    }

    #[test]
    fn replicas_fold_into_one_service_row_under_its_service_name() {
        let frame = frame(&view());
        assert_eq!(frame.rows.len(), 3);
        assert!(frame.rows.iter().any(|row| matches!(
            &row.subject,
            Subject::Service(graph)
                if graph.service.to_string() == "shop-production/web"
                    && graph.total == 2
                    && graph.machines.len() == 2
        ) && row.state == State::Failed));
        assert!(frame.rows.iter().any(
            |row| matches!(&row.subject, Subject::Node { name, .. } if name == "Volume data")
        ));
    }

    #[test]
    fn a_blocked_service_reads_not_attempted_with_its_reason_and_fails_the_deploy() {
        let reason = "Volume data has no writer; fsn-2 holds its only copy";
        let mut view = view();
        view.runtime_names.clear();
        view.deployment.outcome = Some(
            serde_json::from_value(json!({
                "type": "executed", "summary": {}, "reason": format!("db: {reason}"), "cause": []
            }))
            .unwrap(),
        );
        view.nodes = serde_json::from_value(json!([
            {"type": "service", "id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "name": "web", "outcome": "deployed", "rows": [
                {"machine_id": "a".repeat(32), "server": "edge", "state": "completed", "started_at": 1, "finished_at": 2}
            ]},
            {"type": "service", "id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd", "name": "db", "outcome": "not_attempted", "rows": []},
            {"type": "volume", "id": "eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee", "name": "data", "outcome": "not_attempted", "rows": []}
        ]))
        .unwrap();
        view.preview = Some(
            serde_json::from_value(json!({
                "namespace": "shop-production", "warnings": [],
                "operations": [],
                "blocked": [{
                    "service": "db",
                    "error": {"code": "conflict", "message": reason, "details": {"reason": "no_writer"}}
                }]
            }))
            .unwrap(),
        );

        let text = crate::ui::progress::settled_text(&frame(&view));

        assert!(
            text.contains(&format!("db: not started ({reason})")),
            "{text}"
        );
        assert!(text.contains("2 not attempted"), "{text}");
        let failure = completion(&view, None, &[], command).unwrap_err();
        assert_eq!(crate::ui::after_result(&failure), 1);
        assert!(failure.to_string().contains(reason), "{failure}");
        assert!(
            !failure
                .hints()
                .iter()
                .any(|hint| matches!(hint, Hint::Retry(_))),
            "{:?}",
            failure.hints()
        );
    }

    #[test]
    fn a_blocked_reason_prints_an_invisible_character_as_its_code() {
        let mut view = view();
        view.runtime_names.clear();
        view.nodes = serde_json::from_value(json!([
            {"type": "service", "id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd", "name": "db", "outcome": "not_attempted", "rows": []}
        ]))
        .unwrap();
        view.preview = Some(
            serde_json::from_value(json!({
                "namespace": "shop-production", "warnings": [], "operations": [],
                "blocked": [{
                    "service": "db",
                    "error": {"code": "conflict", "message": "Config app has no file a\u{202E}b.txt", "details": {}}
                }]
            }))
            .unwrap(),
        );

        assert_eq!(
            details(&view),
            [Some("Config app has no file a\\u{202e}b.txt".to_owned())]
        );
    }

    #[test]
    fn a_service_this_store_doesnt_hold_reads_removed_and_counts_in_the_summary() {
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        view.deployment.outcome = None;
        view.nodes.clear();
        view.preview = Some(
            serde_json::from_value(json!({
                "namespace": "shop-production", "warnings": [],
                "would_remove": ["shop-production/db"],
                "operations": [{
                    "index": 0, "machine_id": "a".repeat(32), "service_name": "db",
                    "operation": {"type": "remove_container", "machine_id": "a".repeat(32), "container_id": "b".repeat(64), "purpose": "retire"},
                    "status": {"type": "completed"}
                }]
            }))
            .unwrap(),
        );
        view.untargeted = serde_json::from_value(json!({"db": [
            {"machine_id": "a".repeat(32), "server": "edge", "state": "completed", "started_at": 1, "finished_at": 2}
        ]}))
        .unwrap();
        let text = crate::ui::progress::settled_text(&frame(&view));
        assert!(
            text.contains("db: removed 1/1 (not in this environment)"),
            "{text}"
        );
        assert!(text.contains("1 removed · across 1 server"), "{text}");
    }

    #[test]
    fn a_followed_deploy_counts_the_server_whose_copy_it_kept() {
        let plan = crate::global_fixtures::replicated_plan(8, &['1'], 8);
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        view.namespace = "app".parse().unwrap();
        view.runtime_names.clear();
        let rows: Vec<_> = plan
            .preview()
            .operations
            .iter()
            .map(|row| json!({"machine_id": row.machine_id, "server": "edge", "state": "completed", "started_at": 1, "finished_at": 2}))
            .collect();
        view.nodes = vec![serde_json::from_value(json!({
            "type": "service", "id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "name": "api", "outcome": "deployed", "rows": rows
        }))
        .unwrap()];
        view.preview = Some(plan.preview().clone());
        let frame = frame(&view);
        assert!(
            frame.rows.iter().any(|row| matches!(
                &row.subject,
                Subject::Service(graph) if graph.machines.len() == 8
            )),
            "{:?}",
            frame.rows
        );
    }

    #[test]
    fn a_renamed_service_reads_by_its_service_name_not_its_private_name() {
        let mut removal = view();
        removal.deployment.status = DeploymentStatus::Applied;
        removal.deployment.remove = true;
        removal.nodes.truncate(1);
        let node = removal.nodes.first_mut().unwrap();
        node.outcome = NodeStatus::Removed;
        for row in &mut node.rows {
            row.state = RowState::Completed;
        }
        let lines = crate::ui::progress::live_lines(&frame(&removal), 0, 80, 40, false);
        assert!(lines.iter().any(|line| line.contains(" web ")), "{lines:?}");
        assert!(
            !lines.iter().any(|line| line.contains("private-web")),
            "{lines:?}"
        );
        let failure = failure(&view(), &[], command);
        let text = crate::ui::plain(&failure);
        assert!(
            text.contains("web on edge: CreateContainer failed"),
            "{text}"
        );
        assert!(
            !text.contains("private-web on edge: CreateContainer"),
            "{text}"
        );
        assert!(
            text.contains("Last 1 log lines from web on edge:"),
            "{text}"
        );
        assert!(!text.contains("from private-web"), "{text}");
        // JSON keeps the runtime identity that log lookups use.
        assert_eq!(
            failure.json().pointer("/details/progress/logs/0/service"),
            Some(&json!("shop-production/private-web"))
        );
    }

    #[test]
    fn a_renamed_services_port_warning_names_it_by_its_service_name() {
        let mut view = view();
        view.deployment.warnings = vec![serde_json::from_value(json!({"warning": {
            "type": "image_port_mismatch", "service": "private-web", "service_id": "c".repeat(32),
            "image": "nginx", "machines": ["a".repeat(32)], "exposed": [80], "routed": [8080],
        }}))
        .unwrap()];
        let notices: Vec<_> = frame(&view)
            .notices
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(
            notices,
            [
                "web's image nginx exposes 80; its domains send to 8080. Set the domain port or PORT."
            ]
        );
    }

    #[test]
    fn a_failed_limits_update_keeps_dockers_words_and_says_to_deploy_again() {
        let docker = "Docker responded with status code 400: Conflicting options: Nano CPUs and CPU Period cannot both be set";
        let reason = "web: Server edge could not confirm the new limits";
        let cause = ["Docker operation failed", docker];
        let mut view = view();
        view.deployment.outcome = Some(
            serde_json::from_value(json!({
                "type": "executed", "summary": {}, "reason": reason, "cause": cause,
            }))
            .unwrap(),
        );
        view.nodes = vec![serde_json::from_value(json!({
            "type": "service", "id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "name": "web", "outcome": "failed", "rows": [
                {"machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "server": "edge", "state": "failed", "reason": &reason[5..], "cause": cause, "log": null, "started_at": 1, "finished_at": 2}
            ]
        }))
        .unwrap()];
        let failure = failure(&view, &[], command);
        assert_eq!(
            crate::ui::plain(&failure),
            format!(
                "error: {reason}.\n  cause: {docker}\nretry: ployz deploy --project shop --env production\n"
            )
        );
        assert_eq!(
            hints(&failure),
            ["retry: ployz deploy --project shop --env production"]
        );
        assert_eq!(failure.json().get("cause"), Some(&json!(cause)));
    }

    #[test]
    fn a_failed_deploy_prints_what_became_of_the_old_container() {
        let mut view = view();
        view.deployment.outcome = Some(
            serde_json::from_value(serde_json::json!({
                "type": "executed",
                "summary": {},
                "reason": "web: CreateContainer failed",
                "cause": ["pull failed", "denied"],
                "compensation": [
                    "Stopped the new Container.",
                    "Restored the old Container; it serves again.",
                ],
            }))
            .unwrap(),
        );
        let text = crate::ui::plain(&failure(&view, &[], command));
        assert!(
            text.contains(
                "  Stopped the new Container.\n  Restored the old Container; it serves again."
            ),
            "{text}"
        );
    }

    #[test]
    fn duplicate_outcome_is_printed_once_and_identical_failure_on_another_server_survives() {
        let view = view();
        let failure = failure(&view, &[], command);
        let text = crate::ui::plain(&failure);
        assert_eq!(text.matches("cause: denied").count(), 2, "{text}");
        assert!(text.contains("startup failed"));
        assert!(!text.contains("Last 0"));
        assert!(!text.contains("restarted"));
        let hints = failure.hints();
        let commands: Vec<_> = hints
            .iter()
            .filter_map(|hint| match hint {
                Hint::Inspect(command) => Some(shell_words::split(command).unwrap()),
                Hint::Next(_)
                | Hint::Retry(_)
                | Hint::Undo(_)
                | Hint::Closest(_)
                | Hint::Valid(_) => None,
            })
            .collect();
        assert_eq!(
            commands,
            [[
                "ployz",
                "logs",
                "web",
                "--project",
                "shop",
                "--env",
                "production"
            ]]
        );
    }

    /// The step of every failed row.
    fn failed_rows(view: &mut DeploymentView) -> impl Iterator<Item = &mut Option<FailedAt>> {
        view.nodes
            .iter_mut()
            .flat_map(|node| node.rows.iter_mut())
            .filter_map(|row| {
                if let RowState::Failed { step, .. } = &mut row.state {
                    Some(step)
                } else {
                    None
                }
            })
    }

    fn inspects(failure: &Failure) -> Vec<String> {
        failure
            .hints()
            .iter()
            .filter_map(|hint| {
                if let Hint::Inspect(command) = hint {
                    Some(command.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    #[test]
    fn a_pull_failure_inspects_the_image_instead_of_logs_no_container_wrote() {
        let mut view = view();
        for row in failed_rows(&mut view) {
            *row = Some(FailedAt::Pull);
        }
        assert_eq!(
            inspects(&failure(&view, &[], command)),
            ["ployz get web.image --project shop --env production"]
        );

        if let Some(row) = failed_rows(&mut view).nth(1) {
            *row = Some(FailedAt::Start);
        }
        assert_eq!(
            inspects(&failure(&view, &[], command)),
            ["ployz logs web --project shop --env production"]
        );
    }

    #[test]
    fn a_pull_whose_answer_was_lost_blames_neither_the_image_nor_the_change() {
        let mut view = view();
        for node in &mut view.nodes {
            for row in &mut node.rows {
                if let RowState::Failed {
                    step, unanswered, ..
                } = &mut row.state
                {
                    *step = Some(FailedAt::Pull);
                    *unanswered = true;
                }
            }
        }
        let failure = failure(&view, &[], command);
        assert!(inspects(&failure).is_empty(), "{:?}", inspects(&failure));
        assert!(
            !failure
                .hints()
                .iter()
                .any(|hint| matches!(hint, Hint::Undo(_))),
            "{:?}",
            failure.hints()
        );
    }

    #[test]
    fn a_create_failure_offers_no_logs_only_the_old_container_wrote() {
        let mut view = view();
        for node in &mut view.nodes {
            for row in &mut node.rows {
                if let RowState::Failed { step, log, .. } = &mut row.state {
                    *step = Some(FailedAt::Create);
                    *log = None;
                }
            }
        }
        let failed = failure(&view, &[], command);
        assert!(inspects(&failed).is_empty());
        assert!(
            failed
                .hints()
                .iter()
                .any(|hint| matches!(hint, Hint::Retry(_)))
        );

        for row in failed_rows(&mut view) {
            *row = None;
        }
        assert!(inspects(&failure(&view, &[], command)).is_empty());
    }

    #[test]
    fn a_container_out_of_memory_names_its_limit_and_says_to_raise_it() {
        let failed = |out_of_memory: Option<ployz_core::OutOfMemory>| {
            let error = ployz_core::ExecutionError::Health {
                container_id: ployz_core::ContainerId::parse("c".repeat(64)).unwrap(),
                failure: ployz_core::HealthFailure::TimedOut {
                    deadline_millis: Some(30_000),
                    last_check: Some(ployz_core::LastHealthCheck::exited(
                        1,
                        "wget: can't connect to remote host (127.0.0.1): Connection refused",
                    )),
                },
                out_of_memory,
                last_run: None,
                departing: None,
            };
            let machine_id = "a".repeat(32);
            let operations: Vec<ployz_core::OperationRow> = serde_json::from_value(json!([{
                "index": 0, "machine_id": machine_id, "service_name": "web",
                "operation": {"type": "remove_container", "machine_id": machine_id, "container_id": "c".repeat(64)},
                "status": {"type": "failed", "error": error}
            }]))
            .unwrap();
            let state = ployz_store::RowTracker::default()
                .changes(&operations)
                .remove(0)
                .state;
            let RowState::Failed { reason, cause, .. } = &state else {
                panic!("expected a failed row, got {state:?}");
            };
            let mut view = view();
            view.deployment.outcome = Some(ployz_store::Outcome::Executed {
                summary: Default::default(),
                reason: Some(format!("web: {reason}")),
                cause: cause.clone(),
                services: Vec::new(),
                compensation: Vec::new(),
                cancelled: false,
            });
            let rows = &mut view.nodes.first_mut().unwrap().rows;
            rows.truncate(1);
            rows.first_mut().unwrap().state = state;
            failure(&view, &[], command)
        };
        let next = |failure: &Failure| {
            failure
                .hints()
                .iter()
                .filter_map(|hint| match hint {
                    Hint::Next(command) => Some(command.clone()),
                    Hint::Inspect(_)
                    | Hint::Retry(_)
                    | Hint::Undo(_)
                    | Hint::Closest(_)
                    | Hint::Valid(_) => None,
                })
                .collect::<Vec<_>>()
        };

        let oom = failed(Some(ployz_core::OutOfMemory {
            memory_bytes: Some(ployz_core::ByteQuantity::try_from(16 << 20).unwrap()),
        }));
        let text = crate::ui::plain(&oom);
        assert!(
            text.contains("web: The Container ran out of memory under its 16 MiB memLimit"),
            "{text}"
        );
        assert!(
            text.contains("did not become healthy within 30s; last check exited 1: wget"),
            "{text}"
        );
        assert_eq!(
            next(&oom),
            ["ployz set 'web.memLimit=32MiB' --project shop --env production"]
        );

        let unlimited = failed(Some(ployz_core::OutOfMemory { memory_bytes: None }));
        assert!(
            crate::ui::plain(&unlimited).contains("web: The Container ran out of memory"),
            "{}",
            crate::ui::plain(&unlimited)
        );
        assert!(next(&unlimited).is_empty());

        let healthy_enough = failed(None);
        assert!(
            crate::ui::plain(&healthy_enough)
                .contains("web: The Container failed its health check"),
            "{}",
            crate::ui::plain(&healthy_enough)
        );
        assert!(next(&healthy_enough).is_empty());

        let limit_gb = |gb: i64| {
            Some(ployz_core::OutOfMemory {
                memory_bytes: Some(ployz_core::ByteQuantity::try_from(gb * 1_000_000_000).unwrap()),
            })
        };
        let at_most = failed(limit_gb(1024));
        assert!(next(&at_most).is_empty(), "{:?}", next(&at_most));
        let text = crate::ui::plain(&at_most);
        assert!(
            text.contains(
                "web already has the most memory memLimit allows, 1024 GB: it needs to use less."
            ),
            "{text}"
        );
        let large = failed(limit_gb(600));
        assert_eq!(
            next(&large),
            ["ployz set 'web.memLimit=1024GB' --project shop --env production"]
        );
        for proposal in [next(&oom), next(&large)].concat() {
            admits(&proposal);
        }
    }

    /// `proposal` parses as `ployz set` and the Config Store admits what it sets.
    fn admits(proposal: &str) {
        use ployz_store::{
            Actor, Change, ConfigStore, CreateProject, CreateService, Edit, EnvironmentId,
            EnvironmentRef, OrganizationId, ProjectId, ProjectName, SealingKey, ServiceLineageId,
            SettingPath,
        };

        let words = shell_words::split(proposal).unwrap();
        let matches = crate::cli::command()
            .try_get_matches_from(&words)
            .unwrap_or_else(|error| panic!("{proposal}: {error}"));
        let Some(("set", set)) = matches.subcommand() else {
            panic!("{proposal} is not a set");
        };
        // As `ployz set` reads its assignments.
        let changes = set
            .get_many::<String>("assignment")
            .unwrap()
            .map(|assignment| {
                let (path, value) = assignment.split_once('=').unwrap();
                Change::Set {
                    path: SettingPath::parse(path).unwrap(),
                    value: serde_json::Value::String(value.to_owned()),
                }
            })
            .collect();
        let store =
            ConfigStore::open("sqlite::memory:", SealingKey::new(b"test").unwrap()).unwrap();
        let who = Actor::system(OrganizationId::parse("org").unwrap());
        store
            .write(
                &who,
                &CreateProject {
                    id: ProjectId::parse("00000000-0000-4000-8000-000000000001").unwrap(),
                    name: ProjectName::parse("shop").unwrap(),
                    default_environment: EnvironmentId::parse(
                        "00000000-0000-4000-8000-000000000002",
                    )
                    .unwrap(),
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &CreateService {
                    id: ServiceLineageId::parse("00000000-0000-4000-8000-000000000003").unwrap(),
                    environment: EnvironmentRef::default(),
                    name: ployz_core::ServiceName::parse("web").unwrap(),
                    image: Some("nginx".into()),
                    template: None,
                },
            )
            .unwrap();
        store
            .write(
                &who,
                &Edit {
                    environment: EnvironmentRef::default(),
                    expect: None,
                    changes,
                },
            )
            .unwrap_or_else(|error| panic!("{proposal}: {error:?}"));
    }

    #[test]
    fn a_deploy_a_newer_one_serves_past_is_a_conflict_without_the_losing_copys_logs() {
        let mut view = view();
        view.deployment.outcome = Some(
            serde_json::from_value(json!({
                "type": "executed",
                "summary": {"type": "failed", "completed": 1, "unexecuted": 0, "reason": "newer_serving"},
                "reason": "web: a newer deploy is already serving web",
            }))
            .unwrap(),
        );
        for row in failed_rows(&mut view) {
            *row = None;
        }
        let failed = failure(&view, &[], command);
        assert_eq!(failed.report().code, RpcErrorCode::Conflict);
        assert!(failed.is_outcome());
        assert_eq!(crate::ui::exit_code(&failed.report().code), 1);
        assert_eq!(failed.json().pointer("/details/progress/logs/0"), None);
        let text = crate::ui::plain(&failed);
        assert!(!text.contains("startup failed"), "{text}");
        assert!(inspects(&failed).is_empty());
        assert!(
            failed
                .hints()
                .iter()
                .any(|hint| matches!(hint, Hint::Retry(_)))
        );
    }

    #[test]
    fn a_built_service_that_failed_to_pull_has_no_image_setting_to_inspect() {
        let mut view = view();
        for row in failed_rows(&mut view) {
            *row = Some(FailedAt::Pull);
        }
        view.builds = serde_json::from_value(json!([
            {"service": "web", "commit": null, "status": "built", "message": null}
        ]))
        .unwrap();
        assert!(inspects(&failure(&view, &[], command)).is_empty());
    }

    #[test]
    fn a_failed_named_deploy_retries_the_services_it_named() {
        let mut view = view();
        view.deployment.services = ["web", "api"]
            .map(|name| ServiceName::parse(name).unwrap())
            .into();
        assert!(
            hints(&failure(&view, &["web"], command))
                .contains(&"retry: ployz deploy web --project shop --env production".to_owned())
        );
    }

    fn hints(failure: &Failure) -> Vec<String> {
        failure
            .hints()
            .iter()
            .map(|hint| match hint {
                Hint::Next(command) => format!("next: {command}"),
                Hint::Inspect(command) => format!("inspect: {command}"),
                Hint::Retry(command) => format!("retry: {command}"),
                Hint::Undo(command) => format!("undo: {command}"),
                Hint::Closest(name) => format!("closest: {name}"),
                Hint::Valid(names) => format!("valid: {}", names.join(", ")),
            })
            .collect()
    }

    fn failed_build(view: &mut DeploymentView) {
        view.nodes.first_mut().unwrap().rows.clear();
        view.builds = vec![
            serde_json::from_value(json!({
                "service": "web", "commit": null, "status": "failed", "message": "exit code: 7"
            }))
            .unwrap(),
        ];
    }

    #[test]
    fn a_failed_build_names_its_build_log_and_never_says_to_deploy_the_same_again() {
        let mut view = view();
        failed_build(&mut view);
        assert_eq!(
            hints(&failure(&view, &[], command)),
            ["inspect: ployz logs --deployment 7 --build web --project shop --env production"]
        );
    }

    fn refused(view: &mut DeploymentView, same_again: bool) {
        view.nodes.clear();
        view.deployment.outcome = Some(
            serde_json::from_value(json!({
                "type": "not_executed",
                "reason": "hostname shop.example.com is requested by both shop/web and api: keep it on one of them",
                "cause": [], "needs_upload": [], "same_again": same_again
            }))
            .unwrap(),
        );
    }

    #[test]
    fn a_refusal_that_fails_the_same_way_again_never_says_to_deploy_again() {
        let mut view = view();
        refused(&mut view, true);
        assert_eq!(hints(&failure(&view, &["web"], command)), [""; 0]);
    }

    #[test]
    fn a_refusal_a_retry_may_get_past_still_says_to_deploy_again() {
        let mut view = view();
        refused(&mut view, false);
        assert_eq!(
            hints(&failure(&view, &["web"], command)),
            ["retry: ployz deploy web --project shop --env production"]
        );
    }

    #[test]
    fn a_refused_upload_never_says_to_up_again_either() {
        let mut view = view();
        view.deployment.upload =
            Some(serde_json::from_value(json!({"digest": "a".repeat(64)})).unwrap());
        refused(&mut view, true);
        assert_eq!(hints(&failure(&view, &[], command)), [""; 0]);
    }

    #[test]
    fn a_failed_upload_retries_with_up_since_a_deploy_ships_the_last_one_that_built() {
        let mut view = view();
        view.deployment.upload =
            Some(serde_json::from_value(json!({"digest": "a".repeat(64)})).unwrap());
        failed_build(&mut view);
        assert_eq!(
            hints(&failure(&view, &[], command)),
            [
                "inspect: ployz logs --deployment 7 --build web --project shop --env production",
                "retry: ployz up --project shop --env production",
            ]
        );
    }

    #[test]
    fn an_image_that_wont_pull_says_to_undo_it_and_never_to_deploy_it_again() {
        let mut view = view();
        for row in &mut view.nodes.first_mut().unwrap().rows {
            if let RowState::Failed { step, .. } = &mut row.state {
                *step = Some(FailedAt::Pull);
            }
        }
        assert_eq!(
            hints(&failure(&view, &[], command)),
            [
                "inspect: ployz get web.image --project shop --env production",
                "undo: ployz history undo 1 --project shop --env production",
            ]
        );
    }

    #[test]
    fn an_image_that_wont_pull_on_one_server_still_says_to_undo_when_another_fails_to_start() {
        let mut view = view();
        for row in failed_rows(&mut view) {
            *row = Some(FailedAt::Pull);
        }
        if let Some(row) = failed_rows(&mut view).nth(1) {
            *row = Some(FailedAt::Start);
        }
        assert_eq!(
            hints(&failure(&view, &[], command)),
            [
                "inspect: ployz logs web --project shop --env production",
                "undo: ployz history undo 1 --project shop --env production",
            ]
        );
    }

    #[test]
    fn a_superseded_deployment_names_its_successor_and_never_says_to_deploy_again() {
        let mut view = view();
        view.deployment.status = DeploymentStatus::Superseded;
        view.deployment.outcome = None;
        let mut newer = view.deployment.clone();
        newer.number = Some(8);
        newer.push = Some(
            serde_json::from_value(json!({
                "repository_id": 1, "branch": "main", "head": format!("bbe4f9e{}", "a".repeat(33))
            }))
            .unwrap(),
        );
        let failure = completion(&view, Some(&newer), &[], command).unwrap_err();
        let text = crate::ui::plain(&failure);
        assert!(
            text.contains("Deployment #7 was superseded by #8 (push bbe4f9e)."),
            "{text}"
        );
        assert_eq!(
            hints(&failure),
            ["inspect: ployz deployment show 8 --project shop --env production"]
        );
        let alone = completion(&view, None, &[], command).unwrap_err();
        assert!(crate::ui::plain(&alone).contains("Deployment #7 was superseded."));
        assert_eq!(
            hints(&alone),
            ["inspect: ployz deployment ls --project shop --env production"]
        );
    }

    #[test]
    fn a_deployment_another_took_over_names_it_and_never_mentions_a_runner() {
        let mut view = view();
        view.deployment.status = DeploymentStatus::Unknown;
        view.deployment.outcome = None;
        let mut newer = view.deployment.clone();
        newer.number = Some(8);
        let failure = replaced(&view, &newer, command);
        let text = crate::ui::plain(&failure);
        assert!(
            text.contains("Deployment #7 was replaced by #8 before it finished."),
            "{text}"
        );
        assert!(!text.contains("runner"), "{text}");
        assert_eq!(
            hints(&failure),
            ["inspect: ployz deployment show 8 --project shop --env production"]
        );
    }

    #[test]
    fn only_a_newer_deployment_that_started_took_over() {
        let mut view = view();
        view.deployment.status = DeploymentStatus::Unknown;
        view.deployment.started_at = Some(100);
        let numbered = |number: u64, status: DeploymentStatus, started_at: Option<i64>| {
            let mut newer = view.deployment.clone();
            newer.id =
                serde_json::from_value(json!(format!("cccccccc-cccc-4ccc-8ccc-{number:012}")))
                    .unwrap();
            newer.number = Some(number);
            newer.status = status;
            newer.started_at = started_at;
            newer.outcome = None;
            newer
        };
        let older = numbered(6, DeploymentStatus::Applied, Some(50));
        let queued = numbered(8, DeploymentStatus::Queued, None);
        let mut refused = numbered(8, DeploymentStatus::Failed, Some(120));
        refused.outcome = Some(ployz_store::Outcome::NotExecuted {
            reason: "refused at claim".into(),
            cause: Vec::new(),
            needs_upload: Vec::new(),
            same_again: false,
        });
        let took_over = numbered(8, DeploymentStatus::Applied, Some(120));
        let after_it = numbered(9, DeploymentStatus::Running, Some(130));
        let named = |candidates: &[DeploymentSummary]| {
            taker(&view, candidates).and_then(|newer| newer.number)
        };

        // A lapsed claim with only an older or a queued one names none.
        assert_eq!(named(&[older.clone(), queued.clone()]), None);
        // The first newer one that started took over, whatever came after it.
        assert_eq!(
            named(&[
                after_it.clone(),
                queued.clone(),
                took_over.clone(),
                older.clone()
            ]),
            Some(8)
        );
        // When the first that started never ran, it may still have taken over: no
        // later one is named in its place.
        assert_eq!(named(&[after_it.clone(), refused, older]), None);
        // Nor when the read missed a Deployment between this one and the first named.
        assert_eq!(named(&[after_it]), None);
        let failure = replaced(&view, &took_over, command);
        let details = failure.report().details;
        assert_eq!(details.get("deployment"), Some(&json!(view.deployment.id)));
        assert_eq!(details.get("replaced_by"), Some(&json!(took_over.id)));
    }

    #[test]
    fn a_failed_removal_never_says_to_retry_with_deploy() {
        let retries = |view: &DeploymentView| {
            failure(view, &[], command)
                .hints()
                .iter()
                .filter(|hint| matches!(hint, Hint::Retry(_)))
                .count()
        };
        let mut removal = view();
        assert_eq!(retries(&removal), 1);
        removal.deployment.remove = true;
        assert_eq!(retries(&removal), 0);
    }

    #[test]
    fn a_failed_deploy_is_its_result_and_a_failed_removal_stays_partial() {
        let mut view = view();
        assert!(
            completion(&view, None, &[], command)
                .unwrap_err()
                .is_outcome()
        );
        view.deployment.remove = true;
        assert!(
            !completion(&view, None, &[], command)
                .unwrap_err()
                .is_outcome()
        );
    }

    #[test]
    fn an_applied_deploy_that_left_a_copy_running_is_partial() {
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        view.deployment.outcome = None;
        assert!(completion(&view, None, &[], command).is_ok());
        view.deployment.warnings = vec![
            DeploymentWarning::Deploy {
                warning: ployz_core::DeployWarning::LeftRunning {
                    service: "shop-production/web".parse().unwrap(),
                    container_id: ployz_core::ContainerId::parse("c".repeat(64)).unwrap(),
                    machine: "one".into(),
                    reason: "one did not answer".into(),
                },
            },
            DeploymentWarning::Deploy {
                warning: ployz_core::DeployWarning::MixedAfterReturn {
                    machine: "one".into(),
                },
            },
        ];
        let failure = completion(&view, None, &[], command).unwrap_err();
        assert_eq!(crate::ui::after_result(&failure), 3);
    }

    /// A removal that could not reach one Server takes the live copies out, records
    /// the Service removed, names the Server, and exits partial. The next full
    /// Deploy prunes the copy it left.
    #[test]
    fn a_removal_beside_an_unreached_server_reads_removed_names_the_server_and_is_partial() {
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        view.deployment.outcome = None;
        view.deployment.remove = true;
        view.runtime_names.clear();
        view.preview = Some(
            serde_json::from_value(json!({
                "namespace": "shop-production", "warnings": [],
                "would_remove": ["shop-production/rmme"],
                "operations": [{
                    "index": 0, "machine_id": "a".repeat(32), "service_name": "rmme",
                    "operation": {"type": "remove_container", "machine_id": "a".repeat(32), "container_id": "d".repeat(64), "purpose": "retire"},
                    "status": {"type": "completed"}
                }, {
                    "index": 1, "machine_id": "b".repeat(32), "service_name": "rmme",
                    "operation": {"type": "remove_container", "machine_id": "b".repeat(32), "container_id": "e".repeat(64), "purpose": "retire"},
                    "status": {"type": "completed"}
                }]
            }))
            .unwrap(),
        );
        view.nodes = serde_json::from_value(json!([{
            "type": "service", "id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "name": "rmme", "outcome": "removed", "rows": [
                {"machine_id": "a".repeat(32), "server": "machine-1", "state": "completed", "started_at": 1, "finished_at": 2},
                {"machine_id": "b".repeat(32), "server": "machine-2", "state": "completed", "started_at": 1, "finished_at": 2}
            ]
        }]))
        .unwrap();
        // The plan names machine-3 as one it could not check, and the run names the
        // copy it holds. The row says why once.
        view.preview.as_mut().unwrap().warnings =
            vec![ployz_core::DeployWarning::ObservationOmitted {
                kind: ployz_core::ObservationKind::Container,
                machine_id: ployz_core::MachineId::parse("c".repeat(32)).unwrap(),
                gap: Some(ployz_core::ObservationGap {
                    machine_name: ployz_core::MachineName::parse("machine-3").unwrap(),
                    reason: ployz_core::ObservationGapReason::Down,
                }),
            }];
        view.deployment.warnings = vec![DeploymentWarning::Deploy {
            warning: ployz_core::DeployWarning::LeftRunning {
                service: "shop-production/rmme".parse().unwrap(),
                container_id: ployz_core::ContainerId::parse("c".repeat(64)).unwrap(),
                machine: "machine-3".into(),
                reason:
                    "machine-3 didn't answer, the next full deploy removes its copy once it is back"
                        .into(),
            },
        }];
        let text = crate::ui::progress::settled_text(&frame(&view));
        assert!(
            text.contains(
                "rmme: removed 2/3; machine-3 didn't answer, the next full deploy removes its copy once it is back\n"
            ),
            "{text}"
        );
        assert!(!text.contains("couldn't check"), "{text}");
        assert!(
            text.contains("1 copy left running · 1 removed · across 2 servers"),
            "{text}"
        );
        let failure = completion(&view, None, &[], command).unwrap_err();
        assert_eq!(crate::ui::after_result(&failure), 3);
    }

    /// The only copy of a removed Service may sit on a Server the Deploy could not
    /// check. Nothing was planned, so the Service reads removed with nothing counted,
    /// names that Server, and the Deploy is partial.
    #[test]
    fn a_removal_with_nothing_to_plan_beside_an_unchecked_server_reads_removed_and_names_it() {
        use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        view.deployment.outcome = None;
        view.deployment.remove = true;
        view.runtime_names.clear();
        let mut preview: ployz_core::DeployPreview = serde_json::from_value(json!({
            "namespace": "shop-production", "warnings": [],
            "would_remove": ["shop-production/rmme"], "operations": [],
            "prune_refusal": "incomplete_snapshot"
        }))
        .unwrap();
        preview.warnings = vec![ployz_core::DeployWarning::ObservationFailed {
            gap: Some(ObservationGap {
                machine_name: MachineName::parse("machine-3").unwrap(),
                reason: ObservationGapReason::Failed,
            }),
            kind: ObservationKind::Container,
            machine_id: ployz_core::MachineId::random(),
            machine_name: None,
            message: "Machine machine-3 did not answer: deadline has elapsed".into(),
        }];
        view.preview = Some(preview);
        view.nodes = serde_json::from_value(json!([{
            "type": "service", "id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "name": "rmme", "outcome": "removed", "rows": []
        }]))
        .unwrap();
        let text = crate::ui::progress::settled_text(&frame(&view));
        assert!(
            text.contains(
                "rmme: removed 0/0; couldn't check machine-3, the next full deploy removes any copy there"
            ),
            "{text}"
        );
        let failure = completion(&view, None, &[], command).unwrap_err();
        assert_eq!(crate::ui::after_result(&failure), 3);
    }

    #[test]
    fn an_applied_deploy_with_a_gap_found_at_run_time_is_partial() {
        use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        view.deployment.outcome = None;
        assert!(completion(&view, None, &[], command).is_ok());
        view.deployment.warnings = vec![DeploymentWarning::Deploy {
            warning: ployz_core::DeployWarning::ObservationFailed {
                gap: Some(ObservationGap {
                    machine_name: MachineName::parse("one").unwrap(),
                    reason: ObservationGapReason::Failed,
                }),
                kind: ObservationKind::Container,
                machine_id: ployz_core::MachineId::random(),
                machine_name: None,
                message: "Machine one did not answer: deadline has elapsed".into(),
            },
        }];
        let failure = completion(&view, None, &[], command).unwrap_err();
        assert_eq!(crate::ui::after_result(&failure), 3);
    }

    #[test]
    fn an_applied_deploy_that_couldnt_check_ingress_is_partial() {
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        view.deployment.outcome = None;
        view.deployment.warnings = vec![DeploymentWarning::IngressUnchecked {
            cause: "Server lab-1 isn't answering".into(),
        }];
        let failure = completion(&view, None, &[], command).unwrap_err();
        assert_eq!(crate::ui::after_result(&failure), 3);
    }

    #[test]
    fn only_a_failed_deploy_exits_1_and_a_superseded_one_stays_partial() {
        let mut view = view();
        for (status, code) in [
            (DeploymentStatus::Failed, 1),
            (DeploymentStatus::Superseded, 3),
            (DeploymentStatus::Cancelled, 3),
            (DeploymentStatus::Unknown, 3),
        ] {
            view.deployment.status = status;
            let failure = completion(&view, None, &[], command).unwrap_err();
            assert_eq!(crate::ui::after_result(&failure), code, "{status:?}");
        }
    }

    #[test]
    fn a_failed_removal_retried_by_deployment_says_to_retry_it_again() {
        let mut removal = view();
        assert_eq!(removal_retry(&removal, command), None);
        removal.deployment.remove = true;
        let retry = Some(Hint::Retry(
            "ployz deployment redeploy 7 --project shop --env production".to_owned(),
        ));
        for status in [
            DeploymentStatus::Failed,
            DeploymentStatus::Unknown,
            DeploymentStatus::Cancelled,
        ] {
            removal.deployment.status = status;
            assert_eq!(removal_retry(&removal, command), retry, "{status:?}");
        }
        for status in [
            DeploymentStatus::Applied,
            DeploymentStatus::Queued,
            DeploymentStatus::Running,
            DeploymentStatus::Cancelling,
            DeploymentStatus::Superseded,
        ] {
            removal.deployment.status = status;
            assert_eq!(removal_retry(&removal, command), None, "{status:?}");
        }
    }

    #[test]
    fn every_retry_hint_is_a_command_the_cli_accepts() {
        let mut removal = view();
        removal.deployment.remove = true;
        let hints = failure(&view(), &[], command)
            .hints()
            .iter()
            .cloned()
            .chain(removal_retry(&removal, command))
            .filter_map(|hint| {
                if let Hint::Retry(retry) = hint {
                    Some(retry)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(hints.len(), 2, "{hints:?}");
        for retry in hints {
            let words = shell_words::split(&retry).unwrap();
            if let Err(error) = crate::cli::command().try_get_matches_from(&words) {
                panic!("{retry}: {error}");
            }
        }
    }

    #[test]
    fn applied_result_with_a_qualified_gap_is_partial_without_changing_its_status() {
        use ployz_core::{
            DeployPreview, DeployWarning, MachineId, MachineName, ObservationGap,
            ObservationGapReason, ObservationKind,
        };
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        for node in &mut view.nodes {
            node.outcome = NodeStatus::Unchanged;
        }
        view.preview = Some(DeployPreview::new(
            Vec::new(),
            Vec::new(),
            view.namespace.clone(),
        ));
        assert!(completion(&view, None, &[], command).is_ok());
        assert!(noop(&view));
        assert_eq!(
            up_to_date(&view),
            "Nothing changed on the Servers; Deployment #7 shipped version #1."
        );
        let warning = DeployWarning::ObservationOmitted {
            kind: ObservationKind::Container,
            machine_id: MachineId::random(),
            gap: None,
        };
        view.preview.as_mut().unwrap().warnings.push(warning);
        assert!(completion(&view, None, &[], command).is_ok());
        let DeployWarning::ObservationOmitted { gap, .. } =
            view.preview.as_mut().unwrap().warnings.first_mut().unwrap()
        else {
            unreachable!()
        };
        *gap = Some(ObservationGap {
            machine_name: MachineName::parse("peer").unwrap(),
            reason: ObservationGapReason::Down,
        });
        assert_eq!(
            completion(&view, None, &[], command)
                .unwrap_err()
                .printed_exit(),
            Some(3)
        );
        assert!(!noop(&view));
        assert_eq!(view.deployment.status, DeploymentStatus::Applied);
        let encoded = serde_json::to_value(&view).unwrap();
        assert_eq!(
            encoded.get("status").and_then(serde_json::Value::as_str),
            Some("applied")
        );
        assert!(encoded.pointer("/preview/warnings/0/gap").is_some());
    }

    #[test]
    fn an_ingress_repair_is_work_and_one_left_down_warns_with_its_server_and_cause() {
        use ployz_core::{DeployPreview, MachineId};
        use ployz_store::IngressRepair;
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        for node in &mut view.nodes {
            node.outcome = NodeStatus::Unchanged;
        }
        view.preview = Some(DeployPreview::new(
            Vec::new(),
            Vec::new(),
            view.namespace.clone(),
        ));
        assert!(noop(&view) && !visible(&view));
        let (held, started) = (MachineId::random(), MachineId::random());
        view.deployment.ingress = vec![
            IngressRepair {
                machine: held,
                server: "web-1".into(),
                action: IngressAction::Create,
                failure: Some(IngressFailure::PortTaken {
                    cause: "address already in use".into(),
                }),
            },
            IngressRepair {
                machine: started,
                server: "web-2".into(),
                action: IngressAction::Start,
                failure: None,
            },
        ];
        assert!(!noop(&view) && visible(&view));
        assert!(completion(&view, None, &[], command).is_ok());
        let rows: Vec<_> = frame(&view)
            .rows
            .into_iter()
            .filter(|row| matches!(&row.subject, Subject::ServiceOnServer { service, .. } if *service == QualifiedService::system_ingress()))
            .map(|row| (row.subject, row.state, row.detail))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    Subject::ServiceOnServer {
                        service: QualifiedService::system_ingress(),
                        machine: held,
                        server: "web-1".into(),
                    },
                    State::Warned,
                    Some("create".into()),
                ),
                (
                    Subject::ServiceOnServer {
                        service: QualifiedService::system_ingress(),
                        machine: started,
                        server: "web-2".into(),
                    },
                    State::Completed,
                    Some("start".into()),
                ),
            ]
        );
        assert_eq!(
            ingress_warnings(&view),
            [
                "Ingress isn't running on web-1: something else holds port 80 or 443 (address already in use). \
              See what holds it with `sudo ss -ltnp '( sport = :80 or sport = :443 )'`. \
              Ployz starts ingress on its own once the port is free."
            ]
        );
        view.deployment.ingress.first_mut().unwrap().failure = Some(IngressFailure::Failed {
            cause: "pull failed".into(),
        });
        assert_eq!(
            ingress_warnings(&view),
            ["Ingress isn't running on web-1: pull failed. Deploy again to retry."]
        );
        view.deployment.ingress.first_mut().unwrap().failure = Some(IngressFailure::Failed {
            cause: "pull failed.".into(),
        });
        assert_eq!(
            ingress_warnings(&view),
            ["Ingress isn't running on web-1: pull failed. Deploy again to retry."]
        );
        view.deployment.ingress.first_mut().unwrap().failure =
            Some(IngressFailure::Failed { cause: " ".into() });
        assert_eq!(
            ingress_warnings(&view),
            ["Ingress isn't running on web-1. Deploy again to retry."]
        );
    }

    #[test]
    fn an_unchecked_ingress_is_work_with_an_unknown_row_and_a_retry_warning() {
        use ployz_core::DeployPreview;
        let mut view = view();
        view.deployment.status = DeploymentStatus::Applied;
        for node in &mut view.nodes {
            node.outcome = NodeStatus::Unchanged;
        }
        let mut preview = DeployPreview::new(Vec::new(), Vec::new(), view.namespace.clone());
        preview.depths = [
            ("private-web".parse().unwrap(), 0),
            ("api".parse().unwrap(), 1),
        ]
        .into();
        view.preview = Some(preview);
        view.deployment.warnings = vec![
            DeploymentWarning::IngressUnchecked {
                cause: "lab-1 went unobserved (connection refused)".into(),
            },
            DeploymentWarning::IngressUnchecked {
                cause: "lab-2 went unobserved (no answer)".into(),
            },
        ];
        assert!(!noop(&view) && visible(&view));
        let partial = completion(&view, None, &[], command).unwrap_err();
        assert_eq!(crate::ui::after_result(&partial), 3);
        assert!(frame(&view).rows.iter().any(|row| {
            row.subject == Subject::Unlocated(QualifiedService::system_ingress())
                && row.state == State::Unknown
        }));
        let lines = crate::ui::progress::live_lines(&frame(&view), 0, 80, 40, false);
        let unchecked = lines
            .iter()
            .position(|line| line == "! ingress  unknown (not checked)");
        let graph = lines.iter().position(|line| line.starts_with("depth "));
        assert!(unchecked.is_some() && unchecked < graph);
        assert_eq!(
            ingress_warnings(&view),
            [
                "Couldn't check ingress: lab-1 went unobserved (connection refused). Deploy again to retry.",
                "Couldn't check ingress: lab-2 went unobserved (no answer). Deploy again to retry."
            ]
        );
        view.deployment.warnings = vec![DeploymentWarning::IngressUnchecked { cause: "".into() }];
        assert_eq!(
            ingress_warnings(&view),
            ["Couldn't check ingress. Deploy again to retry."]
        );
    }

    fn planned_run(index: u32, name: &str, image: &str, policy: &str) -> ployz_core::OperationRow {
        let requested: ployz_core::RequestedServiceSpec = serde_json::from_value(json!({
            "name": name, "mode": {"mode": "replicated", "replicas": 1},
            "container": {"image": image, "pull_policy": policy}
        }))
        .unwrap();
        let spec = requested
            .to_resolved(
                ployz_core::ServiceId::random(),
                ployz_core::ResolvedUpdateConfig {
                    order: ployz_core::UpdateOrder::StartFirst,
                    monitor_millis: None,
                },
            )
            .unwrap();
        let machine: ployz_core::MachineId = "a".repeat(32).parse().unwrap();
        ployz_core::OperationRow {
            index,
            machine_id: machine,
            machine_name: None,
            service_name: Some(name.parse().unwrap()),
            display_name: None,
            operation: ployz_core::DeployOperation::RunContainer {
                machine_id: machine,
                spec,
                skip_health_monitor: false,
            },
            status: ployz_core::OperationStatus::Pending,
        }
    }

    /// A running Deploy of web (pytorch, pulled when missing) and api (redis under
    /// `api_policy`), each with its one row in `web` and `api` state (#2879).
    fn pulling_view(web: &str, api: &str, api_policy: &str) -> DeploymentView {
        let mut view = view();
        view.deployment.status = DeploymentStatus::Running;
        view.deployment.outcome = None;
        view.runtime_names.clear();
        view.preview = Some(ployz_core::DeployPreview::new(
            vec![
                planned_run(0, "web", "pytorch/pytorch:2.5.1", "missing"),
                planned_run(1, "api", "redis:7", api_policy),
            ],
            Vec::new(),
            view.namespace.clone(),
        ));
        let row = |state: &str| match state {
            "running" => {
                json!({"machine_id": "a".repeat(32), "server": "edge", "state": "running", "phase": "starting", "serving": false})
            }
            _ => json!({"machine_id": "a".repeat(32), "server": "edge", "state": state}),
        };
        view.nodes = serde_json::from_value(json!([
            {"type": "service", "id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "name": "web", "outcome": "pending", "rows": [row(web)]},
            {"type": "service", "id": "dddddddd-dddd-4ddd-8ddd-dddddddddddd", "name": "api", "outcome": "pending", "rows": [row(api)]}
        ]))
        .unwrap();
        view
    }

    fn details(view: &DeploymentView) -> Vec<Option<String>> {
        frame(view).rows.into_iter().map(|row| row.detail).collect()
    }

    #[test]
    fn a_service_whose_servers_pull_its_image_says_so() {
        assert_eq!(
            details(&pulling_view("pending", "pending", "missing")),
            [
                Some("pulling pytorch/pytorch:2.5.1".to_owned()),
                Some("pulling redis:7".to_owned())
            ]
        );
    }

    #[test]
    fn a_service_that_never_pulls_does_not_say_it_is_pulling() {
        assert_eq!(
            details(&pulling_view("pending", "pending", "never")),
            [Some("pulling pytorch/pytorch:2.5.1".to_owned()), None]
        );
    }

    #[test]
    fn once_a_server_starts_work_no_service_is_pulling() {
        assert_eq!(
            details(&pulling_view("running", "pending", "missing")),
            [None, None]
        );
    }

    #[test]
    fn a_removal_that_started_ends_the_pull() {
        let mut view = pulling_view("pending", "pending", "missing");
        view.untargeted = serde_json::from_value(json!({"old": [
            {"machine_id": "a".repeat(32), "server": "edge", "state": "running", "phase": "removing_container", "serving": false}
        ]}))
        .unwrap();
        let frame = frame(&view);
        assert!(frame.rows.iter().all(|row| {
            row.detail
                .as_deref()
                .is_none_or(|detail| !detail.starts_with("pulling"))
        }));
    }

    /// Volumes are prepared after the pulls, before any Service row moves.
    #[test]
    fn a_deploy_that_prepares_volumes_does_not_claim_only_a_pull() {
        let mut view = pulling_view("pending", "pending", "never");
        let machine: ployz_core::MachineId = "a".repeat(32).parse().unwrap();
        if let Some(preview) = view.preview.as_mut() {
            preview.operations.insert(
                0,
                ployz_core::OperationRow {
                    index: 2,
                    machine_id: machine,
                    machine_name: None,
                    service_name: None,
                    display_name: None,
                    operation: ployz_core::DeployOperation::PrepareVolumes {
                        machine_id: machine,
                        specs: Vec::new(),
                    },
                    status: ployz_core::OperationStatus::Pending,
                },
            );
        }
        assert_eq!(
            details(&view),
            [
                Some("pulling pytorch/pytorch:2.5.1 or preparing Volumes".to_owned()),
                None
            ]
        );
    }

    #[test]
    fn recorded_builds_are_visible_without_inventing_server_placement() {
        let mut view = view();
        view.preview = None;
        view.deployment.status = DeploymentStatus::Running;
        view.nodes.truncate(1);
        let node = view.nodes.first_mut().unwrap();
        node.rows.clear();
        node.outcome = NodeStatus::Pending;
        view.builds.push(ployz_store::BuildView {
            service: "web".parse().unwrap(),
            commit: None,
            status: BuildStatus::Pending,
            message: None,
            started_at: None,
            finished_at: None,
            server: None,
            github_skipped: None,
        });
        assert!(!visible(&view));
        for (status, detail, state) in [
            (BuildStatus::Building, "building", State::Pending),
            (BuildStatus::Built, "built", State::Pending),
            (BuildStatus::Failed, "build failed", State::Failed),
        ] {
            view.builds.first_mut().unwrap().status = status;
            assert!(visible(&view));
            let frame = frame(&view);
            let row = frame.rows.first().unwrap();
            assert!(matches!(
                &row.subject,
                Subject::Service(graph)
                    if graph.service.to_string() == "shop-production/web" && graph.total == 0
            ));
            assert_eq!(row.detail.as_deref(), Some(detail));
            assert_eq!(row.state, state);
            let lines = crate::ui::progress::live_lines(&frame, 0, 80, 40, false);
            assert!(!lines.iter().any(|line| line.starts_with("depth ")));
        }
        view.builds.first_mut().unwrap().status = BuildStatus::Reused;
        view.deployment.status = DeploymentStatus::Applied;
        view.nodes.first_mut().unwrap().outcome = NodeStatus::Unchanged;
        view.preview = Some(ployz_core::DeployPreview::new(
            Vec::new(),
            Vec::new(),
            view.namespace.clone(),
        ));
        assert!(!visible(&view));
        assert!(noop(&view));
    }

    #[test]
    fn a_build_names_its_server_while_it_runs_and_once_it_ends() {
        let mut view = view();
        view.deployment.status = DeploymentStatus::Running;
        view.nodes.truncate(1);
        view.nodes.first_mut().unwrap().rows.clear();
        view.builds = serde_json::from_value(json!([
            {"service": "web", "commit": null, "status": "building", "message": null, "server": "machine-1"}
        ]))
        .unwrap();
        assert_eq!(
            frame(&view).rows.first().unwrap().detail.as_deref(),
            Some("building on machine-1")
        );
        view.builds.first_mut().unwrap().status = BuildStatus::Built;
        assert_eq!(
            frame(&view).rows.first().unwrap().detail.as_deref(),
            Some("built on machine-1")
        );
        assert_eq!(builders(&view), ["web built on machine-1."]);
    }

    #[test]
    fn a_build_github_passed_on_says_why_and_where_it_built_instead() {
        let mut view = view();
        view.builds = serde_json::from_value(json!([
            {"service": "web", "commit": null, "status": "built", "message": null, "server": "machine-2",
             "github_skipped": "acme/web has no .github/workflows/ployz-build.yml on its default branch"},
            {"service": "api", "commit": null, "status": "failed", "message": "exit code: 1",
             "github_skipped": "GitHub couldn't take the build: rate limited."},
            {"service": "worker", "commit": null, "status": "reused", "message": null}
        ]))
        .unwrap();
        assert_eq!(
            builders(&view),
            [
                "web built on machine-2. GitHub skipped it: acme/web has no .github/workflows/ployz-build.yml on its default branch.",
                "GitHub skipped api: GitHub couldn't take the build: rate limited.",
            ]
        );
    }
}
