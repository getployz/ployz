//! URL user info, masked in everything Ployz prints: `scheme://user:password@host` reads
//! `scheme://<USER>:<PASSWORD>@host`, and `scheme://token@host` reads
//! `scheme://<USER>@host`. The scan needs no URL to parse, so a bad port, an unknown
//! scheme, an SSH or Git URL, or a password holding a `/` is masked all the same.

use std::borrow::Cow;

use serde_json::Value;

const USER: &[u8] = b"<USER>";
const USER_AND_PASSWORD: &[u8] = b"<USER>:<PASSWORD>";

/// `text` with the user info of every URL in it masked.
pub(crate) fn mask(text: &str) -> Cow<'_, str> {
    match mask_bytes(text.as_bytes()) {
        Cow::Borrowed(_) => Cow::Borrowed(text),
        // Each cut starts right after `://` and ends at an `@`, both ASCII.
        Cow::Owned(bytes) => Cow::Owned(String::from_utf8(bytes).expect("cut at ASCII")),
    }
}

/// [`mask`] over bytes, which need not be UTF-8.
pub(crate) fn mask_bytes(text: &[u8]) -> Cow<'_, [u8]> {
    let mut masked = Vec::new();
    let mut copied = 0;
    let mut from = 0;
    while let Some(start) = after_scheme(text, from) {
        let word = word_at(text, start);
        let Some(at) = user_info_end(word).filter(|&at| at > 0) else {
            from = start;
            continue;
        };
        let info = word.get(..at).unwrap_or_default();
        let placeholder = match info.contains(&b':') {
            true => USER_AND_PASSWORD,
            false => USER,
        };
        if info != placeholder {
            masked.extend_from_slice(text.get(copied..start).unwrap_or_default());
            masked.extend_from_slice(placeholder);
            copied = start + at;
        }
        from = start + at;
    }
    if copied == 0 {
        return Cow::Borrowed(text);
    }
    masked.extend_from_slice(text.get(copied..).unwrap_or_default());
    Cow::Owned(masked)
}

/// Every string in `value`, keys included, [`mask`]ed.
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
                *fields = std::mem::take(fields)
                    .into_iter()
                    .map(|(key, value)| (mask(&key).into_owned(), value))
                    .collect();
            }
            fields.values_mut().for_each(mask_json);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

/// Where the text after the next `://` at or past `from` starts.
fn after_scheme(text: &[u8], from: usize) -> Option<usize> {
    let found = text
        .get(from..)?
        .windows(3)
        .position(|three| three == b"://")?;
    Some(from + found + 3)
}

/// The text from `start` to where a URL's text ends: whitespace or a control character.
/// ESC is kept, so a color code inside a URL doesn't end it.
fn word_at(text: &[u8], start: usize) -> &[u8] {
    text.get(start..)
        .unwrap_or_default()
        .split(|&byte| (byte <= b' ' && byte != 0x1b) || byte == 0x7f)
        .next()
        .unwrap_or_default()
}

/// Where the user info in `word`, the text after `://`, ends: the index of its `@`.
fn user_info_end(word: &[u8]) -> Option<usize> {
    let authority = word
        .split(|byte| b"/?#\\".contains(byte))
        .next()
        .unwrap_or_default();
    if let Some(at) = authority.iter().rposition(|&byte| byte == b'@') {
        return Some(at);
    }
    // No `@` before the path. A password holding `/`, `?`, `#` or `\` still reads
    // `user:password@`, unless what follows the host's `:` is a port.
    let host = match authority.first() {
        Some(b'[') => authority
            .iter()
            .position(|&byte| byte == b']')
            .map_or(authority.len(), |end| end + 1),
        _ => 0,
    };
    let after_host = authority.get(host..).unwrap_or_default();
    let colon = after_host.iter().position(|&byte| byte == b':')?;
    let port = after_host.get(colon + 1..).unwrap_or_default();
    if !port.is_empty() && port.iter().all(u8::is_ascii_digit) {
        return None;
    }
    word.iter().rposition(|&byte| byte == b'@')
}

