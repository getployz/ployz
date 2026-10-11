//! Decoding JSON a caller sent. A refusal keeps serde's path and reason but never a
//! value from the input, so a secret pasted into the wrong field is not echoed back.

use serde::de::DeserializeOwned;
use serde_json::Value;
use serde_path_to_error::Segment;
use thiserror::Error;

/// Why JSON did not decode: where, and what was expected there.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{}{reason}", path.as_deref().map(|path| format!("{path}: ")).unwrap_or_default())]
pub struct DecodeError {
    path: Option<String>,
    reason: String,
}

impl DecodeError {
    /// The path into the input, like `mounts[0].path`; none for the input itself.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// What was expected at [`Self::path`], naming no value from the input.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// Decode `value` as a `T`.
///
/// # Errors
/// Returns where `value` does not fit `T`, and what fits there.
pub fn decode<T: DeserializeOwned>(value: &Value) -> Result<T, DecodeError> {
    serde_path_to_error::deserialize(value).map_err(|error| {
        let reason = reason(&error.inner().to_string(), value);
        let mut segments: Vec<_> = error.path().iter().collect();
        // An unknown field's path ends in its name, which is the caller's text.
        if reason.starts_with("unknown field") {
            segments.pop();
        }
        DecodeError {
            path: render(&segments),
            reason,
        }
    })
}

/// `segments` as `mounts[0].path`; none for the input itself.
fn render(segments: &[&Segment]) -> Option<String> {
    let mut path = String::new();
    for segment in segments {
        match segment {
            Segment::Seq { index } => path.push_str(&format!("[{index}]")),
            Segment::Map { key } | Segment::Enum { variant: key } => {
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
            }
            Segment::Unknown => path.push_str(if path.is_empty() { "?" } else { ".?" }),
        }
    }
    (!path.is_empty()).then_some(path)
}

/// serde's `message`, without what it quotes from `input`.
fn reason(message: &str, input: &Value) -> String {
    for kind in ["unknown variant", "unknown field"] {
        if message.starts_with(kind) {
            return match message.rsplit_once("`, expected ") {
                Some((_, expected))
                    if !message.ends_with(" there are no variants")
                        && !message.ends_with(" there are no fields") =>
                {
                    format!("{kind}, expected {expected}")
                }
                _ => format!("{kind}, there are none"),
            };
        }
    }
    for kind in ["invalid type", "invalid value", "invalid length"] {
        if let Some((_, expected)) = message
            .strip_prefix(kind)
            .and_then(|rest| rest.rsplit_once(", expected "))
        {
            return format!("{kind}, expected {expected}");
        }
    }
    if message.starts_with("missing field `") || message.starts_with("duplicate field `") {
        return message.to_owned();
    }
    // A `ValueError` reads `invalid KIND "VALUE": EXPECTED`, its value quoted and escaped.
    if let Some((kind, expected)) = message.strip_prefix("invalid ").and_then(|rest| {
        let (kind, _) = rest.split_once(" \"")?;
        let (_, expected) = rest.rsplit_once("\": ")?;
        Some((kind, expected))
    }) {
        return format!("invalid {kind}, expected {expected}");
    }
    match quotes(message, input) {
        true => "invalid value".to_owned(),
        false => message.to_owned(),
    }
}

/// Whether `message` contains any text or number from `input`.
fn quotes(message: &str, input: &Value) -> bool {
    match input {
        Value::Null | Value::Bool(_) => false,
        Value::Number(number) => message.contains(&number.to_string()),
        Value::String(text) => !text.is_empty() && message.contains(text.as_str()),
        Value::Array(items) => items.iter().any(|item| quotes(message, item)),
        Value::Object(fields) => fields.values().any(|value| quotes(message, value)),
    }
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Deserializer};
    use serde_json::json;

    use super::decode;

    const MARKER: &str = "MARKER_9f3c";

    /// Refuses every text by quoting it, as a careless `Deserialize` would.
    #[derive(Debug)]
    struct Echo;

    impl<'de> Deserialize<'de> for Echo {
        fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
            let text = String::deserialize(deserializer)?;
            Err(serde::de::Error::custom(format!("no good: {text}")))
        }
    }

    #[derive(Debug, Deserialize)]
    #[expect(dead_code, reason = "only decoded")]
    struct Outer {
        inner: Vec<Echo>,
    }

    #[test]
    fn a_reason_that_quotes_the_input_is_withheld() {
        let error = decode::<Outer>(&json!({ "inner": [MARKER] })).unwrap_err();
        assert_eq!(error.to_string(), "inner[0]: invalid value");
    }

    #[test]
    fn a_reason_that_quotes_nothing_is_kept() {
        let error = decode::<Outer>(&json!({ "inner": [""] })).unwrap_err();
        assert_eq!(error.to_string(), "inner[0]: no good: ");
    }

    #[test]
    fn a_value_error_keeps_what_was_expected() {
        let error = decode::<crate::ServiceName>(&json!(MARKER)).unwrap_err();
        assert_eq!(error.path(), None);
        assert!(
            error
                .reason()
                .starts_with("invalid Service Name, expected "),
            "{error}"
        );
        assert!(!error.reason().contains(MARKER), "{error}");
    }
}
