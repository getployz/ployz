//! Present the Store's recorded Deployment evidence without new Cluster reads.

use crate::failure::Failure;
use crate::ui::{
    Hint,
    progress::{Detail, Diagnostic, Frame, LogTail, Row, Run, State, Subject, Timing},
};
use ployz_core::{QualifiedService, RpcError, RpcErrorCode};
use ployz_store::{
    BuildStatus, DeployedNode, DeploymentStatus, DeploymentView, NodeStatus, RowState,
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
            DeployedNode::Volume { .. } => None,
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
                DeployedNode::Volume { .. } => false,
            });
            rows.push(Row {
                subject: service.map_or_else(
                    || Subject::Node {
                        index,
                        name: format!("Volume {}", node.node.name()),
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
    view.preview
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
        && view
            .nodes
            .iter()
            .all(|node| node.outcome == NodeStatus::Unchanged)
        && view
            .preview
            .as_ref()
            .is_some_and(|preview| preview.noop() && !preview.has_observation_gaps())
}

pub(super) fn completion(view: &DeploymentView) -> Result<(), Failure> {
    if view.deployment.status != DeploymentStatus::Applied {
        return Err(failure(view));
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

pub(super) fn failure(view: &DeploymentView) -> Failure {
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
                lines: log.clone(),
            });
            hints.push(Hint::Inspect(shell_words::join([
                "ployz",
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
    failure.hint(Hint::Retry(shell_words::join([
        "ployz",
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

    pub(super) fn view() -> DeploymentView {
        serde_json::from_value(json!({
            "id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa", "environment_id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "number": 7,
            "status": "failed", "saved": 1, "services": [], "remove": false, "admitted_at": 1, "in_flight": false,
            "outcome": {"type": "executed", "summary": {}, "reason": "web: CreateContainer failed", "cause": ["pull failed", "denied"]},
            "environment": {"id": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb", "project": "shop", "name": "production", "revision": 1},
            "namespace": "shop-production", "preview": null, "builds": [], "runtime_names": {"web": "private-web"},
            "nodes": [
                {"type": "service", "id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc", "name": "web", "outcome": "failed", "rows": [
                    {"machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", "server": "edge", "state": "failed", "reason": "CreateContainer failed", "cause": ["pull failed", "denied"], "log": ["startup failed"], "started_at": 1, "finished_at": 2},
                    {"machine_id": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", "server": "edge", "state": "failed", "reason": "CreateContainer failed", "cause": ["pull failed", "denied"], "log": [], "started_at": 1, "finished_at": 2}
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
        let failure = failure(&view);
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
        assert!(completion(&view).is_ok());
        assert!(noop(&view));
        let warning = DeployWarning::ObservationOmitted {
            kind: ObservationKind::Container,
            machine_id: MachineId::random(),
            gap: None,
        };
        view.preview.as_mut().unwrap().warnings.push(warning);
        assert!(completion(&view).is_ok());
        let DeployWarning::ObservationOmitted { gap, .. } =
            view.preview.as_mut().unwrap().warnings.first_mut().unwrap()
        else {
            unreachable!()
        };
        *gap = Some(ObservationGap {
            machine_name: MachineName::parse("peer").unwrap(),
            reason: ObservationGapReason::Down,
        });
        assert_eq!(completion(&view).unwrap_err().printed_exit(), Some(3));
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
            (
                BuildStatus::Built,
                "built",
                State::Pending,
            ),
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
