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

use ployz_core::{MachineFailure, MachineId, PartialResult, RpcError};
use serde::Serialize;

use crate::failure::Failure;

// One process runs one command, so the mode is process-wide rather than threaded
// through every handler and the deploy renderer.
static JSON: AtomicBool = AtomicBool::new(false);

thread_local! {
    // Results are printed on the handler's thread; per-thread keeps in-process tests apart.
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
    human: impl FnOnce(&T),
) -> Result<(), Failure> {
    if json() {
        emit(value)
    } else {
        EMITTED.set(true);
        human(value);
        Ok(())
    }
}

/// Print `value` as the JSON result; without `--json` the command already said it.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn emit<T: Serialize + ?Sized>(value: &T) -> Result<(), Failure> {
    EMITTED.set(true);
    if !json() {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    serde_json::to_writer_pretty(&mut stdout, value)?;
    writeln!(stdout)?;
    Ok(())
}

/// Print one line of a streamed JSON result; a no-op without `--json`.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn emit_line<T: Serialize + ?Sized>(value: &T) -> Result<(), Failure> {
    if !json() {
        return Ok(());
    }
    let mut stdout = io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    writeln!(stdout)?;
    stdout.flush()?;
    Ok(())
}

/// Whether the command already produced its result, so a later failure is partial.
pub(crate) fn emitted() -> bool {
    EMITTED.get()
}

/// Per-Machine gaps in a fan-out result: `failures` answered with an error,
/// `omitted` never answered. Any gap makes the command exit [`Failure::partial`].
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

    pub(crate) fn is_empty(&self) -> bool {
        self.failures.is_empty() && self.omitted.is_empty()
    }

    /// `Ok` when every Machine answered; otherwise the partial exit.
    pub(crate) fn outcome(&self) -> Result<(), Failure> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(Failure::partial())
        }
    }
}
