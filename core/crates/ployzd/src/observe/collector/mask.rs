use ployz_core::scrub::{SealedValues, marker_text};
use serde_json::json;

#[derive(Debug, PartialEq, Eq)]
struct Rule {
    pattern: String,
    marker: String,
}

pub(super) fn fragment(values: &SealedValues) -> String {
    let statements = rules(values).iter().map(statement).collect::<Vec<_>>();
    let statements = if statements.is_empty() {
        vec![statement(&Rule {
            pattern: r"[^\s\S]".to_owned(),
            marker: String::new(),
        })]
    } else {
        statements
    };
    serde_json::to_string_pretty(&json!({
        "processors": {
            "transform/mask": {
                "error_mode": "ignore",
                "log_statements": [{ "context": "log", "statements": statements }],
            }
        }
    }))
    .expect("a mask fragment serializes to JSON")
}

fn rules(values: &SealedValues) -> Vec<Rule> {
    values
        .mask_texts()
        .into_iter()
        .flat_map(|(name, text)| {
            let marker = marker_text(name);
            let escaped = json_escaped(text);
            let escaped = (escaped != text).then_some(escaped);
            std::iter::once(text.to_owned())
                .chain(escaped)
                .map(move |form| Rule {
                    pattern: regex::escape(&form),
                    marker: marker.clone(),
                })
        })
        .collect()
}

fn json_escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).expect("a string serializes to JSON");
    quoted[1..quoted.len() - 1].to_owned()
}

fn statement(rule: &Rule) -> String {
    format!(
        "replace_pattern(log.body, {}, {})",
        quoted(&rule.pattern),
        quoted(&rule.marker.replace('$', "$$$$")),
    )
}

fn quoted(text: &str) -> String {
    serde_json::to_string(text).expect("a string serializes to JSON")
}

#[cfg(test)]
mod tests {
    use regex::Regex;

    use super::*;

    const AWKWARD: &str = "pa\"ss\\word\nsecond-row.1";

    fn values() -> SealedValues {
        [("DB_PASSWORD".to_owned(), AWKWARD.to_owned())]
            .into_iter()
            .collect()
    }

    fn statements(fragment: &str) -> Vec<String> {
        let fragment: serde_json::Value = serde_json::from_str(fragment).unwrap();
        serde_json::from_value(
            fragment
                .pointer("/processors/transform~1mask/log_statements/0/statements")
                .unwrap()
                .clone(),
        )
        .unwrap()
    }

    fn matches(rules: &[Rule], body: &str) -> bool {
        rules
            .iter()
            .any(|rule| Regex::new(&rule.pattern).unwrap().is_match(body))
    }

    #[test]
    fn a_value_with_a_quote_a_backslash_and_a_newline_gets_a_raw_and_an_escaped_pattern() {
        let rules = rules(&values());
        let raw = Regex::new(
            &rules
                .iter()
                .find(|r| r.pattern.contains('\n'))
                .unwrap()
                .pattern,
        )
        .unwrap();
        assert!(raw.is_match(&format!("password is {AWKWARD}!")));
        let escaped = json_escaped(AWKWARD);
        assert_ne!(escaped, AWKWARD);
        assert!(matches(&rules, &format!(r#"{{"msg":"{escaped}"}}"#)));
        assert!(
            !Regex::new(&regex::escape(AWKWARD))
                .unwrap()
                .is_match(&format!(r#"{{"msg":"{escaped}"}}"#))
        );
        assert!(
            rules
                .iter()
                .all(|rule| rule.marker == "[secret DB_PASSWORD]")
        );
    }

    #[test]
    fn a_value_json_writes_unchanged_gets_one_pattern_and_a_short_one_none() {
        let values: SealedValues = [
            ("TOKEN".to_owned(), "abcdef123456".to_owned()),
            ("PIN".to_owned(), "123".to_owned()),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            rules(&values),
            [Rule {
                pattern: "abcdef123456".into(),
                marker: "[secret TOKEN]".into()
            }]
        );
    }

    #[test]
    fn statements_are_json_strings_the_collector_reads_back_as_the_pattern() {
        let rules = rules(&values());
        let statements = statements(&fragment(&values()));
        assert_eq!(statements.len(), rules.len());
        for (statement, rule) in statements.iter().zip(&rules) {
            assert!(statement.starts_with("replace_pattern(log.body, \""));
            let literal = &statement["replace_pattern(log.body, ".len()..];
            let end = literal.find("\", \"").unwrap() + 1;
            assert_eq!(
                serde_json::from_str::<String>(&literal[..end]).unwrap(),
                rule.pattern
            );
        }
    }

    #[test]
    fn no_values_masks_nothing() {
        assert_eq!(
            statements(&fragment(&SealedValues::none())),
            [r#"replace_pattern(log.body, "[^\\s\\S]", "")"#]
        );
    }

    #[test]
    fn a_dollar_in_a_name_survives_the_config_and_the_replacement_expansions() {
        let rule = Rule {
            pattern: "x".into(),
            marker: "[secret A$1]".into(),
        };
        assert_eq!(
            statement(&rule),
            r#"replace_pattern(log.body, "x", "[secret A$$$$1]")"#
        );
    }
}
