//! The error vocabulary: hints and the `error:` / `cause:` lines.

use std::{error::Error, io};

use serde_json::{Map, Value};

use super::Tone;

/// Most choices a `valid:` line lists before it says how many more there are.
const VALID_SHOWN: usize = 8;

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
    /// The choices the input could have named.
    Valid(Vec<String>),
}

impl Hint {
    #[must_use]
    pub fn valid<I: IntoIterator<Item = S>, S: Into<String>>(names: I) -> Self {
        Self::Valid(names.into_iter().map(Into::into).collect())
    }

    const fn label(&self) -> &'static str {
        match self {
            Self::Next(_) => "next:",
            Self::Inspect(_) => "inspect:",
            Self::Retry(_) => "retry:",
            Self::Undo(_) => "undo:",
            Self::Valid(_) => "valid:",
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Next(command)
            | Self::Inspect(command)
            | Self::Retry(command)
            | Self::Undo(command) => command.clone(),
            Self::Valid(names) if names.len() > VALID_SHOWN => format!(
                "{}, and {} more",
                names[..VALID_SHOWN].join(", "),
                names.len() - VALID_SHOWN
            ),
            Self::Valid(names) => names.join(", "),
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
            .chain((!valid.is_empty()).then_some(Self::Valid(valid)))
            .collect()
    }

    /// Write `hints` into `--json` `details`: `next`, `retry` and `undo` are one
    /// command each, `inspect` and `valid_children` are arrays.
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

/// The `cause:` lines under an error: each `source()` below the top, raw, with
/// a wrapper that only repeats the line above it dropped.
#[must_use]
pub fn causes(top: &(dyn Error + 'static)) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut above = top.to_string();
    let mut next = top.source();
    while let Some(error) = next {
        let line = error.to_string();
        if line != above && !line.is_empty() {
            lines.push(line.clone());
        }
        above = line;
        next = error.source();
    }
    lines
}

/// Write the human error: `error:` with our sentence, a `cause:` line per source,
/// then the hints.
///
/// # Errors
///
/// Returns the writer's error.
pub fn write(
    out: &mut dyn io::Write,
    message: &str,
    causes: &[String],
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
    for cause in causes {
        writeln!(out, "  {} {cause}", Tone::Bad.paint("cause:"))?;
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
                "valid_children": ["web", "db"],
            })
        );
        assert_eq!(Hint::from_details(&details), hints);
    }
}
