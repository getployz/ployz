//! Container log text that is safe to show: scrubbed of the Sealed values its
//! reader knows and of secret-shaped text. Every other value, plain variables and
//! image env included, is shown as written.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use regex::{Regex, bytes};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// A value shorter than this is too likely to occur by chance, so it is scrubbed only
/// where its name labels it.
const SHORT_VALUE: usize = 6;
const LINE_BYTES: usize = 500;
const TOTAL_BYTES: usize = 16 * 1024;
const CUT_MARKER: &str = " [cut]";

/// Sealed variable values, by name, for the Containers whose logs are scrubbed together.
/// Never serialized or cloned, and its Debug prints names only.
pub struct SealedValues(BTreeSet<(String, String)>);

impl SealedValues {
    /// No Sealed values: only secret-shaped text is scrubbed.
    #[must_use]
    pub const fn none() -> Self {
        Self(BTreeSet::new())
    }
}

impl FromIterator<(String, String)> for SealedValues {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(values: I) -> Self {
        Self(values.into_iter().collect())
    }
}

impl Extend<(String, String)> for SealedValues {
    fn extend<I: IntoIterator<Item = (String, String)>>(&mut self, values: I) {
        self.0.extend(values);
    }
}

impl fmt::Debug for SealedValues {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list()
            .entries(self.0.iter().map(|(name, _)| name))
            .finish()
    }
}

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
        let mut redacted = 0;
        let text = SealedForms::compile(sealed).scrub(&text, &mut redacted);
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

/// The byte forms of a `SealedValues`, compiled once so every frame of a stream is matched
/// without rebuilding the regex.
///
/// A value of at least `SHORT_VALUE` bytes is matched exact, base64 with and without
/// padding, percent-encoded with upper and lower hex, and each of its lines that long on
/// its own. A shorter value is matched only where its name labels it, `NAME = value`.
pub struct SealedForms {
    /// Every long form in one alternation, longest first, so a value containing a shorter
    /// one is replaced whole.
    long: Option<bytes::Regex>,
    names: BTreeMap<Vec<u8>, String>,
    labelled: Vec<Labelled>,
    longest: usize,
    multiline: bool,
}

impl SealedForms {
    #[must_use]
    pub fn compile(sealed: &SealedValues) -> Self {
        let mut names: BTreeMap<Vec<u8>, String> = BTreeMap::new();
        let mut labelled = Vec::new();
        for (name, value) in &sealed.0 {
            if value.is_empty() {
                continue;
            }
            if value.len() < SHORT_VALUE {
                labelled.push(Labelled::new(name, value));
                continue;
            }
            let lines = value
                .lines()
                .filter(|line| line.len() >= SHORT_VALUE)
                .map(str::to_owned);
            let encoded = [
                value.clone(),
                STANDARD.encode(value),
                STANDARD_NO_PAD.encode(value),
                percent_encoded(value, b"0123456789ABCDEF"),
                percent_encoded(value, b"0123456789abcdef"),
            ];
            for form in encoded.into_iter().chain(lines) {
                names
                    .entry(form.into_bytes())
                    .or_insert_with(|| name.clone());
            }
        }
        let mut forms: Vec<&[u8]> = names.keys().map(Vec::as_slice).collect();
        forms.sort_by_key(|form| std::cmp::Reverse(form.len()));
        let long = (!forms.is_empty()).then(|| {
            let pattern = forms
                .iter()
                .map(|form| regex::escape(&String::from_utf8_lossy(form)))
                .collect::<Vec<_>>()
                .join("|");
            bytes::Regex::new(&pattern).expect("escaped literals always compile")
        });
        let longest = forms
            .iter()
            .map(|form| form.len())
            .chain(labelled.iter().map(|short| short.longest))
            .max()
            .unwrap_or_default();
        let multiline = forms.iter().any(|form| form.contains(&b'\n'));
        Self {
            long,
            names,
            labelled,
            longest,
            multiline,
        }
    }

    #[must_use]
    pub fn none() -> Self {
        Self::compile(&SealedValues::none())
    }

