//! Lists and records: aligned for a person at a terminal, tab- or `=`-separated
//! for a pipe.

use std::{borrow::Cow, io};

use unicode_properties::{GeneralCategory, UnicodeGeneralCategory};
use unicode_width::UnicodeWidthStr as _;

use super::Tone;

const GAP: &str = "   ";

/// One table cell: text, and the tone it carries on a color terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    text: String,
    tone: Option<Tone>,
}

impl Cell {
    /// A status cell: colored on a terminal, with a mark when aligned.
    #[must_use]
    pub fn status(text: impl Into<String>, tone: Tone) -> Self {
        Self {
            text: text.into(),
            tone: Some(tone),
        }
    }

    fn shown(&self) -> &str {
        if self.text.is_empty() {
            "-"
        } else {
            &self.text
        }
    }

    fn mark(&self) -> &'static str {
        match self.tone {
            Some(Tone::Good) => "✔ ",
            Some(Tone::Bad) => "✘ ",
            Some(Tone::Change | Tone::Muted | Tone::Name | Tone::Link | Tone::Label) | None => "",
        }
    }

    fn width(&self) -> usize {
        self.mark().width() + self.shown().width()
    }

    /// The cell as one TSV field: no tab or newline may split it.
    fn field(&self) -> String {
        self.shown().replace(['\t', '\n'], " ")
    }
}

/// The cell's text in its tone, as a record shows it.
impl std::fmt::Display for Cell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.tone {
            Some(tone) => write!(f, "{}", tone.paint(self.shown())),
            None => f.write_str(self.shown()),
        }
    }
}

impl From<String> for Cell {
    fn from(text: String) -> Self {
        Self { text, tone: None }
    }
}

impl From<&str> for Cell {
    fn from(text: &str) -> Self {
        text.to_owned().into()
    }
}

impl From<&String> for Cell {
    fn from(text: &String) -> Self {
        text.clone().into()
    }
}

/// Rows under an UPPERCASE header, and the sentence that stands in for no rows.
#[derive(Debug)]
pub struct Table {
    header: Vec<Cow<'static, str>>,
    rows: Vec<Vec<Cell>>,
    empty: String,
}

impl Table {
    /// `empty` is the sentence a person reads when there are no rows, such as
    /// `No Services in production yet.`
    #[must_use]
    pub fn new<H: Into<Cow<'static, str>>>(
        header: impl IntoIterator<Item = H>,
        empty: impl Into<String>,
    ) -> Self {
        Self {
            header: header.into_iter().map(Into::into).collect(),
            rows: Vec::new(),
            empty: empty.into(),
        }
    }

    pub fn row<C: Into<Cell>>(&mut self, cells: impl IntoIterator<Item = C>) {
        self.rows.push(cells.into_iter().map(Into::into).collect());
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub(crate) fn empty_sentence(&self) -> &str {
        &self.empty
    }

    /// Write the table: aligned with a bold header on a terminal, else TSV.
    /// An aligned empty table writes nothing; its sentence goes elsewhere.
    ///
    /// # Errors
    ///
    /// Returns the writer's error.
    pub fn write(&self, out: &mut dyn io::Write, aligned: bool) -> io::Result<()> {
        if aligned {
            self.write_aligned(out)
        } else {
            self.write_tsv(out)
        }
    }

    fn write_tsv(&self, out: &mut dyn io::Write) -> io::Result<()> {
        writeln!(out, "{}", self.header.join("\t"))?;
        for row in &self.rows {
            let fields: Vec<String> = row.iter().map(Cell::field).collect();
            writeln!(out, "{}", fields.join("\t"))?;
        }
        Ok(())
    }

    fn write_aligned(&self, out: &mut dyn io::Write) -> io::Result<()> {
        if self.rows.is_empty() {
            return Ok(());
        }
        let mut widths: Vec<usize> = self.header.iter().map(|title| title.width()).collect();
        for row in &self.rows {
            for (column, cell) in row.iter().enumerate() {
                if let Some(width) = widths.get_mut(column) {
                    *width = (*width).max(cell.width());
                }
            }
        }
        let last = widths.len().saturating_sub(1);
        let header: Vec<String> = self
            .header
            .iter()
            .zip(&widths)
            .enumerate()
            .map(|(column, (title, width))| pad(title, title.width(), *width, column == last))
            .collect();
        writeln!(out, "{}", Tone::Name.paint(header.concat()))?;
        for row in &self.rows {
            let mut line = String::new();
            for (column, cell) in row.iter().enumerate() {
                let text = format!("{}{}", cell.mark(), cell.shown());
                let painted = match cell.tone {
                    Some(tone) => tone.paint(&text).to_string(),
                    None => text,
                };
                let width = widths.get(column).copied().unwrap_or_default();
                line.push_str(&pad(&painted, cell.width(), width, column >= last));
            }
            writeln!(out, "{}", line.trim_end())?;
        }
        Ok(())
    }
}

