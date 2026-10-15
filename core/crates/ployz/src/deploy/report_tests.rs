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
    planned(
        index,
        DeployOperation::RunContainer {
            machine_id: machine,
            spec: spec(),
            skip_health_monitor: false,
        },
        status,
    )
}
fn planned(index: u32, operation: DeployOperation, status: OperationStatus) -> OperationRow {
    OperationRow {
        index,
        machine_id: operation.machine_id(),
        machine_name: Some(MachineName::parse("edge").unwrap()),
        service_name: Some(ServiceName::parse("web").unwrap()),
        display_name: None,
        operation,
        status,
    }
}
fn old(hex: char) -> ployz_core::ContainerId {
    ployz_core::ContainerId::parse(hex.to_string().repeat(64)).unwrap()
}
fn replace(machine: MachineId, order: UpdateOrder) -> DeployOperation {
    let mut spec = spec();
    spec.update.order = order;
    DeployOperation::ReplaceContainer(ployz_core::ReplacementOperation {
        machine_id: machine,
        old_container_id: old('f'),
        spec,
        skip_health_monitor: false,
    })
}
fn remove(machine: MachineId) -> DeployOperation {
    DeployOperation::RemoveContainer {
        machine_id: machine,
        container_id: old('e'),
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
    }
}
fn running(phase: OperationPhase) -> OperationStatus {
    OperationStatus::Running { phase }
}
fn tracked(operations: Vec<OperationRow>) -> Vec<String> {
    let mut preview = DeployPreview::new(operations, Vec::new(), Namespace::parse("shop").unwrap());
    preview.depths = [(ServiceName::parse("web").unwrap(), 0)].into();
    lines(&Direct::new(&preview, "Deploying shop".into()).frame())
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
    let mut preview = DeployPreview::new(
        operations.clone(),
        Vec::new(),
        Namespace::parse("shop").unwrap(),
    );
    preview.depths = [(ServiceName::parse("web").unwrap(), 0)].into();
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

#[test]
fn a_service_the_plan_gives_no_depth_renders_outside_the_depth_groups() {
    let machine = MachineId::random();
    let preview = DeployPreview::new(
        vec![operation(
            0,
            machine,
            OperationStatus::Running {
                phase: OperationPhase::StartingContainer,
            },
        )],
        Vec::new(),
        Namespace::parse("shop").unwrap(),
    );
    let frame = Direct::new(&preview, "Deploying shop".into()).frame();
    assert!(matches!(
        &frame.rows.first().unwrap().subject,
        Subject::Service(graph) if graph.depth.is_none()
    ));
    let live = lines(&frame);
    assert!(
        !live.iter().any(|line| line.starts_with("depth ")),
        "{live:?}"
    );
}

#[test]
fn a_failed_replica_keeps_its_service_active_while_others_still_run() {
    let groups = [group("web", &[])];
    let frame = graph_frame(&groups, &[("web", &["failed", "booting"])]);
    let row = frame.rows.first().unwrap();
    assert!(row.state.active(), "{row:?}");
    assert_eq!(row.detail.as_deref(), Some("1 failed"));
    assert_eq!(
        lines(&frame),
        ["depth 0", "  ⠋ web  booting 0/2 (1 failed)"]
    );
    let settled = graph_frame(&groups, &[("web", &["failed", "completed"])]);
    let row = settled.rows.first().unwrap();
    assert_eq!(row.state, State::Failed);
    assert_eq!(row.detail, None);
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
            phase: RowPhase::StartingContainer,
            serving: false,
        },
        "health" => RowState::Running {
            phase: RowPhase::WaitingForHealth,
            serving: false,
        },
        "switch" => RowState::Running {
            phase: RowPhase::WaitingForSwitch,
            serving: false,
        },
        "compensating" => RowState::Running {
            phase: RowPhase::Compensating,
            serving: false,
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
                    Some(depths.get(&name).copied().unwrap_or(0)),
                    states.iter().map(|state| Replica {
                        machine: MachineId::random(),
                        state,
                        timing: Timing::Unavailable,
                        starts: true,
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
fn a_server_ready_for_the_switch_counts_while_another_still_boots() {
    let groups = [group("app", &[])];
    assert_eq!(
        lines(&graph_frame(&groups, &[("app", &["switch", "booting"])])),
        ["depth 0", "  ◐ app  booting 1/2"]
    );
    assert_eq!(
        lines(&graph_frame(&groups, &[("app", &["booting", "switch"])])),
        ["depth 0", "  ◐ app  booting 1/2"]
    );
    assert_eq!(
        lines(&graph_frame(&groups, &[("app", &["switch", "switch"])])),
        ["depth 0", "  ⠋ app  switching 2/2"]
    );
}

#[test]
fn a_server_ready_for_the_switch_never_reads_switching_before_every_server_starts() {
    assert_eq!(
        lines(&graph_frame(
            &[group("app", &[])],
            &[("app", &["switch", "pending"])]
        )),
        ["depth 0", "  ◐ app  booting 1/2"]
    );
}

#[test]
fn a_failure_cancels_the_switch_a_ready_server_waited_for() {
    assert_eq!(
        lines(&graph_frame(
            &[group("app", &[])],
            &[("app", &["switch", "failed"])]
        )),
        ["depth 0", "  ✘ app  failed (0/2 ready)"]
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
            "  ✘ api  failed (1/2 ready)",
            "depth 2",
            "  ⊘ web  not started",
            "  ⊘ worker  not started",
        ]
    );
}

#[test]
fn a_start_first_switch_reads_switched_while_old_containers_drain() {
    let (one, two) = (MachineId::random(), MachineId::random());
    let switched = |status| {
        vec![
            planned(
                0,
                replace(one, UpdateOrder::StartFirst),
                OperationStatus::Completed,
            ),
            planned(
                1,
                replace(two, UpdateOrder::StartFirst),
                OperationStatus::Completed,
            ),
            planned(2, remove(two), status),
        ]
    };
    assert_eq!(
        tracked(switched(running(OperationPhase::RemovingContainer))),
        ["depth 0", "  ⠋ web  switched 2/2  0.0s", "    draining old"]
    );
    assert_eq!(
        tracked(switched(OperationStatus::Pending)),
        ["depth 0", "  ⠋ web  switched 2/2  0.0s", "    draining old"]
    );
    assert_eq!(
        tracked(switched(OperationStatus::Completed)),
        ["depth 0", "  ✔ web  switched 2/2"]
    );
}

#[test]
fn a_start_first_switch_counts_only_servers_that_start_a_container() {
    let (one, two) = (MachineId::random(), MachineId::random());
    assert_eq!(
        tracked(vec![
            operation(0, one, OperationStatus::Completed),
            planned(1, remove(two), running(OperationPhase::StoppingContainer)),
        ]),
        ["depth 0", "  ⠋ web  switched 1/1  0.0s", "    draining old"]
    );
}

#[test]
fn removal_only_work_reads_removing_then_removed() {
    let (one, two) = (MachineId::random(), MachineId::random());
    let removal = |first, second| {
        vec![
            planned(0, remove(one), first),
            planned(1, remove(two), second),
        ]
    };
    assert_eq!(
        tracked(removal(OperationStatus::Pending, OperationStatus::Pending)),
        ["depth 0", "  · web  waiting to remove 0/2"]
    );
    assert_eq!(
        tracked(removal(
            running(OperationPhase::StoppingContainer),
            OperationStatus::Pending
        )),
        ["depth 0", "  ⠋ web  stopping 0/2  0.0s"]
    );
    assert_eq!(
        tracked(removal(
            OperationStatus::Completed,
            running(OperationPhase::RemovingContainer)
        )),
        ["depth 0", "  ◐ web  removing 1/2  0.0s"]
    );
    assert_eq!(
        tracked(removal(
            OperationStatus::Completed,
            OperationStatus::Completed
        )),
        ["depth 0", "  ✔ web  removed 2/2"]
    );
}

fn moving_off_an_old_hook() -> DeployPreview {
    use ployz_core::{
        ContainerKind, ContainerObservation, ContainerObservationParts,
        ContainerRuntimeObservation, HealthObservation, Machine, MachineObservation,
        MembershipObservation, PlacementConstraint, WireGuardPublicKey,
    };
    let id = |hex: char| MachineId::parse(hex.to_string().repeat(32)).unwrap();
    let server = |hex: char, name: &str| {
        MachineObservation::new(
            Machine {
                id: id(hex),
                name: MachineName::parse(name).unwrap(),
                labels: [("fixture".parse().unwrap(), name.parse().unwrap())].into(),
                accepts_builds: true,
                accepts_services: true,
                accepts_ingress: true,
                subnet: format!("10.210.{hex}.0/24").parse().unwrap(),
                public_key: WireGuardPublicKey([hex as u8; 32]),
                public_ip: None,
                advertised_endpoints: Vec::new(),
                runtime: Default::default(),
                build_concurrency: None,
            },
            MembershipObservation::Up,
        )
    };
    let mut requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": {"mode": "replicated", "replicas": 1},
        "container": {"image": "web:2", "pull_policy": "missing"},
        "pre_deploy": {"command": ["db", "migrate"]},
    }))
    .unwrap();
    let mut current = requested.clone();
    current.container.image = "web:1".into();
    requested.placement.constraints =
        [PlacementConstraint::parse("node.labels.fixture == first").unwrap()].into();
    let resolved = current
        .to_resolved(
            ployz_core::ServiceId::random(),
            ResolvedUpdateConfig {
                order: UpdateOrder::StartFirst,
                monitor_millis: None,
            },
        )
        .unwrap();
    let observed = |hex: char, kind| {
        ContainerObservation::try_from(ContainerObservationParts {
            container_id: old(hex),
            display_name: format!("web-{hex}"),
            created_at_unix_nanos: 0,
            machine_id: id('2'),
            namespace: Namespace::parse("shop").unwrap(),
            kind,
            runtime: ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            effective_healthcheck: None,
            resolved_spec: resolved.clone(),
            address: None,
            labels: Default::default(),
        })
        .unwrap()
    };
    let snapshot = crate::deploy::DeploySnapshot {
        machines: vec![server('1', "first"), server('2', "second")],
        containers: vec![
            observed('b', ContainerKind::ServiceContainer),
            observed('c', ContainerKind::PreDeployHook),
        ],
        ..Default::default()
    };
    let mut preview = crate::deploy::preview_deploy(
        &ployz_core::DeployIntent::apply_all(
            Namespace::parse("shop").unwrap(),
            [&requested],
            ployz_core::PlanOptions::default(),
        ),
        &snapshot,
    )
    .unwrap();
    preview.depths = [(ServiceName::parse("web").unwrap(), 0)].into();
    let order: Vec<_> = preview
        .operations
        .iter()
        .map(|row| (&row.operation, row.machine_id == id('2')))
        .collect();
    assert!(
        matches!(
            order.as_slice(),
            [
                (DeployOperation::StopHook { .. }, true),
                (DeployOperation::RunHook { .. }, false),
                (DeployOperation::RunContainer { .. }, false),
                (DeployOperation::RemoveContainer { .. }, true),
            ]
        ),
        "{order:?}"
    );
    preview
}

