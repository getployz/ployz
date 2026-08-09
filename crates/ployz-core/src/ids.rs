//! Typed identifiers used in storage keys, subjects, operations, and routing.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// A canonical ULID used by Corrosion's deterministic reader law.
///
/// The stored spelling is exactly 26 uppercase Crockford Base32 characters,
/// so lexical ordering and ULID numeric ordering are identical.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"CorrosionUlid\">"))]
#[serde(try_from = "String", into = "String")]
pub struct CorrosionUlid(String);

impl CorrosionUlid {
    pub const TEXT_LENGTH: usize = 26;

    /// Mints a new canonical ULID.
    #[must_use]
    pub fn generate() -> Self {
        Self(ulid::Ulid::new().to_string())
    }

    /// Parses a canonical Corrosion row identifier.
    pub fn try_new(value: impl Into<String>) -> Result<Self, CorrosionUlidError> {
        let value = value.into();
        if value.len() != Self::TEXT_LENGTH {
            return Err(CorrosionUlidError::InvalidLength {
                value,
                expected: Self::TEXT_LENGTH,
            });
        }

        let parsed =
            value
                .parse::<ulid::Ulid>()
                .map_err(|_| CorrosionUlidError::InvalidEncoding {
                    value: value.clone(),
                })?;
        let canonical = parsed.to_string();
        if value != canonical {
            return Err(CorrosionUlidError::NonCanonical { value, canonical });
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Display for CorrosionUlid {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for CorrosionUlid {
    type Err = CorrosionUlidError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

impl TryFrom<String> for CorrosionUlid {
    type Error = CorrosionUlidError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<CorrosionUlid> for String {
    fn from(value: CorrosionUlid) -> Self {
        value.into_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CorrosionUlidError {
    #[error("Corrosion ULID must contain exactly {expected} characters: {value}")]
    InvalidLength { value: String, expected: usize },
    #[error("Corrosion ULID is not valid Crockford Base32: {value}")]
    InvalidEncoding { value: String },
    #[error("Corrosion ULID is not canonical: {value}; canonical spelling is {canonical}")]
    NonCanonical { value: String, canonical: String },
}

macro_rules! corrosion_ulid_id {
    (
        pub struct $name:ident;
        ts_brand: $brand:literal;
    ) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "ts", derive(ts_rs::TS))]
        #[cfg_attr(feature = "ts", ts(type = $brand))]
        #[serde(transparent)]
        pub struct $name(CorrosionUlid);

        impl $name {
            #[must_use]
            pub fn generate() -> Self {
                Self(CorrosionUlid::generate())
            }

            pub fn try_new(value: impl Into<String>) -> Result<Self, CorrosionUlidError> {
                Ok(Self(CorrosionUlid::try_new(value)?))
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
            type Err = CorrosionUlidError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::try_new(value)
            }
        }
    };
}

corrosion_ulid_id! { pub struct ClusterId; ts_brand: "Brand<string, \"ClusterId\">"; }
corrosion_ulid_id! { pub struct TokenId; ts_brand: "Brand<string, \"TokenId\">"; }
corrosion_ulid_id! { pub struct PeerId; ts_brand: "Brand<string, \"PeerId\">"; }
corrosion_ulid_id! { pub struct MachineRowId; ts_brand: "Brand<string, \"MachineRowId\">"; }
corrosion_ulid_id! { pub struct NamespaceRowId; ts_brand: "Brand<string, \"NamespaceRowId\">"; }
corrosion_ulid_id! { pub struct ServiceRowId; ts_brand: "Brand<string, \"ServiceRowId\">"; }
corrosion_ulid_id! { pub struct OperationRowId; ts_brand: "Brand<string, \"OperationRowId\">"; }
corrosion_ulid_id! { pub struct RouteBindingRowId; ts_brand: "Brand<string, \"RouteBindingRowId\">"; }
corrosion_ulid_id! { pub struct ControllerAppointmentId; ts_brand: "Brand<string, \"ControllerAppointmentId\">"; }

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
