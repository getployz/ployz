//! Template resolution over supplied producer facts. No storage or provider execution.
use super::{ValuePart, ValuePartOwner};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Plaintext inputs belong only to an authorized server-side resolution call.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResolverValue {
    Literal { value: String },
    Secret { value: String },
    Template { parts: Vec<ValuePart> },
}

/// A supplied variable value keyed by its owner and stable reference scope.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VariableProducer {
    pub owner_id: String,
    pub owner: ValuePartOwner,
    pub key: String,
    pub value: ResolverValue,
}

/// A template and the producer facts available to resolve it.
#[derive(Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ResolveVariablesInput {
    pub parts: Vec<ValuePart>,
    pub self_owner_id: String,
    pub producers: Vec<VariableProducer>,
}

/// A missing producer reference encountered during template resolution.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct TemplateWarning {
    #[ts(type = "'missing'")]
    pub kind: String,
    pub owner_id: Option<String>,
    pub key: String,
}

/// Resolved text with secret provenance, or the cycle that prevents resolution.
#[derive(Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResolveVariablesResult {
    Resolved {
        value: String,
        secret: bool,
        warnings: Vec<TemplateWarning>,
    },
    Cycle {
        path: Vec<String>,
    },
}

/// Resolve supplied templates transitively, retaining secret provenance and reporting missing producers or cycles.
#[must_use]
pub fn resolve_variables(input: &ResolveVariablesInput) -> ResolveVariablesResult {
    let mut warnings = Vec::new();
    let mut memo = BTreeMap::new();
    let mut stack = Vec::new();
    match resolve_parts(
        &input.parts,
        &input.self_owner_id,
        &input.producers,
        &mut warnings,
        &mut memo,
        &mut stack,
    ) {
        Ok((value, secret)) => ResolveVariablesResult::Resolved {
            value,
            secret,
            warnings,
        },
        Err(path) => ResolveVariablesResult::Cycle { path },
    }
}

/// A Config file with its references resolved. Its `Debug` leaves the content out.
#[derive(Clone, Eq, PartialEq)]
pub struct ResolvedConfigFile {
    pub content: String,
    /// Whether a secret reached `content`.
    pub secret: bool,
    /// References whose value may break the file's syntax, each once.
    pub fragile: Vec<FragileReference>,
}

impl std::fmt::Debug for ResolvedConfigFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedConfigFile")
            .field("content_len", &self.content.len())
            .field("secret", &self.secret)
            .field("fragile", &self.fragile)
            .finish()
    }
}

/// A reference whose value holds a line break or the quote it sits inside, or holds a
/// line break in a YAML file. Pasted raw, it may corrupt the file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FragileReference {
    pub owner: ValuePartOwner,
    pub key: String,
}

/// Resolve Config file `name`'s `parts` against `producers`. A Config names no
/// owner of its own, so every reference names its Service.
///
/// # Errors
/// Returns the cycle that prevents resolution, as [`ResolveVariablesResult::Cycle`] does.
pub fn resolve_config_file(
    name: &str,
    parts: &[ValuePart],
    producers: &[VariableProducer],
) -> Result<ResolvedConfigFile, Vec<String>> {
    let name = name.to_ascii_lowercase();
    let yaml = name.ends_with(".yml") || name.ends_with(".yaml");
    let mut warnings = Vec::new();
    let mut memo = BTreeMap::new();
    let mut stack = Vec::new();
    let mut file = ResolvedConfigFile {
        content: String::new(),
        secret: false,
        fragile: Vec::new(),
    };
    let mut quote = None;
    for part in parts {
        let (owner, key) = match part {
            ValuePart::Text { value } => {
                for c in value.chars() {
                    quote = match (quote, c) {
                        (_, '\n') => None,
                        (None, '"' | '\'') => Some(c),
                        (Some(open), c) if c == open => None,
                        (open, _) => open,
                    };
                }
                file.content.push_str(value);
                continue;
            }
            ValuePart::Ref { owner, key } => (owner, key),
        };
        let (value, secret) = resolve_parts(
            std::slice::from_ref(part),
            "",
            producers,
            &mut warnings,
            &mut memo,
            &mut stack,
        )?;
        let newline = value.contains('\n');
        let breaks = quote.is_some_and(|open| newline || value.contains(open)) || (yaml && newline);
        let reference = FragileReference {
            owner: owner.clone(),
            key: key.clone(),
        };
        if breaks && !file.fragile.contains(&reference) {
            file.fragile.push(reference);
        }
        file.content.push_str(&value);
        file.secret |= secret;
    }
    Ok(file)
}

