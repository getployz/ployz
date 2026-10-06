//! Where a command's output goes.
//!
//! stdout carries the result: a list, a record, a done-sentence, streamed
//! lines, or under `--json` exactly one JSON object (one per line for a
//! streaming command). stderr carries everything else: progress, notes,
//! warnings, hints and prompts. `--json` never prompts.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    fmt::Display,
    io::{self, IsTerminal, Write},
};

use ployz_core::{MachineFailure, MachineId, MachineName, PartialResult, RpcError};
use serde::Serialize;

use super::{Fields, Hint, Mode, Table, Tone};
use crate::failure::Failure;

thread_local! {
    // `EMITTED` is per-execution state. Results are printed on the handler's thread,
    // so thread-local keeps in-process handler tests apart.
    static EMITTED: Cell<bool> = const { Cell::new(false) };
    /// Set while a command runs as a step of another: its JSON result lands here.
    static CAPTURED: RefCell<Option<Option<serde_json::Value>>> = const { RefCell::new(None) };
    /// Warnings said so far; the next JSON result carries them as `warnings`.
    static WARNINGS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Whether this command writes its result as JSON.
#[must_use]
pub fn json() -> bool {
    super::mode() == Mode::Json
}

/// Whether the command may prompt: a terminal on both ends and no `--json`.
#[must_use]
pub(crate) fn interactive() -> bool {
    !json() && io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Whether results are laid out for a person: stdout is a terminal.
fn aligned() -> bool {
    io::stdout().is_terminal()
}

/// One line of the result on stdout; under `--json`, on stderr beside the object.
pub(crate) fn stream(line: impl Display) {
    if json() {
        let _ = writeln!(anstream::stderr(), "{line}");
    } else {
        let _ = writeln!(anstream::stdout(), "{line}");
    }
}

/// One line beside the result, on stderr: progress, a heads-up, a prompt's context.
pub(crate) fn note(line: impl Display) {
    let _ = writeln!(anstream::stderr(), "{line}");
}

/// Text on stderr with no newline, flushed: a prompt, or a line that finishes later.
pub(crate) fn note_inline(text: impl Display) {
    let mut stderr = anstream::stderr();
    let _ = write!(stderr, "{text}");
    let _ = stderr.flush();
}

/// A hint line on stderr. `--json` carries hints as result keys instead.
pub(crate) fn hint(hint: &Hint) {
    if !json() {
        let _ = hint.write(&mut anstream::stderr());
    }
}

/// Warn on one stderr line; the command's JSON result lists it under `warnings`.
pub(crate) fn warn(warning: impl Into<String>) {
    let warning = warning.into();
    flag(&warning);
    WARNINGS.with_borrow_mut(|warnings| warnings.push(warning));
}

/// A warning on stderr that the JSON result already carries elsewhere.
fn flag(warning: &str) {
    let _ = writeln!(anstream::stderr(), "{} {warning}", Tone::Change.paint("!"));
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

/// Finish with a list: `value` under `--json`, else `table` on stdout. An
/// empty list says so on stderr, and a pipe still gets the header.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn list<T: Serialize + ?Sized>(value: &T, table: &Table) -> Result<(), Failure> {
    finish(value, || rows(table))
}

/// A list's human form, inside a `finish` that owns the result.
pub(crate) fn rows(table: &Table) {
    if table.is_empty() {
        note(table.empty_sentence());
    }
    let _ = table.write(&mut anstream::stdout(), aligned());
}

/// Finish with one record: `value` under `--json`, else `record` on stdout.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn fields<T: Serialize + ?Sized>(value: &T, record: &Fields) -> Result<(), Failure> {
    finish(value, || {
        let _ = record.write(&mut anstream::stdout(), aligned());
    })
}

/// Finish with what the command did: `value` under `--json`, else `sentence`
/// on stdout.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn done<T: Serialize + ?Sized>(
    value: &T,
    sentence: impl Display,
) -> Result<(), Failure> {
    finish(value, || stream(sentence))
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
        fields.insert("follow_up_error".into(), error.json());
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
    /// How a warning names each Machine; one missing here is named by id.
    #[serde(skip)]
    names: BTreeMap<MachineId, MachineName>,
}

impl Gaps {
    pub(crate) fn of<T>(result: &PartialResult<T, RpcError>) -> Self {
        Self {
            failures: result.failures.clone(),
            omitted: result.omissions.clone(),
            names: BTreeMap::new(),
        }
    }

    /// Name the Machines in warnings by these names.
    #[must_use]
    pub(crate) fn named<'a>(
        mut self,
        names: impl IntoIterator<Item = (MachineId, &'a MachineName)>,
    ) -> Self {
        self.names
            .extend(names.into_iter().map(|(id, name)| (id, name.clone())));
        self
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

    fn name(&self, machine_id: &MachineId) -> String {
        self.names
            .get(machine_id)
            .map_or_else(|| machine_id.to_string(), ToString::to_string)
    }

    /// One warning per Machine that didn't answer, by name, with its cause.
    pub(crate) fn warn(&self) {
        for failure in &self.failures {
            flag(&format!(
                "{} did not answer; its rows are missing.",
                self.name(&failure.machine_id)
            ));
            if let Some(cause) = super::causes(&failure.error).last() {
                note(format_args!("  {} {cause}", Tone::Bad.paint("cause:")));
            }
        }
        for machine_id in &self.omitted {
            flag(&format!(
                "{} did not answer; its rows are missing.",
                self.name(machine_id)
            ));
        }
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

/// Finish a fan-out: `{key: value, failures, omitted}` or `human`, then in either
/// mode one warning per Machine that didn't answer, then the partial exit if any gap.
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
    gaps.warn();
    gaps.outcome()
}

#[derive(Serialize)]
struct Fanout<'a, T> {
    #[serde(flatten)]
    value: BTreeMap<&'a str, &'a T>,
    #[serde(flatten)]
    gaps: &'a Gaps,
}

impl<'a, T> Fanout<'a, T> {
    fn new(key: &'a str, value: &'a T, gaps: &'a Gaps) -> Self {
        Self {
            value: BTreeMap::from([(key, value)]),
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
