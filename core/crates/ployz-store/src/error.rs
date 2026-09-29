//! The Store's errors: the RPC error vocabulary. An invalid value is never echoed back.

use ployz_core::{RpcError, RpcErrorCode};
use serde_json::Value;

fn error(code: RpcErrorCode, message: impl Into<String>, details: Value) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        details,
    }
}

pub(crate) fn invalid(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::InvalidArgument, message, details)
}

pub(crate) fn not_found(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::NotFound, message, details)
}

pub(crate) fn ambiguous(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::Ambiguous, message, details)
}

pub(crate) fn conflict(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::Conflict, message, details)
}

pub(crate) fn unavailable(message: impl Into<String>) -> RpcError {
    error(RpcErrorCode::Unavailable, message, Value::Null)
}

pub(crate) fn internal(message: impl Into<String>) -> RpcError {
    error(RpcErrorCode::Internal, message, Value::Null)
}

/// The closest of `options` to a mistyped `input`, if any is close: same letters
/// ignoring case and `-`/`_`, or a small edit distance. Never applied, only suggested.
pub(crate) fn did_you_mean<'a>(
    input: &str,
    options: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let squash = |text: &str| {
        text.chars()
            .filter(|c| !matches!(c, '-' | '_'))
            .flat_map(char::to_lowercase)
            .collect::<Vec<_>>()
    };
    let input = squash(input);
    options
        .into_iter()
        .map(|option| (distance(&input, &squash(option)), option))
        .filter(|(distance, _)| *distance <= (input.len() / 3).max(2))
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, option)| option)
}

/// `not_found` for a mistyped name: the closest of `names`, if one is close, and
/// every one there is.
pub(crate) fn choices<'a>(
    message: impl Into<String>,
    input: &str,
    names: impl IntoIterator<Item = &'a str> + Clone,
) -> RpcError {
    not_found(
        message,
        serde_json::json!({
            "did_you_mean": did_you_mean(input, names.clone()),
            "valid_children": names.into_iter().collect::<Vec<_>>(),
        }),
    )
}

/// Levenshtein distance.
fn distance(a: &[char], b: &[char]) -> usize {
    let mut previous = (0..=b.len()).collect::<Vec<_>>();
    for (i, x) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for ((y, above_left), above) in b.iter().zip(&previous).zip(previous.iter().skip(1)) {
            let left = current.last().copied().unwrap_or_default();
            current.push(
                (above_left + usize::from(x != y))
                    .min(above + 1)
                    .min(left + 1),
            );
        }
        previous = current;
    }
    previous.last().copied().unwrap_or_default()
}

/// Stored data the Store cannot read back: a bug or a hand edit, never user input.
pub(crate) fn corrupt(what: &str) -> RpcError {
    internal(format!("The Config Store holds an unreadable {what}"))
}

/// Destroying something needs the caller to name it: `details` says what goes and
/// what to accept.
pub(crate) fn confirmation_required(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::ConfirmationRequired, message, details)
}

/// Evidence a write needs is missing or incomplete, so it refuses rather than guess.
pub(crate) fn unobserved(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::Unavailable, message, details)
}