/// `text` padded to `width` columns and followed by the gap, unless it ends the line.
fn pad(text: &str, shown: usize, width: usize, last: bool) -> String {
    if last {
        return text.to_owned();
    }
    format!("{text}{}{GAP}", " ".repeat(width.saturating_sub(shown)))
}

/// One record: a label and a value per line.
#[derive(Debug, Default)]
pub struct Fields {
    fields: Vec<(Cow<'static, str>, String)>,
}

impl Fields {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn field(
        mut self,
        label: impl Into<Cow<'static, str>>,
        value: impl std::fmt::Display,
    ) -> Self {
        self.push(label, value);
        self
    }

    pub fn push(&mut self, label: impl Into<Cow<'static, str>>, value: impl std::fmt::Display) {
        self.fields.push((label.into(), value.to_string()));
    }

    /// Write the record: `label  value` aligned on a terminal, else `label = value`.
    ///
    /// # Errors
    ///
    /// Returns the writer's error.
    pub fn write(&self, out: &mut dyn io::Write, aligned: bool) -> io::Result<()> {
        let width = self
            .fields
            .iter()
            .map(|(label, _)| label.width())
            .max()
            .unwrap_or_default();
        for (label, value) in &self.fields {
            let value = if value.is_empty() { "-" } else { value };
            let painted = Tone::Muted.paint(label.as_ref());
            if aligned {
                let padding = " ".repeat(width - label.width());
                writeln!(out, "{painted}{padding}  {value}")?;
            } else {
                writeln!(out, "{painted} = {value}")?;
            }
        }
        Ok(())
    }
}

pub(crate) fn short_id(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

pub(crate) fn unprintable(c: char) -> bool {
    c.is_control()
        || matches!(
            c.general_category(),
            GeneralCategory::Format
                | GeneralCategory::LineSeparator
                | GeneralCategory::ParagraphSeparator
        )
}

/// `text` with each [`unprintable`] character as its `\u{…}` escape, so a value shows as stored.
pub(crate) fn escaped(text: &str) -> String {
    text.chars()
        .map(|c| {
            if unprintable(c) {
                c.escape_unicode().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

/// `text` [`escaped`] line by line, so it keeps its line breaks.
pub(crate) fn escaped_lines(text: &str) -> String {
    text.split('\n').map(escaped).collect::<Vec<_>>().join("\n")
}

/// `arg` as one shell word: a word with an [`unprintable`] character in `$'…'` form, so it
/// shows as its bytes and pastes back as itself.
pub(crate) fn shell_word(arg: &str) -> std::borrow::Cow<'_, str> {
    use std::fmt::Write as _;
    if !arg.chars().any(unprintable) {
        return shell_words::quote(arg);
    }
    let mut word = String::from("$'");
    for c in arg.chars() {
        match c {
            '\\' | '\'' => word.extend(['\\', c]),
            c if !unprintable(c) => word.push(c),
            // Bash decodes \u by the locale; bytes read the same under any.
            _ => {
                for byte in c.encode_utf8(&mut [0; 4]).bytes() {
                    let _ = write!(word, "\\x{byte:02x}");
                }
            }
        }
    }
    word.push('\'');
    word.into()
}

/// `command` with each word that holds an [`unprintable`] character in [`shell_word`]'s
/// `$'…'` form, so it shows safely and still pastes as the same command. Every other word
/// stays as written; a word bash would read differently, or a command that doesn't parse,
/// prints [`escaped`].
pub(crate) fn pasteable(command: &str) -> String {
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

pub(crate) const REPLACED: char = '\u{FFFD}';

pub(crate) fn printable(text: &str) -> String {
    text.chars()
        .map(|c| if unprintable(c) { REPLACED } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_warning_prints_an_invisible_character_as_its_code_and_keeps_its_lines() {
        assert_eq!(
            escaped_lines("Left KEY empty in app/a\u{202E}b.txt: it is not set.\nDeploy\r again."),
            "Left KEY empty in app/a\\u{202e}b.txt: it is not set.\nDeploy\\u{d} again."
        );
        assert_eq!(
            escaped_lines("Docker volume (not recommended)"),
            "Docker volume (not recommended)"
        );
    }

    #[test]
    fn a_retry_line_escapes_what_would_steer_the_terminal() {
        assert_eq!(shell_word("web-*"), "'web-*'");
        assert_eq!(shell_word("web-1"), "web-1");
        assert_eq!(shell_word("web\u{1b}[31m'1"), "$'web\\x1b[31m\\'1'");
        assert_eq!(shell_word("evil\u{202E}1\\"), "$'evil\\xe2\\x80\\xae1\\\\'");
        assert_eq!(
            shell_word("a\u{85}\u{E0001}"),
            "$'a\\xc2\\x85\\xf3\\xa0\\x80\\x81'"
        );
    }

    #[test]
    fn bash_reads_a_retry_word_back_as_the_same_bytes_in_any_locale() {
        let words = [
            "a\u{85}b",
            "evil\u{202E}1",
            "\u{E0001}tag\u{85}a",
            "\u{1F600}\u{1b}[0m\u{1F600}",
        ];
        for locale in ["C", "C.UTF-8"] {
            for word in words {
                let output = std::process::Command::new("bash")
                    .args(["-c", &format!("printf %s {}", shell_word(word))])
                    .env("LC_ALL", locale)
                    .output()
                    .unwrap();
                assert!(output.status.success(), "{locale} {word:?}");
                assert_eq!(output.stdout, word.as_bytes(), "{locale} {word:?}");
            }
        }
    }

    #[test]
    fn a_pasteable_command_escapes_only_the_words_that_need_it() {
        let plain = "ployz set 'sentry.env.REDIS=VALUE' --env production";
        assert_eq!(pasteable(plain), plain);
        assert_eq!(
            pasteable(&shell_words::join([
                "ployz",
                "config",
                "put",
                "app",
                "a\u{202E}b.txt"
            ])),
            "ployz config put app $'a\\xe2\\x80\\xaeb.txt'"
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

    fn written(table: &Table, aligned: bool) -> String {
        let mut out = anstream::StripStream::new(Vec::new());
        table.write(&mut out, aligned).unwrap();
        String::from_utf8(out.into_inner()).unwrap()
    }

    fn services() -> Table {
        let mut table = Table::new(
            ["NAME", "IMAGE", "STATUS"],
            "No Services in production yet.",
        );
        table.row(
            ["web", "acme/web:1.5"]
                .map(Cell::from)
                .into_iter()
                .chain([Cell::status("running", Tone::Good)]),
        );
        table.row([
            Cell::from("日本語"),
            Cell::from(""),
            Cell::status("crashed", Tone::Bad),
        ]);
        table
    }

    #[test]
    fn a_host_cannot_print_escape_sequences_or_reorder_text() {
        assert_eq!(printable("web\u{1b}[2J-1"), "web\u{FFFD}[2J-1");
        assert_eq!(printable("a\u{7}b"), "a\u{FFFD}b");
        assert_eq!(printable("evil\u{202E}1-bew"), "evil\u{FFFD}1-bew");
        assert_eq!(printable("we\u{200B}b"), "we\u{FFFD}b");
        assert_eq!(printable("web\u{2028}1\u{2029}"), "web\u{FFFD}1\u{FFFD}");
        assert_eq!(printable("wéb-1 · 東京"), "wéb-1 · 東京");
    }

    #[test]
    fn aligned_columns_count_wide_characters_and_leave_no_trailing_space() {
        assert_eq!(
            written(&services(), true),
            "NAME     IMAGE          STATUS\n\
             web      acme/web:1.5   ✔ running\n\
             日本語   -              ✘ crashed\n"
        );
    }

    #[test]
    fn a_pipe_gets_tsv_without_marks() {
        let mut table = services();
        table.row(["tab\there", "line\nbreak", ""]);
        assert_eq!(
            written(&table, false),
            "NAME\tIMAGE\tSTATUS\n\
             web\tacme/web:1.5\trunning\n\
             日本語\t-\tcrashed\n\
             tab here\tline break\t-\n"
        );
    }

    #[test]
    fn an_empty_table_is_only_its_header_on_a_pipe() {
        let table = Table::new(["NAME", "IMAGE"], "No Services in production yet.");
        assert_eq!(written(&table, false), "NAME\tIMAGE\n");
        assert_eq!(written(&table, true), "");
    }

    #[test]
    fn a_record_aligns_on_a_terminal_and_uses_equals_on_a_pipe() {
        let record = Fields::new()
            .field("name", "web")
            .field("replicas", 2)
            .field("domain", "");
        let write = |aligned| {
            let mut out = anstream::StripStream::new(Vec::new());
            record.write(&mut out, aligned).unwrap();
            String::from_utf8(out.into_inner()).unwrap()
        };
        assert_eq!(write(true), "name      web\nreplicas  2\ndomain    -\n");
        assert_eq!(write(false), "name = web\nreplicas = 2\ndomain = -\n");
    }
}
