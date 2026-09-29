//! The settings catalog as JSON Schema 2020-12: what `schema`, `explain` and
//! completion show. It is rendered from the Setting types in [`crate::settings`],
//! so the catalog, `get` and validation never disagree. Keys come out sorted.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::error;
use crate::settings::{ServiceSetting, SettingPath};

/// Bumped whenever a published path, type or meaning changes.
pub const CATALOG_VERSION: u32 = 1;

const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// A node name: a lowercase DNS label.
const NODE_NAME: &str = "^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$";

/// The whole catalog, or the part at `SERVICE` or `SERVICE.SETTING`.
///
/// # Errors
/// Returns `invalid_argument` for a malformed path or an unknown Setting, with
/// `did_you_mean` and `valid_children`.
pub fn schema(path: Option<&str>) -> Result<Value, RpcError> {
    let Some(path) = path else {
        return Ok(versioned(json!({
            "title": "Ployz Environment Settings",
            "description": "An Environment's Settings by Service name. A path is SERVICE.SETTING, for example web.replicas.",
            "type": "object",
            "patternProperties": { NODE_NAME: { "$ref": "#/$defs/service" } },
            "additionalProperties": false,
            "$defs": { "service": service() },
        })));
    };
    let path = SettingPath::parse(path)?;
    Ok(versioned(path.setting().map_or_else(service, setting)))
}

/// One Setting: its canonical path and its schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Explained {
    /// The Setting's canonical path.
    pub path: SettingPath,
    /// Its JSON Schema.
    pub schema: Value,
}

/// Describe the Setting at `SERVICE.SETTING`.
///
/// # Errors
/// Returns `invalid_argument` for a path that names no Setting, with
/// `did_you_mean` and `valid_children`.
pub fn explain(path: &str) -> Result<Explained, RpcError> {
    let parsed = SettingPath::parse(path)?;
    let Some(one) = parsed.setting() else {
        let names = ServiceSetting::ALL.map(ServiceSetting::name);
        return Err(error::invalid(
            "Name a Setting: SERVICE.SETTING",
            json!({
                "valid_children": names,
                "example": format!("{}.replicas", parsed.service()),
            }),
        ));
    };
    Ok(Explained {
        schema: setting(one),
        path: parsed,
    })
}

/// Setting paths that complete `input` once it names a Service: `web.re` gives
/// `web.replicas` and `web.restartPolicy`. Service names come from the Store.
#[must_use]
pub fn complete(input: &str) -> Vec<String> {
    let Some((service, prefix)) = input.split_once('.') else {
        return Vec::new();
    };
    ServiceSetting::ALL
        .into_iter()
        .filter(|one| one.name().starts_with(prefix))
        .map(|one| format!("{service}.{}", one.name()))
        .collect()
}

fn versioned(mut schema: Value) -> Value {
    if let Some(object) = schema.as_object_mut() {
        object.insert("$schema".into(), json!(DIALECT));
        object.insert("x-ployz-version".into(), json!(CATALOG_VERSION));
    }
    schema
}

fn service() -> Value {
    let properties = ServiceSetting::ALL
        .into_iter()
        .map(|one| (one.name().to_owned(), setting(one)))
        .collect::<Map<_, _>>();
    json!({
        "title": "Service",
        "description": "A Service's Settings. `get SERVICE --json` prints them as `values`; `set SERVICE --patch` takes the same shape.",
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    })
}

fn setting(one: ServiceSetting) -> Value {
    let mut schema = one.expected();
    let object = schema.as_object_mut().expect("expected values are objects");
    object.insert("title".into(), json!(one.label()));
    object.insert("description".into(), json!(one.description()));
    if !one.default().is_null() {
        object.insert("default".into(), one.default());
    }
    object.insert("examples".into(), one.examples());
    object.insert("x-ployz-apply".into(), json!(one.apply()));
    object.insert("x-ployz-secret".into(), json!(false));
    object.insert("x-ployz-data-loss".into(), json!(false));
    schema
}

#[cfg(test)]
#[expect(
    clippy::indexing_slicing,
    reason = "Fixed test fixtures use indexing; missing entries must fail the test."
)]
mod tests {
    use super::*;

    #[test]
    fn output_is_deterministic_and_versioned() {
        let first = serde_json::to_string(&schema(None).unwrap()).unwrap();
        assert_eq!(
            first,
            serde_json::to_string(&schema(None).unwrap()).unwrap()
        );
        let whole = schema(None).unwrap();
        assert_eq!(whole["$schema"], DIALECT);
        assert_eq!(whole["x-ployz-version"], CATALOG_VERSION);
        assert_eq!(
            schema(Some("web.cpuLimit")).unwrap()["title"],
            json!("CPU limit")
        );
        assert_eq!(
            schema(Some("web")).unwrap()["properties"],
            whole["$defs"]["service"]["properties"]
        );
    }

    #[test]
    fn every_example_and_default_satisfies_its_schema() {
        let service = schema(Some("web")).unwrap();
        for (name, property) in service["properties"].as_object().unwrap() {
            for example in property["examples"].as_array().unwrap() {
                assert!(satisfies(example, property), "{name}: {example}");
            }
            if let Some(default) = property.get("default") {
                assert!(satisfies(default, property), "{name} default");
            }
        }
    }

    /// The JSON Schema keywords the catalog emits.
    fn satisfies(value: &Value, schema: &Value) -> bool {
        let number = |key: &str| schema.get(key).and_then(Value::as_f64);
        let typed = match schema["type"].as_str().unwrap() {
            "string" => value.as_str().is_some_and(|text| {
                let length = text.chars().count() as u64;
                schema
                    .get("minLength")
                    .is_none_or(|min| length >= min.as_u64().unwrap())
                    && schema
                        .get("maxLength")
                        .is_none_or(|max| length <= max.as_u64().unwrap())
            }),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            other => panic!("unexpected type {other}"),
        };
        let bounded = value.as_f64().is_none_or(|n| {
            number("minimum").is_none_or(|min| n >= min)
                && number("maximum").is_none_or(|max| n <= max)
                && number("exclusiveMinimum").is_none_or(|min| n > min)
        });
        let listed = schema
            .get("enum")
            .is_none_or(|values| values.as_array().unwrap().contains(value));
        typed && bounded && listed
    }

    #[test]
    fn a_mistyped_path_names_the_fix() {
        let error = explain("web.replica").unwrap_err();
        assert_eq!(error.details["did_you_mean"], "replicas");
        assert_eq!(error.details["valid_children"][0], "cpuLimit");
        let error = explain("web.cpu_limit").unwrap_err();
        assert_eq!(error.details["did_you_mean"], "cpuLimit");
        let error = explain("web.zzzzzz").unwrap_err();
        assert_eq!(error.details["did_you_mean"], Value::Null);
        assert!(explain("web").is_err());
        assert_eq!(
            explain("web.replicas").unwrap().path.to_string(),
            "web.replicas"
        );
    }

    #[test]
    fn completion_follows_the_service_name() {
        assert_eq!(complete("web.re"), ["web.replicas", "web.restartPolicy"]);
        assert!(complete("web").is_empty());
    }
}
