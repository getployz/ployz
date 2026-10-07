//! Driver behavior with a controllable inbox clock and a real indicatif terminal.

use super::*;
use indicatif::{InMemoryTerm, TermLike};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

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
    assert!(text.contains("1 updated · across 1 servers"), "{text}");
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
    assert!(!text.contains("updated"));
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
        visible.ends_with("1 updated · across 1 servers"),
        "{visible}"
    );
    assert!(!visible.contains("starting"), "{visible}");
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

#[derive(Clone)]
struct SlowWriter(Arc<Mutex<Vec<u8>>>);
impl Write for SlowWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        thread::sleep(Duration::from_millis(2));
        self.0.lock().unwrap().extend_from_slice(bytes);
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
    let mut sink = SlowWriter(bytes.clone());
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
    next.rows.first_mut().unwrap().state = State::Completed;
    progress.finish(next, Disposition::Settled);
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert_eq!(text.matches("alpha:").count(), 6, "{text}");
    assert!(text.ends_with("1 updated · across 1 servers\n"));
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
    let initial = frame(State::Pending);
    let (sender, receiver) = mpsc::channel();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let mut sink = SlowWriter(bytes.clone());
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
    progress.update(next);
    drop(progress);
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(text.ends_with("alpha: starting\n"), "{text}");
    assert!(!text.contains("updated"));
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
        ProgressDrawTarget::term_like(Box::new(EchoRoom(terminal.clone()))),
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

/// Reports the narrowed width the stderr draw target gives indicatif.
#[derive(Debug)]
struct EchoRoom(InMemoryTerm);
impl TermLike for EchoRoom {
    fn width(&self) -> u16 {
        self.0.width() - terminal::ECHO_ROOM
    }
    fn height(&self) -> u16 {
        self.0.height()
    }
    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_up(n)
    }
    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_down(n)
    }
    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_right(n)
    }
    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        self.0.move_cursor_left(n)
    }
    fn write_line(&self, value: &str) -> io::Result<()> {
        self.0.write_line(value)
    }
    fn write_str(&self, value: &str) -> io::Result<()> {
        self.0.write_str(value)
    }
    fn clear_line(&self) -> io::Result<()> {
        self.0.clear_line()
    }
    fn flush(&self) -> io::Result<()> {
        self.0.flush()
    }
}
