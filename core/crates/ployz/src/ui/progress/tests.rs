//! Driver behavior with a controllable inbox clock and a real indicatif terminal.

use super::*;
use indicatif::{InMemoryTerm, TermLike};
use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};

struct Script {
    now: Duration,
    events: VecDeque<(Duration, Message)>,
}
impl Inbox for Script {
    fn now(&self) -> Duration {
        self.now
    }
    fn receive(&mut self, timeout: Duration) -> Result<Message, RecvTimeoutError> {
        let deadline = self.now + timeout;
        if self.events.front().is_some_and(|(at, _)| *at <= deadline) {
            let (at, event) = self.events.pop_front().unwrap();
            self.now = self.now.max(at);
            Ok(event)
        } else {
            self.now = deadline;
            Err(RecvTimeoutError::Timeout)
        }
    }
    fn ready(&mut self) -> Option<Message> {
        if self.events.front().is_some_and(|(at, _)| *at <= self.now) {
            self.events.pop_front().map(|(_, message)| message)
        } else {
            None
        }
    }
}

fn frame(state: State) -> Frame {
    Frame {
        run: Run::Direct(Namespace::parse("shop").unwrap()),
        title: "Deploying shop".into(),
        rows: vec![Row {
            subject: Subject::ServiceOnServer {
                service: QualifiedService::parse("shop/web").unwrap(),
                machine: MachineId::random(),
                server: "alpha".into(),
            },
            state,
            detail: None,
            timing: Timing::Unavailable,
        }],
        notices: Vec::new(),
    }
}

fn plain(initial: Frame, events: Vec<(u64, Message)>) -> String {
    let script = Script {
        now: Duration::ZERO,
        events: events
            .into_iter()
            .map(|(ms, event)| (Duration::from_millis(ms), event))
            .collect(),
    };
    let mut output = Vec::new();
    drive(script, initial, Backend::Plain, &mut output, false);
    String::from_utf8(output).unwrap()
}

#[test]
fn quiet_heartbeat_uses_actual_driver_deadline_and_ignores_timing_snapshots() {
    let initial = frame(State::Running(RowPhase::WaitingForHealth));
    let mut timed = initial.clone();
    timed.rows.first_mut().unwrap().timing = Timing::Finished(Duration::from_secs(29));
    let mut done = initial.clone();
    done.rows.first_mut().unwrap().state = State::Completed;
    let text = plain(
        initial,
        vec![
            (29_999, Message::Update(timed)),
            (30_001, Message::Finish(done, Disposition::Settled)),
        ],
    );
    assert_eq!(text.matches("still waiting:").count(), 1, "{text}");
    assert_eq!(
        text.matches("alpha: waiting for health").count(),
        1,
        "{text}"
    );
    assert!(text.find("still waiting:") < text.find("alpha: completed"));
    assert!(!text.contains('\x1b'));
}

#[test]
fn completion_at_deadline_wins_over_stale_heartbeat() {
    let initial = frame(State::Running(RowPhase::WaitingForHealth));
    let mut done = initial.clone();
    done.rows.first_mut().unwrap().state = State::Completed;
    let text = plain(
        initial,
        vec![(30_000, Message::Finish(done, Disposition::Settled))],
    );
    assert!(!text.contains("still waiting:"), "{text}");
    assert!(text.contains("1 deployed · across 1 server"), "{text}");
}

#[test]
fn a_removed_service_counts_as_removed_in_the_summary() {
    let removal = |state| {
        crate::deploy::service_row(
            QualifiedService::parse("shop/web").unwrap(),
            None,
            None,
            [crate::deploy::Replica {
                machine: MachineId::random(),
                state,
                timing: Timing::Unavailable,
                starts: false,
                updates: false,
            }],
        )
    };
    let frame = |row| Frame {
        run: Run::Direct(Namespace::parse("shop").unwrap()),
        title: "Deploying shop".into(),
        rows: vec![row],
        notices: Vec::new(),
    };
    let text = plain(
        frame(removal(&ployz_store::RowState::Pending)),
        vec![(
            1,
            Message::Finish(
                frame(removal(&ployz_store::RowState::Completed)),
                Disposition::Settled,
            ),
        )],
    );
    assert!(text.contains("1 removed · across 1 server"), "{text}");
    assert!(!text.contains("deployed"), "{text}");
}

