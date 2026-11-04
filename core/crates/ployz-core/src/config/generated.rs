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
        "secret's charset is printable ASCII and ranges such as a-z, with at least two different characters and no spaces, quotes or backslashes, even inside a range"
    )]
    Charset,
}

/// The fingerprint an [arriving](super::VariableValue::Arriving) generated variable is
/// stored with: never a real one, which is versioned.
pub(super) const AWAITING_FINGERPRINT: &str = "generating";

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
/// `-` first or last is itself. None is a quote or backslash, even inside a range
/// such as `!-~`, so a value pastes into a shell or connection string unescaped.
fn expand(charset: &str) -> Result<Vec<u8>, GeneratorError> {
    let bytes = charset.as_bytes();
    if !bytes.iter().all(u8::is_ascii_graphic) {
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
    if alphabet.len() < 2
        || alphabet
            .iter()
            .any(|byte| matches!(byte, b'"' | b'\'' | b'\\'))
    {
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
        assert!(serde_json::from_str::<Generator>(r#"{"length":32,"charset":"a'b"}"#).is_err());
    }

    #[test]
    fn a_variable_without_a_generator_keeps_its_stored_bytes() {
        let stored = r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"f","value":{"kind":"literal","value":"x"}}"#;
        let variable: crate::config::SavedVariableIntent = serde_json::from_str(stored).unwrap();
        assert_eq!(variable.generator(), None);
        assert_eq!(serde_json::to_string(&variable).unwrap(), stored);
    }

    /// Each shape a variable is stored in today, from Working, Saved and Applied State
    /// and from Sync bases, with the generator and lifecycle it reads as.
    const STORED: &[(&str, &str, bool, bool)] = &[
        (
            "plain literal",
            r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"p1:x","value":{"kind":"literal","value":"x"}}"#,
            false,
            false,
        ),
        (
            "template",
            r#"{"id":"v","key":"K","description":null,"exported":true,"valueFingerprint":"p1:t","value":{"kind":"template","parts":[{"kind":"text","value":"http://"},{"kind":"ref","owner":{"scope":"service","lineageId":"l"},"key":"HOST"}]}}"#,
            false,
            false,
        ),
        (
            "emptied template base",
            r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"p1:t","value":{"kind":"template","parts":[]}}"#,
            false,
            false,
        ),
        (
            "generated, made",
            r#"{"id":"v","key":"K","description":"d","exported":false,"valueFingerprint":"p1:g","value":{"kind":"literal","value":"abcdefgh"},"generated":{"length":8,"charset":"a-z"}}"#,
            true,
            false,
        ),
        (
            "generated, awaiting its value",
            r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"generating","value":{"kind":"literal","value":""},"generated":{"length":32,"charset":"a-zA-Z0-9"}}"#,
            true,
            true,
        ),
        (
            "emptied generated base",
            r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"p1:g","value":{"kind":"literal","value":""},"generated":{"length":32,"charset":"a-z"}}"#,
            true,
            true,
        ),
        (
            "secret, redacted",
            r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"s1:k","value":{"kind":"secret","encryptedValue":null}}"#,
            false,
            false,
        ),
        (
            "secret, client-sealed",
            r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"s1:k","value":{"kind":"secret","encryptedValue":{"sealedFor":"00000000-0000-4000-8000-000000000002","value":{"sealed":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+Pw==","scope":{"service":"00000000-0000-4000-8000-000000000003"},"key":"K"}}}}"#,
            false,
            false,
        ),
        (
            "secret, legacy",
            r#"{"id":"v1","key":"K","description":null,"exported":false,"valueFingerprint":"v1:ab","value":{"kind":"secret","encryptedValue":{"version":1,"iv":"aXY=","tag":"dGFn","ciphertext":"Y3Q="}}}"#,
            false,
            false,
        ),
        (
            "secret without a value",
            r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"","value":{"kind":"secret_without_value"}}"#,
            false,
            false,
        ),
    ];

    #[test]
    fn every_stored_variable_shape_keeps_its_bytes_and_lifecycle() {
        for &(shape, stored, generated, awaits) in STORED {
            let variable: crate::config::SavedVariableIntent =
                serde_json::from_str(stored).unwrap_or_else(|error| panic!("{shape}: {error}"));
            assert_eq!(serde_json::to_string(&variable).unwrap(), stored, "{shape}");
            assert_eq!(variable.generator().is_some(), generated, "{shape}");
            assert_eq!(variable.awaits_value(), awaits, "{shape}");
        }
    }

    /// Only a secret without a value has no fingerprint. A variable that disagrees never
    /// reads, so no writer that skips parsing can store one.
    #[test]
    fn a_variable_whose_fingerprint_disagrees_with_its_value_never_reads() {
        for (shape, stored) in [
            (
                "secret without a value, with a fingerprint",
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"f","value":{"kind":"secret_without_value"}}"#,
            ),
            (
                "literal without one",
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"","value":{"kind":"literal","value":"x"}}"#,
            ),
            (
                "template without one",
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"","value":{"kind":"template","parts":[]}}"#,
            ),
            (
                "secret without one",
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"","value":{"kind":"secret","encryptedValue":null}}"#,
            ),
            (
                "generated without one",
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"","value":{"kind":"literal","value":"x"},"generated":{"length":32,"charset":"a-z"}}"#,
            ),
        ] {
            assert!(
                serde_json::from_str::<crate::config::SavedVariableIntent>(stored).is_err(),
                "{shape} read"
            );
        }
    }

    /// Only a literal is generated: a marker beside any other value, such as one sealed
    /// since, reads and is gone, so it is never written back.
    #[test]
    fn a_marker_beside_a_value_that_is_not_generated_is_never_written_back() {
        for (shape, stored, kept) in [
            (
                "secret",
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"s1:k","value":{"kind":"secret","encryptedValue":null},"generated":{"length":32,"charset":"a-z"}}"#,
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"s1:k","value":{"kind":"secret","encryptedValue":null}}"#,
            ),
            (
                "secret without a value",
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"","value":{"kind":"secret_without_value"},"generated":{"length":32,"charset":"a-z"}}"#,
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"","value":{"kind":"secret_without_value"}}"#,
            ),
            (
                "template",
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"p1:t","value":{"kind":"template","parts":[]},"generated":{"length":32,"charset":"a-z"}}"#,
                r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"p1:t","value":{"kind":"template","parts":[]}}"#,
            ),
        ] {
            let variable: crate::config::SavedVariableIntent =
                serde_json::from_str(stored).unwrap();
            assert_eq!(variable.generator(), None, "{shape}");
            assert_eq!(serde_json::to_string(&variable).unwrap(), kept, "{shape}");
        }
    }

    #[test]
    fn a_marker_on_a_sealed_value_reads_and_is_dropped() {
        let stored = r#"{"id":"v","key":"K","description":null,"exported":false,"valueFingerprint":"","value":{"kind":"secret_without_value"},"generated":{"length":32,"charset":"a-z"}}"#;
        let variable: crate::config::SavedVariableIntent = serde_json::from_str(stored).unwrap();
        assert_eq!(variable.generator(), None);
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
            ("${{ secret(32, \"a'b\") }}", GeneratorError::Charset),
            ("${{ secret(32, \"!-~\") }}", GeneratorError::Charset),
        ] {
            assert_eq!(Generator::parse(text), Err(error), "{text}");
        }
    }
}
