//! Shared JSON wire helpers for core value objects.
//!
//! The greenfield public control-plane JSON contract encodes wide `u64`
//! values as decimal strings. NATS sequence numbers and Unix timestamps can
//! exceed JavaScript's precise integer range, and JSON has no portable integer
//! width. Rust keeps `u64` internally; the JSON wire stays lossless.

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PositiveU64StringError {
    Zero,
    Invalid { value: String },
}

pub(crate) fn parse_positive_u64_string(value: String) -> Result<u64, PositiveU64StringError> {
    let parsed = value
        .parse::<u64>()
        .map_err(|_| PositiveU64StringError::Invalid {
            value: value.clone(),
        })?;

    if parsed == 0 {
        return Err(PositiveU64StringError::Zero);
    }

    Ok(parsed)
}

/// Defines a positive-`u64` newtype that crosses the JSON wire as a decimal
/// string. The expansion is the canonical wire-newtype scaffolding: `try_new`,
/// a `const` accessor, `TryFrom<u64>`/`Into<u64>`, and the string wire codec.
macro_rules! positive_u64_wire_newtype {
    (
        $(#[$meta:meta])*
        pub struct $name:ident;
        ts_brand: $brand:literal;
        accessor: $accessor:ident;
        error: $error:ident;
    ) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            ::serde::Serialize,
            ::serde::Deserialize,
        )]
        #[cfg_attr(feature = "typescript", derive(::ts_rs::TS))]
        #[cfg_attr(feature = "typescript", ts(type = $brand))]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(::std::num::NonZeroU64);

        impl $name {
            pub fn try_new(value: u64) -> Result<Self, $error> {
                let Some(value) = ::std::num::NonZeroU64::new(value) else {
                    return Err($error::Zero);
                };

                Ok(Self(value))
            }

            /// The first value in the series (one), proven valid at compile time.
            #[must_use]
            pub const fn first() -> Self {
                Self(::std::num::NonZeroU64::MIN)
            }

            #[must_use]
            pub const fn $accessor(self) -> u64 {
                self.0.get()
            }
        }

        impl TryFrom<u64> for $name {
            type Error = $error;

            fn try_from(value: u64) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl From<$name> for u64 {
            fn from(value: $name) -> Self {
                value.$accessor()
            }
        }

        impl TryFrom<String> for $name {
            type Error = $error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                match $crate::wire::parse_positive_u64_string(value) {
                    Ok(value) => Self::try_new(value),
                    Err($crate::wire::PositiveU64StringError::Zero) => Err($error::Zero),
                    Err($crate::wire::PositiveU64StringError::Invalid { value }) => {
                        Err($error::InvalidWireValue { value })
                    }
                }
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.$accessor().to_string()
            }
        }
    };
}

/// Defines the paired error enum for [`positive_u64_wire_newtype!`]. Kept
/// separate so two newtypes can share one error type.
macro_rules! positive_u64_wire_error {
    (
        pub enum $error:ident;
        noun: $noun:literal;
    ) => {
        #[derive(Debug, Clone, PartialEq, Eq, ::thiserror::Error)]
        pub enum $error {
            #[error("{} must be greater than zero", $noun)]
            Zero,
            #[error("{} {value:?} must be a positive integer string", $noun)]
            InvalidWireValue { value: String },
        }
    };
}

/// Defines a non-empty text newtype that crosses the JSON wire as a plain
/// string, rejecting values that are empty after trimming.
macro_rules! nonempty_text_newtype {
    (
        $(#[$meta:meta])*
        pub struct $name:ident;
        ts_brand: $brand:literal;
        error: $error:ident;
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, ::serde::Serialize, ::serde::Deserialize)]
        #[cfg_attr(feature = "typescript", derive(::ts_rs::TS))]
        #[cfg_attr(feature = "typescript", ts(type = $brand))]
        #[serde(try_from = "String", into = "String")]
        pub struct $name(String);

        impl $name {
            pub fn try_new(value: impl Into<String>) -> Result<Self, $error> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err($error::Empty);
                }

                Ok(Self(value))
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl TryFrom<String> for $name {
            type Error = $error;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                Self::try_new(value)
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.0
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

pub(crate) use {nonempty_text_newtype, positive_u64_wire_error, positive_u64_wire_newtype};
