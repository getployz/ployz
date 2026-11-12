//! Hints: the lines that tell a reader what to do about an outcome, and the `details` keys
//! that carry them over the wire.
//!
//! The keys are frozen: `next`, `retry` and `undo` hold one command each, `did_you_mean`
//! one name, `inspect` and `valid_children` arrays of strings. A value of another shape
//! reads as absent. Terminal rendering belongs to the CLI.

use serde_json::{Map, Value};

use crate::RpcError;

/// One line telling the reader what they can do about an outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Hint {
    /// The one obvious command to run now.
    Next(String),
    /// A read-only command that shows more.
    Inspect(String),
    /// The command again, with what it was missing.
    Retry(String),
    /// The command that reverses what just happened.
    Undo(String),
    /// The choice closest to what the input named.
    Closest(String),
    /// The choices the input could have named.
    Valid(Vec<String>),
}

impl Hint {
    #[must_use]
    pub fn valid<I: IntoIterator<Item = S>, S: Into<String>>(names: I) -> Self {
        Self::Valid(names.into_iter().map(Into::into).collect())
    }

    /// Whether `other` says nothing this one doesn't: the same hint, or another
    /// of a kind a reader gets only one of.
    #[must_use]
    pub fn replaces(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Inspect(_), Self::Inspect(_)) => self == other,
            (Self::Next(_), Self::Next(_))
            | (Self::Retry(_), Self::Retry(_))
            | (Self::Undo(_), Self::Undo(_))
            | (Self::Closest(_), Self::Closest(_))
            | (Self::Valid(_), Self::Valid(_)) => true,
            _ => false,
        }
    }

    /// Hints carried as `details` keys by an error that crossed the wire.
    #[must_use]
    pub fn from_details(details: &Value) -> Vec<Self> {
        let text = |key: &str| details.get(key).and_then(Value::as_str).map(str::to_owned);
        let list = |key: &str| -> Vec<String> {
            details
                .get(key)
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let valid = list("valid_children");
        text("next")
            .map(Self::Next)
            .into_iter()
            .chain(list("inspect").into_iter().map(Self::Inspect))
            .chain(text("retry").map(Self::Retry))
            .chain(text("undo").map(Self::Undo))
            .chain(text("did_you_mean").map(Self::Closest))
            .chain((!valid.is_empty()).then_some(Self::Valid(valid)))
            .collect()
    }

    /// Write `hints` into `--json` `details`: `next`, `retry` and `undo` are one
    /// command each, `did_you_mean` one name, `inspect` and `valid_children` arrays.
    /// They merge into the keys already there; details that are not an object become one.
    pub fn into_details(hints: &[Self], details: &mut Value) {
        if hints.is_empty() {
            return;
        }
        if !details.is_object() {
            *details = Value::Object(Map::new());
        }
        let Some(fields) = details.as_object_mut() else {
            return;
        };
        let mut inspect = Vec::new();
        for hint in hints {
            match hint {
                Self::Next(command) => {
                    fields.insert("next".into(), command.as_str().into());
                }
                Self::Retry(command) => {
                    fields.insert("retry".into(), command.as_str().into());
                }
                Self::Undo(command) => {
                    fields.insert("undo".into(), command.as_str().into());
                }
                Self::Inspect(command) => inspect.push(Value::from(command.as_str())),
                Self::Closest(name) => {
                    fields.insert("did_you_mean".into(), name.as_str().into());
                }
                Self::Valid(names) => {
                    fields.insert("valid_children".into(), names.clone().into());
                }
            }
        }
        if !inspect.is_empty() {
            fields.insert("inspect".into(), inspect.into());
        }
    }

    /// The `details` that carry `hints` alone.
    #[must_use]
    pub fn details(hints: &[Self]) -> Value {
        let mut details = Value::Null;
        Self::into_details(hints, &mut details);
        details
    }
}

