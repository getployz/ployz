//! The palette: colors chosen by meaning, never by a call site.

use std::fmt;

use anstyle::{AnsiColor, Style};

/// What a span of text means, which decides how it looks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// ✔, created, healthy.
    Good,
    /// ~, updated, warning.
    Change,
    /// ✘, removed, failed, `error:`.
    Bad,
    /// Connective words, separators, timings.
    Muted,
    /// Names you act on.
    Name,
    /// URLs.
    Link,
    /// Hint labels: `next:`, `inspect:`, `retry:`, `undo:`, `valid:`.
    Label,
}

impl Tone {
    #[must_use]
    pub const fn style(self) -> Style {
        match self {
            Self::Good => AnsiColor::Green.on_default(),
            Self::Change => AnsiColor::Yellow.on_default(),
            Self::Bad => AnsiColor::Red.on_default().bold(),
            Self::Muted => Style::new().dimmed(),
            Self::Name => Style::new().bold(),
            Self::Link => Style::new().underline(),
            Self::Label => AnsiColor::Cyan.on_default().bold(),
        }
    }

    /// `text` in this tone. The ANSI codes are stripped by the `anstream` writer
    /// when color is off, so painting is safe on any stream.
    #[must_use]
    pub fn paint<T: fmt::Display>(self, text: T) -> Painted<T> {
        Painted { tone: self, text }
    }
}

/// Text with a tone, printed with its style around it.
pub struct Painted<T> {
    tone: Tone,
    text: T,
}

impl<T: fmt::Display> fmt::Display for Painted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let style = self.tone.style();
        write!(f, "{style}{}{style:#}", self.text)
    }
}

/// clap's help and parse errors in the same palette.
#[must_use]
pub fn clap_styles() -> clap::builder::Styles {
    clap::builder::Styles::styled()
        .header(Tone::Name.style().underline())
        .usage(Tone::Name.style().underline())
        .literal(Tone::Name.style())
        .placeholder(Style::new())
        .error(Tone::Bad.style())
        .valid(Tone::Good.style())
        .invalid(Tone::Change.style())
        .context(Tone::Muted.style())
}
