//! A single stderr owner for live Deployment evidence and its settled summary.

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use ployz_core::{MachineId, Namespace, QualifiedService};
use ployz_store::{DeploymentId, RowPhase};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{Mode, Tone};

const HEARTBEAT: Duration = Duration::from_secs(30);
const REDRAW: Duration = Duration::from_millis(100);

#[derive(Clone, Debug, PartialEq, Eq)]
/// The execution whose rows replace the current frame.
pub(crate) enum Run {
    Stored(DeploymentId),
    Direct(Namespace),
    CatchUp(MachineId),
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Identity and display labels for located work or an unlocated aggregate.
pub(crate) enum Subject {
    ServiceOnServer {
        service: QualifiedService,
        machine: MachineId,
        server: String,
    },
    Service(QualifiedService),
    Node {
        index: usize,
        name: String,
    },
}

impl Subject {
    fn same_identity(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::ServiceOnServer {
                    service: a,
                    machine: am,
                    ..
                },
                Self::ServiceOnServer {
                    service: b,
                    machine: bm,
                    ..
                },
            ) => a == b && am == bm,
            (Self::Service(a), Self::Service(b)) => a == b,
            (Self::Node { index: a, .. }, Self::Node { index: b, .. }) => a == b,
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Observed work state; catch-up running does not imply health.
pub(crate) enum State {
    Pending,
    Running(RowPhase),
    Completed,
    ObservedRunning,
    Unchanged,
    Failed,
    NotAttempted,
    Unknown,
    Excluded,
}

impl State {
    fn active(&self) -> bool {
        matches!(self, Self::Pending | Self::Running(_))
    }

    fn word(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running(phase) => match phase {
                RowPhase::Starting => "starting",
                RowPhase::CreatingContainer => "creating",
                RowPhase::StartingContainer => "starting",
                RowPhase::WaitingForHealth => "waiting for health",
                RowPhase::WaitingForHook => "waiting for hook",
                RowPhase::StoppingContainer => "stopping",
                RowPhase::RemovingContainer => "removing",
                RowPhase::RemovingVolume => "removing volume",
                RowPhase::Compensating => "compensating",
            },
            Self::Completed => "completed",
            Self::ObservedRunning => "running",
            Self::Unchanged => "unchanged",
            Self::Failed => "failed",
            Self::NotAttempted => "not attempted",
            Self::Unknown => "unknown",
            Self::Excluded => "excluded",
        }
    }

