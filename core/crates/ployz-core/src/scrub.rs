//! Sealed values and the forms they take in log text, and the scrubber that replaces them
//! in a stream of frames before the text is shown. Shared by every reader and writer of
//! Container logs, so a value is scrubbed the same way wherever it would appear.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::ops::Range;

use aho_corasick::{AhoCorasick, MatchKind};
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};

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
/// without rebuilding the matcher.
///
/// A value of at least `SHORT_VALUE` bytes is matched exact, base64 with and without
/// padding, percent-encoded with upper and lower hex, and each of its lines that long on
/// its own. A shorter value is matched only where its name labels it, `NAME = value`.
pub struct SealedForms {
    forms: Vec<Form>,
    /// One pattern per form, in `forms` order, so a match's pattern id indexes it.
    matcher: AhoCorasick,
    longest: usize,
    multiline: bool,
}

struct Form {
    bytes: Vec<u8>,
    name: String,
    kind: FormKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FormKind {
    Exact,
    Base64,
    Base64NoPad,
    PercentUpper,
    PercentLower,
    Line,
    /// A short value's bytes, a match only when `NAME = ` precedes them.
    Labelled,
}

/// One stretch of `text` to replace: `start..end`, of which the first `keep` bytes are a
/// label shown as written. Overlapping forms are unioned into one span.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Span<'f> {
    start: usize,
    end: usize,
    keep: usize,
    name: &'f str,
    /// Bytes of the longest form in the union, whose name the marker takes.
    form_len: usize,
}

impl<'f> Span<'f> {
    fn overlaps(&self, other: &Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    fn union(self, other: Self) -> Self {
        let (first, second) = if self.start <= other.start {
            (self, other)
        } else {
            (other, self)
        };
        let (name, form_len) = if other.form_len > self.form_len {
            (other.name, other.form_len)
        } else {
            (self.name, self.form_len)
        };
        Self {
            start: first.start,
            end: self.end.max(other.end),
            keep: first.keep.min(second.start - first.start),
            name,
            form_len,
        }
    }

    fn straddles(&self, at: usize) -> bool {
        self.start < at && at < self.end
    }
}

impl SealedForms {
    #[must_use]
    pub fn compile(sealed: &SealedValues) -> Self {
        let mut long: BTreeMap<Vec<u8>, (String, FormKind)> = BTreeMap::new();
        let mut labelled = Vec::new();
        for (name, value) in &sealed.0 {
            if value.is_empty() {
                continue;
            }
            if value.len() < SHORT_VALUE {
                labelled.push(Form {
                    bytes: value.clone().into_bytes(),
                    name: name.clone(),
                    kind: FormKind::Labelled,
                });
                continue;
            }
            let lines = value
                .lines()
                .filter(|line| line.len() >= SHORT_VALUE)
                .map(|line| (FormKind::Line, line.to_owned()));
            let encoded = [
                (FormKind::Exact, value.clone()),
                (FormKind::Base64, STANDARD.encode(value)),
                (FormKind::Base64NoPad, STANDARD_NO_PAD.encode(value)),
                (
                    FormKind::PercentUpper,
                    percent_encoded(value, b"0123456789ABCDEF"),
                ),
                (
                    FormKind::PercentLower,
                    percent_encoded(value, b"0123456789abcdef"),
                ),
            ];
            for (kind, form) in encoded.into_iter().chain(lines) {
                long.entry(form.into_bytes())
                    .or_insert_with(|| (name.clone(), kind));
            }
        }
        let longest = long
            .keys()
            .map(Vec::len)
            .chain(labelled.iter().map(Form::labelled_len))
            .max()
            .unwrap_or_default();
        let multiline = long.keys().any(|form| form.contains(&b'\n'));
        let forms: Vec<Form> = long
            .into_iter()
            .map(|(bytes, (name, kind))| Form { bytes, name, kind })
            .chain(labelled)
            .collect();
        let matcher = AhoCorasick::builder()
            .match_kind(MatchKind::Standard)
            .build(forms.iter().map(|form| &form.bytes))
            .expect("a few hundred short literals fit any automaton");
        Self {
            forms,
            matcher,
            longest,
            multiline,
        }
    }

    #[must_use]
    pub fn none() -> Self {
        Self::compile(&SealedValues::none())
    }

