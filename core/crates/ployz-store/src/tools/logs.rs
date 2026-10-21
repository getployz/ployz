//! Container log text that is safe to show: scrubbed of the Sealed values its
//! reader knows and of secret-shaped text. Every other value, plain variables and
//! image env included, is shown as written.

use std::sync::LazyLock;

use ployz_core::scrub::{SealedForms, SealedValues};
use regex::Regex;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

const LINE_BYTES: usize = 500;
const TOTAL_BYTES: usize = 16 * 1024;
const CUT_MARKER: &str = " [cut]";

/// How many of the newest lines an excerpt keeps.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "u16", into = "u16")]
#[ts(type = "number")]
pub struct LineCap(u16);

impl TryFrom<u16> for LineCap {
    type Error = String;

    fn try_from(lines: u16) -> Result<Self, String> {
        Self::new(lines).ok_or_else(|| format!("a line count from 1 to {}", Self::MAX))
    }
}

impl From<LineCap> for u16 {
    fn from(cap: LineCap) -> Self {
        cap.0
    }
}

impl LineCap {
    pub const MAX: u16 = 200;

    /// `lines` when it is within 1 to [`Self::MAX`].
    #[must_use]
    pub fn new(lines: u16) -> Option<Self> {
        (1..=Self::MAX).contains(&lines).then_some(Self(lines))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

impl Default for LineCap {
    fn default() -> Self {
        Self(100)
    }
}

/// Log lines that went through [`LogExcerpt::scrub`]. Fields are private so nothing else
/// can build one from raw text.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct LogExcerpt {
    lines: Vec<String>,
    /// Whether a cap dropped any line or byte.
    cut: bool,
    /// How many Sealed values and secret-shaped values were replaced.
    redacted: u32,
}

/// Log text as it may be shown, or why it is not.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LogText {
    Shown(LogExcerpt),
    Withheld { why: Withheld },
}

/// Why log text is not shown.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Withheld {
    /// The Sealed values the lines may hold could not be learned, so they could not be
    /// scrubbed.
    NoSecretContext,
    /// The logs could not be read in time.
    Unanswered,
}

impl LogExcerpt {
    /// Scrub raw log `frames`, chunks of one stream in order, into text that can be shown.
    ///
    /// Order matters: frames are joined first so a value split across two is still matched;
    /// then each Sealed value's exact, base64 and url-encoded forms become `[secret NAME]`,
    /// longest first; then secret-shaped patterns become `[redacted]`; caps apply last, so a
    /// cut never exposes the prefix of a value it would otherwise have matched.
    #[must_use]
    pub fn scrub(
        frames: impl IntoIterator<Item = String>,
        sealed: &SealedValues,
        cap: LineCap,
    ) -> Self {
        let text: String = frames.into_iter().collect();
        let scrubbed = SealedForms::compile(sealed).scrub(&text);
        let mut redacted = scrubbed.redacted();
        let text = String::from_utf8(scrubbed.into_bytes())
            .expect("replacing whole UTF-8 forms with ASCII keeps text valid");
        let text = scrub_patterns(&text, &mut redacted);
        let (lines, cut) = capped(&text, cap);
        Self {
            lines,
            cut,
            redacted,
        }
    }

    #[must_use]
    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    #[must_use]
    pub const fn cut(&self) -> bool {
        self.cut
    }

    #[must_use]
    pub const fn redacted(&self) -> u32 {
        self.redacted
    }
}

/// Secret-shaped text, each with the replacement that keeps its harmless context.
static PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    [
        (
            r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
            "[redacted]",
        ),
        (
            r"\beyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+",
            "[redacted]",
        ),
        (r"\b(?:AKIA|ASIA)[A-Z0-9]{16}\b", "[redacted]"),
        (
            r"(?i)\b(bearer\s+)[A-Za-z0-9\-._~+/]+=*",
            "${1}[redacted]",
        ),
        (
            r#"(?i)([a-z0-9_-]*(?:password|passwd|token|secret)["']?\s*[=:]\s*)(?:"[^"]*"|'[^']*'|[^\s,;&\["'][^\s,;&]*)"#,
            "${1}[redacted]",
        ),
        (
            r"\b([A-Za-z][A-Za-z0-9+.-]*://)[^/\s:@]+:[^/\s@]+@",
            "${1}[redacted]@",
        ),
    ]
    .into_iter()
    .map(|(pattern, replacement)| {
        (
            Regex::new(pattern).expect("static pattern compiles"),
            replacement,
        )
    })
    .collect()
});

fn scrub_patterns(text: &str, redacted: &mut u32) -> String {
    let mut text = text.to_owned();
    for (pattern, replacement) in PATTERNS.iter() {
        let found = pattern.find_iter(&text).count();
        if found > 0 {
            *redacted += u32::try_from(found).unwrap_or(u32::MAX);
            text = pattern.replace_all(&text, *replacement).into_owned();
        }
    }
    text
}

/// The newest lines within `cap`, each at most [`LINE_BYTES`] and all at most
/// [`TOTAL_BYTES`], and whether anything was dropped.
fn capped(text: &str, cap: LineCap) -> (Vec<String>, bool) {
    let mut cut = false;
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| {
            if line.len() <= LINE_BYTES {
                return line.to_owned();
            }
            cut = true;
            let mut end = LINE_BYTES;
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            let (kept, _) = line.split_at(end);
            format!("{kept}{CUT_MARKER}")
        })
        .collect();
    let keep = usize::from(cap.get());
    if lines.len() > keep {
        cut = true;
        lines.drain(..lines.len() - keep);
    }
    let mut total: usize = lines.iter().map(String::len).sum();
    let mut drop = 0;
    for line in &lines {
        if total <= TOTAL_BYTES {
            break;
        }
        total -= line.len();
        drop += 1;
    }
    if drop > 0 {
        cut = true;
        lines.drain(..drop);
    }
    (lines, cut)
}

#[cfg(test)]
#[path = "logs_tests.rs"]
mod tests;