/// Between two copies' removals, one copy is gone and the other has not begun:
/// the row still reads as removing.
#[test]
fn a_removal_that_has_begun_reads_as_removing_not_starting() {
    use ployz_store::RowState;
    let machines = [MachineId::random(), MachineId::random()];
    let removal = |states: [&RowState; 2]| {
        let row = crate::deploy::service_row(
            QualifiedService::parse("shop/api").unwrap(),
            None,
            None,
            machines
                .iter()
                .zip(states)
                .map(|(machine, state)| crate::deploy::Replica {
                    machine: *machine,
                    state,
                    timing: Timing::Unavailable,
                    starts: false,
                    updates: false,
                }),
        );
        Frame {
            run: Run::Direct(Namespace::parse("shop").unwrap()),
            title: "Deploying shop".into(),
            rows: vec![row],
            notices: Vec::new(),
        }
    };
    let removing = RowState::Running {
        phase: RowPhase::RemovingContainer,
        serving: false,
    };
    let text = plain(
        removal([&RowState::Pending, &RowState::Pending]),
        vec![
            (1, Message::Update(removal([&removing, &RowState::Pending]))),
            (
                2,
                Message::Update(removal([&RowState::Completed, &RowState::Pending])),
            ),
            (
                3,
                Message::Update(removal([&RowState::Completed, &removing])),
            ),
            (
                4,
                Message::Finish(
                    removal([&RowState::Completed, &RowState::Completed]),
                    Disposition::Settled,
                ),
            ),
        ],
    );
    assert!(!text.contains("starting"), "{text}");
    assert!(text.contains("api: waiting to remove 0/2"), "{text}");
    assert!(text.contains("api: removing 0/2"), "{text}");
    assert!(text.contains("api: removing 1/2"), "{text}");
    assert!(text.contains("api: removed 2/2"), "{text}");
}

#[test]
fn phase_and_detail_changes_are_preserved_and_restart_quiet_period() {
    let initial = frame(State::Pending);
    let mut starting = initial.clone();
    starting.rows.first_mut().unwrap().state = State::Running(RowPhase::Starting);
    let mut detail = starting.clone();
    detail.rows.first_mut().unwrap().detail = Some("image ready".into());
    let mut done = detail.clone();
    done.rows.first_mut().unwrap().state = State::Completed;
    let text = plain(
        initial,
        vec![
            (1, Message::Update(starting)),
            (29_999, Message::Update(detail)),
            (59_998, Message::Finish(done, Disposition::Settled)),
        ],
    );
    assert_eq!(text.matches("alpha: starting").count(), 2, "{text}");
    assert!(text.contains("starting (image ready)"));
    assert!(!text.contains("still waiting:"));
}

#[test]
fn replacement_run_resets_row_deduplication() {
    let initial = frame(State::Pending);
    let mut replacement = initial.clone();
    replacement.run = Run::CatchUp(MachineId::random());
    let text = plain(
        initial,
        vec![(1, Message::Update(replacement)), (2, Message::Abandon)],
    );
    assert_eq!(text.matches("alpha: pending").count(), 2);
    assert!(!text.contains("deployed"));
}

#[test]
fn duplicate_server_names_remain_distinct_and_names_can_change() {
    let mut initial = frame(State::Pending);
    let mut other = initial.rows.first().unwrap().clone();
    if let Subject::ServiceOnServer { machine, .. } = &mut other.subject {
        *machine = MachineId::random();
    }
    initial.rows.push(other);
    let mut changed = initial.clone();
    changed.rows.get_mut(1).unwrap().state = State::Completed;
    let text = plain(
        initial,
        vec![(1, Message::Finish(changed, Disposition::Settled))],
    );
    assert_eq!(text.matches("alpha (").count(), 3, "{text}");
    assert!(text.contains("across 2 servers"));
}

#[test]
fn same_named_services_in_different_namespaces_keep_distinct_visible_outcomes() {
    let mut initial = frame(State::Pending);
    let mut other = initial.rows.first().unwrap().clone();
    if let Subject::ServiceOnServer { service, .. } = &mut other.subject {
        *service = QualifiedService::parse("prod/web").unwrap();
    }
    initial.rows.push(other);
    let mut done = initial.clone();
    done.rows.first_mut().unwrap().state = State::ObservedRunning;
    done.rows.get_mut(1).unwrap().state = State::Failed;
    let text = plain(
        initial.clone(),
        vec![(1, Message::Finish(done.clone(), Disposition::Settled))],
    );
    assert!(text.contains("shop/web on alpha: running"), "{text}");
    assert!(text.contains("prod/web on alpha: failed"), "{text}");
    let terminal = InMemoryTerm::new(20, 90);
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal.clone())),
        room,
    );
    drive(
        Script {
            now: Duration::ZERO,
            events: VecDeque::from([(REDRAW, Message::Finish(done, Disposition::Settled))]),
        },
        initial,
        backend,
        &mut TerminalWriter(terminal.clone()),
        false,
    );
    let visible = terminal.contents();
    assert!(visible.contains("shop/web on alpha  running"), "{visible}");
    assert!(visible.contains("prod/web on alpha  failed"), "{visible}");
    assert!(!visible.contains("pending"), "{visible}");
}

#[derive(Clone)]
struct TerminalWriter(InMemoryTerm);
impl Write for TerminalWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0
            .write_str(&std::str::from_utf8(bytes).unwrap().replace("\n", "\r\n"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

fn room() -> (usize, usize) {
    (90, 20)
}

#[test]
fn each_animation_frame_is_written_once_at_the_driver_cadence() {
    let terminal = InMemoryTerm::new(20, 90);
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal.clone())),
        room,
    );
    drive(
        Script {
            now: Duration::ZERO,
            events: VecDeque::from([(Duration::from_millis(250), Message::Abandon)]),
        },
        frame(State::Running(RowPhase::Starting)),
        backend,
        &mut TerminalWriter(terminal.clone()),
        false,
    );
    let writes = terminal.moves_since_last_check();
    assert_eq!(writes.matches("Deploying shop").count(), 3, "{writes}");
    for spinner in ["⠋", "⠙", "⠹"] {
        assert_eq!(writes.matches(spinner).count(), 1, "{writes}");
    }
    assert!(!writes.contains("⠸"), "{writes}");
    assert!(terminal.contents().is_empty());
}