    /// Replace every form in `text` with `[secret NAME]`.
    #[must_use]
    pub fn scrub(&self, text: &str) -> Scrubbed {
        let bytes = text.as_bytes();
        let spans = self.find(bytes);
        Scrubbed::new(bytes, 0..bytes.len(), &spans)
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

    /// Every form in `text` in order, overlapping ones unioned so no byte of a form that
    /// only partly overlaps another survives. A short value counts only with its label.
    fn find(&self, text: &[u8]) -> Vec<Span<'_>> {
        let mut spans: Vec<Span<'_>> = Vec::new();
        for found in self.matcher.find_overlapping_iter(text) {
            let form = self
                .forms
                .get(found.pattern().as_usize())
                .expect("pattern ids index forms");
            let start = match form.kind {
                FormKind::Labelled => match label_before(text, found.start(), &form.name) {
                    Some(start) => start,
                    None => continue,
                },
                FormKind::Exact
                | FormKind::Base64
                | FormKind::Base64NoPad
                | FormKind::PercentUpper
                | FormKind::PercentLower
                | FormKind::Line => found.start(),
            };
            let mut span = Span {
                start,
                end: found.end(),
                keep: found.start() - start,
                name: &form.name,
                form_len: form.bytes.len(),
            };
            while let Some(last) = spans.pop_if(|last| last.overlaps(&span)) {
                span = last.union(span);
            }
            spans.push(span);
        }
        spans
    }

    fn labelled(&self) -> impl Iterator<Item = &Form> {
        self.forms
            .iter()
            .filter(|form| form.kind == FormKind::Labelled)
    }

    fn long(&self) -> impl Iterator<Item = &Form> {
        self.forms
            .iter()
            .filter(|form| form.kind != FormKind::Labelled)
    }

    /// Where a labelled form has begun at the end of `text` but its value has not arrived.
    fn pending_start(&self, text: &[u8]) -> Option<usize> {
        self.labelled()
            .filter_map(|form| form.pending_start(text))
            .min()
    }

    /// The longest proper prefix of a long form that `text` ends with, and the form's name.
    fn prefix_tail(&self, text: &[u8]) -> Option<(usize, &str)> {
        let last = text.last()?;
        self.long()
            .filter_map(|form| {
                let cut = (1..form.bytes.len()).rev().find(|&n| {
                    form.bytes.get(n - 1) == Some(last)
                        && form
                            .bytes
                            .get(..n)
                            .is_some_and(|prefix| text.ends_with(prefix))
                })?;
                Some((cut, form.name.as_str()))
            })
            .max_by_key(|(cut, _)| *cut)
    }

    /// Hide a trailing proper prefix of a form behind its marker, at the end of a stream.
    fn hide_prefix(&self, shown: &mut Scrubbed) {
        if let Some((cut, name)) = self.prefix_tail(&shown.bytes) {
            shown.hide_tail(cut, name);
            return;
        }
        for form in self.labelled() {
            let Some(cut) = form.pending_prefix(&shown.bytes) else {
                continue;
            };
            shown.hide_tail(cut, &form.name);
            return;
        }
    }
}

impl fmt::Debug for SealedForms {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let names: BTreeSet<&str> = self.forms.iter().map(|form| form.name.as_str()).collect();
        f.debug_tuple("SealedForms").field(&names).finish()
    }
}

impl Form {
    /// `NAME=value`, the shortest text a labelled form takes.
    fn labelled_len(&self) -> usize {
        self.name.len() + 1 + self.bytes.len()
    }

    /// Where `NAME \s* ([=:] \s* PREFIX?)?` ends `text`, PREFIX a proper prefix of the value.
    fn pending_start(&self, text: &[u8]) -> Option<usize> {
        let bare = skip_space_back(text, text.len());
        let after_separator = separator_before(text, bare).map(|at| skip_space_back(text, at));
        let after_prefix = (1..self.bytes.len())
            .filter(|&n| {
                self.bytes
                    .get(..n)
                    .is_some_and(|prefix| text.ends_with(prefix))
            })
            .filter_map(|n| {
                let at = skip_space_back(text, text.len() - n);
                separator_before(text, at).map(|at| skip_space_back(text, at))
            });
        [Some(bare), after_separator]
            .into_iter()
            .flatten()
            .chain(after_prefix)
            .filter_map(|end| label_ending_at(text, &self.name, end))
            .min()
    }

    /// The longest proper prefix of the value that ends `text` with its label before it.
    fn pending_prefix(&self, text: &[u8]) -> Option<usize> {
        (1..self.bytes.len()).rev().find(|&n| {
            self.bytes
                .get(..n)
                .is_some_and(|prefix| text.ends_with(prefix))
                && label_before(text, text.len() - n, &self.name).is_some()
        })
    }
}

/// Where `\b NAME \s* [=:] \s*` starts when it ends at `at`.
fn label_before(text: &[u8], at: usize, name: &str) -> Option<usize> {
    let at = skip_space_back(text, at);
    let at = separator_before(text, at)?;
    label_ending_at(text, name, skip_space_back(text, at))
}

fn label_ending_at(text: &[u8], name: &str, end: usize) -> Option<usize> {
    let start = end.checked_sub(name.len())?;
    let found = text.get(start..end)? == name.as_bytes();
    (found && word_boundary(text, start)).then_some(start)
}

fn skip_space_back(text: &[u8], mut at: usize) -> usize {
    while at > 0 && text.get(at - 1).is_some_and(|byte| is_space(*byte)) {
        at -= 1;
    }
    at
}

fn separator_before(text: &[u8], at: usize) -> Option<usize> {
    let at = at.checked_sub(1)?;
    matches!(text.get(at), Some(b'=' | b':')).then_some(at)
}

