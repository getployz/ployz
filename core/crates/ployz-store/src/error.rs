//! The Store's errors: the RPC error vocabulary. An invalid value is never echoed back.

use std::fmt::Display;

use ployz_core::{Hint, RpcError, RpcErrorCode};
use serde::Serialize;
use serde_json::Value;

use crate::review::ApprovalRequired;

mod confirmation;
pub(crate) use confirmation::data_loss_message;
pub use confirmation::{Accept, Confirmation};

fn error(code: RpcErrorCode, message: impl Into<String>, details: Value) -> RpcError {
    RpcError::new(code, message).with_details(details)
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
    not_found(message, suggest(input, names))
}

/// Details for a mistyped name: the closest of `names`, if one is close, and every
/// one there is.
pub(crate) fn suggest<'a>(input: &str, names: impl IntoIterator<Item = &'a str> + Clone) -> Value {
    let closest = did_you_mean(input, names.clone()).map(|closest| Hint::Closest(closest.into()));
    Hint::details(
        &closest
            .into_iter()
            .chain([Hint::valid(names)])
            .collect::<Vec<_>>(),
    )
}

/// Details naming every choice there is, when no mistyped name stands for the choice.
pub(crate) fn valid<S: Display>(names: impl IntoIterator<Item = S>) -> Value {
    Hint::details(&[Hint::valid(names.into_iter().map(|name| name.to_string()))])
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

/// Stored data the Store cannot read back: a bug, a hand edit or a newer Cloud's
/// write, never user input. Cloud names the Environment by this message's prefix.
pub(crate) fn corrupt(what: &str) -> RpcError {
    internal(format!("The Config Store holds an unreadable {what}"))
}

/// Destroying something needs the caller to name it: `details` says what goes and
/// what to accept.
pub(crate) fn confirmation_required(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::ConfirmationRequired, message, details)
}

pub(crate) fn approval_required(
    message: impl Into<String>,
    required: &ApprovalRequired<impl Serialize>,
) -> RpcError {
    error(
        RpcErrorCode::from(ployz_core::APPROVAL_REQUIRED_CODE),
        message,
        serde_json::to_value(required).expect("an approval's details are JSON"),
    )
}

pub(crate) fn forbidden(message: impl Into<String>, details: Value) -> RpcError {
    error(
        RpcErrorCode::from(ployz_core::FORBIDDEN_CODE),
        message,
        details,
    )
}

/// Evidence a write needs is missing or incomplete, so it refuses rather than guess.
pub(crate) fn unobserved(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::Unavailable, message, details)
}

pub(crate) fn unsupported(message: impl Into<String>, details: Value) -> RpcError {
    error(RpcErrorCode::Unsupported, message, details)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_mistyped_name_gets_the_closest_and_every_name() {
        let names = ["web", "worker", "db"];
        assert_eq!(
            suggest("wbe", names),
            json!({ "did_you_mean": "web", "valid_children": ["web", "worker", "db"] })
        );
    }

    #[test]
    fn a_name_nothing_is_close_to_gets_only_the_list() {
        assert_eq!(
            suggest("zzzzzzzz", ["web", "db"]),
            json!({ "valid_children": ["web", "db"] })
        );
        assert_eq!(
            suggest("web", std::iter::empty()),
            json!({ "valid_children": [] })
        );
    }

    #[test]
    fn choices_is_not_found_with_the_suggestion_as_its_details() {
        let refused = choices("No Service wbe", "wbe", ["web", "db"]);
        assert_eq!(refused.code, RpcErrorCode::NotFound);
        assert_eq!(refused.message, "No Service wbe");
        assert_eq!(
            refused.details,
            json!({ "did_you_mean": "web", "valid_children": ["web", "db"] })
        );
    }

    #[test]
    fn named_choices_list_as_the_strings_they_serialize_to() {
        use crate::id::{EnvironmentName, VolumeName};
        let environments = [
            EnvironmentName::parse("staging").unwrap(),
            EnvironmentName::parse("production").unwrap(),
        ];
        let volume = VolumeName::parse("data").unwrap();
        assert_eq!(
            valid(&environments),
            json!({ "valid_children": serde_json::to_value(environments).unwrap() })
        );
        assert_eq!(
            valid([&volume]),
            json!({ "valid_children": [serde_json::to_value(&volume).unwrap()] })
        );
        assert_eq!(valid(Vec::<String>::new()), json!({ "valid_children": [] }));
    }

    #[test]
    fn an_unknown_setting_suggests_a_setting_and_says_what_it_expects() {
        let refused = crate::settings::ServiceSetting::parse("repicas").unwrap_err();
        assert_eq!(refused.code, RpcErrorCode::InvalidArgument);
        let details = refused.details.as_object().unwrap();
        assert_eq!(details["did_you_mean"], "replicas");
        assert!(details["valid_children"].as_array().unwrap().ends_with(&[
            json!("env"),
            json!("mounts"),
            json!("domains")
        ]));
        assert_eq!(
            details.keys().collect::<Vec<_>>(),
            ["did_you_mean", "example", "expected", "valid_children"]
        );
    }
}
