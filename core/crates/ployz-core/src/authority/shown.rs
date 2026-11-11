//! Text a person reads before they sign, built so a value Cloud relays can't pass for
//! anything else. A relayed string enters only through [`Shown::text`] or
//! [`Shown::value`], which quote it unless it is a plain token and escape every
//! character outside printable ASCII. So no control character reaches the terminal, no
//! value can hold the separators around it, and no look-alike character passes for
//! another. Fixed wording enters only as `&'static str`.

use std::fmt;

use serde::Serialize;

use crate::config::Generator;

/// Text safe to show a person before they sign.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Shown(String);

impl Shown {
    /// Fixed wording.
    #[must_use]
    pub fn words(words: &'static str) -> Self {
        Self(words.to_owned())
    }

    /// A relayed string: bare when it is a plain token, such as `main` or `acme/web`,
    /// quoted and escaped otherwise.
    #[must_use]
    pub fn text(text: &str) -> Self {
        Self::default().and_text(text)
    }

    /// A relayed value as JSON whose every string, keys included, is quoted and escaped.
    #[must_use]
    pub fn value(value: &impl Serialize) -> Self {
        Self::default().and_value(value)
    }

    /// `items`, each shown, separated by `", "`. A shown item can't hold `", "` outside
    /// its quotes, so each boundary is one.
    #[must_use]
    pub fn list(items: impl IntoIterator<Item = Self>) -> Self {
        let mut shown = Self::default();
        for (index, item) in items.into_iter().enumerate() {
            if index > 0 {
                shown.0.push_str(", ");
            }
            shown.0.push_str(&item.0);
        }
        shown
    }

    /// A generator as it is written, such as `${{ secret(32, "a-zA-Z0-9") }}`. Its
    /// charset is printable ASCII with no quotes or backslashes, so it can't leave its
    /// quotes.
    #[must_use]
    pub fn generator(generator: &Generator) -> Self {
        Self(generator.to_string())
    }

    #[must_use]
    pub fn and(mut self, next: Self) -> Self {
        self.0.push_str(&next.0);
        self
    }

    #[must_use]
    pub fn and_words(mut self, words: &'static str) -> Self {
        self.0.push_str(words);
        self
    }

    #[must_use]
    pub fn and_text(mut self, text: &str) -> Self {
        if bare(text) {
            self.0.push_str(text);
        } else {
            quote(&mut self.0, text);
        }
        self
    }

    #[must_use]
    pub fn and_value(mut self, value: &impl Serialize) -> Self {
        let value = serde_json::to_value(value).expect("a shown value serializes to JSON");
        json(&mut self.0, &value);
        self
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Shown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<Shown> for String {
    fn from(shown: Shown) -> Self {
        shown.0
    }
}

/// Whether `text` reads unambiguously without quotes: not empty, and only letters,
/// digits and `-_./:@+`, none of which a line uses to separate values.
fn bare(text: &str) -> bool {
    !text.is_empty()
        && text.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | ':' | '@' | '+')
        })
}

/// `text` in double quotes, with `"` and `\` escaped and every character outside
/// printable ASCII written as an escape, such as `\n` or `\u{1b}`.
fn quote(out: &mut String, text: &str) {
    out.push('"');
    for c in text.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ' '..='~' => out.push(c),
            _ => out.extend(c.escape_unicode()),
        }
    }
    out.push('"');
}

/// `value` as compact JSON, each string through [`quote`].
fn json(out: &mut String, value: &serde_json::Value) {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            out.push_str(&value.to_string());
        }
        serde_json::Value::String(text) => quote(out, text),
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                json(out, item);
            }
            out.push(']');
        }
        serde_json::Value::Object(fields) => {
            out.push('{');
            for (index, (name, field)) in fields.iter().enumerate() {
                if index > 0 {
                    out.push(',');
                }
                quote(out, name);
                out.push(':');
                json(out, field);
            }
            out.push('}');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_token_shows_bare() {
        assert_eq!(Shown::text("acme/web").as_str(), "acme/web");
        assert_eq!(Shown::text("linux/amd64").as_str(), "linux/amd64");
    }

    #[test]
    fn anything_else_shows_quoted_and_escaped() {
        let cases = [
            ("", r#""""#),
            ("a b", r#""a b""#),
            ("a, Y=b", r#""a, Y=b""#),
            ("say \"hi\"\\", r#""say \"hi\"\\""#),
            ("a\r\nb\tc", r#""a\r\nb\tc""#),
            ("\u{1b}[2Kok", r#""\u{1b}[2Kok""#),
            ("\u{9b}2K", r#""\u{9b}2K""#),
            ("evil\u{202e}txt", r#""evil\u{202e}txt""#),
            ("\u{200b}main", r#""\u{200b}main""#),
            ("аcme", r#""\u{430}cme""#),
        ];
        for (text, shown) in cases {
            assert_eq!(Shown::text(text).as_str(), shown, "{text:?}");
        }
    }

    #[test]
    fn a_value_escapes_every_string_and_key() {
        let value = serde_json::json!({"a\nb": ["x\u{1b}", 1, true, null]});
        assert_eq!(
            Shown::value(&value).as_str(),
            r#"{"a\nb":["x\u{1b}",1,true,null]}"#
        );
    }

    #[test]
    fn nothing_shown_holds_a_control_character() {
        let hostile: String = (0u32..0x250)
            .chain(0x2000..0x2070)
            .chain([0xfeff, 0xe0001])
            .filter_map(char::from_u32)
            .collect();
        for shown in [
            Shown::text(&hostile),
            Shown::value(&serde_json::json!({ hostile.clone(): [hostile.clone()] })),
        ] {
            assert!(
                shown.as_str().chars().all(|c| matches!(c, ' '..='~')),
                "{shown}"
            );
        }
    }
}
