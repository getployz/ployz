//! The CLI output contract.
//!
//! Under `--json`, stdout carries exactly one JSON object: the command's result, or
//! `{"error": …}` when it fails. Streaming commands write one object per line instead.
//! Human text — tables, progress, prompts — goes to stdout without `--json` and to
//! stderr with it, so stdout stays parseable. `--json` never prompts.

use std::{
    cell::Cell,
    io::{self, IsTerminal, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use ployz_core::{MachineFailure, MachineId, PartialResult, RpcError, VolumeObservationFailure};
use serde::Serialize;

use crate::failure::Failure;

// `JSON` is process config: set once before dispatch, read from any thread (the
// deploy renderer and spawned tasks included).
static JSON: AtomicBool = AtomicBool::new(false);

thread_local! {
    // `EMITTED` is per-execution state. Results are printed on the handler's thread,
    // so thread-local keeps in-process handler tests apart.
    static EMITTED: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn set_json(json: bool) {
    JSON.store(json, Ordering::Relaxed);
}

/// Whether this command writes its result as JSON.
#[must_use]
pub fn json() -> bool {
    JSON.load(Ordering::Relaxed)
}

/// Whether the command may prompt: a terminal on both ends and no `--json`.
#[must_use]
pub(crate) fn interactive() -> bool {
    !json() && io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Whether the stream carrying human text is a terminal.
#[must_use]
pub(crate) fn human_is_terminal() -> bool {
    if json() {
        io::stderr().is_terminal()
    } else {
        io::stdout().is_terminal()
    }
}

/// Where human text goes: stdout, or stderr under `--json`.
pub(crate) fn human() -> Box<dyn Write> {
    if json() {
        Box::new(io::stderr())
    } else {
        Box::new(io::stdout())
    }
}

/// Human text line, routed by [`human`].
macro_rules! say {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!($crate::output::human(), $($arg)*);
    }};
}

/// Human text without a newline, flushed, routed by [`human`].
macro_rules! say_inline {
    ($($arg:tt)*) => {{
        use std::io::Write as _;
        let mut human = $crate::output::human();
        let _ = write!(human, $($arg)*);
        let _ = human.flush();
    }};
}

pub(crate) use {say, say_inline};

/// Finish with `value`: printed as the JSON result, or rendered by `human`.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn finish<T: Serialize + ?Sized>(
    value: &T,
    human: impl FnOnce(),
) -> Result<(), Failure> {
    if json() {
        emit(value)
    } else {
        human();
        EMITTED.set(true);
        Ok(())
    }
}

/// Print `value` as pretty JSON in both modes, for inspect-style commands.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn show<T: Serialize + ?Sized>(value: &T) -> Result<(), Failure> {
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    EMITTED.set(true);
    Ok(())
}

/// Print `value` as the JSON result; without `--json` the command already said it.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn emit<T: Serialize + ?Sized>(value: &T) -> Result<(), Failure> {
    if json() {
        show(value)
    } else {
        EMITTED.set(true);
        Ok(())
    }
}

/// Print a committed `result`, with `follow_up_error` when a step after the commit
/// failed; that failure then makes the command partial.
///
/// # Errors
///
/// Returns a serialization or stdout write error, or the follow-up failure.
pub(crate) fn emit_committed(
    mut result: serde_json::Value,
    follow_up: Result<(), Failure>,
) -> Result<(), Failure> {
    if let (Err(error), Some(fields)) = (&follow_up, result.as_object_mut()) {
        fields.insert("follow_up_error".into(), serde_json::json!(error.report()));
    }
    emit(&result)?;
    follow_up
}

/// Print one line of a streamed JSON result; a no-op without `--json`.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn emit_line<T: Serialize + ?Sized>(value: &T) -> Result<(), Failure> {
    if json() {
        let mut stdout = io::stdout().lock();
        serde_json::to_writer(&mut stdout, value)?;
        writeln!(stdout)?;
        stdout.flush()?;
    }
    EMITTED.set(true);
    Ok(())
}

