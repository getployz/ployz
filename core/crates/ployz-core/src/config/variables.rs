//! Template resolution over supplied producer facts. No storage or provider execution.
use super::{ValuePart, ValuePartOwner};
use crate::{EnvPart, EnvValue, SealedValue, Template};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use ts_rs::TS;

/// Plaintext inputs belong only to an authorized server-side resolution call.
#[derive(Clone, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ResolverValue {
    Literal {
        value: String,
    },
    Secret {
        value: String,
    },
    Template {
        parts: Vec<ValuePart>,
    },
    /// A value the browser sealed for the Environment resolving: it reaches what
    /// references it as a Sealed part, opened only on the Machine.
    Sealed {
        value: SealedValue,
    },
    /// A secret without a value: what references it is skipped.
    NotSet,
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

/// Resolved text with secret provenance, a reference to a secret without a value,
/// or the cycle that prevents resolution. Text that reads a missing producer
/// anywhere resolves empty, with a warning for each one. A value that reads a
/// Sealed part is a Template holding it.
#[derive(Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResolveVariablesResult {
    Resolved {
        value: EnvValue,
        secret: bool,
        warnings: Vec<TemplateWarning>,
    },
    NotSet {
        /// The missing producers it also reads.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(optional, as = "Option<Vec<TemplateWarning>>")]
        warnings: Vec<TemplateWarning>,
    },
    Cycle {
        path: Vec<String>,
    },
}

/// What reached a resolved value, ordered: the most telling wins.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
enum Taint {
    #[default]
    Plain,
    Secret,
    /// It reads a missing producer.
    Broken,
    NotSet,
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
        Ok((_, Taint::NotSet)) => ResolveVariablesResult::NotSet { warnings },
        Ok((value, taint)) => ResolveVariablesResult::Resolved {
            value: value.into_value(),
            secret: taint == Taint::Secret,
            warnings,
        },
        Err(path) => ResolveVariablesResult::Cycle { path },
    }
}

/// A Config file with its references resolved. Its `Debug` leaves the content out.
#[derive(Clone, Eq, PartialEq)]
pub struct ResolvedConfigFile {
    pub content: String,
    /// Whether a secret reached `content`. One without a value reaches it empty.
    pub secret: bool,
    /// References whose value may break the file's syntax, each once.
    pub fragile: Vec<FragileReference>,
    /// References that are, or read, a secret without a value, each once.
    pub not_set: Vec<FragileReference>,
    /// References that are, or read, a variable no producer has, each once. Each
    /// reaches `content` empty.
    pub broken: Vec<FragileReference>,
    /// References that are, or read, a value sealed in the browser, each once. A
    /// file can't hold one: nothing of it reaches `content`.
    pub sealed: Vec<FragileReference>,
}

impl std::fmt::Debug for ResolvedConfigFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedConfigFile")
            .field("content_len", &self.content.len())
            .field("secret", &self.secret)
            .field("fragile", &self.fragile)
            .field("not_set", &self.not_set)
            .field("broken", &self.broken)
            .field("sealed", &self.sealed)
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
    let mut stack = Vec::new();
    let mut file = ResolvedConfigFile {
        content: String::new(),
        secret: false,
        fragile: Vec::new(),
        not_set: Vec::new(),
        broken: Vec::new(),
        sealed: Vec::new(),
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
        // Each reference resolves afresh, so every one that reads a missing
        // variable reports it, not only the first.
        let mut warnings = Vec::new();
        let mut memo = BTreeMap::new();
        let (assembled, taint) = resolve_parts(
            std::slice::from_ref(part),
            "",
            producers,
            &mut warnings,
            &mut memo,
            &mut stack,
        )?;
        let reference = FragileReference {
            owner: owner.clone(),
            key: key.clone(),
        };
        let Some(value) = assembled.text() else {
            if !file.sealed.contains(&reference) {
                file.sealed.push(reference);
            }
            file.secret = true;
            continue;
        };
        let newline = value.contains('\n');
        let breaks = quote.is_some_and(|open| newline || value.contains(open)) || (yaml && newline);
        if taint == Taint::NotSet && !file.not_set.contains(&reference) {
            file.not_set.push(reference.clone());
        }
        if !warnings.is_empty() && !file.broken.contains(&reference) {
            file.broken.push(reference.clone());
        }
        if breaks && !file.fragile.contains(&reference) {
            file.fragile.push(reference);
        }
        file.content.push_str(value);
        file.secret |= matches!(taint, Taint::Secret | Taint::NotSet);
    }
    Ok(file)
}