    /// Replace every form in `text` with `[secret NAME]`, counting each into `redacted`.
    #[must_use]
    pub fn scrub(&self, text: &str, redacted: &mut u32) -> String {
        String::from_utf8(self.scrub_bytes(text.as_bytes(), redacted))
            .expect("replacing whole UTF-8 forms with ASCII keeps text valid")
    }

    /// Bytes of the longest form, a labelled one counted with no whitespace.
    #[must_use]
    pub const fn longest(&self) -> usize {
        self.longest
    }

    /// Whether any form contains a newline, so a line end cannot close a match.
    #[must_use]
    pub const fn multiline(&self) -> bool {
        self.multiline
    }

    fn scrub_bytes(&self, text: &[u8], redacted: &mut u32) -> Vec<u8> {
        let mut text = match &self.long {
            Some(long) => long
                .replace_all(text, |found: &bytes::Captures| {
                    *redacted += 1;
                    let form = found.get(0).map_or(&b""[..], |m| m.as_bytes());
                    marker(self.names.get(form).map_or("", String::as_str))
                })
                .into_owned(),
            None => text.to_vec(),
        };
        for short in &self.labelled {
            text = short
                .whole
                .replace_all(&text, |found: &bytes::Captures| {
                    *redacted += 1;
                    let mut out = found.get(1).map_or(&b""[..], |m| m.as_bytes()).to_vec();
                    out.extend(marker(&short.name));
                    out
                })
                .into_owned();
        }
        text
    }

    /// Byte spans of every form in `text`, as the batch scrub would match them.
    fn spans(&self, text: &[u8]) -> Vec<(usize, usize)> {
        let long = self.long.iter().flat_map(|long| long.find_iter(text));
        let short = self
            .labelled
            .iter()
            .flat_map(|short| short.whole.find_iter(text));
        long.chain(short).map(|m| (m.start(), m.end())).collect()
    }

    /// Where a labelled form has begun at the end of `text` but its value has not arrived.
    fn pending_start(&self, text: &[u8]) -> Option<usize> {
        self.labelled
            .iter()
            .filter_map(|short| short.pending.find(text))
            .map(|m| m.start())
            .min()
    }

    /// Hide a trailing proper prefix of a form behind its marker, at the end of a stream.
    fn hide_prefix(&self, text: &mut Vec<u8>) {
        let hidden = self
            .names
            .iter()
            .filter_map(|(form, name)| {
                let cut = (1..form.len())
                    .rev()
                    .find(|&n| form.get(..n).is_some_and(|prefix| text.ends_with(prefix)))?;
                Some((cut, name))
            })
            .max_by_key(|(cut, _)| *cut);
        if let Some((cut, name)) = hidden {
            text.truncate(text.len() - cut);
            text.extend(marker(name));
            return;
        }
        for short in &self.labelled {
            let Some(found) = short.pending.captures(text).and_then(|c| c.get(1)) else {
                continue;
            };
            text.truncate(found.start());
            text.extend(marker(&short.name));
            return;
        }
    }
}

impl fmt::Debug for SealedForms {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: BTreeSet<&str> = self
            .names
            .values()
            .map(String::as_str)
            .chain(self.labelled.iter().map(|short| short.name.as_str()))
            .collect();
        f.debug_tuple("SealedForms").field(&names).finish()
    }
}

fn marker(name: &str) -> Vec<u8> {
    format!("[secret {name}]").into_bytes()
}

/// A value too short to match on its own, matched only as `NAME = value`.
struct Labelled {
    name: String,
    /// The labelled form; the label is kept and the value replaced.
    whole: bytes::Regex,
    /// Text ending inside the label, its separator, or group 1, a proper prefix of the value.
    pending: bytes::Regex,
    longest: usize,
}

impl Labelled {
    fn new(name: &str, value: &str) -> Self {
        let label = regex::escape(name);
        let whole = bytes::Regex::new(&format!(r"\b({label}\s*[=:]\s*){}", regex::escape(value)))
            .expect("escaped literals always compile");
        let prefixes = (1..value.len())
            .rev()
            .filter_map(|n| value.get(..n))
            .map(regex::escape)
            .collect::<Vec<_>>()
            .join("|");
        let prefix = if prefixes.is_empty() {
            String::new()
        } else {
            format!("({prefixes})?")
        };
        let pending = bytes::Regex::new(&format!(r"\b{label}\s*(?:[=:]\s*{prefix})?$"))
            .expect("escaped literals always compile");
        Self {
            name: name.to_owned(),
            whole,
            pending,
            longest: name.len() + 1 + value.len(),
        }
    }
}

