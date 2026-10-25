//! Generated variables: a value written `${{ secret(32, "a-zA-Z") }}` that the Config
//! Store fills with random characters, once per Environment.

use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// How a generated variable's value is made: `length` characters drawn uniformly
/// from `charset`, written as `secret` takes it, such as `a-zA-Z0-9`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    rename_all = "camelCase",
    deny_unknown_fields,
    try_from = "StoredGenerator"
)]
pub struct Generator {
    length: u16,
    charset: String,
}

/// A [`Generator`] as stored, checked as `secret` checks it before it is one.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredGenerator {
    length: u16,
    charset: String,
}

impl TryFrom<StoredGenerator> for Generator {
    type Error = GeneratorError;

    fn try_from(stored: StoredGenerator) -> Result<Self, Self::Error> {
        Self::new(stored.length, &stored.charset)
    }
}

/// Why text that calls `secret` is not a generated value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum GeneratorError {
    #[error("`${{{{ secret(…) }}}}` must be the whole value")]
    NotWhole,
    #[error("secret takes a length, then a quoted charset: secret(32, \"a-zA-Z0-9\")")]
    Malformed,
    #[error("secret's length is {MIN_LENGTH} to {MAX_LENGTH}")]
    Length,
    #[error(
        "secret's charset is printable ASCII and ranges such as a-z, without spaces, quotes or backslashes, with at least two different characters"
    )]
    Charset,
}

/// The fingerprint a generated variable holds while it [awaits its
/// value](super::SavedVariableIntent::awaits_value): never a real one, which is
/// versioned.
pub const AWAITING_FINGERPRINT: &str = "generating";

pub const DEFAULT_LENGTH: u16 = 32;
pub const DEFAULT_CHARSET: &str = "a-zA-Z0-9";
pub const MIN_LENGTH: u16 = 8;
pub const MAX_LENGTH: u16 = 256;

impl Generator {
    /// The generator `text` asks for: `None` when it calls no `secret`, so it is
    /// ordinary text. `$${{ secret() }}` is the literal text, as for references.
    ///
    /// # Errors
    /// [`GeneratorError`] when `text` calls `secret` beside other text, or with a
    /// length or charset it can't take.
    pub fn parse(text: &str) -> Result<Option<Self>, GeneratorError> {
        let Some(call) = whole_call(text) else {
            return if calls_secret(text) {
                Err(GeneratorError::NotWhole)
            } else {
                Ok(None)
            };
        };
        let args = call.trim();
        if args.is_empty() {
            return Self::new(DEFAULT_LENGTH, DEFAULT_CHARSET).map(Some);
        }
        let (length, charset) = match args.split_once(',') {
            Some((length, charset)) => (length.trim(), Some(quoted(charset.trim())?)),
            None => (args, None),
        };
        if length.is_empty() || !length.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(GeneratorError::Malformed);
        }
        let length = length.parse().map_err(|_| GeneratorError::Length)?;
        Self::new(length, charset.unwrap_or(DEFAULT_CHARSET)).map(Some)
    }

    /// # Errors
    /// [`GeneratorError::Length`] or [`GeneratorError::Charset`] for one `secret`
    /// refuses.
    pub fn new(length: u16, charset: &str) -> Result<Self, GeneratorError> {
        if !(MIN_LENGTH..=MAX_LENGTH).contains(&length) {
            return Err(GeneratorError::Length);
        }
        expand(charset)?;
        Ok(Self {
            length,
            charset: charset.to_owned(),
        })
    }

    #[must_use]
    pub const fn length(&self) -> u16 {
        self.length
    }

    /// The distinct characters the value is drawn from, in ASCII order.
    #[must_use]
    pub fn alphabet(&self) -> Vec<u8> {
        // Every way to make a Generator checked its charset.
        expand(&self.charset).unwrap_or_default()
    }
}

impl fmt::Display for Generator {
    /// The value as written, so a read shows what to type to ask for it again.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "${{{{ secret({}, \"{}\") }}}}",
            self.length, self.charset
        )
    }
}

/// The arguments of `${{ secret(…) }}` when it is the whole of `text`.
fn whole_call(text: &str) -> Option<&str> {
    let body = text.strip_prefix("${{")?.strip_suffix("}}")?.trim();
    if body.contains("{{") || body.contains("}}") {
        return None;
    }
    body.strip_prefix("secret")?
        .trim_start()
        .strip_prefix('(')?
        .strip_suffix(')')
}

/// Whether an unescaped `${{` in `text` opens a `secret` call.
fn calls_secret(text: &str) -> bool {
    text.match_indices("${{").any(|(at, _)| {
        let escaped = at > 0 && text.as_bytes().get(at - 1) == Some(&b'$');
        let after = text.get(at + 3..).unwrap_or_default().trim_start();
        !escaped
            && after
                .strip_prefix("secret")
                .is_some_and(|rest| rest.trim_start().starts_with('('))
    })
}

fn quoted(text: &str) -> Result<&str, GeneratorError> {
    text.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .ok_or(GeneratorError::Malformed)
}

