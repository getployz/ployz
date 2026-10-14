//! Direct progress aggregation, retained diagnostics, and recovery commands.
use super::*;
use crate::ui::Hint;
use ployz_core::{
    DeployPreview, MachineAction, MachineName, RequestedServiceSpec, ResolvedServiceSpec,
    ResolvedUpdateConfig, UpdateOrder,
};
use ployz_store::RowPhase;

fn spec() -> ResolvedServiceSpec {
    let requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({"name":"web", "mode":{"mode":"replicated","replicas":1}, "container":{"image":"nginx", "pull_policy":"missing"}})).unwrap();
    requested
        .to_resolved(
            ployz_core::ServiceId::random(),
            ResolvedUpdateConfig {
                order: UpdateOrder::StartFirst,
                monitor_millis: None,
            },
        )
        .unwrap()
}
fn operation(index: u32, machine: MachineId, status: OperationStatus) -> OperationRow {
    OperationRow {
        index,
        machine_id: machine,
        machine_name: Some(MachineName::parse("edge").unwrap()),
        service_name: Some(ServiceName::parse("web").unwrap()),
        display_name: None,
        operation: DeployOperation::RunContainer {
            machine_id: machine,
            spec: spec(),
            skip_health_monitor: false,
        },
        status,
    }
}
fn denied() -> ExecutionError {
    ExecutionError::Machine {
        action: MachineAction::CreateContainer,
        error: RpcError {
            code: RpcErrorCode::Unavailable,
            message: "Image pull failed".into(),
            cause: vec![
                "registry request failed".into(),
                "denied by registry".into(),
            ],
            details: serde_json::Value::Null,
        },
    }
}

#[test]
fn replicas_on_two_machines_fold_into_one_partial_service_row() {
    let one = MachineId::random();
    let two = MachineId::random();
    let operations = vec![
        operation(0, one, OperationStatus::Completed),
        operation(
            1,
            one,
            OperationStatus::Running {
                phase: OperationPhase::StartingContainer,
            },
        ),
        operation(2, two, OperationStatus::Completed),
    ];
    let preview = DeployPreview::new(
        operations.clone(),
        Vec::new(),
        Namespace::parse("shop").unwrap(),
    );
    let mut direct = Direct::new(&preview, "Deploying shop".into());
    let frame = direct.frame();
    assert_eq!(frame.rows.len(), 1);
    let row = frame.rows.first().unwrap();
    assert_eq!(
        row.state,
        State::Running(ployz_store::RowPhase::StartingContainer)
    );
    assert!(matches!(
        &row.subject,
        Subject::Service(graph) if graph.ready == 1 && graph.total == 2 && graph.machines.len() == 2
    ));
    let live = lines(&frame);
    let [depth, web] = live.as_slice() else {
        panic!("expected a depth header and one row: {live:?}");
    };
    assert_eq!(depth, "depth 0");
    assert!(web.starts_with("  ◐ web  booting 1/2"), "{live:?}");
    let completed: Vec<_> = operations
        .into_iter()
        .map(|row| OperationRow {
            status: OperationStatus::Completed,
            ..row
        })
        .collect();
    direct.observe(&completed);
    assert_eq!(lines(&direct.frame()), ["depth 0", "  ✔ web  switched 2/2"]);
}

fn group(service: &str, dependencies: &[&str]) -> ServiceGroup {
    ServiceGroup {
        service: Some(ServiceName::parse(service).unwrap()),
        dependencies: dependencies
            .iter()
            .map(|dependency| ServiceName::parse(*dependency).unwrap())
            .collect(),
        steps: Vec::new(),
        publish: true,
        kept: Vec::new(),
    }
}

fn row_state(state: &str) -> RowState {
    match state {
        "pending" => RowState::Pending,
        "booting" => RowState::Running {
            phase: RowPhase::WaitingForHealth,
        },
        "draining" => RowState::Running {
            phase: RowPhase::StoppingContainer,
        },
        "completed" => RowState::Completed,
        "failed" => RowState::Failed {
            reason: "health check failed".into(),
            cause: Vec::new(),
            step: None,
            log: None,
        },
        "not_attempted" => RowState::NotAttempted,
        other => panic!("unknown row state {other}"),
    }
}