#[test]
fn interactive_finish_retains_full_frame_once_and_preserves_earlier_output() {
    let terminal = InMemoryTerm::new(20, 90);
    terminal.write_line("Earlier command output").unwrap();
    let initial = frame(State::Running(RowPhase::Starting));
    let mut done = initial.clone();
    done.rows.first_mut().unwrap().state = State::Completed;
    done.rows.first_mut().unwrap().timing = Timing::Finished(Duration::from_secs(2));
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal.clone())),
        room,
    );
    let events = VecDeque::from([(
        Duration::from_millis(250),
        Message::Finish(done, Disposition::Settled),
    )]);
    drive(
        Script {
            now: Duration::ZERO,
            events,
        },
        initial,
        backend,
        &mut TerminalWriter(terminal.clone()),
        false,
    );
    let visible = terminal.contents();
    assert!(
        visible.starts_with("Earlier command output\nDeploying shop\n"),
        "{visible}"
    );
    assert_eq!(visible.matches("web on alpha").count(), 1, "{visible}");
    assert!(
        visible.contains("✔ web on alpha  completed  2.0s"),
        "{visible}"
    );
    assert!(
        visible.ends_with("1 deployed · across 1 server"),
        "{visible}"
    );
    assert!(!visible.contains("starting"), "{visible}");
}

fn unsized_pty() -> (usize, usize) {
    window(Ok((0, 0)))
}

#[test]
fn an_unsized_terminal_still_names_each_row() {
    let terminal = InMemoryTerm::new(20, 90);
    let initial = frame(State::Running(RowPhase::Starting));
    let mut failed = initial.clone();
    failed.rows.first_mut().unwrap().state = State::Failed;
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal.clone())),
        unsized_pty,
    );
    drive(
        Script {
            now: Duration::ZERO,
            events: VecDeque::from([(
                Duration::from_millis(250),
                Message::Finish(failed, Disposition::Settled),
            )]),
        },
        initial,
        backend,
        &mut TerminalWriter(terminal.clone()),
        false,
    );
    let visible = terminal.contents();
    assert!(visible.contains("✘ web on alpha  failed"), "{visible}");
}

#[test]
fn abandoned_interactive_driver_clears_live_rows_without_claiming_outcome() {
    let terminal = InMemoryTerm::new(20, 90);
    terminal.write_line("Earlier command output").unwrap();
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal.clone())),
        room,
    );
    drive(
        Script {
            now: Duration::ZERO,
            events: VecDeque::from([(REDRAW, Message::Abandon)]),
        },
        frame(State::Pending),
        backend,
        &mut TerminalWriter(terminal.clone()),
        false,
    );
    assert_eq!(terminal.contents(), "Earlier command output");
}

#[test]
fn short_unicode_viewport_prioritizes_failures_and_never_wraps() {
    let mut initial = frame(State::Completed);
    for i in 0..12 {
        initial.rows.push(Row {
            subject: Subject::Node {
                index: i,
                name: "資料サーバー👩‍💻".repeat(5),
            },
            state: if i == 11 {
                State::Failed
            } else {
                State::Completed
            },
            detail: None,
            timing: Timing::Unavailable,
        });
    }
    let lines = live_lines(&initial, 0, 20, 5, false);
    assert_eq!(lines.len(), 4);
    assert!(lines.get(1).unwrap().starts_with('✘'));
    assert!(lines.last().unwrap().contains("more rows"));
    assert!(lines.iter().all(|line| line.width() < 20));
    for height in 0..=3 {
        let lines = live_lines(&initial, 0, 2, height, false);
        assert!(lines.iter().all(|line| line.width() <= 1));
    }
}

#[test]
fn shrinking_viewport_leaves_no_rows_behind() {
    let terminal = InMemoryTerm::new(20, 90);
    let mut initial = frame(State::Pending);
    initial.rows.push(Row {
        subject: Subject::Node {
            index: 1,
            name: "removed from frame".into(),
        },
        state: State::Pending,
        detail: None,
        timing: Timing::Unavailable,
    });
    let mut smaller = initial.clone();
    smaller.rows.pop();
    smaller.rows.first_mut().unwrap().state = State::Completed;
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal.clone())),
        room,
    );
    drive(
        Script {
            now: Duration::ZERO,
            events: VecDeque::from([
                (REDRAW, Message::Update(smaller.clone())),
                (REDRAW * 2, Message::Finish(smaller, Disposition::Settled)),
            ]),
        },
        initial,
        backend,
        &mut TerminalWriter(terminal.clone()),
        false,
    );
    assert!(!terminal.contents().contains("removed from frame"));
}