/// The distinct characters `charset` names: single characters and `x-y` ranges. A
/// `-` first or last is itself.
fn expand(charset: &str) -> Result<Vec<u8>, GeneratorError> {
    let bytes = charset.as_bytes();
    if !bytes
        .iter()
        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'"' | b'\\'))
    {
        return Err(GeneratorError::Charset);
    }
    let mut alphabet = Vec::new();
    let mut at = 0;
    while let Some(&first) = bytes.get(at) {
        match (bytes.get(at + 1), bytes.get(at + 2)) {
            (Some(b'-'), Some(&last)) => {
                if first > last {
                    return Err(GeneratorError::Charset);
                }
                alphabet.extend(first..=last);
                at += 3;
            }
            _ => {
                alphabet.push(first);
                at += 1;
            }
        }
    }
    alphabet.sort_unstable();
    alphabet.dedup();
    if alphabet.len() < 2 {
        return Err(GeneratorError::Charset);
    }
    Ok(alphabet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(text: &str) -> Generator {
        Generator::parse(text).unwrap().unwrap()
    }

    #[test]
    fn secret_with_no_arguments_is_32_alphanumerics() {
        let generator = parsed("${{ secret() }}");
        assert_eq!(generator.length(), 32);
        assert_eq!(generator.alphabet().len(), 62);
    }

    #[test]
    fn secret_takes_a_length_and_a_charset() {
        let generator = parsed("${{secret( 48 , \"a-zA-Z\" )}}");
        assert_eq!(generator.length(), 48);
        assert_eq!(generator.alphabet().len(), 52);
        assert_eq!(parsed("${{ secret(16) }}").alphabet().len(), 62);
    }

    #[test]
    fn a_charset_mixes_ranges_and_characters() {
        assert_eq!(parsed("${{ secret(8, \"a-c_-\") }}").alphabet(), b"-_abc");
        assert_eq!(parsed("${{ secret(8, \"-ab\") }}").alphabet(), b"-ab");
    }

    #[test]
    fn written_back_it_parses_to_itself() {
        let generator = parsed("${{ secret(40, \"a-f0-9\") }}");
        assert_eq!(parsed(&generator.to_string()), generator);
    }

    #[test]
    fn a_stored_generator_is_checked_as_written_ones_are() {
        let stored: Generator = serde_json::from_str(r#"{"length":32,"charset":"a-z"}"#).unwrap();
        assert_eq!(stored, Generator::new(32, "a-z").unwrap());
        assert!(serde_json::from_str::<Generator>(r#"{"length":3,"charset":"a-z"}"#).is_err());
        assert!(serde_json::from_str::<Generator>(r#"{"length":32,"charset":"a"}"#).is_err());
    }

    #[test]
    fn a_variable_without_a_generator_keeps_its_stored_bytes() {
        let stored = r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"f","value":{"kind":"literal","value":"x"}}"#;
        let variable: crate::config::SavedVariableIntent = serde_json::from_str(stored).unwrap();
        assert_eq!(variable.generated, None);
        assert_eq!(serde_json::to_string(&variable).unwrap(), stored);
    }

    #[test]
    fn a_marker_on_a_sealed_value_reads_and_canonicalizing_drops_it() {
        let stored = r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"f","value":{"kind":"secret_without_value"},"generated":{"length":32,"charset":"a-z"}}"#;
        let variable: crate::config::SavedVariableIntent = serde_json::from_str(stored).unwrap();
        assert_eq!(variable.generator(), None);
        let mut variable = variable;
        variable.drop_stale_generator();
        assert_eq!(variable.generated, None);
    }

    #[test]
    fn text_without_a_secret_call_is_ordinary() {
        for text in [
            "plain",
            "${{ PASSWORD }}",
            "${{ db.secret }}",
            "$${{ secret() }}",
            "secret(32)",
        ] {
            assert_eq!(Generator::parse(text), Ok(None), "{text}");
        }
    }

    #[test]
    fn a_secret_call_beside_other_text_is_refused() {
        assert_eq!(
            Generator::parse("pre-${{ secret() }}"),
            Err(GeneratorError::NotWhole)
        );
        assert_eq!(
            Generator::parse("${{ secret() }}${{ secret() }}"),
            Err(GeneratorError::NotWhole)
        );
    }

    #[test]
    fn bad_arguments_are_refused() {
        for (text, error) in [
            ("${{ secret(x) }}", GeneratorError::Malformed),
            ("${{ secret(32, a-z) }}", GeneratorError::Malformed),
            ("${{ secret(-1) }}", GeneratorError::Malformed),
            ("${{ secret(7) }}", GeneratorError::Length),
            ("${{ secret(257) }}", GeneratorError::Length),
            ("${{ secret(99999999) }}", GeneratorError::Length),
            ("${{ secret(32, \"a\") }}", GeneratorError::Charset),
            ("${{ secret(32, \"aaaa\") }}", GeneratorError::Charset),
            ("${{ secret(32, \"z-a\") }}", GeneratorError::Charset),
            ("${{ secret(32, \"a b\") }}", GeneratorError::Charset),
            ("${{ secret(32, \"é-z\") }}", GeneratorError::Charset),
        ] {
            assert_eq!(Generator::parse(text), Err(error), "{text}");
        }
    }
}