impl RpcError {
    /// `self` with `hint` written into its details, beside the keys already there.
    #[must_use]
    pub fn with_hint(mut self, hint: Hint) -> Self {
        Hint::into_details(&[hint], &mut self.details);
        self
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::RpcErrorCode;

    fn written(hints: &[Hint], mut details: Value) -> Value {
        Hint::into_details(hints, &mut details);
        details
    }

    #[test]
    fn hints_round_trip_through_json_details() {
        let hints = vec![
            Hint::Next("ployz deploy".into()),
            Hint::Inspect("ployz logs web".into()),
            Hint::Inspect("ployz ps".into()),
            Hint::Retry("ployz project rm blog --confirm blog".into()),
            Hint::Undo("ployz env sync --undo s3".into()),
            Hint::Closest("web".into()),
            Hint::valid(["web", "db"]),
        ];
        let mut details = serde_json::json!({ "deployment": "d1" });
        Hint::into_details(&hints, &mut details);
        assert_eq!(
            details,
            serde_json::json!({
                "deployment": "d1",
                "next": "ployz deploy",
                "inspect": ["ployz logs web", "ployz ps"],
                "retry": "ployz project rm blog --confirm blog",
                "undo": "ployz env sync --undo s3",
                "did_you_mean": "web",
                "valid_children": ["web", "db"],
            })
        );
        assert_eq!(Hint::from_details(&details), hints);
    }

    #[test]
    fn each_hint_writes_its_own_key_and_shape() {
        let null = Value::Null;
        assert_eq!(
            written(&[Hint::Next("ployz deploy".into())], null.clone()),
            json!({ "next": "ployz deploy" })
        );
        assert_eq!(
            written(&[Hint::Retry("ployz rm web --yes".into())], null.clone()),
            json!({ "retry": "ployz rm web --yes" })
        );
        assert_eq!(
            written(
                &[Hint::Undo("ployz env sync --undo s3".into())],
                null.clone()
            ),
            json!({ "undo": "ployz env sync --undo s3" })
        );
        assert_eq!(
            written(&[Hint::Closest("web".into())], null.clone()),
            json!({ "did_you_mean": "web" })
        );
        assert_eq!(
            written(&[Hint::valid(["web", "db"])], null.clone()),
            json!({ "valid_children": ["web", "db"] })
        );
        assert_eq!(
            written(&[Hint::Inspect("ployz ps".into())], null),
            json!({ "inspect": ["ployz ps"] })
        );
    }

    #[test]
    fn hints_merge_into_the_details_already_there() {
        let details = json!({
            "deployment": "d1",
            "next": "ployz old",
            "inspect": ["ployz old-inspect"],
            "valid_children": ["old"],
            "retry": "ployz old retry",
        });
        assert_eq!(
            written(
                &[
                    Hint::Next("ployz deploy".into()),
                    Hint::Inspect("ployz ps".into()),
                    Hint::valid(["web"]),
                ],
                details
            ),
            json!({
                "deployment": "d1",
                "next": "ployz deploy",
                "inspect": ["ployz ps"],
                "valid_children": ["web"],
                "retry": "ployz old retry",
            })
        );
    }

    #[test]
    fn the_last_hint_of_a_single_valued_kind_wins_and_inspects_collect_in_order() {
        assert_eq!(
            written(
                &[
                    Hint::Next("ployz one".into()),
                    Hint::Inspect("ployz b".into()),
                    Hint::Next("ployz two".into()),
                    Hint::Inspect("ployz a".into()),
                    Hint::Closest("x".into()),
                    Hint::Closest("y".into()),
                ],
                json!({})
            ),
            json!({
                "next": "ployz two",
                "inspect": ["ployz b", "ployz a"],
                "did_you_mean": "y",
            })
        );
    }

    #[test]
    fn details_that_are_not_an_object_become_one_when_a_hint_is_written() {
        for before in [
            Value::Null,
            json!("text"),
            json!(7),
            json!(["next"]),
            json!(true),
        ] {
            assert_eq!(
                written(&[Hint::Next("ployz deploy".into())], before),
                json!({ "next": "ployz deploy" })
            );
        }
    }

    #[test]
    fn no_hints_leave_the_details_as_they_were() {
        for before in [Value::Null, json!("text"), json!({ "a": 1 })] {
            assert_eq!(written(&[], before.clone()), before);
        }
        assert_eq!(Hint::details(&[]), Value::Null);
    }

    #[test]
    fn an_empty_valid_list_is_absent_and_an_empty_valid_hint_writes_an_empty_list() {
        assert_eq!(Hint::from_details(&json!({ "valid_children": [] })), []);
        assert_eq!(
            Hint::from_details(&json!({ "valid_children": [1, null, {}] })),
            []
        );
        assert_eq!(
            written(&[Hint::Valid(Vec::new())], json!({})),
            json!({ "valid_children": [] })
        );
    }

    #[test]
    fn details_of_the_wrong_shape_read_as_no_hint() {
        assert_eq!(Hint::from_details(&Value::Null), []);
        assert_eq!(Hint::from_details(&json!("next")), []);
        assert_eq!(
            Hint::from_details(&json!({
                "next": ["ployz deploy"],
                "retry": 3,
                "undo": null,
                "did_you_mean": { "name": "web" },
                "inspect": "ployz ps",
                "valid_children": "web",
            })),
            []
        );
    }

    #[test]
    fn a_list_key_reads_its_strings_and_skips_the_rest() {
        assert_eq!(
            Hint::from_details(&json!({
                "inspect": ["ployz ps", 3, null, "ployz logs web"],
                "valid_children": ["web", 1, "db"],
            })),
            [
                Hint::Inspect("ployz ps".into()),
                Hint::Inspect("ployz logs web".into()),
                Hint::valid(["web", "db"]),
            ]
        );
    }

    #[test]
    fn hints_read_in_one_fixed_order_whatever_the_key_order() {
        let details = json!({
            "valid_children": ["web"],
            "did_you_mean": "web",
            "undo": "ployz u",
            "retry": "ployz r",
            "inspect": ["ployz i"],
            "next": "ployz n",
            "unrelated": 1,
        });
        assert_eq!(
            Hint::from_details(&details),
            [
                Hint::Next("ployz n".into()),
                Hint::Inspect("ployz i".into()),
                Hint::Retry("ployz r".into()),
                Hint::Undo("ployz u".into()),
                Hint::Closest("web".into()),
                Hint::valid(["web"]),
            ]
        );
    }

    #[test]
    fn a_kind_a_reader_gets_one_of_replaces_another_and_inspects_replace_only_themselves() {
        let next = Hint::Next("ployz a".into());
        assert!(next.replaces(&Hint::Next("ployz b".into())));
        assert!(!next.replaces(&Hint::Retry("ployz a".into())));
        let inspect = Hint::Inspect("ployz ps".into());
        assert!(inspect.replaces(&Hint::Inspect("ployz ps".into())));
        assert!(!inspect.replaces(&Hint::Inspect("ployz ls".into())));
        assert!(Hint::valid(["a"]).replaces(&Hint::valid(["b", "c"])));
        assert!(Hint::Closest("a".into()).replaces(&Hint::Closest("b".into())));
        assert!(!Hint::Closest("a".into()).replaces(&Hint::valid(["a"])));
    }

    #[test]
    fn an_error_takes_a_hint_beside_the_details_it_has() {
        let refused = RpcError::new(RpcErrorCode::NotFound, "No Environment.")
            .with_details(json!({ "project": "blog" }))
            .with_hint(Hint::Next("ployz env ls".into()))
            .with_hint(Hint::valid(["staging"]));
        assert_eq!(
            refused.details,
            json!({
                "project": "blog",
                "next": "ployz env ls",
                "valid_children": ["staging"],
            })
        );
        assert_eq!(
            RpcError::new(RpcErrorCode::NotFound, "No.")
                .with_hint(Hint::Closest("web".into()))
                .details,
            json!({ "did_you_mean": "web" })
        );
    }
}