/// How a reference renders the Service it reads once that Service is gone and nothing
/// at hand knows it, as in `${{ <deleted>.URL }}`.
pub const DELETED_OWNER: &str = "<deleted>";

/// How a reference renders a removed Service last called `name`: `<deleted redis>`,
/// and from the second removed Service shown alike, `#nth`, as in `<deleted redis#2>`.
#[must_use]
pub fn deleted_owner(name: &str, nth: usize) -> String {
    if nth <= 1 {
        format!("<deleted {name}>")
    } else {
        format!("<deleted {name}#{nth}>")
    }
}

/// The hex digits of [`deleted_lineage`]'s label.
const LINEAGE_DIGITS: usize = 8;

/// How a reference renders a removed Service no record names: by the last hex digits
/// of its lineage, as in `<deleted #3f2a9c01>`, so its label depends on nothing that
/// changes. The `#` keeps it apart from a Service called `3f2a9c01`.
#[must_use]
pub fn deleted_lineage(lineage: &str) -> String {
    let digits: Vec<char> = lineage
        .chars()
        .filter(char::is_ascii_hexdigit)
        .map(|digit| digit.to_ascii_lowercase())
        .collect();
    let tail: String = digits
        .iter()
        .skip(digits.len().saturating_sub(LINEAGE_DIGITS))
        .collect();
    format!("<deleted #{tail:0>LINEAGE_DIGITS$}>")
}

/// Whether `token` is a removed Service as [`deleted_owner`] or [`deleted_lineage`]
/// renders it.
#[must_use]
pub fn is_deleted_owner(token: &str) -> bool {
    deleted_token(token).is_some_and(|(_, rest)| rest.is_empty())
}