/// A sink whose writes block until the test opens it.
struct HeldWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    open: Arc<(Mutex<bool>, Condvar)>,
}
impl HeldWriter {
    fn new(bytes: Arc<Mutex<Vec<u8>>>) -> (Self, impl FnOnce()) {
        let open = Arc::new((Mutex::new(false), Condvar::new()));
        let release = Arc::clone(&open);
        let release = move || {
            *release.0.lock().unwrap() = true;
            release.1.notify_all();
        };
        (Self { bytes, open }, release)
    }
}
impl Write for HeldWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let (open, opened) = &*self.open;
        drop(
            opened
                .wait_while(open.lock().unwrap(), |open| !*open)
                .unwrap(),
        );
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn producer_does_not_wait_for_slow_sink_and_finish_joins_after_all_transitions() {
    let initial = frame(State::Pending);
    let (sender, receiver) = mpsc::channel();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (mut sink, open) = HeldWriter::new(bytes.clone());
    let worker_initial = initial.clone();
    let worker = thread::spawn(move || {
        drive(
            Channel {
                receiver,
                start: Instant::now(),
            },
            worker_initial,
            Backend::Plain,
            &mut sink,
            false,
        )
    });
    let mut progress = Progress {
        sender,
        worker: Some(worker),
        previous: initial.clone(),
    };
    let mut next = initial;
    for phase in [
        RowPhase::Starting,
        RowPhase::CreatingContainer,
        RowPhase::StartingContainer,
        RowPhase::WaitingForHealth,
    ] {
        next.rows.first_mut().unwrap().state = State::Running(phase);
        progress.update(next.clone());
    }
    assert!(!progress.worker.as_ref().unwrap().is_finished());
    open();
    next.rows.first_mut().unwrap().state = State::Completed;
    progress.finish(next, Disposition::Settled);
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert_eq!(text.matches("alpha:").count(), 6, "{text}");
    assert!(text.ends_with("1 deployed · across 1 server\n"));
}

#[test]
fn repeated_snapshots_at_the_deadline_cannot_starve_heartbeats() {
    let initial = frame(State::Running(RowPhase::WaitingForHook));
    let text = plain(
        initial.clone(),
        vec![
            (30_000, Message::Update(initial.clone())),
            (30_000, Message::Update(initial.clone())),
            (60_000, Message::Update(initial)),
            (60_001, Message::Abandon),
        ],
    );
    assert_eq!(text.matches("still waiting:").count(), 2, "{text}");
    assert_eq!(text.matches("alpha: waiting for hook").count(), 1, "{text}");
}

#[test]
fn progress_drop_joins_its_worker_without_a_summary() {
    use ployz_core::ObservationKind;

    let initial = frame(State::Pending);
    let (sender, receiver) = mpsc::channel();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let (mut sink, open) = HeldWriter::new(bytes.clone());
    let worker_initial = initial.clone();
    let worker = thread::spawn(move || {
        drive(
            Channel {
                receiver,
                start: Instant::now(),
            },
            worker_initial,
            Backend::Plain,
            &mut sink,
            false,
        )
    });
    let mut progress = Progress {
        sender,
        worker: Some(worker),
        previous: initial.clone(),
    };
    let mut next = initial;
    next.rows.first_mut().unwrap().state = State::Running(RowPhase::Starting);
    next.notices.push(DeployWarning::ObservationFailed {
        kind: ObservationKind::Volume,
        machine_id: MachineId::random(),
        message: "volume query timed out".into(),
        gap: None,
    });
    progress.update(next);
    open();
    drop(progress);
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(text.contains("alpha: starting\n"), "{text}");
    assert_eq!(text.matches("volume query timed out").count(), 1, "{text}");
    assert!(!text.contains("deployed"));
}

#[test]
fn interactive_abandon_keeps_the_observation_notice() {
    use ployz_core::ObservationKind;

    let mut initial = frame(State::Running(RowPhase::Starting));
    initial.notices.push(DeployWarning::ObservationFailed {
        kind: ObservationKind::Volume,
        machine_id: MachineId::random(),
        message: "volume query timed out".into(),
        gap: None,
    });
    let terminal = InMemoryTerm::new(20, 90);
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal::EchoRoom::new(terminal))),
        room,
    );
    let mut output = Vec::new();
    drive(
        Script {
            now: Duration::ZERO,
            events: VecDeque::from([(Duration::from_millis(1), Message::Abandon)]),
        },
        initial,
        backend,
        &mut output,
        false,
    );
    let text = String::from_utf8(output).unwrap();
    assert_eq!(text.matches("volume query timed out").count(), 1, "{text}");
}

#[test]
fn queued_terminal_evidence_after_a_duplicate_wins_over_heartbeat() {
    let initial = frame(State::Running(RowPhase::WaitingForHealth));
    let mut finished = initial.clone();
    finished.rows.first_mut().unwrap().state = State::Completed;
    let text = plain(
        initial.clone(),
        vec![
            (30_000, Message::Update(initial)),
            (30_000, Message::Finish(finished, Disposition::Settled)),
        ],
    );
    assert!(!text.contains("still waiting:"), "{text}");
}

