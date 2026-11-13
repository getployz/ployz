//! The value language of a secret in a Setting: what a read shows and what a write
//! sends. The read side has two shapes and no text, so no read shows a secret; the
//! write side is the only place text sits, and it never prints or serialises.

use std::fmt;

use serde::Deserialize;
use serde_json::{Value, json};

/// What a read shows of a secret: `{"secret": true}` when it has a value,
/// `{"secret": false}` when it awaits one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretCell {
    Set,
    NotSet,
}

impl SecretCell {
    /// `{"secret": true}` or `{"secret": false}`.
    #[must_use]
    pub fn to_value(self) -> Value {
        json!({ "secret": self == Self::Set })
    }

    /// The cell `value` is, exactly: an object holding `secret` and nothing else, as a bool.
    #[must_use]
    pub fn of(value: &Value) -> Option<Self> {
        let fields = value.as_object()?;
        match (fields.len(), fields.get("secret")) {
            (1, Some(Value::Bool(true))) => Some(Self::Set),
            (1, Some(Value::Bool(false))) => Some(Self::NotSet),
            _ => None,
        }
    }

    /// Whether `value` is an object with a `secret` key, whatever it holds. Such a value
    /// is never shown as text.
    #[must_use]
    pub fn is_cell(value: &Value) -> bool {
        value.get("secret").is_some()
    }
}

impl From<bool> for SecretCell {
    /// A secret that has a value is `Set`.
    fn from(valued: bool) -> Self {
        if valued { Self::Set } else { Self::NotSet }
    }
}

/// What a write sends for a secret, the value of the `secret` key: `true` keeps the
/// stored one, text seals a new one, `{"generate": true}` has the Store make one.
///
/// It has no `Serialize` and its `Debug` hides the text, so the secret leaves it only
/// through [`SecretInput::to_value`] and [`SecretInput::seal`].
#[derive(PartialEq, Eq)]
pub enum SecretInput {
    Keep,
    Seal(String),
    Generate,
}

impl SecretInput {
    /// The input `field` is, as the value of a `secret` key. `false` and any other
    /// shape are none.
    #[must_use]
    pub fn of(field: &Value) -> Option<Self> {
        match field {
            Value::Bool(true) => Some(Self::Keep),
            Value::String(text) => Some(Self::Seal(text.clone())),
            Value::Object(asked)
                if asked.len() == 1 && asked.get("generate") == Some(&Value::Bool(true)) =>
            {
                Some(Self::Generate)
            }
            Value::Null
            | Value::Bool(false)
            | Value::Number(_)
            | Value::Array(_)
            | Value::Object(_) => None,
        }
    }

    /// The input an object's `secret` key holds, whatever else the object holds.
    #[must_use]
    pub fn in_object(value: &Value) -> Option<Self> {
        Self::of(value.get("secret")?)
    }

    /// Whether `text`, a JSON document typed as a value, is exactly `{"secret":true}`.
    /// Text never carries a new secret: that would put it in shell history. A typed parse
    /// refuses a duplicate key, so `{"secret":"…","secret":true}` can't slip through; it
    /// also takes `[true]`, hence the object check.
    #[must_use]
    pub fn keeps_by_text(text: &str) -> bool {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Keep {
            secret: bool,
        }
        matches!(
            serde_json::from_str::<Keep>(text),
            Ok(Keep { secret: true })
        ) && text.trim_start().starts_with('{')
    }

    /// Whether it carries new text to seal.
    #[must_use]
    pub fn seals(&self) -> bool {
        matches!(self, Self::Seal(_))
    }

    /// The text to seal, if it has any.
    #[must_use]
    pub fn seal(&self) -> Option<&str> {
        match self {
            Self::Seal(text) => Some(text),
            Self::Keep | Self::Generate => None,
        }
    }

    /// `{"secret": …}`: the value a write sends. It holds the text, so it goes to the
    /// Store and nowhere else.
    #[must_use]
    pub fn to_value(&self) -> Value {
        match self {
            Self::Keep => json!({ "secret": true }),
            Self::Seal(text) => json!({ "secret": text }),
            Self::Generate => json!({ "secret": { "generate": true } }),
        }
    }
}

