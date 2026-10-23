//! The error vocabulary: hints and the `error:` / `cause:` lines.

use std::io;

use serde_json::{Map, Value};

use super::{Tone, escaped, shell_word, unprintable};

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

    const fn label(&self) -> Option<&'static str> {
        match self {
            Self::Next(_) => Some("next:"),
            Self::Inspect(_) => Some("inspect:"),
            Self::Retry(_) => Some("retry:"),
            Self::Undo(_) => Some("undo:"),
            Self::Closest(_) => None,
            Self::Valid(_) => Some("valid:"),
        }
    }

    /// What the reader sees: a command they can paste, or names [`escaped`] to read as stored.
    fn text(&self) -> String {
        match self {
            Self::Next(command)
            | Self::Inspect(command)
            | Self::Retry(command)
            | Self::Undo(command) => pasteable(command),
            Self::Closest(name) => format!("did you mean {}?", escaped(name)),
            Self::Valid(names) => {
                let names: Vec<_> = names.iter().map(|name| escaped(name)).collect();
                match names.split_at_checked(VALID_SHOWN) {
                    Some((shown, rest)) if !rest.is_empty() => {
                        format!("{}, and {} more", shown.join(", "), rest.len())
                    }
                    _ => names.join(", "),
                }
            }
        }
    }

    /// Write this hint as its own line.
    ///
    /// # Errors
    ///
    /// Returns the writer's error.
    pub fn write(&self, out: &mut dyn io::Write) -> io::Result<()> {
        let text = self.text();
        match self.label() {
            Some(label) => writeln!(out, "{} {text}", Tone::Label.paint(label)),
            None => writeln!(out, "{text}"),
        }
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

/// `command` with each word that holds an [`unprintable`] character in [`shell_word`]'s
/// `$'…'` form, so it shows safely and still pastes as the same command. Every other word
/// stays as written; a word bash would read differently, or a command that doesn't parse,
/// prints [`escaped`].
fn pasteable(command: &str) -> String {
    if !command.chars().any(unprintable) {
        return command.to_owned();
    }
    let mut words = Vec::new();
    let mut open: Option<String> = None;
    // A quoted word can hold spaces, so a piece joins the next until it parses.
    for piece in command.split(' ') {
        let chunk = match open.take() {
            Some(open) => format!("{open} {piece}"),
            None => piece.to_owned(),
        };
        match shell_words::split(&chunk) {
            Err(_) => open = Some(chunk),
            Ok(_) if !chunk.chars().any(unprintable) => words.push(chunk),
            Ok(parsed) => words.push(match parsed.as_slice() {
                [word] if quoted_literally(&chunk) => shell_word(word).into_owned(),
                _ => escaped(&chunk),
            }),
        }
    }
    match open {
        Some(_) => escaped(command),
        None => words.join(" "),
    }
}

/// Whether bash reads `word` as [`shell_words::split`] does: outside single quotes it holds
/// nothing bash would expand, glob or read as an operator, and no unquoted `=` that could
/// make it an assignment. A backslash escapes the same in both, but one that ends `word`
/// would join it to the next.
fn quoted_literally(word: &str) -> bool {
    let (mut single, mut double, mut escaping) = (false, false, false);
    for c in word.chars() {
        match c {
            _ if escaping => escaping = false,
            '\'' if !double => single = !single,
            _ if single => {}
            '\\' => escaping = true,
            '"' => double = !double,
            '$' | '`' | '!' => return false,
            '~' | '*' | '?' | '[' | '{' | ';' | '&' | '|' | '<' | '>' | '(' | ')' | '='
                if !double =>
            {
                return false;
            }
            _ => {}
        }
    }
    !escaping
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
    joined(text.lines().map(str::trim))
}

/// [`one_line`] for the human `cause:` line: a trim keeps an [`unprintable`] character
/// other than a tab, so [`escaped`] can show it.
fn shown_line(text: &str) -> String {
    let blank = |c: char| c == '\t' || (c.is_whitespace() && !unprintable(c));
    joined(text.split('\n').map(|line| line.trim_matches(blank)))
}

fn joined<'a>(trimmed: impl Iterator<Item = &'a str>) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in trimmed.filter(|line| !line.is_empty()) {
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

/// `text` as a failure's first line: a sentence, ending with a period.
pub(crate) fn sentence(mut text: String) -> String {
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    text
}

/// `next` follows `message` as its own sentence, with one break however
/// `message` ended. A dangling colon introduces nothing, so it becomes the break.
pub(crate) fn then_sentence(message: &str, next: &str) -> String {
    match message.trim_end().trim_end_matches(':').trim_end() {
        "" => next.to_owned(),
        message => format!("{} {next}", sentence(message.to_owned())),
    }
}

/// `what`, then `: reason` when there is a reason to give.
pub(crate) fn with_reason(what: impl std::fmt::Display, reason: &str) -> String {
    match reason.trim() {
        "" => what.to_string(),
        reason => format!("{what}: {reason}"),
    }
}

#[cfg(test)]
pub(crate) fn chain_text(error: Chain<'_>) -> String {
    inline(error)
}

/// Write the human error: `error:` with our sentence, one `cause:` line with the
/// deepest source, then the hints. Each line is [`escaped`] so a name from the Store reads
/// as stored, and a command hint is [`pasteable`]. `--json` keeps every cause, and the raw text.
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
    let mut lines = message.split_terminator('\n').map(escaped);
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
        writeln!(
            out,
            "  {} {}",
            Tone::Bad.paint("cause:"),
            escaped(&shown_line(cause))
        )?;
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
    fn a_carriage_return_prints_as_its_code() {
        let mut out = anstream::StripStream::new(Vec::new());
        write(
            &mut out,
            "Config app has no file a.txt\r\n  Shown as stored.\n",
            Some("refused a.txt\r\n\tdenied \n"),
            &[],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            "error: Config app has no file a.txt\\u{d}\n  Shown as stored.\n  \
             cause: refused a.txt\\u{d}; denied\n"
        );
    }

    #[test]
    fn a_foreign_line_end_trims_off_the_one_line_form() {
        let error = io::Error::other("refused a.txt\r\n\tdenied \r\n");
        assert_eq!(chain_text(&error), "refused a.txt; denied");
        assert_eq!(row(&error), "refused a.txt; denied");
        let wire = serde_json::to_value(rpc_error(ployz_core::RpcErrorCode::Internal, &error));
        assert_eq!(
            wire.unwrap().pointer("/message").unwrap(),
            "refused a.txt; denied"
        );
    }

    #[test]
    fn an_invisible_character_prints_as_its_code_once() {
        let mut out = anstream::StripStream::new(Vec::new());
        write(
            &mut out,
            "Config app has no file a\u{202E}b.txt\nAlready shown: a\\u{202e}b.txt",
            Some("refused a\u{202E}b.txt"),
            &[
                Hint::Closest("a\u{202E}b.txt".into()),
                Hint::Next("ployz config rm-file app a\u{202E}b.txt".into()),
            ],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            "error: Config app has no file a\\u{202e}b.txt\n\
             Already shown: a\\u{202e}b.txt\n  \
             cause: refused a\\u{202e}b.txt\n\
             did you mean a\\u{202e}b.txt?\n\
             next: ployz config rm-file app $'a\\xe2\\x80\\xaeb.txt'\n"
        );
    }

    #[test]
    fn a_command_hint_quotes_an_invisible_character_so_it_pastes_back() {
        let name = "a\u{202E}b.txt";
        let config = format!("/tmp/{name}/config.yaml");
        let next = ["ployz", "config", "rm-file", "app", name];
        let retry = ["ployz", "--ployz-config", config.as_str(), "deploy"];
        let mut out = anstream::StripStream::new(Vec::new());
        write(
            &mut out,
            "Config app has no file.",
            None,
            &[
                Hint::Next(shell_words::join(next)),
                Hint::Retry(shell_words::join(retry)),
                Hint::Undo("ployz config rm-file app 'a\u{202E}b.txt".into()),
            ],
        )
        .unwrap();
        let shown = String::from_utf8(out.into_inner()).unwrap();
        assert_eq!(
            shown,
            "error: Config app has no file.\n\
             next: ployz config rm-file app $'a\\xe2\\x80\\xaeb.txt'\n\
             retry: ployz --ployz-config $'/tmp/a\\xe2\\x80\\xaeb.txt/config.yaml' deploy\n\
             undo: ployz config rm-file app 'a\\u{202e}b.txt\n"
        );
        for (line, words) in shown.lines().skip(1).zip([&next[..], &retry[..]]) {
            let (_, command) = line.split_once(": ").unwrap();
            let parsed = std::process::Command::new("bash")
                .args(["-c", &format!("printf '%s\\0' {command}")])
                .output()
                .unwrap();
            assert!(parsed.status.success(), "{parsed:?}");
            let parsed = String::from_utf8(parsed.stdout).unwrap();
            assert_eq!(
                parsed
                    .trim_end_matches('\0')
                    .split('\0')
                    .collect::<Vec<_>>(),
                words
            );
        }
    }

    #[test]
    fn a_command_hint_without_an_invisible_character_prints_as_built() {
        let hints = [
            "unset PLOYZ_TOKEN && ployz login",
            "ployz config put app 'a b.txt' --from \"./x y\"",
            "ployz  env sync --undo $'a\\xe2\\x80\\xaeb'",
            "ployz logs web",
        ];
        let mut out = anstream::StripStream::new(Vec::new());
        write(
            &mut out,
            "Refused.",
            None,
            &[
                Hint::Next(hints[0].into()),
                Hint::Retry(hints[1].into()),
                Hint::Undo(hints[2].into()),
                Hint::Inspect(hints[3].into()),
            ],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            format!(
                "error: Refused.\nnext: {}\nretry: {}\nundo: {}\ninspect: {}\n",
                hints[0], hints[1], hints[2], hints[3]
            )
        );
    }

    #[test]
    fn only_the_word_with_an_invisible_character_is_requoted() {
        let mut out = anstream::StripStream::new(Vec::new());
        Hint::Next("cd \"x y\" && ployz  rm a\u{202E}b".into())
            .write(&mut out)
            .unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            "next: cd \"x y\" && ployz  rm $'a\\xe2\\x80\\xaeb'\n"
        );
    }

    #[test]
    fn a_command_hint_never_pastes_as_a_different_command() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a\u{202E}bX"), "").unwrap();
        // `ployz` prints `$NAME` first, so a leading assignment is seen as bash reads it.
        let argv = |command: &str| {
            let output = std::process::Command::new("bash")
                .args([
                    "-c",
                    &format!("ployz() {{ printf '%s\\0' \"${{NAME-}}\" \"$@\"; }}; {command}"),
                ])
                .current_dir(dir.path())
                .env("HOME", "/home/u")
                .output()
                .unwrap();
            assert!(output.status.success(), "{command}: {output:?}");
            output.stdout
        };
        for hint in [
            "ployz config put app \"$HOME/a\u{202E}b.txt\"",
            "ployz config put app $'a\u{202E}b'",
            "ployz config put app ~/a\u{202E}b",
            "ployz config put app a\u{202E}b*",
            "ployz config put app \"`printf x`a\u{202E}b\"",
            "ployz config put app a\\ \u{202E}b",
            "ployz config put app \"a\\\"\u{202E}b\\$\"",
            "ployz config put app '$HOME/a\u{202E}b*'",
            "ployz config put app 'it'\\''s\u{202E}b'",
            "ployz config put app a\\'$HOME\u{202E}",
            "ployz config put app \u{202E}\\ b",
            "NAME=\"a\u{202E}b\" ployz deploy",
            "NAME=a\u{202E}b ployz deploy",
        ] {
            let shown = pasteable(hint);
            assert!(!shown.chars().any(unprintable), "{shown}");
            assert!(
                shown == escaped(hint) || argv(&shown) == argv(hint),
                "{hint:?} shows as {shown}"
            );
        }
        assert_eq!(
            pasteable("ployz config put app '$HOME/a\u{202E}b*' 'it'\\''s\u{202E}b'"),
            "ployz config put app $'$HOME/a\\xe2\\x80\\xaeb*' $'it\\'s\\xe2\\x80\\xaeb'"
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
             did you mean s11?\n\
             valid: s1, s2, s3, s4, s5, s6, s7, s8, and 3 more\n"
        );
    }
}