/// A frame of `services` in the given order, each with one replica per state,
/// at the depth `groups` place it.
fn graph_frame(groups: &[ServiceGroup], services: &[(&str, &[&str])]) -> Frame {
    let depths = depths(groups);
    let namespace = Namespace::parse("shop").unwrap();
    Frame {
        run: Run::Direct(namespace.clone()),
        title: "Deploying shop".into(),
        rows: services
            .iter()
            .map(|(service, states)| {
                let name = ServiceName::parse(*service).unwrap();
                let states: Vec<_> = states.iter().map(|state| row_state(state)).collect();
                service_row(
                    QualifiedService::new(namespace.clone(), name.clone()),
                    depths.get(&name).copied().unwrap_or(0),
                    states.iter().map(|state| Replica {
                        machine: MachineId::random(),
                        state,
                        timing: Timing::Unavailable,
                    }),
                )
            })
            .collect(),
        notices: Vec::new(),
    }
}

/// The live view below its title line, without color.
fn lines(frame: &Frame) -> Vec<String> {
    crate::ui::progress::live_lines(frame, 0, 80, 40, false)
        .into_iter()
        .skip(1)
        .collect()
}

#[test]
fn a_chain_starts_one_depth_after_another() {
    let groups = [
        group("db", &[]),
        group("api", &["db"]),
        group("web", &["api"]),
    ];
    let frame = graph_frame(
        &groups,
        &[
            ("web", &["pending"]),
            ("api", &["booting"]),
            ("db", &["completed"]),
        ],
    );
    assert_eq!(
        lines(&frame),
        [
            "depth 0",
            "  ✔ db  switched 1/1",
            "depth 1",
            "  ⠋ api  booting 0/1",
            "depth 2",
            "  · web  waiting 0/1",
        ]
    );
}

#[test]
fn a_diamond_places_the_join_below_its_deepest_side() {
    let groups = [
        group("db", &[]),
        group("cache", &[]),
        group("queue", &["db"]),
        group("api", &["queue", "cache"]),
    ];
    let depths = depths(&groups);
    let depth = |service: &str| depths.get(&ServiceName::parse(service).unwrap()).copied();
    assert_eq!(
        [depth("db"), depth("cache"), depth("queue"), depth("api")],
        [Some(0), Some(0), Some(1), Some(2)]
    );
    let frame = graph_frame(
        &groups,
        &[
            ("api", &["pending"]),
            ("queue", &["completed"]),
            ("cache", &["completed"]),
            ("db", &["completed"]),
        ],
    );
    assert_eq!(
        lines(&frame),
        [
            "depth 0",
            "  ✔ cache  switched 1/1",
            "  ✔ db  switched 1/1",
            "depth 1",
            "  ✔ queue  switched 1/1",
            "depth 2",
            "  · api  waiting 0/1",
        ]
    );
}

#[test]
fn a_partially_healthy_service_shows_its_ready_count() {
    let frame = graph_frame(
        &[group("db", &[]), group("app", &["db"])],
        &[
            ("db", &["completed"]),
            ("app", &["completed", "booting", "pending"]),
        ],
    );
    assert_eq!(
        lines(&frame),
        [
            "depth 0",
            "  ✔ db  switched 1/1",
            "depth 1",
            "  ◐ app  booting 1/3",
        ]
    );
}

#[test]
fn a_failure_leaves_every_later_service_not_started() {
    let frame = graph_frame(
        &[
            group("db", &[]),
            group("api", &["db"]),
            group("web", &["api"]),
            group("worker", &["api"]),
        ],
        &[
            ("db", &["completed"]),
            ("api", &["failed", "completed"]),
            ("web", &["not_attempted"]),
            ("worker", &["not_attempted", "not_attempted"]),
        ],
    );
    assert_eq!(
        lines(&frame),
        [
            "depth 0",
            "  ✔ db  switched 1/1",
            "depth 1",
            "  ✘ api  failed 1/2",
            "depth 2",
            "  ⊘ web  not started",
            "  ⊘ worker  not started",
        ]
    );
}

#[test]
fn old_containers_draining_get_a_line_under_their_service() {
    let frame = graph_frame(
        &[group("db", &[]), group("app", &["db"])],
        &[
            ("db", &["completed", "draining"]),
            ("app", &["completed", "draining", "draining"]),
        ],
    );
    assert_eq!(
        lines(&frame),
        [
            "depth 0",
            "  ◐ db  booting 1/2",
            "    draining old",
            "depth 1",
            "  ◐ app  booting 1/3",
            "    draining old (2)",
        ]
    );
}