#[test]
fn stopping_an_old_hook_before_any_start_never_reads_switched() {
    let mut preview = moving_off_an_old_hook();
    if let Some(stop) = preview.operations.first_mut() {
        stop.status = running(OperationPhase::StoppingContainer);
    }
    assert_eq!(
        lines(&Direct::new(&preview, "Deploying shop".into()).frame()),
        ["depth 0", "  ⠋ web  stopping old 0/1  0.0s"]
    );
    for (index, row) in preview.operations.iter_mut().enumerate() {
        row.status = match index {
            0 => OperationStatus::Completed,
            1 => running(OperationPhase::CreatingContainer),
            _ => OperationStatus::Pending,
        };
    }
    assert_eq!(
        lines(&Direct::new(&preview, "Deploying shop".into()).frame()),
        ["depth 0", "  ⠋ web  booting 0/1  0.0s"]
    );
    for row in &mut preview.operations {
        if !matches!(row.operation, DeployOperation::RemoveContainer { .. }) {
            row.status = OperationStatus::Completed;
        }
    }
    assert_eq!(
        lines(&Direct::new(&preview, "Deploying shop".into()).frame()),
        ["depth 0", "  ⠋ web  switched 1/1  0.0s", "    draining old"]
    );
}

#[test]
fn a_pending_removal_drains_only_once_a_new_container_serves() {
    let (one, two) = (MachineId::random(), MachineId::random());
    assert_eq!(
        tracked(vec![
            operation(0, one, running(OperationPhase::CreatingContainer)),
            planned(1, remove(two), OperationStatus::Pending),
        ]),
        ["depth 0", "  ⠋ web  booting 0/1  0.0s"]
    );
}

#[test]
fn a_stop_first_replacement_reads_stopping_old_until_its_start_runs() {
    let machine = MachineId::random();
    let replacement = |phase| {
        tracked(vec![planned(
            0,
            replace(machine, UpdateOrder::StopFirst),
            running(phase),
        )])
    };
    assert_eq!(
        replacement(OperationPhase::StoppingContainer),
        ["depth 0", "  ⠋ web  stopping old 0/1  0.0s"]
    );
    assert_eq!(
        replacement(OperationPhase::CreatingContainer),
        ["depth 0", "  ⠋ web  booting 0/1  0.0s"]
    );
}

#[test]
fn rollback_and_health_waits_read_their_own_words_with_the_count() {
    let groups = [group("web", &[])];
    assert_eq!(
        lines(&graph_frame(
            &groups,
            &[("web", &["completed", "compensating"])]
        )),
        ["depth 0", "  ◐ web  rolling back 1/2"]
    );
    assert_eq!(
        lines(&graph_frame(&groups, &[("web", &["completed", "health"])])),
        ["depth 0", "  ◐ web  waiting for health 1/2"]
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
            "  ✘ api  failed (0/1 ready)",
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
