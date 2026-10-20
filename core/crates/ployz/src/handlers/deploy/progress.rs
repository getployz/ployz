//! Present the Store's recorded Deployment evidence without new Cluster reads.

use crate::deploy::{Replica, service_row, starts};
use crate::failure::Failure;
use crate::ui::{
    Hint,
    progress::{Detail, Diagnostic, Frame, LogTail, Row, Run, State, Subject, Timing},
};
use ployz_core::{QualifiedService, RpcError, RpcErrorCode};
use ployz_store::{
    BuildStatus, DeployedNode, DeploymentStatus, DeploymentSummary, DeploymentView,
    DeploymentWarning, FailedAt, IngressAction, IngressFailure, NodeStatus, RowState,
};
use std::time::{Duration, UNIX_EPOCH};

pub(super) fn frame(view: &DeploymentView) -> Frame {
    let mut rows = Vec::new();
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
                state: node_state(node.outcome, false),
                detail: None,
                timing: Timing::Unavailable,
            });
            continue;
        };
        let runtime = view.runtime_names.get(name).unwrap_or(name);
        let depth = view
            .preview
            .as_ref()
            .and_then(|preview| preview.depths.get(runtime))
            .copied();
        let mut row = service_row(
            QualifiedService::new(view.namespace.clone(), name.clone()),
            depth,
            node.rows.iter().map(|row| Replica {
                machine: row.machine_id,
                state: &row.state,
                starts: view
                    .preview
                    .as_ref()
                    .is_none_or(|preview| starts(&preview.operations, runtime, row.machine_id)),
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
        );
        if node.rows.is_empty() {
            let build = view.builds.iter().find(|build| build.service == *name);
            row.state = node_state(
                node.outcome,
                build.is_some_and(|build| build.status == BuildStatus::Failed),
            );
            row.detail = build.map(|build| {
                match build.status {
                    BuildStatus::Pending => "build pending",
                    BuildStatus::Building => "building",
                    BuildStatus::Built => "built",
                    BuildStatus::Reused => "build reused",
                    BuildStatus::Failed => "build failed",
                }
                .to_owned()
            });
        }
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
            .cloned()
            .collect(),
    }
}

fn node_state(outcome: NodeStatus, build_failed: bool) -> State {
    match outcome {
        NodeStatus::Pending if build_failed => State::Failed,
        NodeStatus::Pending => State::Pending,
        NodeStatus::Deployed | NodeStatus::Removed => State::Completed,
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
                } => format!("Left {variable} out of {service}: it has a broken reference."),
                DeploymentWarning::NotSetInFile {
                    config,
                    file,
                    secret,
                    broken: true,
                } => format!("Left {secret} empty in {config}/{file}: it is a broken reference."),
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
        format!(
            "Couldn't check ingress: {}. Deploy again to retry.",
            cause.trim_end_matches('.')
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
                IngressFailure::Failed { cause } => format!(
                    "Ingress isn't running on {server}: {cause}. Deploy again to retry."
                ),
            })
        })
        .chain(unchecked)
        .collect()
}

pub(super) fn completion(
    view: &DeploymentView,
    successor: Option<&DeploymentSummary>,
    command: impl Fn(&[&str]) -> String,
) -> Result<(), Failure> {
    if view.deployment.status == DeploymentStatus::Superseded {
        return Err(superseded(view, successor, command));
    }
    if view.deployment.status != DeploymentStatus::Applied {
        return Err(failure(view, command));
    }
    if view
        .preview
        .as_ref()
        .is_some_and(|preview| preview.has_observation_gaps())
    {
        return Err(Failure::partial());
    }
    Ok(())
}

/// How a command names a Deployment: its number once it queued, else its ID.
fn target(deployment: &DeploymentSummary) -> String {
    deployment
        .number
        .map_or_else(|| deployment.id.to_string(), |number| number.to_string())
}

