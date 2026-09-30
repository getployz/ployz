//! The CLI output contract.
//!
//! Under `--json`, stdout carries exactly one JSON object: the command's result, or
//! `{"error": …}` when it fails. Streaming commands write one object per line instead.
//! Human text — tables, progress, prompts — goes to stdout without `--json` and to
//! stderr with it, so stdout stays parseable. `--json` never prompts.

use std::{
    cell::{Cell, RefCell},
    io::{self, IsTerminal, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use ployz_core::{MachineFailure, MachineId, PartialResult, RpcError};
use serde::Serialize;

use crate::failure::Failure;

// `JSON` is process config: set once before dispatch, read from any thread (the
// deploy renderer and spawned tasks included).
static JSON: AtomicBool = AtomicBool::new(false);

thread_local! {
    // `EMITTED` is per-execution state. Results are printed on the handler's thread,
    // so thread-local keeps in-process handler tests apart.
    static EMITTED: Cell<bool> = const { Cell::new(false) };
    /// Set while a command runs as a step of another: its JSON result lands here.
    static CAPTURED: RefCell<Option<Option<serde_json::Value>>> = const { RefCell::new(None) };
    /// Warnings said so far; the next JSON result carries them as `warnings`.
    static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Warn on one stderr line; the command's JSON result lists it under `warnings`.
pub(crate) fn warn(warning: impl Into<String>) {
    let warning = warning.into();
    eprintln!("WARNING: {warning}");
    WARNINGS.with_borrow_mut(|warnings| warnings.push(warning));
}

/// Run a command as one step of another: the JSON result it would print is
/// returned instead, so stdout still carries one object. Human text still shows.
pub(crate) fn captured<R>(step: impl FnOnce() -> R) -> (R, Option<serde_json::Value>) {
    let emitted = EMITTED.get();
    CAPTURED.set(Some(None));
    let result = step();
    EMITTED.set(emitted);
    (result, CAPTURED.take().flatten())
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
    let warnings = WARNINGS.take();
    let mut value = serde_json::to_value(value)?;
    if let (false, Some(fields)) = (warnings.is_empty(), value.as_object_mut()) {
        fields.insert("warnings".into(), serde_json::json!(warnings));
    }
    if CAPTURED.with_borrow(Option::is_some) {
        CAPTURED.set(Some(Some(value)));
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, &value)?;
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
/// never came (`omitted`). Any gap makes the command exit [`Failure::partial`].
#[derive(Debug, Default, Serialize)]
pub(crate) struct Gaps {
    pub failures: Vec<MachineFailure<RpcError>>,
    pub omitted: Vec<MachineId>,
}

impl Gaps {
    pub(crate) fn of<T>(result: &PartialResult<T, RpcError>) -> Self {
        Self {
            failures: result.failures.clone(),
            omitted: result.omissions.clone(),
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
        self.failures.is_empty() && self.omitted.is_empty()
    }

    /// `Ok` when every Machine answered; otherwise the partial exit.
    pub(crate) fn outcome(&self) -> Result<(), Failure> {
        if self.is_complete() {
            Ok(())
        } else {
            Err(Failure::partial())
        }
    }
}

/// Finish a fan-out: `{key: value, failures, omitted}`, or
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warnings_land_in_the_next_json_result_once() {
        warn("Docker volume (not recommended)");
        let (_, first) = captured(|| show(&serde_json::json!({ "volume": "data" })));
        let (_, second) = captured(|| show(&serde_json::json!({ "volume": "data" })));
        assert_eq!(
            first.unwrap().get("warnings").cloned(),
            Some(serde_json::json!(["Docker volume (not recommended)"]))
        );
        assert!(second.unwrap().get("warnings").is_none());
    }
}