#[test]
fn unqualified_observation_notice_does_not_repeat_a_named_server_gap() {
    use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
    for qualified in [ObservationKind::Container, ObservationKind::Volume] {
        let machine_id = MachineId::random();
        let mut initial = frame(State::Completed);
        initial.notices = vec![
            DeployWarning::ObservationOmitted {
                kind: ObservationKind::Volume,
                machine_id,
                gap: None,
            },
            DeployWarning::ObservationOmitted {
                kind: qualified,
                machine_id,
                gap: Some(ObservationGap {
                    machine_name: MachineName::parse("machine-2").unwrap(),
                    reason: ObservationGapReason::Down,
                }),
            },
            DeployWarning::ObservationOmitted {
                kind: ObservationKind::Container,
                machine_id,
                gap: None,
            },
        ];
        let text = plain(
            initial.clone(),
            vec![(1, Message::Finish(initial, Disposition::Settled))],
        );
        assert_eq!(
            text.matches("deployment observations are incomplete")
                .count(),
            1,
            "{text}"
        );
        assert!(text.contains("Server machine-2"), "{text}");
        assert!(!text.contains(machine_id.as_str()), "{text}");
        assert!(!text.contains("observation omitted"), "{text}");
    }
}

#[test]
fn unqualified_observation_failures_keep_their_causes_in_the_named_server_gap() {
    use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
    let machine_id = MachineId::random();
    let mut initial = frame(State::Completed);
    initial.notices = vec![
        DeployWarning::ObservationFailed {
            kind: ObservationKind::Volume,
            machine_id,
            message: "volume query timed out".into(),
            gap: None,
        },
        DeployWarning::ObservationFailed {
            kind: ObservationKind::Container,
            machine_id,
            message: "container query timed out".into(),
            gap: Some(ObservationGap {
                machine_name: MachineName::parse("machine-2").unwrap(),
                reason: ObservationGapReason::Failed,
            }),
        },
        DeployWarning::ObservationOmitted {
            kind: ObservationKind::Volume,
            machine_id,
            gap: None,
        },
    ];
    let text = plain(
        initial.clone(),
        vec![(1, Message::Finish(initial, Disposition::Settled))],
    );
    assert_eq!(text.matches("! Server machine-2").count(), 1, "{text}");
    assert!(
        text.contains("containers: container query timed out"),
        "{text}"
    );
    assert!(text.contains("volumes: volume query timed out"), "{text}");
    assert!(!text.contains(machine_id.as_str()), "{text}");
    assert!(!text.contains("observed Down"), "{text}");
}

#[test]
fn observation_cause_waits_for_a_later_named_server_gap() {
    use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
    let machine_id = MachineId::random();
    let mut initial = frame(State::Running(RowPhase::Starting));
    initial.notices.push(DeployWarning::ObservationFailed {
        kind: ObservationKind::Volume,
        machine_id,
        message: "volume query timed out".into(),
        gap: None,
    });
    let mut named = initial.clone();
    named.notices.push(DeployWarning::ObservationOmitted {
        kind: ObservationKind::Container,
        machine_id,
        gap: Some(ObservationGap {
            machine_name: MachineName::parse("machine-2").unwrap(),
            reason: ObservationGapReason::Down,
        }),
    });
    let text = plain(
        initial,
        vec![
            (1, Message::Update(named.clone())),
            (2, Message::Finish(named, Disposition::Settled)),
        ],
    );
    assert_eq!(text.matches("volume query timed out").count(), 1, "{text}");
    assert!(text.contains("Server machine-2"), "{text}");
    assert!(!text.contains(machine_id.as_str()), "{text}");
}

#[test]
fn observation_notice_without_a_next_frame_gap_prints_before_a_later_named_gap() {
    use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
    let machine_id = MachineId::random();
    let mut initial = frame(State::Running(RowPhase::Starting));
    initial.notices.push(DeployWarning::ObservationFailed {
        kind: ObservationKind::Volume,
        machine_id,
        message: "volume query timed out".into(),
        gap: None,
    });
    let mut still_running = initial.clone();
    still_running.rows.first_mut().unwrap().state = State::Running(RowPhase::WaitingForHealth);
    let mut named = still_running.clone();
    named.notices.push(DeployWarning::ObservationOmitted {
        kind: ObservationKind::Container,
        machine_id,
        gap: Some(ObservationGap {
            machine_name: MachineName::parse("machine-2").unwrap(),
            reason: ObservationGapReason::Down,
        }),
    });
    let text = plain(
        initial,
        vec![
            (1, Message::Update(still_running)),
            (2, Message::Update(named.clone())),
            (3, Message::Finish(named, Disposition::Settled)),
        ],
    );
    assert_eq!(text.matches("volume query timed out").count(), 1, "{text}");
    assert!(text.contains(machine_id.as_str()), "{text}");
    assert!(text.contains("Server machine-2"), "{text}");
}

