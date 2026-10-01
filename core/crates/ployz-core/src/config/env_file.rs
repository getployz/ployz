//! The one `.env` reader: `ployz set --from-env-file` and the dashboard's raw editor both use it.

use super::ConfigError;

/// One `KEY=VALUE` of a `.env` text, in file order. A key that repeats appears each time; the caller keeps the last.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
}

/// Read `.env` text: blank lines and `#` comments skipped, an optional `export `, values bare (ending at ` #`),
/// single-quoted (literal) or double-quoted (`\n \r \t \\ \"` escapes), and a quoted value may span lines. Keys keep
/// their case and are letters, digits and `_`, not starting with a digit.
///
/// # Errors
/// Names the first bad line by number, never its text (it may hold a secret).
pub fn parse_env_file(text: &str) -> Result<Vec<EnvEntry>, ConfigError> {
    // Editors on Windows save a byte-order mark before the first key.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text.lines().collect();
    let mut entries = Vec::new();
    let mut index = 0;
    while let Some(raw) = lines.get(index) {
        let number = index + 1;
        // Only the start is trimmed here: a quoted value keeps the spaces inside its quotes.
        let line = raw.trim_start();
        index += 1;
        if line.trim_end().is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line
            .strip_prefix("export ")
            .or_else(|| line.strip_prefix("export\t"))
            .map_or(line, str::trim_start);
        let bad = |message: &str| ConfigError::at("env", &format!("Line {number}: {message}"));
        let Some((key, value)) = line.split_once('=') else {
            return Err(bad("expected KEY=VALUE"));
        };
        let key = key.trim();
        let valid_key = key
            .chars()
            .next()
            .is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
            && key.chars().all(|c| c == '_' || c.is_ascii_alphanumeric());
        if !valid_key {
            return Err(bad(
                "a variable name is letters, digits and _, not starting with a digit",
            ));
        }
        let value = value.trim_start();
        let value = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let mut rest = value.strip_prefix(quote).unwrap_or(value).to_owned();
                // A quoted value may run onto the next lines until its closing quote.
                loop {
                    if let Some(parsed) = quoted(&rest, quote).map_err(bad)? {
                        break parsed;
                    }
                    let Some(next) = lines.get(index) else {
                        return Err(bad(if quote == '"' {
                            "unterminated double-quoted value"
                        } else {
                            "unterminated single-quoted value"
                        }));
                    };
                    index += 1;
                    rest.push('\n');
                    rest.push_str(next);
                }
            }
            // A bare value ends at a comment after a space or a tab.
            _ => value
                .split(" #")
                .next()
                .and_then(|value| value.split("\t#").next())
                .unwrap_or_default()
                .trim_end()
                .to_owned(),
        };
        // A container's environment can't carry NUL.
        if value.contains('\0') {
            return Err(bad("null characters are not allowed"));
        }
        entries.push(EnvEntry {
            key: key.to_owned(),
            value,
        });
    }
    Ok(entries)
}

/// The value inside `quote` at the start of `text` (after the opening quote), or `None` when it hasn't closed yet.
fn quoted(text: &str, quote: char) -> Result<Option<String>, &'static str> {
    let mut value = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\\' && quote == '"' {
            match chars.next() {
                Some('n') => value.push('\n'),
                Some('r') => value.push('\r'),
                Some('t') => value.push('\t'),
                Some(other) => value.push(other),
                None => value.push('\\'),
            }
            continue;
        }
        if c == quote {
            let tail = chars.as_str().trim();
            if !tail.is_empty() && !tail.starts_with('#') {
                return Err("unexpected text after the closing quote");
            }
            return Ok(Some(value));
        }
        value.push(c);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> Vec<(String, String)> {
        parse_env_file(text)
            .unwrap()
            .into_iter()
            .map(|entry| (entry.key, entry.value))
            .collect()
    }

    #[test]
    fn keeps_spaces_inside_quotes() {
        assert_eq!(
            read("T=\"first  \n\n # literal\nFOO=inside\n  last \"\nA=b"),
            [
                (
                    "T".to_owned(),
                    "first  \n\n # literal\nFOO=inside\n  last ".to_owned()
                ),
                ("A".to_owned(), "b".to_owned()),
            ]
        );
    }

    #[test]
    fn reads_what_both_surfaces_used_to_read_differently() {
        let text = "A=1\n# a comment\nexport EXPORTED=yes\nQUOTED=\"has spaces and # hash\"\nSINGLE='single $NOT'\nEMPTY=\nlower=kept\nMULTI=\"line1\nline2\"\nESC=\"a\\tb\\nc\\\\d\\\"e\"\nBARE=value # note\nDUP=first\nDUP=second\n";
        assert_eq!(
            read(text),
            [
                ("A", "1"),
                ("EXPORTED", "yes"),
                ("QUOTED", "has spaces and # hash"),
                ("SINGLE", "single $NOT"),
                ("EMPTY", ""),
                ("lower", "kept"),
                ("MULTI", "line1\nline2"),
                ("ESC", "a\tb\nc\\d\"e"),
                ("BARE", "value"),
                ("DUP", "first"),
                ("DUP", "second"),
            ]
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
        );
    }

    #[test]
    fn reads_windows_and_tab_separated_files() {
        assert_eq!(
            read("\u{feff}A=1\nexport\tB=2\nC=3\t# note\n"),
            [("A", "1"), ("B", "2"), ("C", "3")]
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        );
        assert_eq!(
            parse_env_file("A=x\0y").unwrap_err().message,
            "Line 1: null characters are not allowed"
        );
        assert!(parse_env_file("web.env.A=1").is_err());
    }

    #[test]
    fn names_the_first_bad_line_without_its_text() {
        let error = |text: &str| parse_env_file(text).unwrap_err().message;
        assert_eq!(
            error("A=1\n=novalue\n"),
            "Line 2: a variable name is letters, digits and _, not starting with a digit"
        );
        assert_eq!(
            error("BAD KEY=1"),
            "Line 1: a variable name is letters, digits and _, not starting with a digit"
        );
        assert_eq!(
            error("1X=1"),
            "Line 1: a variable name is letters, digits and _, not starting with a digit"
        );
        assert_eq!(error("A=1\nnot a pair\n"), "Line 2: expected KEY=VALUE");
        assert_eq!(
            error("S=\"secret\nnever closed"),
            "Line 1: unterminated double-quoted value"
        );
        assert_eq!(
            error("S=\"secret\" trailing"),
            "Line 1: unexpected text after the closing quote"
        );
    }
}
