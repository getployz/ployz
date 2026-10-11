//! Container log text that is safe to show: scrubbed of every secret the Store and the
//! Machine know, and withheld when they could not say which values are secret.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A value shorter than this is too likely to occur by chance, so it is scrubbed only
/// where its name labels it.
const SHORT_VALUE: usize = 6;
const LINE_BYTES: usize = 500;
const TOTAL_BYTES: usize = 16 * 1024;
const CUT_MARKER: &str = " [cut]";

/// Secret names and values for one Container. Built from the Store's view of its env, then
/// the Machine's real env. Never serialized or cloned, and its Debug prints names only.
pub struct SecretSet {
    secrets: BTreeMap<String, String>,
    plain: BTreeMap<String, String>,
    store: bool,
    machine: bool,
}

impl SecretSet {
    /// A set with no Store context: anything scrubbed with it is withheld.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            secrets: BTreeMap::new(),
            plain: BTreeMap::new(),
            store: false,
            machine: false,
        }
    }

    /// The Store's env for a Container: `secrets` are scrubbed wherever they appear, `plain`
    /// values stay readable when the Machine's env holds the same value under that name.
    #[must_use]
    pub fn from_store(
        secrets: impl IntoIterator<Item = (String, String)>,
        plain: impl IntoIterator<Item = (String, String)>,
    ) -> Self {
        Self {
            secrets: secrets.into_iter().collect(),
            plain: plain.into_iter().collect(),
            store: true,
            machine: false,
        }
    }

    /// Add the Container's env as its Machine reports it. `None` means it could not be read,
    /// which leaves the set incomplete. Every entry is a secret except one the Store knows as
    /// plain with the identical value.
    pub fn add_machine_env(&mut self, env: Option<&BTreeMap<String, String>>) {
        let Some(env) = env else {
            self.machine = false;
            return;
        };
        for (name, value) in env {
            if self.plain.get(name) != Some(value) {
                self.secrets
                    .entry(name.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        self.machine = true;
    }

    const fn complete(&self) -> bool {
        self.store && self.machine
    }
}

impl fmt::Debug for SecretSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecretSet")
            .field("secrets", &self.secrets.keys().collect::<Vec<_>>())
            .field("complete", &self.complete())
            .finish()
    }
}

/// How many of the newest lines an excerpt keeps.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineCap(u16);

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
    /// How many secrets and secret-shaped values were replaced.
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
    /// Which values are secret was not known, so nothing could be scrubbed reliably.
    NoSecretContext,
    /// The logs could not be read in time.
    Unanswered,
}

impl LogExcerpt {
    /// Scrub raw log `frames`, chunks of one stream in order, into text that can be shown.
    ///
    /// Order matters: frames are joined first so a value split across two is still matched;
    /// then each secret's exact, base64 and url-encoded forms become `[secret NAME]`, longest
    /// first; then secret-shaped patterns become `[redacted]`; caps apply last, so a cut never
    /// exposes the prefix of a value it would otherwise have matched. Without a complete
    /// `secrets` the text is withheld.
    pub fn scrub(
        frames: impl IntoIterator<Item = String>,
        secrets: &SecretSet,
        cap: LineCap,
    ) -> LogText {
        if !secrets.complete() {
            return LogText::Withheld {
                why: Withheld::NoSecretContext,
            };
        }
        let text: String = frames.into_iter().collect();
        let mut redacted = 0;
        let text = scrub_values(&text, secrets, &mut redacted);
        let text = scrub_patterns(&text, &mut redacted);
        let (lines, cut) = capped(&text, cap);
        LogText::Shown(Self {
            lines,
            cut,
            redacted,
        })
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

fn scrub_values(text: &str, secrets: &SecretSet, redacted: &mut u32) -> String {
    let mut forms: Vec<(String, &str)> = Vec::new();
    let mut short = Vec::new();
    for (name, value) in &secrets.secrets {
        if value.is_empty() {
            continue;
        }
        if value.len() < SHORT_VALUE {
            short.push((name.as_str(), value.as_str()));
            continue;
        }
        let encoded = [
            value.clone(),
            STANDARD.encode(value),
            STANDARD_NO_PAD.encode(value),
            percent_encoded(value, b"0123456789ABCDEF"),
            percent_encoded(value, b"0123456789abcdef"),
        ];
        for form in encoded {
            if !forms.iter().any(|(known, _)| *known == form) {
                forms.push((form, name.as_str()));
            }
        }
    }
    let mut text = text.to_owned();
    if !forms.is_empty() {
        forms.sort_by_key(|(form, _)| std::cmp::Reverse(form.len()));
        let pattern = forms
            .iter()
            .map(|(form, _)| regex::escape(form))
            .collect::<Vec<_>>()
            .join("|");
        let names: BTreeMap<&str, &str> = forms
            .iter()
            .map(|(form, name)| (form.as_str(), *name))
            .collect();
        let matcher = Regex::new(&pattern).expect("escaped literals always compile");
        text = matcher
            .replace_all(&text, |found: &Captures| {
                *redacted += 1;
                let form = found.get(0).map_or("", |m| m.as_str());
                format!("[secret {}]", names.get(form).copied().unwrap_or_default())
            })
            .into_owned();
    }
    for (name, value) in short {
        let labelled = Regex::new(&format!(
            r"\b({}\s*[=:]\s*){}",
            regex::escape(name),
            regex::escape(value)
        ))
        .expect("escaped literals always compile");
        text = labelled
            .replace_all(&text, |found: &Captures| {
                *redacted += 1;
                let label = found.get(1).map_or("", |m| m.as_str());
                format!("{label}[secret {name}]")
            })
            .into_owned();
    }
    text
}

fn percent_encoded(value: &str, hex: &[u8; 16]) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push('%');
            for nibble in [byte >> 4, byte & 0xf] {
                encoded.push(char::from(
                    hex.get(usize::from(nibble)).copied().unwrap_or(b'0'),
                ));
            }
        }
    }
    encoded
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