#[test]
fn later_named_observation_cause_does_not_reprint_an_earlier_cause() {
    use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
    let machine_id = MachineId::random();
    let gap = ObservationGap {
        machine_name: MachineName::parse("machine-2").unwrap(),
        reason: ObservationGapReason::Failed,
    };
    let mut initial = frame(State::Running(RowPhase::Starting));
    initial.notices.push(DeployWarning::ObservationFailed {
        kind: ObservationKind::Volume,
        machine_id,
        message: "volume query timed out".into(),
        gap: Some(gap.clone()),
    });
    let mut later = initial.clone();
    later.notices.push(DeployWarning::ObservationFailed {
        kind: ObservationKind::Container,
        machine_id,
        message: "container query timed out".into(),
        gap: Some(gap),
    });
    let text = plain(
        initial,
        vec![
            (1, Message::Update(later.clone())),
            (2, Message::Finish(later, Disposition::Settled)),
        ],
    );
    assert_eq!(text.matches("volume query timed out").count(), 1, "{text}");
    assert_eq!(
        text.matches("container query timed out").count(),
        1,
        "{text}"
    );
}

#[test]
fn observation_without_a_named_gap_still_appears_at_settle() {
    use ployz_core::ObservationKind;
    let mut initial = frame(State::Completed);
    initial.notices.push(DeployWarning::ObservationFailed {
        kind: ObservationKind::Volume,
        machine_id: MachineId::random(),
        message: "volume query timed out".into(),
        gap: None,
    });
    let text = plain(
        initial.clone(),
        vec![(1, Message::Finish(initial, Disposition::Settled))],
    );
    assert_eq!(text.matches("volume query timed out").count(), 1, "{text}");
}

#[test]
fn a_store_warning_prints_an_invisible_character_as_its_code() {
    use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
    let failed = |machine_id| DeployWarning::ObservationFailed {
        kind: ObservationKind::Volume,
        machine_id,
        message: "no file a\u{202E}b.txt".into(),
        gap: None,
    };
    let named = MachineId::random();
    let mut initial = frame(State::Completed);
    initial.notices = vec![
        failed(MachineId::random()),
        failed(named),
        DeployWarning::ObservationOmitted {
            kind: ObservationKind::Container,
            machine_id: named,
            gap: Some(ObservationGap {
                machine_name: MachineName::parse("edge").unwrap(),
                reason: ObservationGapReason::Down,
            }),
        },
    ];
    let settled = plain(
        initial.clone(),
        vec![(1, Message::Finish(initial.clone(), Disposition::Settled))],
    );
    let live = live_lines(&initial, 0, 200, 20, false).join("\n");
    for text in [settled, live] {
        assert!(!text.contains('\u{202E}'), "{text}");
        assert_eq!(text.matches("no file a\\u{202e}b.txt").count(), 2, "{text}");
    }
}

#[test]
fn qualified_notices_group_by_machine_identity_and_count_each_gap_once() {
    use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
    let mut initial = frame(State::Completed);
    let first = MachineId::random();
    let second = MachineId::random();
    for machine_id in [first, second] {
        for kind in [ObservationKind::Container, ObservationKind::Volume] {
            initial.notices.push(DeployWarning::ObservationOmitted {
                kind,
                machine_id,
                gap: Some(ObservationGap {
                    machine_name: MachineName::parse("edge").unwrap(),
                    reason: ObservationGapReason::Down,
                }),
            });
        }
    }
    let text = plain(
        initial.clone(),
        vec![(1, Message::Finish(initial, Disposition::Settled))],
    );
    assert_eq!(
        text.matches("deployment observations are incomplete")
            .count(),
        2,
        "{text}"
    );
    assert!(text.contains(&format!("edge ({first})")), "{text}");
    assert!(text.contains(&format!("edge ({second})")), "{text}");
    assert!(text.contains("2 Server observation gaps"), "{text}");
}

/// The terminal echoes `^C` after the last live row, which indicatif does not track.
#[test]
fn ctrl_c_echo_after_a_full_width_frame_leaves_no_stale_caption() {
    let terminal = InMemoryTerm::new(20, 90);
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal::EchoRoom::new(terminal.clone()))),
        room,
    );
    let mut wide = frame(State::Running(RowPhase::Starting));
    wide.rows.first_mut().unwrap().detail = Some("x".repeat(200));
    backend.draw(&wide, 0, false);
    terminal.write_str("^C").unwrap();
    drop(backend);
    let visible = terminal.contents();
    assert!(!visible.contains("Deploying shop"), "{visible:?}");
}

/// indicatif writes no newline between rows, so each row must fill the line to wrap.
#[test]
fn live_rows_each_keep_their_own_line_on_an_80_column_terminal() {
    let terminal = InMemoryTerm::new(20, 80);
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal::EchoRoom::new(terminal.clone()))),
        || (80, 20),
    );
    let frame = Frame {
        run: Run::Direct(Namespace::parse("shop").unwrap()),
        title: "Deploying shop".into(),
        rows: vec![
            service("api", 0, 1, &ployz_store::RowState::Completed),
            service("web", 1, 2, &ployz_store::RowState::Pending),
        ],
        notices: Vec::new(),
    };
    backend.draw(&frame, 0, false);
    let visible = terminal.contents();
    let rows: Vec<_> = visible.lines().map(str::trim_end).collect();
    assert_eq!(
        rows,
        [
            "Deploying shop  1/2",
            "depth 0",
            "  ✔ api  switched 1/1  5.0s",
            "depth 1",
            "  · web  waiting 0/2  5.0s",
        ],
        "{visible:?}"
    );
}