/// A display template parsed into parts, and the Service names that matched no producer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedTemplate {
    pub parts: Vec<ValuePart>,
    /// Names in `${{ name.KEY }}` that `lineage` did not know; their tokens stay text.
    pub unresolved: Vec<String>,
    /// Whether a `${{` (not `$${{`) has no `}}` after it.
    pub unterminated: bool,
    /// Whether a closed `${{ … }}` is not a reference, such as `${{ a-b }}`; it stays text.
    pub malformed: bool,
}

/// Parse display text into template parts: `${{ KEY }}` reads the owner's own
/// variable, `${{ name.KEY }}` Service `name`'s (by the lineage `lineage` returns),
/// and `$${{` is a literal `${{`. Anything malformed stays text. Inverse of
/// [`super::render_variable_parts`].
#[must_use]
pub fn parse_variable_template(
    text: &str,
    lineage: impl Fn(&str) -> Option<String>,
) -> ParsedTemplate {
    let mut parts = Vec::new();
    let mut unresolved = Vec::new();
    let mut unterminated = false;
    let mut malformed = false;
    let mut pending = String::new();
    let mut rest = text;
    let last_close = text.rfind("}}");
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("$${{") {
            pending.push_str("${{");
            rest = after;
            continue;
        }
        let Some(after) = rest.strip_prefix("${{") else {
            let mut chars = rest.chars();
            pending.extend(chars.next());
            rest = chars.as_str();
            continue;
        };
        let Some((owner, key, after)) = template_token(after) else {
            let closed = last_close.is_some_and(|at| at >= text.len() - after.len());
            unterminated |= !closed;
            malformed |= closed;
            pending.push_str("${{");
            rest = after;
            continue;
        };
        let resolved = match owner {
            None => Some(ValuePartOwner::Self_),
            Some(name) => lineage(name).map(|lineage_id| ValuePartOwner::Service { lineage_id }),
        };
        match (resolved, owner) {
            (Some(owner), _) => {
                if !pending.is_empty() {
                    parts.push(ValuePart::Text {
                        value: std::mem::take(&mut pending),
                    });
                }
                parts.push(ValuePart::Ref {
                    owner,
                    key: key.to_owned(),
                });
            }
            (None, name) => {
                unresolved.extend(name.map(str::to_owned));
                pending.push_str(rest.get(..rest.len() - after.len()).unwrap_or_default());
            }
        }
        rest = after;
    }
    if !pending.is_empty() {
        parts.push(ValuePart::Text { value: pending });
    }
    ParsedTemplate {
        parts,
        unresolved,
        unterminated,
        malformed,
    }
}

/// `[name.]KEY }}` after a `${{`, with optional whitespace inside the braces.
/// Returns the owner name, the key and the text after the token.
fn template_token(text: &str) -> Option<(Option<&str>, &str, &str)> {
    let body = text.trim_start();
    let (first, after) = identifier(body, '-');
    let (owner, key, after) = match after.strip_prefix('.') {
        Some(after) if first.starts_with(|c: char| c.is_ascii_alphanumeric()) => {
            let (key, after) = identifier(after, '_');
            (Some(first), key, after)
        }
        Some(_) => return None,
        None => (None, first, after),
    };
    let valid_key =
        key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && !key.contains('-');
    let after = after.trim_start().strip_prefix("}}")?;
    valid_key.then_some((owner, key, after))
}

/// Split off the leading run of ASCII alphanumerics, `_` and `extra`.
fn identifier(text: &str, extra: char) -> (&str, &str) {
    let end = text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == extra))
        .unwrap_or(text.len());
    text.split_at(end)
}

