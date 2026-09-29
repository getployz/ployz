//! The settings catalog as JSON Schema 2020-12: what `schema`, `explain` and
//! completion show. It is rendered from the Setting types in [`crate::settings`],
//! so the catalog, `get` and validation never disagree. Keys come out sorted.

use ployz_core::RpcError;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use ts_rs::TS;

use crate::error;
use crate::settings::{ServiceSetting, SettingPath, Target};
use crate::variables;

/// Bumped whenever a published path, type or meaning changes.
pub const CATALOG_VERSION: u32 = 1;

const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// A node name: a lowercase DNS label.
const NODE_NAME: &str = "^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$";

/// A variable name, as stored.
const VARIABLE_KEY: &str = "^[A-Z_][A-Z0-9_]{0,127}$";

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
    Ok(versioned(path.target().map_or_else(service, target)))
}

/// One Setting: its canonical path and its schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
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
    let Some(one) = parsed.target() else {
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
        schema: target(one),
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
    let mut properties = ServiceSetting::ALL
        .into_iter()
        .map(|one| (one.name().to_owned(), setting(one)))
        .collect::<Map<_, _>>();
    properties.insert(
        "env".to_owned(),
        json!({
            "title": "Variables",
            "description": "Environment variables by name, each at SERVICE.env.KEY. A patch sets the ones it names and keeps the rest; unset deletes one.",
            "type": "object",
            "patternProperties": { VARIABLE_KEY: variables::schema() },
            "additionalProperties": false,
            "examples": [{
                "LOG_LEVEL": "info",
                "PUBLIC_URL": { "value": "http://${{ PLOYZ_PRIVATE_DOMAIN }}", "exported": true },
            }],
            "x-ployz-apply": "staged",
            "x-ployz-secret": true,
            "x-ployz-data-loss": false,
        }),
    );
    json!({
        "title": "Service",
        "description": "A Service's Settings. `get SERVICE --json` prints them as `values`; `set SERVICE --patch` takes the same shape.",
        "type": "object",
        "properties": properties,
        "additionalProperties": false,
    })
}

fn target(target: &Target) -> Value {
    match target {
        Target::Setting(one) => setting(*one),
        Target::Variable(_) => variables::schema(),
        Target::Exported(_) => json!({
            "title": "Exported",
            "description": "Whether other Services are meant to reference this variable as ${{ service.KEY }}.",
            "type": "boolean",
            "default": false,
            "examples": [true],
            "x-ployz-apply": "staged",
            "x-ployz-secret": false,
            "x-ployz-data-loss": false,
        }),
    }
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
    object.insert(
        "x-ployz-secret".into(),
        json!(one == ServiceSetting::RegistryCredential),
    );
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
        let variable = [
            ("web.env.K".to_owned(), schema(Some("web.env.K")).unwrap()),
            (
                "web.env.K.exported".to_owned(),
                schema(Some("web.env.K.exported")).unwrap(),
            ),
        ];
        let properties = service["properties"].as_object().unwrap().clone();
        for (name, property) in properties.into_iter().chain(variable) {
            for example in property["examples"].as_array().unwrap() {
                assert!(satisfies(example, &property), "{name}: {example}");
            }
            if let Some(default) = property.get("default") {
                assert!(satisfies(default, &property), "{name} default");
            }
        }
    }

    /// The JSON Schema keywords the catalog emits.
    fn satisfies(value: &Value, schema: &Value) -> bool {
        if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
            return options.iter().filter(|one| satisfies(value, one)).count() == 1;
        }
        if let Some(constant) = schema.get("const") {
            return value == constant;
        }
        if schema["type"] == "object" {
            let Some(object) = value.as_object() else {
                return false;
            };
            let required = schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .all(|key| object.contains_key(key.as_str().unwrap()));
            return required
                && object.iter().all(|(key, value)| {
                    let own = schema
                        .get("properties")
                        .and_then(|properties| properties.get(key));
                    // Every pattern is a name pattern; the test does not check names.
                    let pattern = schema
                        .get("patternProperties")
                        .and_then(|patterns| patterns.as_object()?.values().next());
                    own.or(pattern)
                        .is_some_and(|schema| satisfies(value, schema))
                });
        }
        if schema["type"] == "boolean" {
            return value.is_boolean();
        }
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
        assert_eq!(
            complete("web.re"),
            [
                "web.registryCredential",
                "web.replicas",
                "web.restartPolicy",
                "web.repository"
            ]
        );
        assert!(complete("web").is_empty());
    }
}