fn service(name: &str, depth: u32, servers: usize, state: &ployz_store::RowState) -> Row {
    crate::deploy::service_row(
        QualifiedService::parse(format!("shop/{name}")).unwrap(),
        Some(depth),
        None,
        (0..servers).map(|_| crate::deploy::Replica {
            machine: MachineId::random(),
            state,
            timing: Timing::Finished(Duration::from_secs(5)),
            starts: true,
            updates: false,
        }),
    )
}

#[test]
fn an_overflowing_view_drops_switched_rows_before_a_deeper_failure() {
    let failed = ployz_store::RowState::Failed {
        reason: "health check failed".into(),
        cause: Vec::new(),
        step: None,
        log: None,
        unanswered: false,
        out_of_memory: None,
        stopped_by: ployz_store::StoppedBy::NOTHING,
    };
    let mut rows: Vec<_> = (0..12)
        .map(|i| service(&format!("svc{i}"), 0, 1, &ployz_store::RowState::Completed))
        .collect();
    rows.push(service("api", 1, 1, &ployz_store::RowState::Pending));
    rows.push(service("web", 2, 1, &failed));
    let frame = Frame {
        run: Run::Direct(Namespace::parse("shop").unwrap()),
        title: "Deploying shop".into(),
        rows,
        notices: Vec::new(),
    };
    let lines = live_lines(&frame, 0, 80, 8, false);
    assert_eq!(
        lines.get(1..).unwrap(),
        [
            "depth 1",
            "  · api  waiting 0/1  5.0s",
            "depth 2",
            "  ✘ web  failed (0/1 ready)  5.0s",
            "  … 12 more rows",
        ],
        "{lines:?}"
    );
}

#[test]
fn an_overflowing_view_puts_finished_ingress_repairs_after_a_failure() {
    let failed = ployz_store::RowState::Failed {
        reason: "health check failed".into(),
        cause: Vec::new(),
        step: None,
        log: None,
        unanswered: false,
        out_of_memory: None,
        stopped_by: ployz_store::StoppedBy::NOTHING,
    };
    let mut rows: Vec<_> = (0..12)
        .map(|n| Row {
            subject: Subject::ServiceOnServer {
                service: QualifiedService::system_ingress(),
                machine: MachineId::random(),
                server: format!("edge{n}"),
            },
            state: State::Completed,
            detail: None,
            timing: Timing::Unavailable,
        })
        .collect();
    rows.push(service("web", 2, 1, &failed));
    let frame = Frame {
        run: Run::Direct(Namespace::parse("shop").unwrap()),
        title: "Deploying shop".into(),
        rows,
        notices: Vec::new(),
    };
    let lines = live_lines(&frame, 0, 80, 8, false);
    assert_eq!(
        lines.get(1..).unwrap(),
        [
            "depth 2",
            "  ✘ web  failed (0/1 ready)  5.0s",
            "✔ ingress on edge0  completed",
            "✔ ingress on edge1  completed",
            "✔ ingress on edge2  completed",
            "  … 9 more rows",
        ],
        "{lines:?}"
    );
}

#[test]
fn a_retained_row_fits_the_terminal_with_its_count_whole() {
    let name = format!("a{}", "b".repeat(60));
    let terminal = InMemoryTerm::new(20, 80);
    let initial = Frame {
        run: Run::Direct(Namespace::parse("shop").unwrap()),
        title: "Deploying shop".into(),
        rows: vec![service(&name, 0, 10, &ployz_store::RowState::Completed)],
        notices: Vec::new(),
    };
    let backend = Backend::new(
        Mode::Interactive,
        ProgressDrawTarget::term_like(Box::new(terminal.clone())),
        || (80, 20),
    );
    drive(
        Script {
            now: Duration::ZERO,
            events: VecDeque::from([(
                Duration::from_millis(250),
                Message::Finish(initial.clone(), Disposition::Settled),
            )]),
        },
        initial,
        backend,
        &mut TerminalWriter(terminal.clone()),
        false,
    );
    let visible = terminal.contents();
    let row = visible
        .lines()
        .find(|line| line.contains("switched"))
        .unwrap_or_else(|| panic!("{visible}"));
    assert!(row.ends_with("switched 10/10  5.0s"), "{visible}");
    assert!(row.contains('…'), "{visible}");
    assert!(row.width() < 80, "{visible}");
}

#[test]
fn a_notice_raised_mid_deploy_shows_under_the_live_rows_and_in_plain_output() {
    let warning = DeployWarning::ImagePortMismatch {
        service: ployz_core::ServiceName::parse("web").unwrap(),
        service_id: ployz_core::ServiceId::parse("c".repeat(32)).unwrap(),
        image: "nginx".into(),
        machines: vec![ployz_core::MachineId::parse("a".repeat(32)).unwrap()],
        exposed: vec![80],
        routed: vec![8080],
    };
    let line =
        "! web's image nginx exposes 80; its domains send to 8080. Set the domain port or PORT.";
    let initial = frame(State::Running(RowPhase::Starting));
    let mut warned = initial.clone();
    warned.notices.push(warning);

    let lines = live_lines(&warned, 0, 200, 10, false);
    assert_eq!(lines.last().map(String::as_str), Some(line), "{lines:?}");
    assert!(
        live_lines(&warned, 0, 200, 2, false)
            .iter()
            .all(|row| row != line)
    );

    let mut done = warned.clone();
    done.rows.first_mut().unwrap().state = State::Completed;
    let text = plain(
        initial,
        vec![
            (1, Message::Update(warned)),
            (2, Message::Finish(done, Disposition::Settled)),
        ],
    );
    assert_eq!(text.matches(line).count(), 1, "{text}");
}

