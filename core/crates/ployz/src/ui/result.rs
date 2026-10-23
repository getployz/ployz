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

use super::{Fields, Hint, Mode, Table, Tone, escaped};
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

/// Whether a person is at both ends, so a browser may open for them.
#[must_use]
pub(crate) fn interactive() -> bool {
    !json() && io::stdin().is_terminal() && super::stdout_is_terminal()
}

/// Whether results are laid out for a person: stdout is a terminal.
fn aligned() -> bool {
    super::stdout_is_terminal()
}

/// One line of the result on stdout; under `--json`, on stderr beside the object.
pub(crate) fn stream(line: impl Display) {
    if json() {
        let _ = writeln!(anstream::stderr(), "{line}");
    } else {
        let _ = writeln!(super::stdout(), "{line}");
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
    record_warning(warning);
}

/// Retain a warning in the next JSON result when a progress worker owns its display.
pub(crate) fn record_warning(warning: String) {
    WARNINGS.with_borrow_mut(|warnings| warnings.push(warning));
}

/// A warning on stderr that the JSON result already carries elsewhere.
fn flag(warning: &str) {
    let _ = writeln!(
        anstream::stderr(),
        "{} {}",
        Tone::Change.paint("!"),
        shown(warning)
    );
}

/// `warning` [`escaped`] line by line, so a name from the Store reads as stored.
fn shown(warning: &str) -> String {
    warning
        .split('\n')
        .map(escaped)
        .collect::<Vec<_>>()
        .join("\n")
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
    let _ = table.write(&mut super::stdout(), aligned());
}

/// Finish with one record: `value` under `--json`, else `record` on stdout.
///
/// # Errors
///
/// Returns a serialization or stdout write error.
pub(crate) fn fields<T: Serialize + ?Sized>(value: &T, record: &Fields) -> Result<(), Failure> {
    finish(value, || {
        let _ = record.write(&mut super::stdout(), aligned());
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
    let mut stdout = super::stdout_bytes();
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
        let mut stdout = super::stdout_bytes();
        serde_json::to_writer(&mut stdout, value)?;
        writeln!(stdout)?;
        stdout.flush()?;
    }
    EMITTED.set(true);
    Ok(())
}

/// Note that a human stream printed part of the result, so a later failure is partial.
pub(crate) fn printed() {
    EMITTED.set(true);
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

    /// Add a Machine that stopped answering partway, warning by name the first time.
    pub(crate) fn stopped(&mut self, machine_id: MachineId) {
        if self.omitted.contains(&machine_id) {
            return;
        }
        self.omitted.push(machine_id);
        flag(&self.stopped_answering(&machine_id));
    }

    pub(crate) fn stopped_answering(&self, machine_id: &MachineId) -> String {
        format!(
            "{} stopped answering; its lines are missing from here on.",
            self.name(machine_id)
        )
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
            let (warning, cause) = self.warning(failure);
            flag(&warning);
            if let Some(cause) = cause {
                note(format_args!("  {} {cause}", Tone::Bad.paint("cause:")));
            }
        }
        for machine_id in &self.omitted {
            flag(&missing_rows(&self.name(machine_id)));
        }
    }

    /// The warning for `failure`, naming its Machine, and the cause line under it when
    /// the warning doesn't say what happened by itself.
    fn warning(&self, failure: &MachineFailure<RpcError>) -> (String, Option<String>) {
        let name = self.name(&failure.machine_id);
        if let Some(refusal) = failure.error.refusal() {
            return (refusal.naming(name).to_string(), None);
        }
        // The entry Machine stopped listing it between our membership read and this
        // call, as `ployz server rm` does. Its id would only repeat the name.
        if failure.error.server_unlisted(&failure.machine_id) {
            return (
                format!("{name} was removed while this ran; its rows are missing."),
                None,
            );
        }
        (
            missing_rows(&name),
            super::causes(&failure.error).last().cloned(),
        )
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

fn missing_rows(server: &str) -> String {
    format!("{server} did not answer; its rows are missing.")
}

fn did_not_answer(server: &str) {
    flag(&missing_rows(server));
}

/// The end of a read Cloud fanned out for us: one warning per Server it names
/// unanswered, as [`Gaps::warn`] gives, then the partial exit if any.
///
/// # Errors
///
/// Returns [`Failure::partial`] when a Server did not answer.
pub(crate) fn unanswered<'a>(
    servers: impl IntoIterator<Item = &'a MachineName>,
) -> Result<(), Failure> {
    let mut partial = Ok(());
    for server in servers {
        did_not_answer(server.as_str());
        partial = Err(Failure::partial());
    }
    partial
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

    /// A Server the entry Machine stopped listing between our membership read and the
    /// fan-out, as `ployz server rm` does, is named once, without the id its
    /// not-found answer carries (#3277). A Server that didn't answer keeps its cause.
    #[test]
    fn a_server_the_entry_stopped_listing_is_named_without_its_id() {
        use ployz_core::RpcErrorCode;

        let machine_id = MachineId::parse("4".repeat(32)).unwrap();
        let name = MachineName::parse("machine-4").unwrap();
        let unlisted = RpcError {
            code: RpcErrorCode::NotFound,
            message: format!("No Server named {:?}", machine_id.to_string()),
            details: serde_json::json!({
                "inspect": ["ployz server ls"],
                "reason": "no_such_server",
                "targets": [machine_id.to_string()],
            }),
            cause: Vec::new(),
        };
        let silent = RpcError {
            code: RpcErrorCode::Unavailable,
            message: format!("Machine {machine_id} did not answer: target Machine RPC timed out"),
            details: serde_json::Value::Null,
            cause: vec!["deadline has elapsed".into()],
        };
        let gaps = Gaps::default().named([(machine_id, &name)]);
        let warning = |error: RpcError| gaps.warning(&MachineFailure { machine_id, error });

        assert_eq!(
            warning(unlisted),
            (
                "machine-4 was removed while this ran; its rows are missing.".to_owned(),
                None
            )
        );
        assert_eq!(
            warning(silent),
            (
                "machine-4 did not answer; its rows are missing.".to_owned(),
                Some("deadline has elapsed".to_owned())
            )
        );
    }

    #[test]
    fn a_warning_prints_an_invisible_character_as_its_code_and_keeps_its_lines() {
        assert_eq!(
            shown("Left KEY empty in app/a\u{202E}b.txt: it is not set.\nDeploy\r again."),
            "Left KEY empty in app/a\\u{202e}b.txt: it is not set.\nDeploy\\u{d} again."
        );
        assert_eq!(
            shown("Docker volume (not recommended)"),
            "Docker volume (not recommended)"
        );
    }
}
