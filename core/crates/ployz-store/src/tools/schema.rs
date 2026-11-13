//! Input schemas: generated from each tool's input type, then made portable so
//! every model provider's tool format takes them.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value};

/// The pattern a GitHub repository `owner/name` input takes.
pub(crate) const REPOSITORY: &str = "^[A-Za-z0-9-]+/[A-Za-z0-9._-]+$";

/// Keywords a portable schema may use.
const KEYWORDS: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "patternProperties",
    "items",
    "enum",
    "const",
    "title",
    "description",
    "default",
    "examples",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "pattern",
    "minItems",
    "maxItems",
    "uniqueItems",
    "anyOf",
    "format",
];

/// `T`'s input schema, portable.
pub(crate) fn input<T: JsonSchema>() -> Value {
    let generator = SchemaSettings::draft2020_12()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.meta_schema = None;
        })
        .into_generator();
    let mut schema = portable(generator.into_root_schema_for::<T>().to_value());
    if let Value::Object(object) = &mut schema {
        object.remove("title");
        object.remove("description");
    }
    schema
}

/// `schema` in the subset every provider takes: no `$schema`, `$ref`, `oneOf`,
/// nullable types, exclusive bounds or `x-ployz-*` annotations. An exclusive
/// bound becomes the inclusive one; the Store still refuses the edge.
#[must_use]
pub fn portable(mut schema: Value) -> Value {
    let defs = match &mut schema {
        Value::Object(object) => object
            .remove("$defs")
            .and_then(|defs| match defs {
                Value::Object(defs) => Some(defs),
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_) => None,
            })
            .unwrap_or_default(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_) => {
            Map::new()
        }
    };
    tidy(&mut schema, &defs);
    schema
}

/// A default an agent assumes anyway: none, nothing, zero or off.
fn says_nothing(value: &Value) -> bool {
    match value {
        Value::Null | Value::Bool(false) => true,
        Value::Number(number) => number.as_u64() == Some(0),
        Value::Array(items) => items.is_empty(),
        Value::Bool(true) | Value::String(_) | Value::Object(_) => false,
    }
}

