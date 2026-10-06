//! Lists and records: aligned for a person at a terminal, tab- or `=`-separated
//! for a pipe.

use std::io;

use unicode_width::UnicodeWidthStr as _;

use super::Tone;

/// Space between aligned columns.
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
    header: Vec<&'static str>,
    rows: Vec<Vec<Cell>>,
    empty: String,
}

impl Table {
    /// `empty` is the sentence a person reads when there are no rows, such as
    /// `No Services in production yet.`
    #[must_use]
    pub fn new(header: impl IntoIterator<Item = &'static str>, empty: impl Into<String>) -> Self {
        Self {
            header: header.into_iter().collect(),
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
            .enumerate()
            .map(|(column, title)| pad(title, title.width(), widths[column], column == last))
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
    fields: Vec<(&'static str, String)>,
}

impl Fields {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn field(mut self, label: &'static str, value: impl std::fmt::Display) -> Self {
        self.push(label, value);
        self
    }

    pub fn push(&mut self, label: &'static str, value: impl std::fmt::Display) {
        self.fields.push((label, value.to_string()));
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
            if aligned {
                let padding = " ".repeat(width - label.width());
                writeln!(out, "{}{padding}  {value}", Tone::Muted.paint(label))?;
            } else {
                writeln!(out, "{label} = {value}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
