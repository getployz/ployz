use std::collections::BTreeMap;

use serde_yaml::{Mapping, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterpolationFinding {
    pub path: String,
    pub kind: InterpolationFindingKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InterpolationFindingKind {
    MissingVariable { name: String },
    RequiredVariable { name: String, message: String },
    InvalidExpression { message: String },
}

pub(crate) fn interpolate_value(
    value: &mut Value,
    env: &BTreeMap<String, String>,
) -> Vec<InterpolationFinding> {
    let mut findings = Vec::new();
    interpolate_at(value, env, String::new(), &mut findings);
    findings
}

fn interpolate_at(
    value: &mut Value,
    env: &BTreeMap<String, String>,
    path: String,
    findings: &mut Vec<InterpolationFinding>,
) {
    match value {
        Value::String(contents) => {
            *contents = interpolate_string(contents, env, &path, findings);
        }
        Value::Sequence(items) => {
            for (index, item) in items.iter_mut().enumerate() {
                interpolate_at(item, env, format!("{path}[{index}]"), findings);
            }
        }
        Value::Mapping(mapping) => {
            for (key, item) in mapping {
                let field = match key {
                    Value::String(field) if path.is_empty() => field.clone(),
                    Value::String(field) => format!("{path}.{field}"),
                    Value::Null
                    | Value::Bool(_)
                    | Value::Number(_)
                    | Value::Sequence(_)
                    | Value::Mapping(_)
                    | Value::Tagged(_) => path.clone(),
                };
                interpolate_at(item, env, field, findings);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Tagged(_) => {}
    }
}

fn interpolate_string(
    input: &str,
    env: &BTreeMap<String, String>,
    path: &str,
    findings: &mut Vec<InterpolationFinding>,
) -> String {
    let mut out = String::new();
    let mut rest = input;
    while let Some(dollar) = rest.find('$') {
        out.push_str(&rest[..dollar]);
        rest = &rest[dollar + 1..];
        if let Some(after_dollar) = rest.strip_prefix('$') {
            out.push('$');
            rest = after_dollar;
            continue;
        }
        if let Some(after_open) = rest.strip_prefix('{') {
            let Some(close) = after_open.find('}') else {
                findings.push(InterpolationFinding {
                    path: path.to_owned(),
                    kind: InterpolationFindingKind::InvalidExpression {
                        message: "missing closing `}`".to_owned(),
                    },
                });
                out.push('$');
                out.push_str(rest);
                return out;
            };
            let expression = &after_open[..close];
            if expression.contains("${") {
                findings.push(InterpolationFinding {
                    path: path.to_owned(),
                    kind: InterpolationFindingKind::InvalidExpression {
                        message: format!(
                            "nested `${{...}}` in {expression:?} is not supported; \
                             an expression ends at the first `}}`, so defaults cannot \
                             contain `}}` or another `${{...}}`"
                        ),
                    },
                });
                rest = &after_open[close + 1..];
                continue;
            }
            out.push_str(&resolve_braced(expression, env, path, findings));
            rest = &after_open[close + 1..];
            continue;
        }
        let (name, after_name) = take_name(rest);
        if name.is_empty() {
            out.push('$');
            continue;
        }
        out.push_str(&resolve_unbraced(name, env, path, findings));
        rest = after_name;
    }
    out.push_str(rest);
    out
}

fn resolve_unbraced(
    name: &str,
    env: &BTreeMap<String, String>,
    path: &str,
    findings: &mut Vec<InterpolationFinding>,
) -> String {
    match env.get(name) {
        Some(value) => value.clone(),
        None => {
            findings.push(InterpolationFinding {
                path: path.to_owned(),
                kind: InterpolationFindingKind::MissingVariable {
                    name: name.to_owned(),
                },
            });
            String::new()
        }
    }
}

fn resolve_braced(
    expression: &str,
    env: &BTreeMap<String, String>,
    path: &str,
    findings: &mut Vec<InterpolationFinding>,
) -> String {
    let Some((name, operator, word)) = parse_expression(expression) else {
        findings.push(InterpolationFinding {
            path: path.to_owned(),
            kind: InterpolationFindingKind::InvalidExpression {
                message: format!("invalid interpolation expression {expression:?}"),
            },
        });
        return String::new();
    };
    let value = env.get(name);
    match operator {
        InterpolationOperator::Direct => resolve_unbraced(name, env, path, findings),
        InterpolationOperator::DefaultIfUnset => match value {
            Some(value) => value.clone(),
            None => word.to_owned(),
        },
        InterpolationOperator::DefaultIfUnsetOrEmpty => match value {
            Some(value) if !value.is_empty() => value.clone(),
            Some(_) | None => word.to_owned(),
        },
        InterpolationOperator::RequiredIfUnset => match value {
            Some(value) => value.clone(),
            None => {
                findings.push(InterpolationFinding {
                    path: path.to_owned(),
                    kind: InterpolationFindingKind::RequiredVariable {
                        name: name.to_owned(),
                        message: word.to_owned(),
                    },
                });
                String::new()
            }
        },
        InterpolationOperator::RequiredIfUnsetOrEmpty => match value {
            Some(value) if !value.is_empty() => value.clone(),
            Some(_) | None => {
                findings.push(InterpolationFinding {
                    path: path.to_owned(),
                    kind: InterpolationFindingKind::RequiredVariable {
                        name: name.to_owned(),
                        message: word.to_owned(),
                    },
                });
                String::new()
            }
        },
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InterpolationOperator {
    Direct,
    DefaultIfUnset,
    DefaultIfUnsetOrEmpty,
    RequiredIfUnset,
    RequiredIfUnsetOrEmpty,
}

fn parse_expression(expression: &str) -> Option<(&str, InterpolationOperator, &str)> {
    let (name, rest) = take_name(expression);
    if name.is_empty() {
        return None;
    }
    match rest {
        "" => Some((name, InterpolationOperator::Direct, "")),
        _ if rest.starts_with(":-") => Some((
            name,
            InterpolationOperator::DefaultIfUnsetOrEmpty,
            &rest[2..],
        )),
        _ if rest.starts_with('-') => {
            Some((name, InterpolationOperator::DefaultIfUnset, &rest[1..]))
        }
        _ if rest.starts_with(":?") => Some((
            name,
            InterpolationOperator::RequiredIfUnsetOrEmpty,
            &rest[2..],
        )),
        _ if rest.starts_with('?') => {
            Some((name, InterpolationOperator::RequiredIfUnset, &rest[1..]))
        }
        _ => None,
    }
}

fn take_name(input: &str) -> (&str, &str) {
    let mut chars = input.char_indices();
    let Some((_, first)) = chars.next() else {
        return ("", input);
    };
    if !is_name_start(first) {
        return ("", input);
    }
    for (index, ch) in chars {
        if !is_name_continue(ch) {
            return (&input[..index], &input[index..]);
        }
    }
    (input, "")
}

fn is_name_start(ch: char) -> bool {
    ch == '_' || ch.is_ascii_alphabetic()
}

fn is_name_continue(ch: char) -> bool {
    is_name_start(ch) || ch.is_ascii_digit()
}

pub(crate) fn apply_merge(value: &mut Value) {
    match value {
        Value::Mapping(mapping) => apply_mapping_merge(mapping),
        Value::Sequence(items) => {
            for item in items {
                apply_merge(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {}
    }
}

fn apply_mapping_merge(mapping: &mut Mapping) {
    for value in mapping.values_mut() {
        apply_merge(value);
    }
    let merge_key = Value::String("<<".to_owned());
    let Some(merge_value) = mapping.remove(&merge_key) else {
        return;
    };
    let mut merged = Mapping::new();
    match merge_value {
        Value::Mapping(base) => merge_mapping(&mut merged, base),
        Value::Sequence(items) => {
            for item in items {
                if let Value::Mapping(base) = item {
                    merge_mapping(&mut merged, base);
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Tagged(_) => {}
    }
    for (key, value) in std::mem::take(mapping) {
        merged.insert(key, value);
    }
    *mapping = merged;
}

fn merge_mapping(target: &mut Mapping, source: Mapping) {
    for (key, value) in source {
        target.entry(key).or_insert(value);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_yaml::Value;

    use super::{InterpolationFindingKind, apply_merge, interpolate_value};

    #[test]
    fn interpolates_full_compose_grammar() {
        let mut env = BTreeMap::new();
        env.insert("SET".to_owned(), "value".to_owned());
        env.insert("EMPTY".to_owned(), String::new());
        let mut value = Value::String(
            "$SET ${SET} ${MISSING:-fallback} ${EMPTY:-fallback} ${EMPTY-fallback} $$".to_owned(),
        );

        let findings = interpolate_value(&mut value, &env);

        assert!(findings.is_empty());
        assert_eq!(
            value,
            Value::String("value value fallback fallback  $".to_owned())
        );
    }

    #[test]
    fn unset_side_default_without_colon_applies_when_unset() {
        let mut value = Value::String("${MISSING-d}".to_owned());

        let findings = interpolate_value(&mut value, &BTreeMap::new());

        assert!(findings.is_empty());
        assert_eq!(value, Value::String("d".to_owned()));
    }

    #[test]
    fn adjacent_unbraced_variables_resolve_independently() {
        let mut env = BTreeMap::new();
        env.insert("A".to_owned(), "one".to_owned());
        env.insert("B".to_owned(), "two".to_owned());
        let mut value = Value::String("$A$B".to_owned());

        let findings = interpolate_value(&mut value, &env);

        assert!(findings.is_empty());
        assert_eq!(value, Value::String("onetwo".to_owned()));
    }

    #[test]
    fn trailing_dollar_is_literal() {
        let mut value = Value::String("price$".to_owned());

        let findings = interpolate_value(&mut value, &BTreeMap::new());

        assert!(findings.is_empty());
        assert_eq!(value, Value::String("price$".to_owned()));
    }

    #[test]
    fn empty_expression_is_invalid() {
        for input in ["${}", "${:-x}"] {
            let mut value = Value::String(input.to_owned());

            let findings = interpolate_value(&mut value, &BTreeMap::new());

            let first = findings.first().expect("one finding");
            assert!(
                matches!(
                    first.kind,
                    InterpolationFindingKind::InvalidExpression { .. }
                ),
                "{input} is invalid"
            );
        }
    }

    #[test]
    fn nested_expression_is_rejected_set_or_unset() {
        let mut env = BTreeMap::new();
        env.insert("A".to_owned(), "set".to_owned());
        env.insert("B".to_owned(), "inner".to_owned());
        for env in [env, BTreeMap::new()] {
            let mut value = Value::String("${A:-${B}}".to_owned());

            let findings = interpolate_value(&mut value, &env);

            let first = findings.first().expect("one finding");
            assert!(matches!(
                first.kind,
                InterpolationFindingKind::InvalidExpression { .. }
            ));
        }
    }

    #[test]
    fn expression_ends_at_first_close_brace_so_default_cannot_contain_one() {
        let mut value = Value::String("${A:-a}b}".to_owned());

        let findings = interpolate_value(&mut value, &BTreeMap::new());

        assert!(findings.is_empty());
        assert_eq!(value, Value::String("ab}".to_owned()));
    }

    #[test]
    fn unset_without_default_becomes_empty_with_advisory() {
        let mut value = Value::String("before-${MISSING}-after".to_owned());

        let findings = interpolate_value(&mut value, &BTreeMap::new());

        assert_eq!(value, Value::String("before--after".to_owned()));
        assert_eq!(findings.len(), 1);
        let first = findings.first().expect("one finding");
        assert!(matches!(
            first.kind,
            InterpolationFindingKind::MissingVariable { .. }
        ));
    }

    #[test]
    fn required_operator_records_fatal_finding() {
        let mut value = Value::String("${MISSING:?set it}".to_owned());

        let findings = interpolate_value(&mut value, &BTreeMap::new());

        assert_eq!(value, Value::String(String::new()));
        let first = findings.first().expect("one finding");
        assert!(matches!(
            first.kind,
            InterpolationFindingKind::RequiredVariable { .. }
        ));
    }

    #[test]
    fn applies_yaml_merge_before_typed_parse() {
        let mut value: Value = serde_yaml::from_str(
            r#"
            base: &base
              image: nginx
              command: old
            services:
              web:
                <<: *base
                command: new
            "#,
        )
        .expect("yaml parses");

        apply_merge(&mut value);

        let service = field(field(&value, "services"), "web");
        assert_eq!(field(service, "image"), &Value::String("nginx".to_owned()));
        assert_eq!(field(service, "command"), &Value::String("new".to_owned()));
    }

    fn field<'a>(value: &'a Value, name: &str) -> &'a Value {
        let Value::Mapping(mapping) = value else {
            panic!("value is a mapping");
        };
        mapping
            .get(Value::String(name.to_owned()))
            .expect("field exists")
    }
}