fn tidy(schema: &mut Value, defs: &Map<String, Value>) {
    match schema {
        Value::Array(items) => items.iter_mut().for_each(|item| tidy(item, defs)),
        Value::Object(object) => {
            if let Some(Value::String(reference)) = object.remove("$ref") {
                let name = reference.rsplit('/').next().unwrap_or_default();
                if let Some(Value::Object(target)) = defs.get(name) {
                    for (key, value) in target {
                        object.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
            }
            object.retain(|key, value| {
                !(key == "$schema"
                    || key.starts_with("x-ployz-")
                    || (key == "default" && says_nothing(value))
                    || (key == "format" && !value.as_str().is_some_and(is_standard_format)))
            });
            if let Some(one_of) = object.remove("oneOf") {
                object.insert("anyOf".into(), one_of);
            }
            for (exclusive, inclusive) in [
                ("exclusiveMinimum", "minimum"),
                ("exclusiveMaximum", "maximum"),
            ] {
                if let Some(bound) = object.remove(exclusive) {
                    object.entry(inclusive).or_insert(bound);
                }
            }
            drop_null(object);
            for (key, value) in object.iter_mut() {
                match (key.as_str(), value) {
                    ("enum" | "const" | "examples" | "default", _) => {}
                    ("properties" | "patternProperties", Value::Object(named)) => {
                        named.values_mut().for_each(|one| tidy(one, defs));
                    }
                    (_, value) => tidy(value, defs),
                }
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// A format every provider knows; schemars' `uint64` and the like are not.
fn is_standard_format(format: &str) -> bool {
    matches!(
        format,
        "date-time" | "date" | "time" | "email" | "uri" | "uuid"
    )
}

/// Take `null` out of a type: an absent field already means none.
fn drop_null(object: &mut Map<String, Value>) {
    if let Some(Value::Array(types)) = object.get_mut("type") {
        types.retain(|one| one != "null");
        if let [only] = types.as_slice() {
            let only = only.clone();
            object.insert("type".into(), only);
        }
    }
    if let Some(Value::Array(any)) = object.get_mut("anyOf") {
        any.retain(|one| one.get("type").and_then(Value::as_str) != Some("null"));
        if let [only] = any.as_slice() {
            let Value::Object(only) = only.clone() else {
                return;
            };
            object.remove("anyOf");
            for (key, value) in only {
                object.entry(key).or_insert(value);
            }
        }
    }
}

/// A place in a call's input: the keys and array positions that lead to it. It
/// reads `a.b`, `files[0].x` and `files.0.x` alike, and writes `files[0].x`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct FieldPath(Vec<Segment>);

#[derive(Clone, Debug, Eq, PartialEq)]
enum Segment {
    Key(String),
    Index(usize),
}

impl FieldPath {
    pub(crate) fn parse(path: &str) -> Self {
        let mut segments = Vec::new();
        for part in path.split('.').filter(|part| !part.is_empty()) {
            let (key, brackets) = part.find('[').map_or((part, ""), |at| part.split_at(at));
            let Some(positions) = positions(brackets) else {
                segments.push(Segment::Key(part.to_owned()));
                continue;
            };
            if !key.is_empty() {
                segments.push(match key.parse::<usize>() {
                    Ok(position) if key.bytes().all(|byte| byte.is_ascii_digit()) => {
                        Segment::Index(position)
                    }
                    _ => Segment::Key(key.to_owned()),
                });
            }
            segments.extend(positions.into_iter().map(Segment::Index));
        }
        Self(segments)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn push_key(&mut self, key: &str) {
        self.0.push(Segment::Key(key.to_owned()));
    }

    pub(crate) fn push_index(&mut self, position: usize) {
        self.0.push(Segment::Index(position));
    }

    fn under(mut self, segment: Segment) -> Self {
        self.0.insert(0, segment);
        self
    }
}

/// The positions in `brackets`, such as `[0][1]`; none when it is anything else.
fn positions(mut brackets: &str) -> Option<Vec<usize>> {
    let mut found = Vec::new();
    while let Some(after) = brackets.strip_prefix('[') {
        let (digits, rest) = after.split_once(']')?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        found.push(digits.parse().ok()?);
        brackets = rest;
    }
    brackets.is_empty().then_some(found)
}

impl std::fmt::Display for FieldPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (at, segment) in self.0.iter().enumerate() {
            match segment {
                Segment::Key(key) if at == 0 => f.write_str(key)?,
                Segment::Key(key) => write!(f, ".{key}")?,
                Segment::Index(position) => write!(f, "[{position}]")?,
            }
        }
        Ok(())
    }
}

/// What fits at `field`, a path through properties and array items, stopping
/// where the path leads nowhere. An object shows its keys and their types, not
/// their schemas.
pub(crate) fn at(schema: &Value, field: &FieldPath) -> Value {
    let mut here = reach(schema, field).clone();
    for list in ["properties", "patternProperties"] {
        if let Some(Value::Object(named)) = here.get_mut(list) {
            for property in named.values_mut() {
                *property = outline(property);
            }
        }
    }
    if let Some(items) = here.get_mut("items") {
        *items = outline(items);
    }
    if let Some(rest) = here
        .get_mut("additionalProperties")
        .filter(|rest| rest.is_object())
    {
        *rest = outline(rest);
    }
    here
}

/// `schema`'s type, and its values when it takes only a few.
fn outline(schema: &Value) -> Value {
    let mut kept = Map::new();
    for key in ["type", "enum", "const"] {
        if let Some(value) = schema.get(key) {
            kept.insert(key.into(), value.clone());
        }
    }
    if let Some(Value::Array(options)) = schema.get("anyOf") {
        kept.insert("anyOf".into(), options.iter().map(outline).collect());
    }
    Value::Object(kept)
}

fn reach<'schema>(schema: &'schema Value, field: &FieldPath) -> &'schema Value {
    let mut here = schema;
    for segment in &field.0 {
        let part = match segment {
            Segment::Key(key) => key.clone(),
            Segment::Index(position) => position.to_string(),
        };
        let next = here
            .get("properties")
            .and_then(|properties| properties.get(part.as_str()))
            .or_else(|| {
                here.get("patternProperties")
                    .and_then(Value::as_object)
                    .and_then(|patterns| patterns.values().next())
            })
            .or_else(|| {
                here.get("additionalProperties")
                    .filter(|value| value.is_object())
            })
            .or_else(|| {
                matches!(segment, Segment::Index(_))
                    .then(|| here.get("items"))
                    .flatten()
            });
        match next {
            Some(next) => here = next,
            None => return here,
        }
    }
    here
}

/// The first number in `input` past a bound `schema` declares, as its path and
/// the bound.
pub(crate) fn out_of_range(schema: &Value, input: &Value) -> Option<(FieldPath, String)> {
    match input {
        Value::Object(fields) => fields.iter().find_map(|(name, value)| {
            let inner = schema.get("properties")?.get(name)?;
            out_of_range(inner, value)
                .map(|(path, reason)| (path.under(Segment::Key(name.clone())), reason))
        }),
        Value::Array(items) => {
            let inner = schema.get("items")?;
            items.iter().enumerate().find_map(|(index, value)| {
                out_of_range(inner, value)
                    .map(|(path, reason)| (path.under(Segment::Index(index)), reason))
            })
        }
        Value::Number(number) => {
            let number = number.as_f64()?;
            let bound = |key: &str| schema.get(key).and_then(Value::as_f64);
            if let Some(minimum) = bound("minimum").filter(|minimum| number < *minimum) {
                return Some((FieldPath::default(), format!("at least {minimum}")));
            }
            bound("maximum")
                .filter(|maximum| number > *maximum)
                .map(|maximum| (FieldPath::default(), format!("at most {maximum}")))
        }
        Value::Null | Value::Bool(_) | Value::String(_) => None,
    }
}

/// Where `schema` is not portable, as one line per problem; empty when it is.
#[must_use]
pub fn lint(schema: &Value) -> Vec<String> {
    let mut problems = Vec::new();
    if schema.get("type").and_then(Value::as_str) != Some("object") {
        problems.push("the root is not an object".to_owned());
    }
    for combinator in ["anyOf", "oneOf", "allOf", "not"] {
        if schema.get(combinator).is_some() {
            problems.push(format!("the root has {combinator}"));
        }
    }
    walk(schema, "", &mut problems);
    problems
}

fn walk(schema: &Value, path: &str, problems: &mut Vec<String>) {
    let Value::Object(object) = schema else {
        return;
    };
    for (key, value) in object {
        if !KEYWORDS.contains(&key.as_str()) {
            problems.push(format!("{path}: keyword {key}"));
            continue;
        }
        match key.as_str() {
            "properties" | "patternProperties" => {
                for (name, property) in value.as_object().into_iter().flatten() {
                    let here = format!("{path}.{name}");
                    if property.as_object().is_some_and(Map::is_empty) {
                        problems.push(format!("{here}: an empty schema"));
                    }
                    walk(property, &here, problems);
                }
            }
            "items" | "additionalProperties" => walk(value, &format!("{path}[]"), problems),
            "anyOf" => {
                for option in value.as_array().into_iter().flatten() {
                    walk(option, path, problems);
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{FieldPath, at, out_of_range};

    fn input() -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "x": { "type": "integer", "maximum": 3 } },
                    },
                },
                "labels": {
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                },
            },
        })
    }

    #[test]
    fn a_path_reads_every_spelling_and_writes_one() {
        for (read, written) in [
            ("a.b", "a.b"),
            ("files[0].x", "files[0].x"),
            ("files.0.x", "files[0].x"),
            ("a[0][1]", "a[0][1]"),
            ("[2].x", "[2].x"),
            ("a[x]", "a[x]"),
            ("", ""),
        ] {
            assert_eq!(FieldPath::parse(read).to_string(), written, "{read}");
        }
    }

    #[test]
    fn an_array_element_resolves_the_same_in_every_spelling() {
        let schema = input();
        for field in ["files[0]", "files.0"] {
            assert_eq!(
                at(&schema, &FieldPath::parse(field)),
                json!({ "type": "object", "properties": { "x": { "type": "integer" } } }),
                "{field}"
            );
        }
        for field in ["files[0].x", "files.0.x"] {
            assert_eq!(
                at(&schema, &FieldPath::parse(field)).get("maximum"),
                Some(&json!(3)),
                "{field}"
            );
        }
    }

    #[test]
    fn a_numeric_key_of_a_map_still_reaches_the_map_values() {
        assert_eq!(
            at(&input(), &FieldPath::parse("labels.0")),
            json!({ "type": "string" })
        );
    }

    #[test]
    fn a_range_error_in_an_array_names_the_element_as_a_decode_error_does() {
        let (path, reason) =
            out_of_range(&input(), &json!({ "files": [{ "x": 1 }, { "x": 9 }] })).unwrap();
        assert_eq!(
            (path.to_string().as_str(), reason.as_str()),
            ("files[1].x", "at most 3")
        );
        assert_eq!(at(&input(), &path).get("maximum"), Some(&json!(3)));
    }
}