#[test]
fn heartbeat_pairs_each_waiting_row_with_its_state() {
    let mut initial = frame(State::Pending);
    let mut api = initial.rows.first().unwrap().clone();
    api.subject = Subject::ServiceOnServer {
        service: QualifiedService::parse("shop/api").unwrap(),
        machine: MachineId::random(),
        server: "beta".into(),
    };
    api.state = State::Running(RowPhase::WaitingForHealth);
    initial.rows.push(api);
    let text = plain(initial, vec![(30_001, Message::Abandon)]);
    assert!(
        text.contains("still waiting: web on alpha (pending), api on beta (waiting for health)\n"),
        "{text}"
    );
}

#[test]
fn heartbeat_names_what_a_waiting_row_is_doing() {
    let mut initial = frame(State::Pending);
    initial.rows.first_mut().unwrap().detail = Some("pulling pytorch/pytorch:2.5.1".into());
    let text = plain(initial, vec![(30_001, Message::Abandon)]);
    assert!(
        text.contains("still waiting: web on alpha (pending, pulling pytorch/pytorch:2.5.1)\n"),
        "{text}"
    );
}

#[test]
fn copies_left_running_take_the_check_mark_off_the_switched_row_and_count_in_the_summary() {
    let web = QualifiedService::parse("shop/web").unwrap();
    let left = |container: char, reason: &str| DeployWarning::LeftRunning {
        service: web.clone(),
        container_id: ployz_core::ContainerId::parse(container.to_string().repeat(64)).unwrap(),
        machine: "alpha".into(),
        reason: reason.into(),
    };
    let frame = Frame {
        run: Run::Direct(Namespace::parse("shop").unwrap()),
        title: "Deploying shop".into(),
        rows: vec![service("web", 0, 3, &ployz_store::RowState::Completed)],
        notices: vec![left('a', "interrupted"), left('b', "interrupted")],
    };
    let mut written = Vec::new();
    finish(
        &mut written,
        &frame,
        &frame,
        Disposition::LocalInterrupted,
        Some(80),
        false,
        &NoticeHistory::default(),
    )
    .unwrap();
    let text = String::from_utf8(written).unwrap();
    let row = text
        .lines()
        .find(|line| line.contains("switched"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(!row.contains('✔'), "{text}");
    assert!(row.contains("switched 3/3, 2 left running"), "{text}");
    assert!(
        text.contains("1 deployed · 2 copies left running · across 3 servers"),
        "{text}"
    );
}

#[test]
fn an_interrupted_deploy_counts_its_cancelled_and_interrupted_rows() {
    let mut frame = frame(State::Cancelled);
    let mut unchanged = frame.rows.first().unwrap().clone();
    unchanged.state = State::Unchanged;
    let mut interrupted = frame.rows.first().unwrap().clone();
    interrupted.state = State::Interrupted;
    frame.rows.extend([unchanged, interrupted]);
    let mut written = Vec::new();
    finish(
        &mut written,
        &frame,
        &frame,
        Disposition::LocalInterrupted,
        None,
        false,
        &NoticeHistory::default(),
    )
    .unwrap();
    let text = String::from_utf8(written).unwrap();
    assert!(
        text.contains("1 unchanged · 1 cancelled · 1 interrupted · across 1 server\n"),
        "{text}"
    );
    assert!(!text.contains("owned work"), "{text}");
}

/// An Unrecognized Server gets no RPC, so planning qualifies its omissions as a
/// Failed gap: it reads by name, never Down, and counts like any other gap.
#[test]
fn an_unread_server_gap_names_it_and_counts_without_claiming_down() {
    use ployz_core::{MachineName, ObservationGap, ObservationGapReason, ObservationKind};
    let machine_id = MachineId::random();
    let mut initial = frame(State::Completed);
    for kind in [ObservationKind::Container, ObservationKind::Volume] {
        initial.notices.push(DeployWarning::ObservationOmitted {
            kind,
            machine_id,
            gap: Some(ObservationGap {
                machine_name: MachineName::parse("machine-3").unwrap(),
                reason: ObservationGapReason::Failed,
            }),
        });
    }
    let text = plain(
        initial.clone(),
        vec![(1, Message::Finish(initial, Disposition::Settled))],
    );
    assert!(
        text.contains(
            "! Server machine-3: deployment observations are incomplete (not observed by this Entry)"
        ),
        "{text}"
    );
    assert!(text.contains("1 Server observation gap"), "{text}");
    assert!(!text.contains(machine_id.as_str()), "{text}");
    assert!(!text.contains("observed Down"), "{text}");
}
