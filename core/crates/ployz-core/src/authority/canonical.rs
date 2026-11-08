//! The one byte encoding every signed authority statement uses.
//!
//! Canonical JSON is RFC 8785 restricted so Rust and the browser produce it without a
//! library: object keys are ASCII and sorted bytewise, there is no whitespace, strings are
//! escaped the way `JSON.stringify` escapes them, numbers are non-negative integers no
//! larger than 2^53 - 1, and `null` never appears (an absent optional field is omitted).
//! A verifier checks the exact bytes it received are the canonical form of the value they
//! parse to, so one value has one encoding and one signature input.

use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use thiserror::Error;
use ts_rs::TS;

/// The largest integer a signed body may hold: the largest JavaScript keeps exact.
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// Why a value has no canonical form, or bytes are not one.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CanonicalError {
    #[error("a signed body cannot hold a fraction or negative number")]
    Number,
    #[error("a signed body cannot hold an integer above 2^53 - 1")]
    Integer,
    #[error("a signed body omits absent fields rather than writing null")]
    Null,
    #[error("a signed body's object keys are ASCII")]
    Key,
    #[error("the body does not parse as this statement: {0}")]
    Parse(String),
    #[error("the body is not in canonical form")]
    NotCanonical,
}

/// Encode `value` canonically.
///
/// # Errors
/// Returns a [`CanonicalError`] when `value` holds a number, `null` or key canonical JSON
/// refuses.
pub fn to_canonical<T: Serialize + ?Sized>(value: &T) -> Result<String, CanonicalError> {
    let value =
        serde_json::to_value(value).map_err(|error| CanonicalError::Parse(error.to_string()))?;
    let mut out = String::new();
    write_value(&value, &mut out)?;
    Ok(out)
}

/// Parse `body` as `T`, refusing it unless it is the canonical encoding of what it parses to.
///
/// # Errors
/// Returns [`CanonicalError::Parse`] when `body` is not a `T` (unknown fields included, for
/// statements that deny them), and [`CanonicalError::NotCanonical`] when it is a `T` written
/// some other way.
pub fn from_canonical<T: Serialize + DeserializeOwned>(body: &str) -> Result<T, CanonicalError> {
    let value: T =
        serde_json::from_str(body).map_err(|error| CanonicalError::Parse(error.to_string()))?;
    if to_canonical(&value)? != body {
        return Err(CanonicalError::NotCanonical);
    }
    Ok(value)
}

fn write_value(value: &Value, out: &mut String) -> Result<(), CanonicalError> {
    match value {
        Value::Null => Err(CanonicalError::Null),
        Value::Bool(flag) => {
            out.push_str(if *flag { "true" } else { "false" });
            Ok(())
        }
        Value::Number(number) => {
            let Some(integer) = number.as_u64() else {
                return Err(CanonicalError::Number);
            };
            if integer > MAX_SAFE_INTEGER {
                return Err(CanonicalError::Integer);
            }
            write!(out, "{integer}").expect("writing to a String cannot fail");
            Ok(())
        }
        Value::String(text) => {
            write_string(text, out);
            Ok(())
        }
        Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_value(item, out)?;
            }
            out.push(']');
            Ok(())
        }
        Value::Object(map) => {
            let mut entries: Vec<_> = map.iter().collect();
            if entries.iter().any(|(key, _)| !key.is_ascii()) {
                return Err(CanonicalError::Key);
            }
            entries.sort_unstable_by(|(left, _), (right, _)| left.as_bytes().cmp(right.as_bytes()));
            out.push('{');
            for (index, (key, item)) in entries.into_iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                write_string(key, out);
                out.push(':');
                write_value(item, out)?;
            }
            out.push('}');
            Ok(())
        }
    }
}

/// Escape as `JSON.stringify` does: the two-character escapes for `\b \t \n \f \r " \`,
/// lowercase `\u00xx` for every other control character, everything else verbatim.
fn write_string(text: &str, out: &mut String) {
    out.push('"');
    for char in text.chars() {
        match char {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            control if u32::from(control) < 0x20 => {
                write!(out, "\\u{:04x}", u32::from(control))
                    .expect("writing to a String cannot fail");
            }
            other => out.push(other),
        }
    }
    out.push('"');
}