/// How to retry a removal or shutdown that ended without applying when the command that
/// ran it is `deployment retry` or `start`: retry this Deployment in its own Environment.
/// A Deploy can't.
pub(super) fn removal_retry(
    view: &DeploymentView,
    command: impl Fn(&[&str]) -> String,
) -> Option<Hint> {
    // The statuses `deployment retry` accepts.
    let ended = matches!(
        view.deployment.status,
        DeploymentStatus::Failed | DeploymentStatus::Unknown | DeploymentStatus::Cancelled
    );
    (view.deployment.remove && ended).then(|| {
        Hint::Retry(command(&[
            "deployment",
            "retry",
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

fn failure(view: &DeploymentView, command: impl Fn(&[&str]) -> String) -> Failure {
    let (reason, cause) = match &view.deployment.outcome {
        Some(
            ployz_store::Outcome::Executed {
                reason: Some(reason),
                cause,
                ..
            }
            | ployz_store::Outcome::NotExecuted { reason, cause, .. },
        ) => (reason.clone(), cause.clone()),
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
    let mut diagnostic = Diagnostic::default();
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
        for row in &node.rows {
            let RowState::Failed {
                reason: row_reason,
                cause: row_cause,
                step,
                log,
                ..
            } = &row.state
            else {
                continue;
            };
            if !primary_seen && reason == format!("{name}: {row_reason}") && cause == *row_cause {
                primary_seen = true;
            } else {
                diagnostic.failures.push(Detail {
                    message: format!("{name} on {}: {row_reason}.", row.server),
                    causes: row_cause.clone(),
                });
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
                Some(FailedAt::Pull) => pulled = true,
                Some(FailedAt::Build) => build_failed = true,
                Some(FailedAt::Create | FailedAt::Volume) => {}
                None => logs |= log.is_some(),
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
            bad_image = true;
        }
    }
    let mut failure = Failure::from(RpcError {
        code: RpcErrorCode::Internal,
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
    if bad_image {
        let revision = view.deployment.saved.to_string();
        failure = failure.hint(Hint::Undo(command(
            &[&["history", "undo", revision.as_str()][..], &scope].concat(),
        )));
    }
    // An upload deploys again only from `up`: a Deploy ships the last one that built.
    if view.deployment.upload.is_some() {
        return failure.hint(Hint::Retry(command(&[&["up"][..], &scope].concat())));
    }
    if same_again || bad_image {
        return failure;
    }
    failure.hint(Hint::Retry(command(&[&["deploy"][..], &scope].concat())))
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
        let failure = failure(&view(), command);
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
    fn duplicate_outcome_is_printed_once_and_identical_failure_on_another_server_survives() {
        let view = view();
        let failure = failure(&view, command);
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
            inspects(&failure(&view, command)),
            ["ployz get web.image --project shop --env production"]
        );

        if let Some(row) = failed_rows(&mut view).nth(1) {
            *row = Some(FailedAt::Start);
        }
        assert_eq!(
            inspects(&failure(&view, command)),
            ["ployz logs web --project shop --env production"]
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
        let failed = failure(&view, command);
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
        assert!(inspects(&failure(&view, command)).is_empty());
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
        assert!(inspects(&failure(&view, command)).is_empty());
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
            hints(&failure(&view, command)),
            ["inspect: ployz logs --deployment 7 --build web --project shop --env production"]
        );
    }

    #[test]
    fn a_failed_upload_retries_with_up_since_a_deploy_ships_the_last_one_that_built() {
        let mut view = view();
        view.deployment.upload =
            Some(serde_json::from_value(json!({"digest": "a".repeat(64)})).unwrap());
        failed_build(&mut view);
        assert_eq!(
            hints(&failure(&view, command)),
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
            hints(&failure(&view, command)),
            [
                "inspect: ployz get web.image --project shop --env production",
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
        let failure = completion(&view, Some(&newer), command).unwrap_err();
        let text = crate::ui::plain(&failure);
        assert!(
            text.contains("Deployment #7 was superseded by #8 (push bbe4f9e)."),
            "{text}"
        );
        assert_eq!(
            hints(&failure),
            ["inspect: ployz deployment show 8 --project shop --env production"]
        );
        let alone = completion(&view, None, command).unwrap_err();
        assert!(crate::ui::plain(&alone).contains("Deployment #7 was superseded."));
        assert_eq!(
            hints(&alone),
            ["inspect: ployz deployment ls --project shop --env production"]
        );
    }

    #[test]
    fn a_failed_removal_never_says_to_retry_with_deploy() {
        let retries = |view: &DeploymentView| {
            failure(view, command)
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
    fn a_failed_removal_retried_by_deployment_says_to_retry_it_again() {
        let mut removal = view();
        assert_eq!(removal_retry(&removal, command), None);
        removal.deployment.remove = true;
        let retry = Some(Hint::Retry(
            "ployz deployment retry 7 --project shop --env production".to_owned(),
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
        assert!(completion(&view, None, command).is_ok());
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
        assert!(completion(&view, None, command).is_ok());
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
            completion(&view, None, command).unwrap_err().printed_exit(),
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
        assert!(completion(&view, None, command).is_ok());
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
        assert!(completion(&view, None, command).is_ok());
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
}
