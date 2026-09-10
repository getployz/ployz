//! Protected Tailcat capability framing shared by every management adapter.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ValueError;

/// Opaque administrative capability; only protected serialization and helper input expose it.
/// Native Tailcat owns cryptographic parsing and server identity validation.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct TailcatCapability(String);

impl TailcatCapability {
    /// Validate the single-line framing consumed by the native helper.
    ///
    /// # Errors
    /// Returns a redacted [`ValueError`] for empty, oversized, whitespace, or control input.
    pub fn parse(value: impl Into<String>) -> Result<Self, ValueError> {
        let value = value.into();
        // Leave room for the newline in the helper's 16 KiB input frame.
        if value.is_empty()
            || value.len() >= 16 * 1024
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(ValueError::new(
                "Tailcat Capability",
                "[redacted]",
                "a non-empty single-line capability shorter than 16 KiB",
            ));
        }
        Ok(Self(value))
    }

    /// Secret-bearing input for serialization or a protected helper pipe. Do not log it.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for TailcatCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl fmt::Display for TailcatCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[redacted]")
    }
}

impl TryFrom<String> for TailcatCapability {
    type Error = ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl From<TailcatCapability> for String {
    fn from(value: TailcatCapability) -> Self {
        value.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_framing_round_trips_without_diagnostic_disclosure() {
        let secret = "private-capability";
        let capability = TailcatCapability::parse(secret).unwrap();
        assert_eq!(capability.as_str(), secret);
        assert!(!format!("{capability} {capability:?}").contains(secret));
        let encoded = serde_json::to_string(&capability).unwrap();
        assert_eq!(
            serde_json::from_str::<TailcatCapability>(&encoded).unwrap(),
            capability
        );
        assert!(TailcatCapability::parse("x".repeat(16 * 1024 - 1)).is_ok());
        for value in [
            String::new(),
            "private-capability\ncommand".into(),
            "private-capability\r".into(),
            "private capability".into(),
            "private\tcapability".into(),
            "private\0capability".into(),
            "private\u{a0}capability".into(),
            "x".repeat(16 * 1024),
        ] {
            let error = TailcatCapability::parse(value.clone()).unwrap_err();
            assert!(!format!("{error} {error:?}").contains("private"));
            let encoded = serde_json::to_string(&value).unwrap();
            let error = serde_json::from_str::<TailcatCapability>(&encoded).unwrap_err();
            assert!(!error.to_string().contains("private"));
        }
    }
}
