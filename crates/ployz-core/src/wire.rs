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

pub(crate) fn format_u64_string(value: u64) -> String {
    value.to_string()
}
