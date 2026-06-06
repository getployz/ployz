use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"FailureMessage\">"))]
#[serde(try_from = "String", into = "String")]
pub struct FailureMessage(String);

impl FailureMessage {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NonEmptyTextError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(NonEmptyTextError::Empty);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for FailureMessage {
    type Error = NonEmptyTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<FailureMessage> for String {
    fn from(value: FailureMessage) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"CancellationReason\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct CancellationReason(String);

impl CancellationReason {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NonEmptyTextError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(NonEmptyTextError::Empty);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CancellationReason {
    type Error = NonEmptyTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<CancellationReason> for String {
    fn from(value: CancellationReason) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"OperatorHint\">"))]
#[serde(try_from = "String", into = "String")]
pub struct OperatorHint(String);

impl OperatorHint {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NonEmptyTextError> {
        let value = value.into();
        if value.trim().is_empty() {
            return Err(NonEmptyTextError::Empty);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for OperatorHint {
    type Error = NonEmptyTextError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<OperatorHint> for String {
    fn from(value: OperatorHint) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NonEmptyTextError {
    Empty,
}

impl fmt::Display for NonEmptyTextError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("text must not be empty"),
        }
    }
}