/// Milliseconds since the Unix epoch, as the signer's or the Machine's wall clock read it.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "u64", into = "u64")]
#[ts(type = "number")]
pub struct Timestamp(u64);

impl Timestamp {
    /// # Errors
    /// Returns [`CanonicalError::Integer`] above 2^53 - 1, which no signed body can hold.
    pub const fn from_millis(millis: u64) -> Result<Self, CanonicalError> {
        if millis > MAX_SAFE_INTEGER {
            return Err(CanonicalError::Integer);
        }
        Ok(Self(millis))
    }

    /// The wall clock now. Saturates at the largest safe integer, which the clock
    /// does not reach for another quarter of a million years.
    #[must_use]
    pub fn now() -> Self {
        let millis = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis());
        Self(u64::try_from(millis).map_or(MAX_SAFE_INTEGER, |millis| millis.min(MAX_SAFE_INTEGER)))
    }

    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// `self` plus `millis`, saturating at the largest safe integer.
    #[must_use]
    pub const fn plus(self, millis: u64) -> Self {
        let sum = self.0.saturating_add(millis);
        Self(if sum > MAX_SAFE_INTEGER {
            MAX_SAFE_INTEGER
        } else {
            sum
        })
    }
}

impl TryFrom<u64> for Timestamp {
    type Error = CanonicalError;

    fn try_from(millis: u64) -> Result<Self, Self::Error> {
        Self::from_millis(millis)
    }
}

impl From<Timestamp> for u64 {
    fn from(timestamp: Timestamp) -> Self {
        timestamp.0
    }
}

pub(super) fn hex_bytes<const N: usize>(value: &str) -> Option<[u8; N]> {
    if !crate::value::is_lower_hex(value, N * 2) {
        return None;
    }
    let mut bytes = [0; N];
    for (byte, pair) in bytes.iter_mut().zip(value.as_bytes().chunks(2)) {
        *byte = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(bytes)
}

pub(super) fn write_hex(bytes: &[u8], formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
    bytes
        .iter()
        .try_for_each(|byte| write!(formatter, "{byte:02x}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_keys_and_drops_whitespace() {
        let value = serde_json::json!({"b": 1, "a": [true, "x"], "A": {"z": 0}});
        assert_eq!(
            to_canonical(&value).unwrap(),
            r#"{"A":{"z":0},"a":[true,"x"],"b":1}"#
        );
    }

    #[test]
    fn escapes_strings_as_json_stringify_does() {
        let value = "q\"\\\u{8}\t\n\u{c}\r\u{1}\u{1f}/é\u{2028}";
        assert_eq!(
            to_canonical(value).unwrap(),
            "\"q\\\"\\\\\\b\\t\\n\\f\\r\\u0001\\u001f/é\u{2028}\""
        );
    }

    #[test]
    fn refuses_values_javascript_cannot_hold_exactly() {
        assert_eq!(to_canonical(&1.5).unwrap_err(), CanonicalError::Number);
        assert_eq!(to_canonical(&-1).unwrap_err(), CanonicalError::Number);
        assert_eq!(
            to_canonical(&(MAX_SAFE_INTEGER + 1)).unwrap_err(),
            CanonicalError::Integer
        );
        assert_eq!(
            to_canonical(&Option::<u8>::None).unwrap_err(),
            CanonicalError::Null
        );
        assert_eq!(
            to_canonical(&serde_json::json!({"é": 1})).unwrap_err(),
            CanonicalError::Key
        );
    }

    #[test]
    fn parsing_refuses_any_other_spelling_of_the_same_value() {
        let canonical = r#"{"a":1,"b":2}"#;
        assert_eq!(
            from_canonical::<serde_json::Value>(canonical).unwrap(),
            serde_json::json!({"a": 1, "b": 2})
        );
        for other in [r#"{"b":2,"a":1}"#, r#"{"a": 1,"b":2}"#, r#"{"a":1,"b":2} "#] {
            assert_eq!(
                from_canonical::<serde_json::Value>(other).unwrap_err(),
                CanonicalError::NotCanonical,
                "{other}"
            );
        }
    }
}
