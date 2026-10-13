//! Present the Store's recorded Deployment evidence without new Cluster reads.

use crate::failure::Failure;
use crate::ui::{
    Hint,
    progress::{Detail, Diagnostic, Frame, LogTail, Row, Run, State, Subject, Timing},
};
use ployz_core::{QualifiedService, RpcError, RpcErrorCode};
use ployz_store::{
    BuildStatus, DeployedNode, DeploymentStatus, DeploymentView, DeploymentWarning, IngressAction,
    IngressFailure, NodeStatus, RowState,
};
use std::time::{Duration, UNIX_EPOCH};

pub(super) fn frame(view: &DeploymentView) -> Frame {
    let mut rows = Vec::new();
    for (index, node) in view.nodes.iter().enumerate() {
        let service = match &node.node {
            DeployedNode::Service { name, .. } => Some(QualifiedService::new(
                view.namespace.clone(),
                view.runtime_names.get(name).unwrap_or(name).clone(),
            )),
            DeployedNode::Volume { .. } | DeployedNode::Config { .. } => None,
        };
        if let Some(service) = &service
            && !node.rows.is_empty()
        {
            rows.extend(node.rows.iter().map(|row| {
                Row {
                    subject: Subject::ServiceOnServer {
                        service: service.clone(),
                        machine: row.machine_id,
                        server: row.server.clone(),
                    },
                    state: (&row.state).into(),
                    detail: None,
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
                }
            }));
        } else {
            let build = view.builds.iter().find(|build| match &node.node {
                DeployedNode::Service { name, .. } => build.service == *name,
                DeployedNode::Volume { .. } | DeployedNode::Config { .. } => false,
            });
            rows.push(Row {
                subject: service.map_or_else(
                    || Subject::Node {
                        index,
                        name: match &node.node {
                            DeployedNode::Config { name, .. } => format!("Config {name}"),
                            DeployedNode::Volume { name, .. } => format!("Volume {name}"),
                            DeployedNode::Service { name, .. } => name.to_string(),
                        },
                    },
                    Subject::Service,
                ),
                state: match node.outcome {
                    NodeStatus::Pending
                        if build.is_some_and(|build| build.status == BuildStatus::Failed) =>
                    {
                        State::Failed
                    }
                    NodeStatus::Pending => State::Pending,
                    NodeStatus::Deployed | NodeStatus::Removed => State::Completed,
                    NodeStatus::Failed => State::Failed,
                    NodeStatus::NotAttempted => State::NotAttempted,
                    NodeStatus::Unchanged => State::Unchanged,
                    NodeStatus::Unknown => State::Unknown,
                },
                detail: build.map(|build| {
                    match build.status {
                        BuildStatus::Pending => "build pending",
                        BuildStatus::Building => "building",
                        BuildStatus::Built => "built",
                        BuildStatus::Reused => "build reused",
                        BuildStatus::Failed => "build failed",
                    }
                    .to_owned()
                }),
                timing: Timing::Unavailable,
            });
        }
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
            subject: Subject::Service(QualifiedService::system_ingress()),
            state: State::Unknown,
            detail: Some("not checked".to_owned()),
            timing: Timing::Unavailable,
        });
    }
    Frame {
        run: Run::Stored(view.deployment.id.clone()),
        title: format!(
            "Deploying #{} of {}/{}",
            view.deployment.number, view.environment.project, view.environment.name
        ),
        rows,
        notices: view
            .preview
            .iter()
            .flat_map(|preview| &preview.warnings)
            .cloned()
            .collect(),
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
            | DeploymentWarning::IngressNotServing { .. } => None,
        })
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
    command: impl Fn(&[&str]) -> String,
) -> Result<(), Failure> {
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
            format!("Deployment #{} did not complete.", view.deployment.number),
            Vec::new(),
        ),
    };
    let mut diagnostic = Diagnostic::default();
    let mut hints = Vec::new();
    let mut primary_seen = false;
    for node in &view.nodes {
        let DeployedNode::Service { name, .. } = &node.node else {
            continue;
        };
        let service = QualifiedService::new(
            view.namespace.clone(),
            view.runtime_names.get(name).unwrap_or(name).clone(),
        );
        for row in &node.rows {
            let RowState::Failed {
                reason: row_reason,
                cause: row_cause,
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
                    message: format!("{} on {}: {row_reason}.", service.name, row.server),
                    causes: row_cause.clone(),
                });
            }
            diagnostic.logs.push(LogTail {
                service: service.clone(),
                machine: row.machine_id,
                server: row.server.clone(),
                log: log.clone(),
            });
            hints.push(Hint::Inspect(command(&[
                "logs",
                &service.to_string(),
                "--machine",
                &row.machine_id.to_string(),
                "--project",
                view.environment.project.as_str(),
                "--env",
                view.environment.name.as_str(),
            ])));
        }
    }
    let mut failure = Failure::from(RpcError {
        code: RpcErrorCode::Internal,
        message: reason,
        cause,
        details: serde_json::Value::Null,
    })
    .with_diagnostic(diagnostic);
    for hint in hints {
        failure = failure.hint(hint);
    }
    failure.hint(Hint::Retry(command(&[
        "deploy",
        "--project",
        view.environment.project.as_str(),
        "--env",
        view.environment.name.as_str(),
    ])))
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
    fn recorded_children_replace_aggregate_and_keep_runtime_identity() {
        let frame = frame(&view());
        assert_eq!(frame.rows.len(), 4);
        assert_eq!(frame.rows.iter().filter(|row| matches!(&row.subject, Subject::ServiceOnServer { service, .. } if service.to_string() == "shop-production/private-web")).count(), 2);
        assert_eq!(
            frame
                .rows
                .iter()
                .filter(|row| matches!(&row.subject, Subject::Service(_)))
                .count(),
            1
        );
        assert!(frame.rows.iter().any(
            |row| matches!(&row.subject, Subject::Node { name, .. } if name == "Volume data")
        ));
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
        assert_eq!(commands.len(), 2);
        assert!(commands.iter().all(|args| {
            args.iter().any(|arg| arg == "shop-production/private-web")
                && args.iter().any(|arg| arg == "--env")
                && args.iter().any(|arg| arg == "production")
        }));
        assert_ne!(commands.first(), commands.get(1));
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
        assert!(completion(&view, command).is_ok());
        assert!(noop(&view));
        let warning = DeployWarning::ObservationOmitted {
            kind: ObservationKind::Container,
            machine_id: MachineId::random(),
            gap: None,
        };
        view.preview.as_mut().unwrap().warnings.push(warning);
        assert!(completion(&view, command).is_ok());
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
            completion(&view, command).unwrap_err().printed_exit(),
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
        assert!(completion(&view, command).is_ok());
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
        view.preview = Some(DeployPreview::new(
            Vec::new(),
            Vec::new(),
            view.namespace.clone(),
        ));
        view.deployment.warnings = vec![
            DeploymentWarning::IngressUnchecked {
                cause: "lab-1 went unobserved (connection refused)".into(),
            },
            DeploymentWarning::IngressUnchecked {
                cause: "lab-2 went unobserved (no answer)".into(),
            },
        ];
        assert!(!noop(&view) && visible(&view));
        assert!(completion(&view, command).is_ok());
        assert!(frame(&view).rows.iter().any(|row| {
            row.subject == Subject::Service(QualifiedService::system_ingress())
                && row.state == State::Unknown
        }));
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
            assert_eq!(
                row.subject,
                Subject::Service(QualifiedService::parse("shop-production/private-web").unwrap())
            );
            assert_eq!(row.detail.as_deref(), Some(detail));
            assert_eq!(row.state, state);
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
