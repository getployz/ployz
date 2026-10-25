//! Scrubbing Sealed values out of Container output, in one batch or frame by frame as a
//! stream arrives.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use regex::bytes;

/// A value shorter than this is too likely to occur by chance, so it is scrubbed only
/// where its name labels it.
pub const SHORT_VALUE: usize = 6;

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

/// The byte forms of a `SealedValues`, compiled once so every frame of a stream is matched
/// without rebuilding the regex.
///
/// A value of at least `SHORT_VALUE` bytes is matched exact, base64 with and without
/// padding, percent-encoded with upper and lower hex, and each of its log rows that long on
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
            let lines = rows(value)
                .filter(|row| row.len() >= SHORT_VALUE)
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

    /// Each form matched on its own, without a label.
    pub fn long_forms(&self) -> impl Iterator<Item = &[u8]> {
        self.names.keys().map(Vec::as_slice)
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

    /// Whether any form contains a newline, so a line end may fall inside a match.
    #[must_use]
    pub const fn multiline(&self) -> bool {
        self.multiline
    }

    /// [`Self::scrub`] for bytes that need not be UTF-8.
    #[must_use]
    pub fn scrub_bytes(&self, text: &[u8], redacted: &mut u32) -> Vec<u8> {
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

    /// The longest proper prefix of a long form that `text` ends with, and the form's name.
    fn prefix_tail(&self, text: &[u8]) -> Option<(usize, &str)> {
        let last = text.last()?;
        self.names
            .iter()
            .filter_map(|(form, name)| {
                let cut = (1..form.len()).rev().find(|&n| {
                    form.get(n - 1) == Some(last)
                        && form.get(..n).is_some_and(|prefix| text.ends_with(prefix))
                })?;
                Some((cut, name.as_str()))
            })
            .max_by_key(|(cut, _)| *cut)
    }

    /// Hide a trailing proper prefix of a form behind its marker, at the end of a stream.
    fn hide_prefix(&self, text: &mut Vec<u8>) {
        if let Some((cut, name)) = self.prefix_tail(text) {
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

/// Whether `value` spans more than one log row and one of them, not empty, is too short to
/// match on its own, so LogHistory would show that row unscrubbed.
#[must_use]
pub fn has_short_row(value: &str) -> bool {
    rows(value).nth(1).is_some() && rows(value).any(|row| (1..SHORT_VALUE).contains(&row.len()))
}

/// The rows LogHistory shows `value` on. Docker's log copier ends a line at each `\n`,
/// drops the `\n` and keeps a `\r`, so a trailing `\n` starts no row.
fn rows(value: &str) -> std::str::SplitTerminator<'_, char> {
    value.split_terminator('\n')
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
/// start of a form until the next frame or the end of the stream shows what it was. A line
/// end releases the line unless it falls inside a form that spans lines.
///
/// Each channel has its own carry, since a value never continues from stdout to stderr.
/// The carry holds raw bytes, so a frame may split a UTF-8 sequence or a base64 form and
/// the match still lands. A channel whose frame was hidden whole veils what it shows next.
#[derive(Default)]
pub struct FrameScrub {
    stdout: Carry,
    stderr: Carry,
}

#[derive(Default)]
struct Carry {
    held: Vec<u8>,
    veil: Veil,
}

/// What a channel still hides after a frame hidden whole. A form begun in that frame may end
/// in the next shown one, and the scrubber never saw the part it hid, so the first
/// `longest - 1` bytes shown next are hidden too, then any form straddling that point and
/// the rest of the line it ends in. The veil hides released bytes; it never holds any.
#[derive(Default, Clone, Copy)]
enum Veil {
    #[default]
    Off,
    /// A frame was hidden; the forms at the next shown frame say how much to hide.
    Pending,
    /// Bytes still to hide, then the rest of the line they end in.
    Bytes(usize),
}

impl Veil {
    /// Where shown bytes begin in `text`, whose first `at` bytes are released now.
    fn shown_from(&mut self, text: &[u8], at: usize, forms: &SealedForms) -> usize {
        let bytes = match *self {
            Self::Off => return 0,
            Self::Pending => forms.longest().saturating_sub(1),
            Self::Bytes(bytes) => bytes,
        };
        match veil_point(text, bytes, &forms.spans(text)) {
            Ok(end) if end <= at => {
                *self = Self::Off;
                end
            }
            Ok(end) => {
                *self = Self::Bytes(end - at);
                at
            }
            Err(left) => {
                *self = Self::Bytes(text.len() - at + left);
                at
            }
        }
    }
}

/// Prints how much each channel holds, never the held bytes.
impl fmt::Debug for FrameScrub {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameScrub")
            .field("stdout", &self.stdout.held.len())
            .field("stderr", &self.stderr.held.len())
            .finish()
    }
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
    /// The last `longest - 1` bytes stay held unless the text ends a line. At a line end only
    /// a trailing proper prefix of a form that spans lines stays held; a label whose value has
    /// not arrived keeps its text held too. Held bytes never pass `CARRY_CAP` beyond that: the
    /// oldest are scrubbed and released. A released chunk never ends inside a form or a UTF-8
    /// sequence the input had whole. After `discard`, the veil hides the start of what is
    /// released.
    pub fn push(&mut self, channel: Channel, bytes: &[u8], forms: &SealedForms) -> Vec<u8> {
        let carry = self.carry(channel);
        carry.held.extend_from_slice(bytes);
        let text = std::mem::take(&mut carry.held);
        let at = release_point(&text, forms);
        let from = carry.veil.shown_from(&text, at, forms);
        let (release, held) = text.split_at(at);
        carry.held = held.to_vec();
        let mut redacted = 0;
        forms.scrub_bytes(release.split_at(from).1, &mut redacted)
    }

    /// Drop what `channel` holds and veil what it shows next, for a frame hidden whole: a form
    /// begun there is hidden with it, wherever it ends.
    pub fn discard(&mut self, channel: Channel) {
        let carry = self.carry(channel);
        carry.held.clear();
        carry.veil = Veil::Pending;
    }

    /// End the stream: scrub what is held and hide any trailing prefix of a form.
    #[must_use]
    pub fn finish(self, forms: &SealedForms) -> Vec<(Channel, Vec<u8>)> {
        [
            (Channel::Stdout, self.stdout),
            (Channel::Stderr, self.stderr),
        ]
        .into_iter()
        .filter(|(_, carry)| !carry.held.is_empty())
        .map(|(channel, mut carry)| {
            let from = carry.veil.shown_from(&carry.held, carry.held.len(), forms);
            let mut redacted = 0;
            let mut out = forms.scrub_bytes(carry.held.split_at(from).1, &mut redacted);
            forms.hide_prefix(&mut out);
            (channel, out)
        })
        .collect()
    }

    fn carry(&mut self, channel: Channel) -> &mut Carry {
        match channel {
            Channel::Stdout => &mut self.stdout,
            Channel::Stderr => &mut self.stderr,
        }
    }
}

/// Where a veil of `bytes` ends in `text`: past those bytes, then past any form straddling
/// the point, since it began in veiled bytes, and the rest of the line, until a line end
/// outside every form. `Err` when `text` ends first, with the bytes still to hide after it.
fn veil_point(text: &[u8], bytes: usize, spans: &[(usize, usize)]) -> Result<usize, usize> {
    let mut at = bytes;
    loop {
        if at > text.len() {
            return Err(at - text.len());
        }
        if let Some(&(_, end)) = spans.iter().find(|&&(start, end)| start < at && at < end) {
            at = end;
            continue;
        }
        let (hidden, rest) = text.split_at(at);
        if hidden.ends_with(b"\n") {
            return Ok(at);
        }
        match rest.iter().position(|&byte| byte == b'\n') {
            Some(newline) => at += newline + 1,
            None => return Err(0),
        }
    }
}

/// Where `text` splits into bytes to release now and bytes to hold.
fn release_point(text: &[u8], forms: &SealedForms) -> usize {
    let len = text.len();
    let hold = forms.longest().saturating_sub(1);
    let mut at = if text.ends_with(b"\n") {
        // Only a form that spans lines can go on past a line end.
        let open = forms.multiline().then(|| forms.prefix_tail(text)).flatten();
        len - open.map_or(0, |(cut, _)| cut)
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
#[must_use]
pub fn incomplete_tail(text: &[u8]) -> usize {
    let lead = text
        .iter()
        .rev()
        .take(3)
        .enumerate()
        .find(|(_, byte)| *byte & 0xC0 != 0x80);
    let Some((back, byte)) = lead else {
        return 0;
    };
    let need = match byte {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => return 0,
    };
    if need > back + 1 { back + 1 } else { 0 }
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

#[cfg(test)]
#[path = "scrub_tests.rs"]
mod tests;
