//! Typed identifiers used in storage keys, subjects, operations, and routing.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

pub use crate::corrosion::CorrosionNamespaceName;
pub use crate::machine::MachineName;
pub use crate::operation::RouteHostname;

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
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[cfg_attr(feature = "ts", ts(type = $brand))]
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

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = SubjectTokenError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_new(value)
            }
        }
    };
}

subject_token_id! { pub struct OperationId; ts_brand: "Brand<string, \"OperationId\">"; }
subject_token_id! { pub struct ContainerId; ts_brand: "Brand<string, \"ContainerId\">"; }

pub type NamespaceId = CorrosionNamespaceName;
pub type MachineId = MachineName;

// Canonical operator-visible identities for the Corrosion control plane.
// These deliberately have no `generate` constructor: callers must choose the
// durable name that will identify the resource everywhere.
subject_token_id! { pub struct ClusterName; ts_brand: "Brand<string, \"ClusterName\">"; }
subject_token_id! { pub struct PeerName; ts_brand: "Brand<string, \"PeerName\">"; }
subject_token_id! { pub struct TokenName; ts_brand: "Brand<string, \"TokenName\">"; }
subject_token_id! { pub struct DeployName; ts_brand: "Brand<string, \"DeployName\">"; }

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