fn word_boundary(text: &[u8], at: usize) -> bool {
    let before = at
        .checked_sub(1)
        .and_then(|at| text.get(at))
        .is_some_and(|byte| is_word(*byte));
    let after = text.get(at).is_some_and(|byte| is_word(*byte));
    before != after
}

const fn is_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

const fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn marker(name: &str) -> Vec<u8> {
    format!("[secret {name}]").into_bytes()
}

/// Log text with every Sealed form replaced by its marker. Built only by this module, so a
/// reader or writer that takes one knows the bytes went through the scrubber.
///
/// Nothing outside the module can build one from raw bytes:
///
/// ```compile_fail
/// let _ = ployz_core::scrub::Scrubbed {
///     bytes: Vec::new(),
///     names: std::collections::BTreeSet::new(),
///     redacted: 0,
/// };
/// ```
#[derive(Debug)]
pub struct Scrubbed {
    bytes: Vec<u8>,
    names: BTreeSet<String>,
    redacted: u32,
}

impl Scrubbed {
    /// `text[range]` with every span inside it replaced by its marker. A span never
    /// straddles either end of `range`.
    fn new(text: &[u8], range: Range<usize>, spans: &[Span<'_>]) -> Self {
        let mut shown = Self {
            bytes: Vec::with_capacity(range.len()),
            names: BTreeSet::new(),
            redacted: 0,
        };
        let mut cursor = range.start;
        for span in spans
            .iter()
            .filter(|span| range.start <= span.start && span.end <= range.end)
        {
            shown.bytes.extend(
                text.get(cursor..span.start + span.keep)
                    .expect("spans lie within the text, in order"),
            );
            shown.replace(span.name);
            cursor = span.end;
        }
        shown.bytes.extend(
            text.get(cursor..range.end)
                .expect("the range lies within the text"),
        );
        shown
    }

    fn replace(&mut self, name: &str) {
        self.bytes.extend(marker(name));
        self.names.insert(name.to_owned());
        self.redacted += 1;
    }

    /// Replace the last `cut` bytes, a proper prefix of one of `name`'s forms.
    fn hide_tail(&mut self, cut: usize, name: &str) {
        self.bytes.truncate(self.bytes.len() - cut);
        self.replace(name);
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// The Sealed names whose forms were replaced in this batch.
    #[must_use]
    pub const fn names(&self) -> &BTreeSet<String> {
        &self.names
    }

    /// How many forms were replaced in this batch.
    #[must_use]
    pub const fn redacted(&self) -> u32 {
        self.redacted
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
    fn shown_from(&mut self, text: &[u8], at: usize, longest: usize, spans: &[Span<'_>]) -> usize {
        let bytes = match *self {
            Self::Off => return 0,
            Self::Pending => longest.saturating_sub(1),
            Self::Bytes(bytes) => bytes,
        };
        match veil_point(text, bytes, spans) {
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
    pub fn push(&mut self, channel: Channel, bytes: &[u8], forms: &SealedForms) -> Scrubbed {
        let carry = self.carry(channel);
        carry.held.extend_from_slice(bytes);
        let text = std::mem::take(&mut carry.held);
        let spans = forms.find(&text);
        let at = release_point(&text, forms, &spans);
        let from = carry.veil.shown_from(&text, at, forms.longest(), &spans);
        carry.held = text.get(at..).unwrap_or_default().to_vec();
        Scrubbed::new(&text, from..at, &spans)
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
    pub fn finish(self, forms: &SealedForms) -> Vec<(Channel, Scrubbed)> {
        [
            (Channel::Stdout, self.stdout),
            (Channel::Stderr, self.stderr),
        ]
        .into_iter()
        .filter(|(_, carry)| !carry.held.is_empty())
        .map(|(channel, mut carry)| {
            let spans = forms.find(&carry.held);
            let len = carry.held.len();
            let from = carry
                .veil
                .shown_from(&carry.held, len, forms.longest(), &spans);
            let mut shown = Scrubbed::new(&carry.held, from..len, &spans);
            forms.hide_prefix(&mut shown);
            (channel, shown)
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
fn veil_point(text: &[u8], bytes: usize, spans: &[Span<'_>]) -> Result<usize, usize> {
    let mut at = bytes;
    loop {
        if at > text.len() {
            return Err(at - text.len());
        }
        if let Some(span) = spans.iter().find(|span| span.straddles(at)) {
            at = span.end;
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
fn release_point(text: &[u8], forms: &SealedForms, spans: &[Span<'_>]) -> usize {
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
    while let Some(span) = spans.iter().find(|span| span.straddles(at)) {
        at = span.end;
    }
    while text.get(at).is_some_and(|byte| byte & 0xC0 == 0x80) {
        at += 1;
    }
    at.min(len - incomplete_tail(text))
}

/// Bytes at the end of `text` that begin a UTF-8 sequence the text does not finish.
fn incomplete_tail(text: &[u8]) -> usize {
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