#[cfg(test)]
mod tests {
    use super::{mask, mask_json};

    #[test]
    fn a_urls_user_info_is_masked_whether_or_not_it_parses() {
        for (text, masked) in [
            (
                "https://user:SENTINEL@example.invalid:99999/config.yaml",
                "https://<USER>:<PASSWORD>@example.invalid:99999/config.yaml",
            ),
            (
                "connection \"ssh://user:SENTINEL@x.example\" not found",
                "connection \"ssh://<USER>:<PASSWORD>@x.example\" not found",
            ),
            (
                "Could not read https://SENTINEL@github.com/acme/web.git",
                "Could not read https://<USER>@github.com/acme/web.git",
            ),
            (
                "git+ssh://git@github.com:acme/web",
                "git+ssh://<USER>@github.com:acme/web",
            ),
            ("://SENTINEL@host", "://<USER>@host"),
            // A password that holds `/`, `?`, `#` or `@`.
            (
                "postgres://user:SEN/TINEL@db/app",
                "postgres://<USER>:<PASSWORD>@db/app",
            ),
            (
                "https://user:SEN?TI#NEL@host",
                "https://<USER>:<PASSWORD>@host",
            ),
            (
                "https://user:SEN@TINEL@host",
                "https://<USER>:<PASSWORD>@host",
            ),
            (
                "'https://user:SEN'\\''TINEL@host'",
                "'https://<USER>:<PASSWORD>@host'",
            ),
            // A color code inside the URL.
            (
                "\u{1b}[1mhttps://\u{1b}[2muser:SENTINEL\u{1b}[0m@host\u{1b}[0m",
                "\u{1b}[1mhttps://<USER>:<PASSWORD>@host\u{1b}[0m",
            ),
            // Two URLs, the second inside the first's query.
            (
                "https://cloud.example/?next=https://u:SENTINEL@x.example/",
                "https://cloud.example/?next=https://<USER>:<PASSWORD>@x.example/",
            ),
            (
                "a https://u:SENTINEL@a.example and b ssh://SENTINEL@b.example",
                "a https://<USER>:<PASSWORD>@a.example and b ssh://<USER>@b.example",
            ),
        ] {
            assert_eq!(mask(text), masked, "{text}");
            assert!(!mask(text).contains("SENTINEL"), "{text}");
            assert_eq!(
                mask(masked),
                masked,
                "masking again changes nothing: {text}"
            );
        }
    }

    #[test]
    fn text_without_user_info_is_left_as_it_is() {
        for text in [
            "https://cloud.example:8443/api",
            "http://[::1]:3000/x@y",
            "https://registry.npmjs.org/@scope/pkg",
            "https://host/path?email=a@b.example",
            "See https://example.com for help, or mail a@b.example.",
            "nginx@sha256:abc and user@host and https://",
            "{\"a\":\"https://host/x\",\"b\":\"c@d\"}",
            "https://<USER>:<PASSWORD>@host",
            "",
        ] {
            assert!(
                matches!(mask(text), std::borrow::Cow::Borrowed(_)),
                "{text}"
            );
        }
    }

    #[test]
    fn every_string_in_a_json_document_is_masked_keys_too() {
        let mut value = serde_json::json!({
            "query": "https://user:SENTINEL@x.example/a",
            "list": ["ssh://SENTINEL@host", 3, null],
            "https://SENTINEL@key.example": {"deep": "x://u:SENTINEL@y"},
        });
        mask_json(&mut value);
        assert!(!value.to_string().contains("SENTINEL"), "{value}");
        assert_eq!(value.pointer("/list/0").unwrap(), "ssh://<USER>@host");
        assert_eq!(
            value
                .get("https://<USER>@key.example")
                .and_then(|key| key.get("deep"))
                .unwrap(),
            "x://<USER>:<PASSWORD>@y"
        );
    }
}
