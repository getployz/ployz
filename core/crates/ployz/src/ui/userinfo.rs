//! URL user info, masked in everything Ployz prints.
//!
//! Text splits into tokens at ASCII spaces, tabs and line breaks. In a token, user info
//! may run from where its first URL's authority starts to the token's last `@`, and that
//! span prints as `<USER>:<PASSWORD>`, or `<USER>` when it holds no `:`. A URL starts at a
//! `:` that ends a run of scheme characters holding a letter, when a `/` or `\` follows it
//! or the run ends in a special scheme (`http`, `https`, `ws`, `wss`, `ftp`, `file`, any
//! case), which takes user info after any number of slashes, even none. Its authority
//! starts past the `:` and the `/`s after it.
//!
//! The span holds every byte the `url` crate reads as user info in any part of the token,
//! and all a looser reader could take for it, so a password holding `/`, `?`, `#`, `@`, a
//! quote or a bad port is masked whole. A password holding a space or a tab still splits
//! into two tokens (#4592). The span masks more when an `@` follows the authority, as in
//! `https://host/path?email=a@b`, which prints `https://<USER>@b`.

use std::{borrow::Cow, ops::Range};

use serde_json::{Map, Value};

const USER: &str = "<USER>";
const USER_AND_PASSWORD: &str = "<USER>:<PASSWORD>";

/// Schemes whose URLs take user info whether or not slashes follow the `:`.
const SPECIAL: [&[u8]; 6] = [b"http", b"https", b"ws", b"wss", b"ftp", b"file"];

/// Whether `byte` ends a token. A stream holds back the bytes after the last one, since
/// they may still become a URL with user info.
pub(crate) const fn ends_token(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | b'\r')
}

/// `text` with the user info of every URL in it masked.
pub(crate) fn mask(text: &str) -> Cow<'_, str> {
    let mut spans = spans(text.as_bytes()).peekable();
    if spans.peek().is_none() {
        return Cow::Borrowed(text);
    }
    let mut masked = String::with_capacity(text.len());
    let mut copied = 0;
    // Each span starts right after an ASCII `:` or `/` and ends at an `@`, so it cuts
    // at character boundaries.
    for (span, placeholder) in spans {
        masked.push_str(text.get(copied..span.start).unwrap_or_default());
        masked.push_str(placeholder);
        copied = span.end;
    }
    masked.push_str(text.get(copied..).unwrap_or_default());
    Cow::Owned(masked)
}

/// [`mask`] over bytes, which need not be UTF-8.
pub(crate) fn mask_bytes(text: &[u8]) -> Cow<'_, [u8]> {
    let mut spans = spans(text).peekable();
    if spans.peek().is_none() {
        return Cow::Borrowed(text);
    }
    let mut masked = Vec::with_capacity(text.len());
    let mut copied = 0;
    for (span, placeholder) in spans {
        masked.extend_from_slice(text.get(copied..span.start).unwrap_or_default());
        masked.extend_from_slice(placeholder.as_bytes());
        copied = span.end;
    }
    masked.extend_from_slice(text.get(copied..).unwrap_or_default());
    Cow::Owned(masked)
}

/// Every string in `value`, keys included, [`mask`]ed. Two keys that mask alike stay
/// two fields: the later one gains ` (2)`, ` (3)` and so on.
pub(crate) fn mask_json(value: &mut Value) {
    match value {
        Value::String(text) => {
            if let Cow::Owned(masked) = mask(text) {
                *text = masked;
            }
        }
        Value::Array(items) => items.iter_mut().for_each(mask_json),
        Value::Object(fields) => {
            if fields.keys().any(|key| matches!(mask(key), Cow::Owned(_))) {
                let mut masked = Map::new();
                for (key, value) in std::mem::take(fields) {
                    let key = unused(&masked, mask(&key).into_owned());
                    masked.insert(key, value);
                }
                *fields = masked;
            }
            fields.values_mut().for_each(mask_json);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn unused(fields: &Map<String, Value>, key: String) -> String {
    let mut unused = key.clone();
    let mut count = 2;
    while fields.contains_key(&unused) {
        unused = format!("{key} ({count})");
        count += 1;
    }
    unused
}

/// Each span of `text` to mask, in order, with what replaces it.
fn spans(text: &[u8]) -> impl Iterator<Item = (Range<usize>, &'static str)> + '_ {
    let mut offset = 0;
    text.split(|&byte| ends_token(byte))
        .filter_map(move |token| {
            let start = offset;
            offset += token.len() + 1;
            let span = user_info(token)?;
            let info = token.get(span.clone())?;
            let placeholder = match info.contains(&b':') {
                true => USER_AND_PASSWORD,
                false => USER,
            };
            (info != placeholder.as_bytes())
                .then_some((start + span.start..start + span.end, placeholder))
        })
}

/// Where user info may run in `token`: from its first URL's authority to its last `@`.
fn user_info(token: &[u8]) -> Option<Range<usize>> {
    let at = token.iter().rposition(|&byte| byte == b'@')?;
    let start = authority(token.get(..at)?)?;
    (start < at).then_some(start..at)
}

/// Where the authority of the first URL in `text` starts.
fn authority(text: &[u8]) -> Option<usize> {
    let mut run = 0;
    let mut letter = false;
    for (index, &byte) in text.iter().enumerate() {
        if byte == b':' && letter {
            let scheme = text.get(run..index).unwrap_or_default();
            let rest = text.get(index + 1..).unwrap_or_default();
            if matches!(rest.first(), Some(b'/' | b'\\'))
                || SPECIAL.iter().any(|name| ends_in(scheme, name))
            {
                return Some(index + 1 + rest.iter().take_while(|&&byte| byte == b'/').count());
            }
        }
        if byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.') {
            letter |= byte.is_ascii_alphabetic();
        } else {
            run = index + 1;
            letter = false;
        }
    }
    None
}

fn ends_in(scheme: &[u8], name: &[u8]) -> bool {
    scheme
        .len()
        .checked_sub(name.len())
        .and_then(|from| scheme.get(from..))
        .is_some_and(|tail| tail.eq_ignore_ascii_case(name))
}

#[cfg(test)]
#[path = "userinfo_tests.rs"]
mod tests;