impl fmt::Debug for SecretInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keep => formatter.write_str("Keep"),
            Self::Seal(_) => formatter.write_str("Seal(..)"),
            Self::Generate => formatter.write_str("Generate"),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{SecretCell, SecretInput};

    #[test]
    fn a_cell_reads_back_as_the_json_it_writes() {
        for (cell, json) in [
            (SecretCell::Set, json!({ "secret": true })),
            (SecretCell::NotSet, json!({ "secret": false })),
        ] {
            assert_eq!(cell.to_value(), json);
            assert_eq!(SecretCell::of(&json), Some(cell));
        }
        assert_eq!(SecretCell::from(true), SecretCell::Set);
        assert_eq!(SecretCell::from(false), SecretCell::NotSet);
    }

    #[test]
    fn a_cell_is_nothing_but_a_bool_alone() {
        for malformed in [
            json!(null),
            json!(true),
            json!("secret"),
            json!({}),
            json!({ "secret": "text" }),
            json!({ "secret": null }),
            json!({ "secret": { "generate": true } }),
            json!({ "secret": true, "username": "u" }),
            json!({ "value": { "secret": true } }),
        ] {
            assert_eq!(SecretCell::of(&malformed), None, "{malformed}");
        }
    }

    #[test]
    fn a_cell_is_spelled_by_its_key_whatever_it_holds() {
        assert!(SecretCell::is_cell(&json!({ "secret": "text" })));
        assert!(SecretCell::is_cell(&json!({ "secret": false })));
        assert!(!SecretCell::is_cell(&json!({ "value": 1 })));
        assert!(!SecretCell::is_cell(&json!("secret")));
    }

    #[test]
    fn an_input_reads_back_as_the_json_it_writes() {
        for (input, json) in [
            (SecretInput::Keep, json!({ "secret": true })),
            (
                SecretInput::Seal("s3".to_owned()),
                json!({ "secret": "s3" }),
            ),
            (
                SecretInput::Generate,
                json!({ "secret": { "generate": true } }),
            ),
        ] {
            assert_eq!(input.to_value(), json);
            assert_eq!(SecretInput::in_object(&json), Some(input));
        }
    }

    #[test]
    fn an_input_refuses_each_malformed_field() {
        for malformed in [
            json!(null),
            json!(false),
            json!(1),
            json!([true]),
            json!({}),
            json!({ "generate": false }),
            json!({ "generate": true, "length": 8 }),
            json!({ "sealedFor": "key" }),
        ] {
            assert!(SecretInput::of(&malformed).is_none(), "{malformed}");
        }
        assert!(SecretInput::in_object(&json!({})).is_none());
        assert!(SecretInput::in_object(&json!("text")).is_none());
    }

    #[test]
    fn only_text_seals() {
        assert!(SecretInput::Seal(String::new()).seals());
        assert_eq!(SecretInput::Seal("s3".to_owned()).seal(), Some("s3"));
        for input in [SecretInput::Keep, SecretInput::Generate] {
            assert!(!input.seals());
            assert_eq!(input.seal(), None);
        }
    }

    #[test]
    fn typed_text_keeps_a_secret_and_never_seals_one() {
        for kept in [
            r#"{"secret":true}"#,
            r#" {"secret": true}"#,
            "{\n\"secret\": true\n}",
        ] {
            assert!(SecretInput::keeps_by_text(kept), "{kept}");
        }
        for refused in [
            r#"{"secret":"FAKE-3588"}"#,
            r#"{"secret":false}"#,
            r#"{"secret":true,"username":"u"}"#,
            r#"{"secret":"FAKE-3588","secret":true}"#,
            r#"{"secret":true,"secret":"FAKE-3588"}"#,
            "[true]",
            "true",
            "secret",
            "",
        ] {
            assert!(!SecretInput::keeps_by_text(refused), "{refused}");
        }
    }

    #[test]
    fn an_input_never_prints_its_text() {
        let input = SecretInput::Seal("FAKE-3588".to_owned());
        assert!(!format!("{input:?}").contains("FAKE-3588"));
    }
}