    fn tone(&self) -> Tone {
        match self {
            Self::Completed | Self::ObservedRunning => Tone::Good,
            Self::Failed => Tone::Bad,
            Self::Pending | Self::Running(_) | Self::Unknown => Tone::Change,
            Self::Unchanged | Self::NotAttempted | Self::Excluded => Tone::Muted,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// A known start or elapsed duration, without inventing missing timestamps.
pub(crate) enum Timing {
    Unavailable,
    Started(SystemTime),
    Finished(Duration),
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// One task, its observed state, and optional factual detail.
pub(crate) struct Row {
    pub subject: Subject,
    pub state: State,
    pub detail: Option<String>,
    pub timing: Timing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Replacement evidence for one execution, in stable display order.
pub(crate) struct Frame {
    pub run: Run,
    pub title: String,
    pub rows: Vec<Row>,
    pub notices: Vec<String>,
}

impl Frame {
    fn same_evidence(&self, other: &Self) -> bool {
        self.run == other.run
            && self.title == other.title
            && self.notices == other.notices
            && self.rows.len() == other.rows.len()
            && self
                .rows
                .iter()
                .zip(&other.rows)
                .all(|(a, b)| a.subject == b.subject && a.state == b.state && a.detail == b.detail)
    }
}

#[derive(Clone, Copy, Debug)]
/// Why presentation ended, independent of the command's exit policy.
pub(crate) enum Disposition {
    Settled,
    UpToDate,
    CloudContinues,
    LocalStopped,
}

/// Ordered updates deliberately trade memory for keeping every observed transition.
/// A slow stderr never holds the producer; finish and drop wait for it to drain.
pub(crate) struct Progress {
    sender: Sender<Message>,
    worker: Option<JoinHandle<()>>,
    previous: Frame,
}

impl Progress {
    /// Start the sole presentation worker with the caller's resolved output mode.
    pub(crate) fn start(initial: Frame) -> Self {
        let mode = super::mode();
        let color = anstream::AutoStream::choice(&io::stderr()) != anstream::ColorChoice::Never;
        let (sender, receiver) = mpsc::channel();
        let previous = initial.clone();
        let worker = thread::Builder::new()
            .name("ployz-progress".into())
            .spawn(move || {
                let backend = Backend::new(mode, ProgressDrawTarget::stderr(), terminal_size);
                let mut writer = anstream::AutoStream::new(
                    io::stderr(),
                    if color {
                        anstream::ColorChoice::Always
                    } else {
                        anstream::ColorChoice::Never
                    },
                );
                drive(
                    Channel {
                        receiver,
                        start: Instant::now(),
                    },
                    initial,
                    backend,
                    &mut writer,
                    color,
                );
            })
            .ok();
        Self {
            sender,
            worker,
            previous,
        }
    }

    /// Queue changed evidence without waiting for stderr. Time-only changes are silent.
    pub(crate) fn update(&mut self, frame: Frame) {
        if !self.previous.same_evidence(&frame) {
            let _ = self.sender.send(Message::Update(frame.clone()));
            self.previous = frame;
        }
    }

    /// Retain final evidence and join the worker before the caller emits its result.
    pub(crate) fn finish(mut self, frame: Frame, disposition: Disposition) {
        let _ = self.sender.send(Message::Finish(frame, disposition));
        self.join();
    }

    fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Progress {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.sender.send(Message::Abandon);
            self.join();
        }
    }
}

enum Message {
    Update(Frame),
    Finish(Frame, Disposition),
    Abandon,
}

// The clock and inbox form one seam so deadline tests exercise the production loop.
trait Inbox {
    fn now(&self) -> Duration;
    fn receive(&mut self, timeout: Duration) -> Result<Message, RecvTimeoutError>;
    fn ready(&mut self) -> Option<Message>;
}

struct Channel {
    receiver: Receiver<Message>,
    start: Instant,
}
impl Inbox for Channel {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
    fn receive(&mut self, timeout: Duration) -> Result<Message, RecvTimeoutError> {
        self.receiver.recv_timeout(timeout)
    }
    fn ready(&mut self) -> Option<Message> {
        self.receiver.try_recv().ok()
    }
}

struct Live {
    multi: MultiProgress,
    bar: ProgressBar,
    size: fn() -> (usize, usize),
}

enum Backend {
    Interactive(Live),
    Plain,
}
impl Backend {
    fn new(mode: Mode, target: ProgressDrawTarget, size: fn() -> (usize, usize)) -> Self {
        match mode {
            Mode::Interactive => {
                let multi = MultiProgress::with_draw_target(target);
                let bar = multi.add(ProgressBar::new_spinner());
                bar.set_style(
                    ProgressStyle::with_template("{msg}").expect("fixed progress template"),
                );
                Self::Interactive(Live { multi, bar, size })
            }
            Mode::Plain | Mode::Json => Self::Plain,
        }
    }

    fn clear(&self) {
        if let Self::Interactive(live) = self {
            let _ = live.multi.clear();
        }
    }

    fn draw(&self, frame: &Frame, tick: usize, color: bool) {
        if let Self::Interactive(live) = self {
            let (width, height) = (live.size)();
            live.bar
                .set_message(live_lines(frame, tick, width, height, color).join("\n"));
            live.bar.tick();
        }
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.clear();
    }
}

fn drive(
    mut inbox: impl Inbox,
    mut frame: Frame,
    backend: Backend,
    writer: &mut impl Write,
    color: bool,
) {
    let mut tick = 0;
    let interactive = matches!(backend, Backend::Interactive(_));
    let interval = if interactive { REDRAW } else { HEARTBEAT };
    let mut deadline = inbox.now() + interval;
    if !interactive {
        let _ = changes(writer, None, &frame, color);
    }
    backend.draw(&frame, tick, color);
    loop {
        let received = inbox.receive(deadline.saturating_sub(inbox.now()));
        let received = match received {
            Err(RecvTimeoutError::Timeout) => {
                inbox.ready().map_or(Err(RecvTimeoutError::Timeout), Ok)
            }
            other => other,
        };
        match received {
            Ok(Message::Update(next)) => {
                let changed = !frame.same_evidence(&next);
                if changed {
                    if !interactive {
                        let _ = changes(writer, Some(&frame), &next, color);
                    }
                    if frame.run != next.run {
                        backend.clear();
                    }
                    frame = next;
                    deadline = inbox.now() + interval;
                    backend.draw(&frame, tick, color);
                }
            }
            Ok(Message::Finish(final_frame, disposition)) => {
                drop(backend);
                let _ = finish(
                    writer,
                    &frame,
                    &final_frame,
                    disposition,
                    interactive,
                    color,
                );
                let _ = writer.flush();
                return;
            }
            Ok(Message::Abandon) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        if inbox.now() >= deadline {
            if interactive {
                tick = tick.wrapping_add(1);
                backend.draw(&frame, tick, color);
            } else if frame.rows.iter().any(|row| row.state.active()) {
                let waiting = frame
                    .rows
                    .iter()
                    .filter(|row| row.state.active())
                    .map(|row| format!("{}, {}", label(&row.subject, &frame), row.state.word()))
                    .collect::<Vec<_>>()
                    .join(", ");
                let _ = writeln!(writer, "  still waiting: {waiting}");
                let _ = writer.flush();
            }
            deadline = inbox.now() + interval;
        }
    }
}

fn terminal_size() -> (usize, usize) {
    crossterm::terminal::size().map_or((80, 24), |(w, h)| (usize::from(w), usize::from(h)))
}

fn label(subject: &Subject, frame: &Frame) -> String {
    match subject {
        Subject::ServiceOnServer {
            service,
            machine,
            server,
        } => {
            let ambiguous = frame.rows.iter().any(|row| matches!(&row.subject, Subject::ServiceOnServer { machine: other, server: name, .. } if name == server && other != machine));
            if ambiguous {
                format!(
                    "{} on {server} ({})",
                    service.name,
                    super::short_id(&machine.to_string())
                )
            } else {
                format!("{} on {server}", service.name)
            }
        }
        Subject::Service(service) => service.name.to_string(),
        Subject::Node { name, .. } => name.clone(),
    }
}

fn paint(text: &str, tone: Tone, color: bool) -> String {
    if color {
        tone.paint(text).to_string()
    } else {
        text.into()
    }
}

fn row_text(row: &Row, frame: &Frame, mark: &str) -> String {
    let elapsed = match row.timing {
        Timing::Unavailable => String::new(),
        Timing::Started(at) => format!("  {:.1}s", at.elapsed().unwrap_or_default().as_secs_f64()),
        Timing::Finished(elapsed) => format!("  {:.1}s", elapsed.as_secs_f64()),
    };
    let detail = row
        .detail
        .as_deref()
        .map(|detail| format!(" ({detail})"))
        .unwrap_or_default();
    format!(
        "{mark} {}  {}{detail}{elapsed}",
        label(&row.subject, frame),
        row.state.word()
    )
}

fn mark(state: &State, tick: usize) -> &'static str {
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    match state {
        State::Running(_) => SPINNER.get(tick % SPINNER.len()).copied().unwrap_or("~"),
        State::Completed | State::ObservedRunning | State::Unchanged => "✔",
        State::Failed => "✘",
        State::Pending | State::NotAttempted | State::Excluded => "·",
        State::Unknown => "!",
    }
}

fn clip(text: &str, width: usize) -> String {
    let text = text.replace(['\n', '\r', '\t'], " ");
    if text.width() <= width {
        return text;
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let size = grapheme.width();
        if used + size > width - 1 {
            break;
        }
        result.push_str(grapheme);
        used += size;
    }
    result.push('…');
    result
}

fn live_lines(frame: &Frame, tick: usize, width: usize, height: usize, color: bool) -> Vec<String> {
    let width = width.saturating_sub(1);
    let height = height.saturating_sub(1).max(1);
    let mut rows: Vec<_> = frame.rows.iter().collect();
    rows.sort_by_key(|row| {
        if row.state == State::Failed {
            0
        } else if row.state.active() {
            1
        } else {
            2
        }
    });
    if height == 1 {
        return rows.first().map_or_else(
            || vec![clip(&frame.title, width)],
            |row| {
                vec![paint(
                    &clip(&row_text(row, frame, mark(&row.state, tick)), width),
                    row.state.tone(),
                    color,
                )]
            },
        );
    }
    let overflow = rows.len() + 1 > height;
    let available = height.saturating_sub(if overflow && height > 2 { 2 } else { 1 });
    let mut lines = vec![clip(
        &format!(
            "{}  {}/{}",
            frame.title,
            frame.rows.iter().filter(|row| !row.state.active()).count(),
            frame.rows.len()
        ),
        width,
    )];
    lines.extend(rows.iter().take(available).map(|row| {
        paint(
            &clip(&row_text(row, frame, mark(&row.state, tick)), width),
            row.state.tone(),
            color,
        )
    }));
    if overflow && height > 2 {
        lines.push(clip(
            &format!("  … {} more rows", rows.len().saturating_sub(available)),
            width,
        ));
    }
    lines
}

fn changes(
    writer: &mut impl Write,
    previous: Option<&Frame>,
    frame: &Frame,
    color: bool,
) -> io::Result<()> {
    let previous = previous.filter(|previous| previous.run == frame.run);
    if previous.is_none_or(|previous| previous.title != frame.title) {
        writeln!(writer, "{}", frame.title)?;
    }
    for row in &frame.rows {
        let old = previous.and_then(|previous| {
            previous
                .rows
                .iter()
                .find(|old| old.subject.same_identity(&row.subject))
        });
        if old.is_none_or(|old| {
            old.state != row.state || old.detail != row.detail || old.subject != row.subject
        }) {
            let detail = row
                .detail
                .as_deref()
                .map(|detail| format!(" ({detail})"))
                .unwrap_or_default();
            writeln!(
                writer,
                "  {}: {}{detail}",
                label(&row.subject, frame),
                paint(row.state.word(), row.state.tone(), color)
            )?;
        }
    }
    for notice in &frame.notices {
        if previous.is_none_or(|previous| !previous.notices.contains(notice)) {
            writeln!(
                writer,
                "{}",
                paint(&format!("! {notice}"), Tone::Change, color)
            )?;
        }
    }
    writer.flush()
}

fn finish(
    writer: &mut impl Write,
    previous: &Frame,
    frame: &Frame,
    disposition: Disposition,
    interactive: bool,
    color: bool,
) -> io::Result<()> {
    if matches!(disposition, Disposition::UpToDate) {
        return writeln!(writer, "Everything is up to date.");
    }
    if interactive {
        writeln!(writer, "{}", frame.title)?;
        for row in &frame.rows {
            writeln!(
                writer,
                "{}",
                paint(
                    &row_text(row, frame, mark(&row.state, 0)),
                    row.state.tone(),
                    color
                )
            )?;
        }
        for notice in &frame.notices {
            writeln!(writer, "! {notice}")?;
        }
    } else {
        changes(writer, Some(previous), frame, color)?;
    }
    let mut counts = Vec::new();
    for (state, word) in [
        (State::Completed, "updated"),
        (State::ObservedRunning, "running"),
        (State::Unchanged, "unchanged"),
        (State::Failed, "failed"),
        (State::NotAttempted, "not attempted"),
        (State::Unknown, "unknown"),
        (State::Excluded, "excluded"),
    ] {
        let count = frame.rows.iter().filter(|row| row.state == state).count();
        if count > 0 {
            counts.push(format!("{count} {word}"));
        }
    }
    let servers: BTreeSet<_> = frame
        .rows
        .iter()
        .filter_map(|row| match &row.subject {
            Subject::ServiceOnServer { machine, .. } => Some(machine),
            Subject::Service(_) | Subject::Node { .. } => None,
        })
        .collect();
    if !servers.is_empty() {
        counts.push(format!("across {} servers", servers.len()));
    }
    if !counts.is_empty() {
        let summary = counts.join(" · ");
        writeln!(writer, "{}\n{summary}", "─".repeat(summary.width()))?;
    }
    match disposition {
        Disposition::CloudContinues => {
            writeln!(writer, "Stopped following. The Cloud Deployment continues.")
        }
        Disposition::LocalStopped => {
            writeln!(writer, "Stopped local work after waiting for its outcome.")
        }
        Disposition::Settled | Disposition::UpToDate => Ok(()),
    }
}

#[cfg(test)]
mod tests;
