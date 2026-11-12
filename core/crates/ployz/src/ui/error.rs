//! The error vocabulary: hints and the `error:` / `cause:` lines.

use std::io;

use ployz_core::Hint;

use super::{Tone, escaped, pasteable, unprintable};

/// Most choices a `valid:` line lists before it says how many more there are.
pub const VALID_SHOWN: usize = 8;

const fn label(hint: &Hint) -> Option<&'static str> {
    match hint {
        Hint::Next(_) => Some("next:"),
        Hint::Inspect(_) => Some("inspect:"),
        Hint::Retry(_) => Some("retry:"),
        Hint::Undo(_) => Some("undo:"),
        Hint::Closest(_) => None,
        Hint::Valid(_) => Some("valid:"),
    }
}

/// What the reader sees: a command they can paste, or names [`escaped`] to read as stored.
fn text(hint: &Hint) -> String {
    match hint {
        Hint::Next(command)
        | Hint::Inspect(command)
        | Hint::Retry(command)
        | Hint::Undo(command) => pasteable(command),
        Hint::Closest(name) => format!("did you mean {}?", escaped(name)),
        Hint::Valid(names) => {
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

/// Write `hint` as its own line.
///
/// # Errors
///
/// Returns the writer's error.
pub(super) fn write_hint(out: &mut dyn io::Write, hint: &Hint) -> io::Result<()> {
    let text = text(hint);
    match label(hint) {
        Some(label) => writeln!(out, "{} {text}", Tone::Label.paint(label)),
        None => writeln!(out, "{text}"),
    }
}

/// [`ployz_core::error_chain::one_line`] for the human `cause:` line: a trim keeps an
/// [`unprintable`] character other than a tab, so [`escaped`] can show it.
fn shown_line(text: &str) -> String {
    let blank = |c: char| c == '\t' || (c.is_whitespace() && !unprintable(c));
    ployz_core::error_chain::joined(text.split('\n').map(|line| line.trim_matches(blank)))
}

type Chain<'a> = &'a (dyn std::error::Error + 'static);

pub(crate) fn warn_cause(what: impl std::fmt::Display, error: Chain<'_>) {
    super::warn(format!("{what}: {}", ployz_core::error_chain::row(error)));
}

/// Prints a wait's notice on stderr.
pub(crate) struct WaitNotices;

impl crate::setup_retry::Observer for WaitNotices {
    fn waiting(&self, notice: &crate::setup_retry::Notice<'_>) {
        match *notice {
            crate::setup_retry::Notice::Expected(crate::setup_retry::Expected(text)) => {
                super::note(text);
            }
            crate::setup_retry::Notice::Retrying {
                operation,
                error,
                seconds_left,
            } => super::note(retrying_line(operation, error, seconds_left)),
        }
    }
}

fn retrying_line(operation: &str, error: Chain<'_>, seconds: u64) -> String {
    format!(
        "{operation}: {}; retrying for up to {seconds}s. Check outbound firewall access if this connection is blocked.",
        ployz_core::error_chain::row(error)
    )
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
        write_hint(out, hint)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_retry_notice_names_the_operation_the_cause_and_the_time_left() {
        let error = std::io::Error::other("connection refused");
        assert_eq!(
            retrying_line("Reconnecting to the Cluster", &error, 60),
            "Reconnecting to the Cluster: connection refused; retrying for up to 60s. Check outbound firewall access if this connection is blocked."
        );
    }

    #[test]
    fn valid_caps_at_eight_and_counts_the_rest() {
        let names = (1..=11).map(|n| format!("s{n}"));
        let mut out = anstream::StripStream::new(Vec::new());
        write_hint(&mut out, &Hint::valid(names)).unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            "valid: s1, s2, s3, s4, s5, s6, s7, s8, and 3 more\n"
        );
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
        write_hint(
            &mut out,
            &Hint::Next("cd \"x y\" && ployz  rm a\u{202E}b".into()),
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out.into_inner()).unwrap(),
            "next: cd \"x y\" && ployz  rm $'a\\xe2\\x80\\xaeb'\n"
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