/// Print the `--json` error object on one stdout line.
pub(crate) fn error(error: &RpcError) {
    // Nothing is left to report a failed write to.
    let _ = writeln!(
        io::stdout().lock(),
        "{}",
        serde_json::json!({ "error": error })
    );
}

/// Whether the command already produced its result, so a later failure is partial.
pub(crate) fn emitted() -> bool {
    EMITTED.get()
}

/// Gaps in a fan-out result: Machines whose answer was an error (`failures`) or
/// never came (`omitted`), and Volumes a Machine answered for but could not
/// inspect (`unavailable_volumes`). Any gap makes the command exit [`Failure::partial`].
#[derive(Debug, Default, Serialize)]
pub(crate) struct Gaps {
    pub failures: Vec<MachineFailure<RpcError>>,
    pub omitted: Vec<MachineId>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unavailable_volumes: Vec<VolumeObservationFailure>,
}

impl Gaps {
    pub(crate) fn of<T>(result: &PartialResult<T, RpcError>) -> Self {
        Self {
            failures: result.failures.clone(),
            omitted: result.omissions.clone(),
            unavailable_volumes: Vec::new(),
        }
    }

    /// Add another fan-out's gaps, keeping each Machine once per list.
    pub(crate) fn extend(&mut self, failures: &[MachineFailure<RpcError>], omitted: &[MachineId]) {
        for failure in failures {
            if !self.failures.contains(failure) {
                self.failures.push(failure.clone());
            }
        }
        for machine_id in omitted {
            if !self.omitted.contains(machine_id) {
                self.omitted.push(*machine_id);
            }
        }
    }

    fn is_complete(&self) -> bool {
        self.failures.is_empty() && self.omitted.is_empty() && self.unavailable_volumes.is_empty()
    }

    /// `Ok` when every Machine answered; otherwise the partial exit.
    pub(crate) fn outcome(&self) -> Result<(), Failure> {
        if self.is_complete() {
            Ok(())
        } else {
            Err(Failure::partial())
        }
    }

    /// A fan-out that found nothing: `not_found` only when every Machine answered,
    /// otherwise an absent value, since absence is unproven.
    pub(crate) fn absence<T>(&self, not_found: Failure) -> Result<Option<T>, Failure> {
        if self.is_complete() {
            Err(not_found)
        } else {
            Ok(None)
        }
    }
}

/// Finish a fan-out: `{key: value, failures, omitted[, unavailable_volumes]}`, or
/// `human`; then the partial exit if any gap.
///
/// # Errors
///
/// Returns a serialization or stdout write error, or [`Failure::partial`].
pub(crate) fn finish_fanout(
    key: &str,
    value: &impl Serialize,
    gaps: &Gaps,
    human: impl FnOnce(),
) -> Result<(), Failure> {
    finish(&Fanout::new(key, value, gaps), human)?;
    gaps.outcome()
}

/// [`finish_fanout`] for inspect-style commands: the object is pretty JSON in both modes.
///
/// # Errors
///
/// Returns a serialization or stdout write error, or [`Failure::partial`].
pub(crate) fn show_fanout(key: &str, value: &impl Serialize, gaps: &Gaps) -> Result<(), Failure> {
    show(&Fanout::new(key, value, gaps))?;
    gaps.outcome()
}

#[derive(Serialize)]
struct Fanout<'a, T> {
    #[serde(flatten)]
    value: std::collections::BTreeMap<&'a str, &'a T>,
    #[serde(flatten)]
    gaps: &'a Gaps,
}

impl<'a, T> Fanout<'a, T> {
    fn new(key: &'a str, value: &'a T, gaps: &'a Gaps) -> Self {
        Self {
            value: std::collections::BTreeMap::from([(key, value)]),
            gaps,
        }
    }
}
