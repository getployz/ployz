//! Typed identifiers used in storage keys, subjects, operations, and routing.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SubjectTokenError {
    #[error("subject token is empty")]
    Empty,
    #[error("subject token contains invalid characters: {value}")]
    InvalidCharacter { value: String },
}

/// Defines a typed identifier that wraps a [`SubjectToken`]. Every ID has the
/// same shape — the only per-type differences are the name and the TypeScript
/// brand literal — so the scaffolding lives here once.
macro_rules! subject_token_id {
    (
        pub struct $name:ident;
        ts_brand: $brand:literal;
    ) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
        #[cfg_attr(feature = "typescript", ts(type = $brand))]
        #[serde(transparent)]
        pub struct $name(SubjectToken);

        impl $name {
            pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
                Ok(Self(SubjectToken::try_new(value)?))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }
    };
}

subject_token_id! { pub struct OperationId; ts_brand: "Brand<string, \"OperationId\">"; }
subject_token_id! { pub struct NamespaceId; ts_brand: "Brand<string, \"NamespaceId\">"; }
subject_token_id! { pub struct MachineId; ts_brand: "Brand<string, \"MachineId\">"; }
subject_token_id! { pub struct ServiceId; ts_brand: "Brand<string, \"ServiceId\">"; }
subject_token_id! { pub struct NamespaceRevisionId; ts_brand: "Brand<string, \"NamespaceRevisionId\">"; }
subject_token_id! { pub struct NamespaceRevisionEntryId; ts_brand: "Brand<string, \"NamespaceRevisionEntryId\">"; }
subject_token_id! { pub struct ContainerId; ts_brand: "Brand<string, \"ContainerId\">"; }
subject_token_id! { pub struct CertId; ts_brand: "Brand<string, \"CertId\">"; }
subject_token_id! { pub struct RouteBindingId; ts_brand: "Brand<string, \"RouteBindingId\">"; }
subject_token_id! { pub struct StepId; ts_brand: "Brand<string, \"StepId\">"; }

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SubjectToken(String);

impl SubjectToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        let value = value.into();
        if value.is_empty() {
            return Err(SubjectTokenError::Empty);
        }

        if !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(SubjectTokenError::InvalidCharacter { value });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for SubjectToken {
    type Error = SubjectTokenError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<SubjectToken> for String {
    fn from(value: SubjectToken) -> Self {
        value.0
    }
}
