//! The error vocabulary: hints and the `error:` / `cause:` lines.

use std::io;

use serde_json::{Map, Value};

use super::Tone;

/// Most choices a `valid:` line lists before it says how many more there are.
pub const VALID_SHOWN: usize = 8;

/// One line telling the reader what they can do about an outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hint {
    /// The one obvious command to run now.
    Next(String),
    /// A read-only command that shows more.
    Inspect(String),
    /// The command again, with what it was missing.
    Retry(String),
    /// The command that reverses what just happened.
    Undo(String),
    /// The choice closest to what the input named.
    Closest(String),
    /// The choices the input could have named.
    Valid(Vec<String>),
}

impl Hint {
    #[must_use]
    pub fn valid<I: IntoIterator<Item = S>, S: Into<String>>(names: I) -> Self {
        Self::Valid(names.into_iter().map(Into::into).collect())
    }

    /// Whether `other` says nothing this one doesn't: the same hint, or another
    /// of a kind a reader gets only one of.
    #[must_use]
    pub fn replaces(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Inspect(_), Self::Inspect(_)) => self == other,
            (Self::Next(_), Self::Next(_))
            | (Self::Retry(_), Self::Retry(_))
            | (Self::Undo(_), Self::Undo(_))
            | (Self::Closest(_), Self::Closest(_))
            | (Self::Valid(_), Self::Valid(_)) => true,
            _ => false,
        }
    }

    const fn label(&self) -> &'static str {
        match self {
            Self::Next(_) => "next:",
            Self::Inspect(_) => "inspect:",
            Self::Retry(_) => "retry:",
            Self::Undo(_) => "undo:",
            Self::Closest(_) | Self::Valid(_) => "valid:",
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Next(command)
            | Self::Inspect(command)
            | Self::Retry(command)
            | Self::Undo(command) => command.clone(),
            Self::Closest(name) => format!("did you mean {name}?"),
            Self::Valid(names) => match names.split_at_checked(VALID_SHOWN) {
                Some((shown, rest)) if !rest.is_empty() => {
                    format!("{}, and {} more", shown.join(", "), rest.len())
                }
                _ => names.join(", "),
            },
        }
    }

    /// Write this hint as its own line.
    ///
    /// # Errors
    ///
    /// Returns the writer's error.
    pub fn write(&self, out: &mut dyn io::Write) -> io::Result<()> {
        writeln!(out, "{} {}", Tone::Label.paint(self.label()), self.text())
    }

    /// Hints carried as `details` keys by an error that crossed the wire.
    #[must_use]
    pub fn from_details(details: &Value) -> Vec<Self> {
        let text = |key: &str| details.get(key).and_then(Value::as_str).map(str::to_owned);
        let list = |key: &str| -> Vec<String> {
            details
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let valid = list("valid_children");
        text("next")
            .map(Self::Next)
            .into_iter()
            .chain(list("inspect").into_iter().map(Self::Inspect))
            .chain(text("retry").map(Self::Retry))
            .chain(text("undo").map(Self::Undo))
            .chain(text("did_you_mean").map(Self::Closest))
            .chain((!valid.is_empty()).then_some(Self::Valid(valid)))
            .collect()
    }

    /// Write `hints` into `--json` `details`: `next`, `retry` and `undo` are one
    /// command each, `did_you_mean` one name, `inspect` and `valid_children` arrays.
    pub fn into_details(hints: &[Self], details: &mut Value) {
        if hints.is_empty() {
            return;
        }
        if !details.is_object() {
            *details = Value::Object(Map::new());
        }
        let Some(fields) = details.as_object_mut() else {
            return;
        };
        let mut inspect = Vec::new();
        for hint in hints {
            match hint {
                Self::Next(command) => {
                    fields.insert("next".into(), command.as_str().into());
                }
                Self::Retry(command) => {
                    fields.insert("retry".into(), command.as_str().into());
                }
                Self::Undo(command) => {
                    fields.insert("undo".into(), command.as_str().into());
                }
                Self::Inspect(command) => inspect.push(Value::from(command.as_str())),
                Self::Closest(name) => {
                    fields.insert("did_you_mean".into(), name.as_str().into());
                }
                Self::Valid(names) => {
                    fields.insert("valid_children".into(), names.clone().into());
                }
            }
        }
        if !inspect.is_empty() {
            fields.insert("inspect".into(), inspect.into());
        }
    }
}

pub use ployz_core::error_chain::causes;