fn resolve_parts(
    parts: &[ValuePart],
    self_owner_id: &str,
    producers: &[VariableProducer],
    warnings: &mut Vec<TemplateWarning>,
    memo: &mut BTreeMap<(String, String), (String, bool)>,
    stack: &mut Vec<(String, String)>,
) -> Result<(String, bool), Vec<String>> {
    let mut value = String::new();
    let mut secret = false;
    for part in parts {
        let ValuePart::Ref { owner, key } = part else {
            if let ValuePart::Text { value: text } = part {
                value.push_str(text);
            }
            continue;
        };
        // ponytail: linear lookup; index by owner/key if large environments make it measurable.
        let found = producers.iter().rev().find(|producer| {
            producer.key == *key
                && match owner {
                    ValuePartOwner::Self_ => producer.owner_id == self_owner_id,
                    ValuePartOwner::Service { .. } => producer.owner == *owner,
                }
        });
        let Some(found) = found else {
            warnings.push(TemplateWarning {
                kind: "missing".into(),
                owner_id: matches!(owner, ValuePartOwner::Self_).then(|| self_owner_id.into()),
                key: key.clone(),
            });
            continue;
        };
        let id = (found.owner_id.clone(), key.clone());
        if stack.contains(&id) {
            return Err(stack
                .iter()
                .chain(std::iter::once(&id))
                .map(|(owner, key)| format!("{owner}::{key}"))
                .collect());
        }
        let resolved = if let Some(cached) = memo.get(&id) {
            cached.clone()
        } else {
            let resolved = match &found.value {
                ResolverValue::Literal { value } => (value.clone(), false),
                ResolverValue::Secret { value } => (value.clone(), true),
                ResolverValue::Template { parts } => {
                    stack.push(id.clone());
                    let result =
                        resolve_parts(parts, &found.owner_id, producers, warnings, memo, stack)?;
                    stack.pop();
                    result
                }
            };
            memo.insert(id, resolved.clone());
            resolved
        };
        value.push_str(&resolved.0);
        secret |= resolved.1;
    }
    Ok((value, secret))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ParsedTemplate {
        parse_variable_template(text, |name| (name == "db").then(|| "lineage-db".to_owned()))
    }

    fn text(value: &str) -> ValuePart {
        ValuePart::Text {
            value: value.into(),
        }
    }

    // The dashboard's `parseDisplayToParts` cases.
    #[test]
    fn display_text_parses_like_the_dashboard() {
        let db = ValuePartOwner::Service {
            lineage_id: "lineage-db".into(),
        };
        let reference = |owner: &ValuePartOwner, key: &str| ValuePart::Ref {
            owner: owner.clone(),
            key: key.into(),
        };
        let display = "postgres://${{ db.USER }}:${{db.PASSWORD}}@${{ HOST }}/app";
        assert_eq!(
            parse(display).parts,
            [
                text("postgres://"),
                reference(&db, "USER"),
                text(":"),
                reference(&db, "PASSWORD"),
                text("@"),
                reference(&ValuePartOwner::Self_, "HOST"),
                text("/app"),
            ]
        );
        let slugs = [("lineage-db".to_owned(), "db".to_owned())].into();
        assert_eq!(
            super::super::render_variable_parts(&parse("a ${{ db.USER }} $${{ b").parts, &slugs),
            "a ${{ db.USER }} $${{ b"
        );
        let ghost = parse("x=${{ ghost.Y }}");
        assert_eq!(ghost.parts, [text("x=${{ ghost.Y }}")]);
        assert_eq!(ghost.unresolved, ["ghost"]);
        assert_eq!(parse("echo $${{ FOO }}").parts, [text("echo ${{ FOO }}")]);
        for malformed in [
            "${{ not-valid",
            "${{ web. }}",
            "${{ _x.Y }}",
            "${{ 9X }}",
            "${{ a-b }}",
        ] {
            assert_eq!(parse(malformed).parts, [text(malformed)], "{malformed}");
            assert_eq!(
                parse(malformed).malformed,
                malformed != "${{ not-valid",
                "{malformed}"
            );
        }
        assert!(!parse("a ${{ db.USER }} $${{ b }}").malformed);
        assert!(parse("${{ db.URL").unterminated);
        assert!(!parse("${{ oops ${{ db.URL }}").unterminated);
        assert!(!parse("$${{ literal").unterminated);
        assert!(parse("").parts.is_empty());
        assert_eq!(parse("é${{ K }}ü").parts.last(), Some(&text("ü")));
    }
}
