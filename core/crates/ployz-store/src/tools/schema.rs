//! Input schemas: generated from each tool's input type, then made portable so
//! every model provider's tool format takes them.

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde_json::{Map, Value};

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
                    || (key == "default" && value.is_null())
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

/// The schema at `field`, a dotted path through properties, or the whole
/// schema when the path leads nowhere.
pub(crate) fn at(schema: &Value, field: &str) -> Value {
    let mut here = schema;
    for part in field.split('.').filter(|part| !part.is_empty()) {
        let next = here
            .get("properties")
            .and_then(|properties| properties.get(part))
            .or_else(|| {
                here.get("patternProperties")
                    .and_then(Value::as_object)
                    .and_then(|patterns| patterns.values().next())
            })
            .or_else(|| {
                here.get("additionalProperties")
                    .filter(|value| value.is_object())
            })
            .or_else(|| part.parse::<usize>().ok().and(here.get("items")));
        match next {
            Some(next) => here = next,
            None => return schema.clone(),
        }
    }
    here.clone()
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