#[expect(
    clippy::disallowed_methods,
    reason = "the one-line sinks below are the only callers"
)]
fn inline(error: Chain<'_>) -> String {
    one_line(&ployz_core::error_chain::inline(error))
}

/// Foreign text on one line: a registry's `denied\ndenied` would break the
/// indented line it lands in. Repeated lines print once.
fn one_line(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if lines.last() != Some(&line) {
            lines.push(line);
        }
    }
    lines.join("; ")
}

type Chain<'a> = &'a (dyn std::error::Error + 'static);

/// A per-Server row, record or report field.
pub(crate) fn row(error: Chain<'_>) -> String {
    inline(error)
}

pub(crate) fn warn_cause(what: impl std::fmt::Display, error: Chain<'_>) {
    super::warn(format!("{what}: {}", inline(error)));
}

pub(crate) fn retrying(operation: &str, error: Chain<'_>, seconds: u64) {
    super::note(format_args!(
        "{operation}: {}; retrying for up to {seconds}s. Check outbound firewall access if this connection is blocked.",
        inline(error)
    ));
}

pub(crate) fn rpc_error(code: ployz_core::RpcErrorCode, error: Chain<'_>) -> ployz_core::RpcError {
    ployz_core::RpcError {
        code,
        message: inline(error),
        details: Value::Null,
        cause: Vec::new(),
    }
}

#[cfg(test)]
pub(crate) fn chain_text(error: Chain<'_>) -> String {
    inline(error)
}

/// Write the human error: `error:` with our sentence, one `cause:` line with the
/// deepest source, then the hints. `--json` keeps every cause.
///
/// # Errors
///
/// Returns the writer's error.
pub fn write(
    out: &mut dyn io::Write,
    message: &str,
    cause: Option<&str>,
    hints: &[Hint],
) -> io::Result<()> {
    let mut lines = message.lines();
    writeln!(
        out,
        "{} {}",
        Tone::Bad.paint("error:"),
        lines.next().unwrap_or_default()
    )?;
    for line in lines {
        writeln!(out, "{line}")?;
    }
    if let Some(cause) = cause {
        writeln!(out, "  {} {}", Tone::Bad.paint("cause:"), one_line(cause))?;
    }
    for hint in hints {
        hint.write(out)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_caps_at_eight_and_counts_the_rest() {
        let names = (1..=11).map(|n| format!("s{n}"));
        let mut out = anstream::StripStream::new(Vec::new());
        Hint::valid(names).write(&mut out).unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            "valid: s1, s2, s3, s4, s5, s6, s7, s8, and 3 more\n"
        );
    }

    #[test]
    fn hints_round_trip_through_json_details() {
        let hints = vec![
            Hint::Next("ployz deploy".into()),
            Hint::Inspect("ployz logs web".into()),
            Hint::Inspect("ployz ps".into()),
            Hint::Retry("ployz project rm blog --confirm blog".into()),
            Hint::Undo("ployz env sync --undo s3".into()),
            Hint::Closest("web".into()),
            Hint::valid(["web", "db"]),
        ];
        let mut details = serde_json::json!({ "deployment": "d1" });
        Hint::into_details(&hints, &mut details);
        assert_eq!(
            details,
            serde_json::json!({
                "deployment": "d1",
                "next": "ployz deploy",
                "inspect": ["ployz logs web", "ployz ps"],
                "retry": "ployz project rm blog --confirm blog",
                "undo": "ployz env sync --undo s3",
                "did_you_mean": "web",
                "valid_children": ["web", "db"],
            })
        );
        assert_eq!(Hint::from_details(&details), hints);
    }

    #[test]
    fn a_foreign_cause_stays_on_its_line() {
        let mut out = anstream::StripStream::new(Vec::new());
        write(
            &mut out,
            "Could not create the Container.",
            Some("error from registry: denied\ndenied\n"),
            &[],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            "error: Could not create the Container.\n  cause: error from registry: denied; denied\n"
        );
    }

    #[test]
    fn the_closest_name_shows_even_when_the_list_is_cut() {
        let details = serde_json::json!({
            "did_you_mean": "s11",
            "valid_children": (1..=11).map(|n| format!("s{n}")).collect::<Vec<_>>(),
        });
        let mut out = anstream::StripStream::new(Vec::new());
        write(
            &mut out,
            "No Setting s1l",
            None,
            &Hint::from_details(&details),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            "error: No Setting s1l\n\
             valid: did you mean s11?\n\
             valid: s1, s2, s3, s4, s5, s6, s7, s8, and 3 more\n"
        );
    }
}
