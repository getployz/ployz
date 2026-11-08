//! How a Deploy graph's Service rows read, by depth.
use super::*;
use crate::ui::progress::{Frame, Run};
use ployz_core::{MachineId, Namespace};
use ployz_store::RowPhase;

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

#[test]
fn a_rollback_does_not_count_new_copies_that_are_going_away() {
    let groups = [group("web", &[])];
    let frame = graph_frame(
        &groups,
        &[("web", &["failed", "compensating", "compensating"])],
    );
    assert_eq!(
        lines(&frame),
        ["depth 0", "  ⠋ web  rolling back (1 failed)"]
    );
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
            unanswered: false,
            out_of_memory: None,
            stopped_by: ployz_store::StoppedBy::NOTHING,
        },
        "cancelled" => RowState::Failed {
            reason: "Cancelled".into(),
            cause: Vec::new(),
            step: Some(ployz_store::FailedAt::Create),
            log: None,
            unanswered: false,
            out_of_memory: None,
            stopped_by: ployz_store::StoppedBy::CANCEL,
        },
        "interrupted" => RowState::Failed {
            reason: "Cancelled".into(),
            cause: Vec::new(),
            step: Some(ployz_store::FailedAt::Create),
            log: None,
            unanswered: false,
            out_of_memory: None,
            stopped_by: ployz_store::StoppedBy::LOST_RUNNER,
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
        run: Run::CatchUp(MachineId::parse("1".repeat(32)).unwrap()),
        title: "Deploying shop".into(),
        rows: services
            .iter()
            .map(|(service, states)| {
                let name = ServiceName::parse(*service).unwrap();
                let states: Vec<_> = states.iter().map(|state| row_state(state)).collect();
                service_row(
                    QualifiedService::new(namespace.clone(), name.clone()),
                    Some(depths.get(&name).copied().unwrap_or(0)),
                    None,
                    states.iter().map(|state| Replica {
                        machine: MachineId::random(),
                        state,
                        timing: Timing::Unavailable,
                        starts: true,
                        updates: false,
                    }),
                )
            })
            .collect(),
        notices: Vec::new(),
    }
}

/// The live view below its title line, without color, with a running row's clock
/// pinned at zero so the view reads the same however long the test takes.
fn lines(frame: &Frame) -> Vec<String> {
    let mut frame = frame.clone();
    for row in &mut frame.rows {
        if let Timing::Started(_) = row.timing {
            row.timing = Timing::Finished(Duration::ZERO);
        }
    }
    crate::ui::progress::live_lines(&frame, 0, 80, 40, false)
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
fn dependencies_pair_each_deployed_dependency_with_its_dependent() {
    let groups = [
        group("db", &[]),
        group("cache", &["redis"]),
        group("queue", &["db"]),
        group("api", &["queue", "cache"]),
        ServiceGroup {
            service: None,
            ..group("unused", &["db"])
        },
    ];
    let pair = |dependency: &str, dependent: &str| {
        (
            ServiceName::parse(dependency).unwrap(),
            ServiceName::parse(dependent).unwrap(),
        )
    };
    assert_eq!(
        dependencies(&groups),
        [
            pair("db", "queue"),
            pair("queue", "api"),
            pair("cache", "api")
        ],
        "redis is not deployed, so cache waits on nothing here"
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
fn a_cancel_reads_cancelled_where_it_stopped_work_and_failed_where_work_failed() {
    let frame = graph_frame(
        &[
            group("db", &[]),
            group("api", &["db"]),
            group("web", &["db"]),
        ],
        &[
            ("db", &["completed"]),
            ("api", &["cancelled", "completed"]),
            ("web", &["failed", "cancelled"]),
        ],
    );
    assert_eq!(
        lines(&frame),
        [
            "depth 0",
            "  ✔ db  switched 1/1",
            "depth 1",
            "  ⊘ api  cancelled (1/2 ready)",
            "  ✘ web  failed (0/2 ready)",
        ]
    );
}

#[test]
fn work_a_lost_runner_stopped_reads_interrupted_not_failed() {
    let frame = graph_frame(
        &[group("db", &[]), group("web", &["db"])],
        &[
            ("db", &["completed"]),
            ("web", &["interrupted", "completed"]),
        ],
    );
    assert_eq!(
        lines(&frame),
        [
            "depth 0",
            "  ✔ db  switched 1/1",
            "depth 1",
            "  ⊘ web  interrupted (1/2 ready)",
        ]
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