#[test]
fn a_failure_never_jumps_ahead_of_an_earlier_depth() {
    let frame = graph_frame(
        &[group("db", &[]), group("api", &["db"])],
        &[("api", &["failed"]), ("db", &["pending"])],
    );
    assert_eq!(
        lines(&frame),
        [
            "depth 0",
            "  · db  waiting 0/1",
            "depth 1",
            "  ✘ api  failed 0/1",
        ]
    );
}

#[test]
fn contextual_failure_keeps_one_deepest_cause_tail_and_exact_inspect_command() {
    let machine = MachineId::random();
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Operation {
            operation: operation(0, machine, OperationStatus::Pending).operation,
            error: denied(),
        },
        unexecuted: Vec::new(),
        services: Vec::new(),
    };
    let tail = LogTail {
        service: QualifiedService::parse("ployz-system/ingress").unwrap(),
        machine,
        server: "edge".into(),
        log: Some(shown("could not load configuration")),
    };
    let failure = crate::handlers::ingress_hints(failure(&outcome, vec![tail]), |args| {
        shell_words::join(
            std::iter::once("ployz")
                .chain(args.iter().copied())
                .chain(["--context", "production"]),
        )
    })
    .context("Server initialized; ingress deployment incomplete.")
    .hint(Hint::Retry(
        "ployz server set edge --accepts-ingress=true".into(),
    ));
    let text = crate::ui::plain(&failure);
    assert!(text.contains("error: Server initialized; ingress deployment incomplete."));
    assert_eq!(text.matches("cause:").count(), 1, "{text}");
    assert!(text.contains("cause: denied by registry"));
    assert!(text.contains("could not load configuration"));
    assert!(text.contains(&format!(
        "inspect: ployz logs ployz-system/ingress --machine {machine} --context production"
    )));
    assert!(text.contains("retry: ployz server set edge --accepts-ingress=true"));
    let json = failure.json();
    assert!(
        json.get("cause")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value == "registry request failed")
    );
    assert_eq!(
        json.pointer("/details/progress/logs/0/log/lines/0")
            .unwrap(),
        "could not load configuration"
    );
}

#[test]
fn missing_tail_retains_inspect_and_compensation_is_only_actual_evidence() {
    let machine = MachineId::random();
    let replacement = ployz_core::ReplacementOperation {
        machine_id: machine,
        old_container_id: ContainerId::parse("a".repeat(64)).unwrap(),
        spec: spec(),
        skip_health_monitor: false,
    };
    let outcome = DeployOutcome::Failed {
        completed: Vec::new(),
        failed: FailedOperation::Replacement {
            operation: replacement,
            error: denied(),
            compensation: ReplacementCompensation::OldStopped {
                stop_new_container: Some(StopAttempt::Stopped),
                restart_old_container: RestartAttempt::Failed { error: denied() },
            },
        },
        unexecuted: Vec::new(),
        services: Vec::new(),
    };
    let tail = LogTail {
        service: QualifiedService::parse("shop/web").unwrap(),
        machine,
        server: "edge".into(),
        log: None,
    };
    let text = crate::ui::plain(&crate::handlers::ingress_hints(
        failure(&outcome, vec![tail]),
        |args| {
            shell_words::join(
                std::iter::once("ployz")
                    .chain(args.iter().copied())
                    .chain(["--context", "production"]),
            )
        },
    ));
    assert!(text.contains("Stopped the new Container."));
    assert!(text.contains("Could not restart the old Container."));
    assert_eq!(
        text.matches("cause: denied by registry").count(),
        2,
        "{text}"
    );
    assert!(text.contains("inspect: ployz logs shop/web"));
    assert!(!text.contains("Last 0"));
    assert!(!text.contains("Restarted the old"));
}

fn shown(text: &str) -> ployz_store::tools::LogText {
    let mut secrets = ployz_store::tools::SecretSet::from_store([], []);
    secrets.add_machine_env(Some(&std::collections::BTreeMap::new()));
    ployz_store::tools::LogExcerpt::scrub(
        [text.to_owned()],
        &secrets,
        ployz_store::tools::LineCap::default(),
    )
}