/// One stream of a Container's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Channel {
    Stdout,
    Stderr,
}

/// Scrubs one reader's frames as they arrive, holding back the tail that could be the
/// start of a form until the next frame or the end of the stream shows what it was.
///
/// Each channel has its own carry, since a value never continues from stdout to stderr.
/// The carry holds raw bytes, so a frame may split a UTF-8 sequence or a base64 form and
/// the match still lands.
#[derive(Debug, Default)]
pub struct FrameScrub {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Held bytes stay under `max(longest - 1, CARRY_CAP)`. A labelled short value whose
/// label and value sit more than this far apart is not scrubbed by the stream.
const CARRY_CAP: usize = 4096;

impl FrameScrub {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `bytes` to `channel` and return what can be shown now.
    ///
    /// The last `longest - 1` bytes stay held unless the text ends a line and no form spans
    /// lines; a label whose value has not arrived keeps its text held too. Held bytes never
    /// pass `CARRY_CAP` beyond that: the oldest are scrubbed and released. A released chunk
    /// never ends inside a form or a UTF-8 sequence the input had whole.
    pub fn push(&mut self, channel: Channel, bytes: &[u8], forms: &SealedForms) -> Vec<u8> {
        let carry = self.carry(channel);
        carry.extend_from_slice(bytes);
        let text = std::mem::take(carry);
        let at = release_point(&text, forms);
        let (release, held) = text.split_at(at);
        let mut redacted = 0;
        let out = forms.scrub_bytes(release, &mut redacted);
        *self.carry(channel) = held.to_vec();
        out
    }

    /// End the stream: scrub what is held and hide any trailing prefix of a form.
    #[must_use]
    pub fn finish(self, forms: &SealedForms) -> Vec<(Channel, Vec<u8>)> {
        [
            (Channel::Stdout, self.stdout),
            (Channel::Stderr, self.stderr),
        ]
        .into_iter()
        .filter(|(_, held)| !held.is_empty())
        .map(|(channel, held)| {
            let mut redacted = 0;
            let mut out = forms.scrub_bytes(&held, &mut redacted);
            forms.hide_prefix(&mut out);
            (channel, out)
        })
        .collect()
    }

    fn carry(&mut self, channel: Channel) -> &mut Vec<u8> {
        match channel {
            Channel::Stdout => &mut self.stdout,
            Channel::Stderr => &mut self.stderr,
        }
    }
}

/// Where `text` splits into bytes to release now and bytes to hold.
fn release_point(text: &[u8], forms: &SealedForms) -> usize {
    let len = text.len();
    let hold = forms.longest().saturating_sub(1);
    let mut at = if text.ends_with(b"\n") && !forms.multiline() {
        len
    } else {
        len.saturating_sub(hold)
    };
    if let Some(start) = forms.pending_start(text) {
        at = at.min(start);
    }
    at = at.max(len.saturating_sub(hold.max(CARRY_CAP)));
    // A form that straddles the point is complete, so release it whole: nothing the batch
    // scrub would match can start inside it.
    let spans = forms.spans(text);
    while let Some(&(_, end)) = spans.iter().find(|&&(start, end)| start < at && at < end) {
        at = end;
    }
    while text.get(at).is_some_and(|byte| byte & 0xC0 == 0x80) {
        at += 1;
    }
    at.min(len - incomplete_tail(text))
}

/// Bytes at the end of `text` that begin a UTF-8 sequence the text does not finish.
fn incomplete_tail(text: &[u8]) -> usize {
    let tail = text.len().min(3);
    let Some(lead) = (1..=tail).find(|&back| {
        text.get(text.len() - back)
            .is_some_and(|byte| byte & 0xC0 != 0x80)
    }) else {
        return 0;
    };
    let byte = text.get(text.len() - lead).copied().unwrap_or_default();
    let need = match byte {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => return 0,
    };
    if need > lead { lead } else { 0 }
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