/// The removed Service's token `text` starts with, as [`deleted_owner`] or
/// [`deleted_lineage`] renders it, and the text after it. Reads no further than the
/// token.
fn deleted_token(text: &str) -> Option<(&str, &str)> {
    let mut rest = text.strip_prefix("<deleted")?;
    if let Some(label) = rest.strip_prefix(' ') {
        let (name, after) = identifier(label, '-');
        if !name.is_empty() && !name.starts_with(|c: char| c.is_ascii_alphanumeric()) {
            return None;
        }
        rest = match after.strip_prefix('#') {
            Some(lineage) if name.is_empty() => {
                let (digits, after) = lineage.split_at(
                    lineage
                        .find(|c: char| !matches!(c, '0'..='9' | 'a'..='f'))
                        .unwrap_or(lineage.len()),
                );
                if digits.len() != LINEAGE_DIGITS {
                    return None;
                }
                after
            }
            Some(nth) => {
                let (digits, after) =
                    nth.split_at(nth.find(|c: char| !c.is_ascii_digit()).unwrap_or(nth.len()));
                if digits.is_empty() || digits.starts_with('0') {
                    return None;
                }
                after
            }
            None if name.is_empty() => return None,
            None => after,
        };
    }
    let after = rest.strip_prefix('>')?;
    Some(text.split_at(text.len() - after.len()))
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
/// variable, `${{ name.KEY }}` Service `name`'s (by the lineage `lineage` returns
/// for `name` and `KEY`, a removed Service's token included), and `$${{` is a literal
/// `${{`. Anything malformed stays text. Inverse of [`super::render_variable_parts`].
#[must_use]
pub fn parse_variable_template(
    text: &str,
    mut lineage: impl FnMut(&str, &str) -> Option<String>,
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
            Some(name) => {
                lineage(name, key).map(|lineage_id| ValuePartOwner::Service { lineage_id })
            }
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
    let deleted = deleted_token(body);
    let (first, after) = deleted.unwrap_or_else(|| identifier(body, '-'));
    let (owner, key, after) = match after.strip_prefix('.') {
        Some(after)
            if deleted.is_some() || first.starts_with(|c: char| c.is_ascii_alphanumeric()) =>
        {
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

/// A value as its parts resolve: text, with each Sealed part where it falls.
#[derive(Clone, Default)]
struct Assembled(Vec<EnvPart>);

impl Assembled {
    fn push_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        match self.0.last_mut() {
            Some(EnvPart::Text(last)) => last.push_str(text),
            _ => self.0.push(EnvPart::Text(text.to_owned())),
        }
    }

    fn push(&mut self, other: &Self) {
        for part in &other.0 {
            match part {
                EnvPart::Text(text) => self.push_text(text),
                EnvPart::Sealed(value) => self.0.push(EnvPart::Sealed(value.clone())),
            }
        }
    }

    /// The text, when no Sealed part reached it.
    fn text(&self) -> Option<&str> {
        match self.0.as_slice() {
            [] => Some(""),
            [EnvPart::Text(text)] => Some(text),
            _ => None,
        }
    }

    /// Text, or a Template once a Sealed part reached it: only a variable's own
    /// Sealed value is bound to its name, so a part read through a reference
    /// opens as a Template part.
    fn into_value(self) -> EnvValue {
        match self.text() {
            Some(text) => EnvValue::Text(text.to_owned()),
            None => EnvValue::Template(Template { template: self.0 }),
        }
    }
}

fn resolve_parts(
    parts: &[ValuePart],
    self_owner_id: &str,
    producers: &[VariableProducer],
    warnings: &mut Vec<TemplateWarning>,
    memo: &mut BTreeMap<(String, String), (Assembled, Taint)>,
    stack: &mut Vec<(String, String)>,
) -> Result<(Assembled, Taint), Vec<String>> {
    let mut value = Assembled::default();
    let mut taint = Taint::Plain;
    for part in parts {
        let ValuePart::Ref { owner, key } = part else {
            if let ValuePart::Text { value: text } = part {
                value.push_text(text);
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
            taint = taint.max(Taint::Broken);
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
            let text = |text: &str| Assembled(vec![EnvPart::Text(text.to_owned())]);
            let resolved = match &found.value {
                ResolverValue::Literal { value } => (text(value), Taint::Plain),
                ResolverValue::Secret { value } => (text(value), Taint::Secret),
                ResolverValue::Sealed { value } => (
                    Assembled(vec![EnvPart::Sealed(value.clone())]),
                    Taint::Secret,
                ),
                ResolverValue::NotSet => (Assembled::default(), Taint::NotSet),
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
        value.push(&resolved.0);
        taint = taint.max(resolved.1);
    }
    if taint >= Taint::Broken {
        value = Assembled::default();
    }
    Ok((value, taint))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> ParsedTemplate {
        parse_variable_template(text, |name, _| {
            (name == "db").then(|| "lineage-db".to_owned())
        })
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
        let shown = [
            DELETED_OWNER.to_owned(),
            deleted_owner("redis-2", 1),
            deleted_owner("redis-2", 2),
            deleted_lineage("00000000-0000-4000-8000-00003F2A9C01"),
        ];
        assert_eq!(
            shown,
            [
                DELETED_OWNER,
                "<deleted redis-2>",
                "<deleted redis-2#2>",
                "<deleted #3f2a9c01>"
            ]
        );
        let gone = parse(
            &shown
                .iter()
                .map(|owner| format!("${{{{ {owner}.Y }}}}"))
                .collect::<String>(),
        );
        assert_eq!((gone.unresolved, gone.malformed), (shown.to_vec(), false));
        for malformed in [
            "<deleted -x>",
            "<deletedx>",
            "<deleted >",
            "<deleted x#0>",
            "<deleted x#>",
            "<deleted #2>",
            "<deleted #3F2A9C01>",
            "<deleted #3f2a9c012>",
        ] {
            assert!(
                parse(&format!("${{{{ {malformed}.Y }}}}")).malformed,
                "{malformed}"
            );
            assert!(!is_deleted_owner(malformed), "{malformed}");
        }
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
