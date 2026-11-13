//! The one byte encoding every signed authority statement uses.
//!
//! Canonical JSON is RFC 8785 restricted so Rust and the browser produce it without a
//! library: object keys are ASCII and sorted bytewise, there is no whitespace, strings are
//! escaped the way `JSON.stringify` escapes them, numbers are non-negative integers no
//! larger than 2^53 - 1, and `null` never appears (an absent optional field is omitted).
//! A verifier checks the exact bytes it received are the canonical form of the value they
//! parse to, so one value has one encoding and one signature input.

use std::fmt::Write as _;

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

    /// This time in UTC to the second, as RFC 3339 writes it: `2026-11-12T02:08:13Z`.
    #[must_use]
    pub fn utc(self) -> String {
        let seconds = self.0 / 1000;
        let (days, rest) = (seconds / 86_400, seconds % 86_400);
        // Howard Hinnant's civil_from_days, for days since 1970-01-01.
        let shifted = days + 719_468;
        let era = shifted / 146_097;
        let of_era = shifted - era * 146_097;
        let year_of_era = (of_era - of_era / 1460 + of_era / 36_524 - of_era / 146_096) / 365;
        let day_of_year = of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let month_index = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * month_index + 2) / 5 + 1;
        let month = if month_index < 10 {
            month_index + 3
        } else {
            month_index - 9
        };
        let year = year_of_era + era * 400 + u64::from(month <= 2);
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
            rest / 3600,
            rest % 3600 / 60,
            rest % 60
        )
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

pub(super) use crate::value::hex::{hex_bytes, write_hex};

/// `deserialize_with` for a list a signed body keeps strictly ascending, so a set of
/// elements has one encoding and one signature input.
///
/// # Errors
/// Returns a deserialize error when the list is not sorted and distinct.
pub(super) fn sorted_distinct<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Ord,
{
    sorted_by(deserializer, |item: &T| item)
}

/// [`sorted_distinct`] for elements ordered by one field of theirs, such as a Service's name.
///
/// # Errors
/// Returns a deserialize error when the keys are not strictly ascending.
pub(super) fn sorted_by<'de, D, T, K>(
    deserializer: D,
    key: fn(&T) -> &K,
) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
    K: Ord + ?Sized,
{
    let items = Vec::<T>::deserialize(deserializer)?;
    if !items.is_sorted_by(|left, right| key(left) < key(right)) {
        return Err(serde::de::Error::custom("must be sorted and distinct"));
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Deserialize)]
    struct Lists {
        #[serde(deserialize_with = "sorted_distinct")]
        plain: Vec<u8>,
        #[serde(deserialize_with = "by_first")]
        pairs: Vec<(u8, u8)>,
    }

    fn by_first<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Vec<(u8, u8)>, D::Error> {
        sorted_by(deserializer, |pair: &(u8, u8)| &pair.0)
    }

    #[test]
    fn a_list_must_be_sorted_and_distinct() {
        let parse = |text: &str| serde_json::from_str::<Lists>(text);
        let lists = parse(r#"{"plain":[1,2],"pairs":[[1,9],[2,0]]}"#).unwrap();
        assert_eq!(
            (lists.plain, lists.pairs),
            (vec![1, 2], vec![(1, 9), (2, 0)])
        );
        assert!(parse(r#"{"plain":[],"pairs":[]}"#).is_ok());
        for text in [
            r#"{"plain":[2,1],"pairs":[]}"#,
            r#"{"plain":[1,1],"pairs":[]}"#,
            r#"{"plain":[],"pairs":[[2,0],[1,0]]}"#,
            r#"{"plain":[],"pairs":[[1,0],[1,9]]}"#,
        ] {
            let error = parse(text).unwrap_err().to_string();
            assert!(error.starts_with("must be sorted and distinct"), "{error}");
        }
    }

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
