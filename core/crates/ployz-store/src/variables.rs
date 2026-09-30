//! Variables: a Service's environment, addressed as `SERVICE.env.KEY`. A value is
//! text, where `${{ KEY }}` and `${{ service.KEY }}` reference other variables, or
//! a secret. Reads show a secret as `{"secret": true}`, and writing that back keeps
//! it; a new secret arrives as `{"secret": "…"}` and is sealed before it is stored.
//! `exported` marks a variable other Services are meant to reference.

use std::collections::BTreeMap;
use std::fmt;

use ployz_core::config::{
    BUILT_IN_VARIABLES, CompiledEnvironmentIntent, ResolveVariablesInput, ResolveVariablesResult, ResolverValue,
    SavedServiceIntent, SavedVariableIntent, SavedVariableValue, ServiceEnvValue, ValuePart,
    ValuePartOwner, VariableProducer, parse_variable_template, render_variable_parts,
    resolve_variables,
};
use ployz_core::{RpcError, ServiceName};
use serde_json::{Map, Value, json};

use crate::error;
use crate::scope::Environment;
use crate::sealing::{SealingKey, plain_fingerprint};

/// A variable's name: letters, digits and `_`, not starting with a digit, at most
/// 128 characters. Stored uppercase, as the dashboard stores it.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(crate) struct VariableKey(String);

impl VariableKey {
    pub(crate) fn parse(key: &str) -> Result<Self, RpcError> {
        let valid = key.len() <= 128
            && key.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(error::invalid(
                "A variable name is letters, digits and _, not starting with a digit",
                json!({ "example": "DATABASE_URL" }),
            ));
        }
        Ok(Self(key.to_ascii_uppercase()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for VariableKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// What a new value asks for.
enum Input {
    Text(String),
    /// `{"secret": true}`: keep the stored secret.
    Keep,
    Seal(String),
}

/// A variable's value as reads show it: text with references by Service name, or
/// `{"secret": true}`.
pub(crate) fn shown(variable: &SavedVariableIntent, names: &BTreeMap<String, String>) -> Value {
    match &variable.value {
        SavedVariableValue::Secret { .. } => json!({ "secret": true }),
        SavedVariableValue::Literal { value } => json!(render_variable_parts(
            &[ValuePart::Text {
                value: value.clone()
            }],
            names
        )),
        SavedVariableValue::Template { parts } => json!(render_variable_parts(parts, names)),
    }
}

/// A Service's variables in the shape `set SERVICE --patch` takes under `env`: the
/// value, or `{"value": …, "exported": true}` for an exported one.
pub(crate) fn patch_values(
    service: &SavedServiceIntent,
    names: &BTreeMap<String, String>,
) -> Map<String, Value> {
    service
        .variables
        .iter()
        .map(|variable| {
            let value = shown(variable, names);
            let value = if variable.exported {
                json!({ "value": value, "exported": true })
            } else {
                value
            };
            (variable.key.clone(), value)
        })
        .collect()
}

/// A Service's variables, sorted by name.
pub(crate) fn sorted(service: &SavedServiceIntent) -> Vec<&SavedVariableIntent> {
    let mut variables = service.variables.iter().collect::<Vec<_>>();
    variables.sort_by(|a, b| a.key.cmp(&b.key));
    variables
}

/// The variable `key` of `service`, or `not_found` naming the close ones.
pub(crate) fn find<'intent>(
    service: &'intent SavedServiceIntent,
    key: &VariableKey,
) -> Result<&'intent SavedVariableIntent, RpcError> {
    service
        .variables
        .iter()
        .find(|variable| variable.key == key.as_str())
        .ok_or_else(|| {
            let keys = service
                .variables
                .iter()
                .map(|variable| variable.key.as_str());
            error::choices(
                format!("{} has no variable {key}", service.slug),
                key.as_str(),
                keys,
            )
        })
}

/// Set variable `key` of `service` from `value`: text, `{"secret": true}` to keep
/// a secret, `{"secret": "…"}` to seal a new one, or `{"value": …, "exported": …}`
/// with either field. Returns whether Working State changed.
pub(crate) fn set(
    environment: &mut Environment,
    service: &ServiceName,
    key: &VariableKey,
    value: Value,
    sealing: &SealingKey,
) -> Result<bool, RpcError> {
    let (input, exported) = match value {
        Value::Object(mut fields) if !fields.contains_key("secret") => {
            if fields
                .keys()
                .any(|field| field != "value" && field != "exported")
            {
                return Err(invalid(key, "expected only value and exported"));
            }
            let exported = fields
                .remove("exported")
                .map(|flag| exported_flag(key, flag));
            let input = fields.remove("value").map(|value| input(key, value));
            (input.transpose()?, exported.transpose()?)
        }
        value @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_)) => (Some(input(key, value)?), None),
    };
    let names = environment.names();
    let current = environment
        .service(service)?
        .variables
        .iter()
        .find(|variable| variable.key == key.as_str());
    let (value, fingerprint) = match (input, current) {
        (None, None) => return Err(invalid(key, "a new variable needs a value")),
        (None, Some(current)) => (current.value.clone(), current.value_fingerprint.clone()),
        (Some(Input::Keep), Some(current)) if secret(current) => {
            (current.value.clone(), current.value_fingerprint.clone())
        }
        (Some(Input::Keep), _) => return Err(invalid(key, "it holds no secret to keep")),
        (Some(Input::Text(_)), Some(current)) if secret(current) => {
            return Err(invalid(
                key,
                "it is secret, and a secret never becomes plain text: set it with --secret",
            ));
        }
        (Some(Input::Text(text)), _) => text_value(key, &text, &names)?,
        (Some(Input::Seal(plaintext)), current) => {
            let fingerprint = sealing.fingerprint(&plaintext);
            match current {
                // Sealing the same secret again changes nothing.
                Some(current) if secret(current) && current.value_fingerprint == fingerprint => {
                    (current.value.clone(), fingerprint)
                }
                Some(_) | None => (
                    SavedVariableValue::Secret {
                        encrypted_value: Some(sealing.seal(&plaintext)),
                    },
                    fingerprint,
                ),
            }
        }
    };
    let next = SavedVariableIntent {
        id: current.map_or_else(
            || uuid::Uuid::new_v4().to_string(),
            |current| current.id.clone(),
        ),
        key: key.as_str().to_owned(),
        description: current.and_then(|current| current.description.clone()),
        exported: exported.unwrap_or_else(|| current.is_some_and(|current| current.exported)),
        value_fingerprint: fingerprint,
        value,
    };
    if current == Some(&next) {
        return Ok(false);
    }
    let variables = &mut environment.service_mut(service)?.variables;
    match variables
        .iter_mut()
        .find(|variable| variable.key == next.key)
    {
        Some(variable) => *variable = next,
        None => variables.push(next),
    }
    Ok(true)
}

/// Text as a variable's value, referencing Services by their names in `names`
/// (lineage → name), with its fingerprint.
pub(crate) fn text_value(
    key: &VariableKey,
    text: &str,
    names: &BTreeMap<String, String>,
) -> Result<(SavedVariableValue, String), RpcError> {
    let template = parse_variable_template(text, |name| {
        names
            .iter()
            .find(|(_, slug)| slug.as_str() == name)
            .map(|(lineage, _)| lineage.clone())
    });
    if template.unterminated {
        return Err(invalid(
            key,
            "a `${{` has no closing `}}`: finish the reference, or write `$${{` for a literal `${{`",
        ));
    }
    if let Some(name) = template.unresolved.first() {
        let names = names.values().map(String::as_str);
        return Err(error::invalid(
            format!("{key}: a reference names no Service in this Environment"),
            error::suggest(name, names),
        ));
    }
    let fingerprint = plain_fingerprint(&template.parts);
    let value = match template.parts.as_slice() {
        [] => SavedVariableValue::Literal {
            value: String::new(),
        },
        [ValuePart::Text { value }] => SavedVariableValue::Literal {
            value: value.clone(),
        },
        _ => SavedVariableValue::Template {
            parts: template.parts,
        },
    };
    Ok((value, fingerprint))
}

/// Refuse variable `key` of `service` if it references a variable its Service
/// doesn't have. Checked once an edit is whole, so a batch may set both in any
/// order. A node the Environment uses live isn't checked: its owner provides them.
pub(crate) fn check_references(
    environment: &Environment,
    service: &ServiceName,
    key: &VariableKey,
) -> Result<(), RpcError> {
    let Some(SavedVariableValue::Template { parts }) = environment
        .service(service)?
        .variables
        .iter()
        .find(|variable| variable.key == key.as_str())
        .map(|variable| &variable.value)
    else {
        return Ok(());
    };
    for part in parts {
        let ValuePart::Ref {
            owner: ValuePartOwner::Service { lineage_id },
            key: wanted,
        } = part
        else {
            continue;
        };
        let Some(service) = environment
            .working
            .services
            .iter()
            .find(|service| service.lineage_id == *lineage_id)
        else {
            continue;
        };
        let has = |name: &str| service.variables.iter().any(|v| v.key == name);
        if has(wanted) || BUILT_IN_VARIABLES.contains(&wanted.as_str()) {
            continue;
        }
        let keys: Vec<&str> = sorted(service)
            .into_iter()
            .map(|variable| variable.key.as_str())
            .collect();
        return Err(error::invalid(
            format!(
                "{key}: {} has no variable {wanted}; it has {}, and the built-ins {}",
                service.slug,
                if keys.is_empty() {
                    "none of its own".to_owned()
                } else {
                    keys.join(", ")
                },
                BUILT_IN_VARIABLES.join(", "),
            ),
            json!({
                "variable": key.as_str(),
                "service": service.slug,
                "keys": keys,
                "built_ins": BUILT_IN_VARIABLES,
            }),
        ));
    }
    Ok(())
}

fn secret(variable: &SavedVariableIntent) -> bool {
    matches!(variable.value, SavedVariableValue::Secret { .. })
}

/// Mark variable `key` exported or not. Returns whether Working State changed.
pub(crate) fn set_exported(
    environment: &mut Environment,
    service: &ServiceName,
    key: &VariableKey,
    value: Value,
) -> Result<bool, RpcError> {
    let exported = exported_flag(key, value)?;
    find(environment.service(service)?, key)?;
    let variable = environment
        .service_mut(service)?
        .variables
        .iter_mut()
        .find(|variable| variable.key == key.as_str())
        .ok_or_else(|| error::corrupt("variable"))?;
    let changed = variable.exported != exported;
    variable.exported = exported;
    Ok(changed)
}

/// Delete variable `key`, or with `exported` only unmark it. Absent is no change,
/// so a retried `unset` succeeds.
pub(crate) fn unset(
    environment: &mut Environment,
    service: &ServiceName,
    key: &VariableKey,
    exported: bool,
) -> Result<bool, RpcError> {
    let variables = &mut environment.service_mut(service)?.variables;
    let before = variables.clone();
    if exported {
        for variable in variables.iter_mut().filter(|v| v.key == key.as_str()) {
            variable.exported = false;
        }
    } else {
        variables.retain(|variable| variable.key != key.as_str());
    }
    Ok(*variables != before)
}

fn input(key: &VariableKey, value: Value) -> Result<Input, RpcError> {
    match value {
        Value::String(text) => Ok(Input::Text(text)),
        Value::Object(fields) if fields.len() == 1 => match fields.get("secret") {
            Some(Value::Bool(true)) => Ok(Input::Keep),
            Some(Value::String(plaintext)) if !plaintext.is_empty() => {
                Ok(Input::Seal(plaintext.clone()))
            }
            Some(_) | None => Err(invalid(key, "expected {\"secret\": true} or a new secret")),
        },
        Value::Null => Err(invalid(key, "null never deletes a variable; unset it")),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err(invalid(key, "expected text or {\"secret\": true}"))
        }
    }
}

fn exported_flag(key: &VariableKey, value: Value) -> Result<bool, RpcError> {
    match value {
        Value::Bool(flag) => Ok(flag),
        Value::String(text) if text == "true" || text == "false" => Ok(text == "true"),
        Value::Null | Value::Number(_) | Value::String(_) | Value::Array(_) | Value::Object(_) => {
            Err(invalid(key, "exported is true or false"))
        }
    }
}

/// A refused variable value: never echoed.
fn invalid(key: &VariableKey, message: &str) -> RpcError {
    error::invalid(
        format!("{key}: {message}"),
        json!({
            "variable": key.as_str(),
            "example": "postgres://${{ db.USER }}@db:5432/app",
        }),
    )
}

/// The JSON Schema of a variable in the catalog.
pub(crate) fn schema() -> Value {
    let secret = json!({
        "type": "object",
        "properties": { "secret": { "const": true } },
        "required": ["secret"],
        "additionalProperties": false,
    });
    json!({
        "title": "Variable",
        "description": "An environment variable. Text may reference variables: ${{ KEY }} for this Service's, ${{ service.KEY }} for another's; $${{ is a literal ${{. A secret reads as {\"secret\": true}, and sending that back keeps it; set a new one with --secret or --from-env-file. {\"value\": …, \"exported\": true} marks one other Services are meant to reference.",
        "oneOf": [
            { "type": "string" },
            secret,
            {
                "type": "object",
                "properties": {
                    "value": { "oneOf": [{ "type": "string" }, secret] },
                    "exported": { "type": "boolean", "default": false },
                },
                "additionalProperties": false,
            },
        ],
        "examples": [
            "postgres://${{ db.USER }}@db:5432/app",
            { "secret": true },
            { "value": "info", "exported": true },
        ],
        "x-ployz-apply": "staged",
        "x-ployz-secret": true,
        "x-ployz-data-loss": false,
    })
}

/// Every Service's environment resolved from compiled Saved State, by Service ID.
/// References resolve against the frozen producers. With `unseal`, secrets and
/// values that reference one resolve to plaintext; without, they are left out.
///
/// # Errors
/// Returns `invalid_argument` for a reference cycle, or `internal` when a sealed
/// value can't be opened.
pub(crate) fn resolve(
    compiled: &CompiledEnvironmentIntent,
    unseal: Option<&SealingKey>,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, RpcError> {
    let open = |sealed: Option<&ployz_core::config::EncryptedSecretValue>| match (unseal, sealed) {
        (Some(key), Some(sealed)) => key.open(sealed).map(Some),
        (Some(_), None) => Err(error::corrupt("sealed secret")),
        (None, _) => Ok(None),
    };
    let mut producers = Vec::new();
    for producer in &compiled.variable_producers {
        let value = match &producer.value {
            SavedVariableValue::Literal { value } => ResolverValue::Literal {
                value: value.clone(),
            },
            SavedVariableValue::Template { parts } => ResolverValue::Template {
                parts: parts.clone(),
            },
            // Without the key a secret still marks what references it.
            SavedVariableValue::Secret { encrypted_value } => ResolverValue::Secret {
                value: open(encrypted_value.as_ref())?.unwrap_or_default(),
            },
        };
        producers.push(VariableProducer {
            owner_id: producer.owner_id.clone(),
            owner: ValuePartOwner::Service {
                lineage_id: producer.owner_lineage_id.clone(),
            },
            key: producer.key.clone(),
            value,
        });
    }
    let mut resolved = BTreeMap::new();
    for node in &compiled.node_snapshots {
        let ployz_core::config::CompiledNodeConfig::Service(config) = &node.snapshot.0 else {
            continue;
        };
        let mut env = BTreeMap::new();
        for (key, value) in &config.env {
            let value = match value {
                ServiceEnvValue::Literal { value, parts: None } => Some(value.clone()),
                ServiceEnvValue::Literal {
                    parts: Some(parts), ..
                } => match resolve_variables(&ResolveVariablesInput {
                    parts: parts.clone(),
                    self_owner_id: node.node_id.clone(),
                    producers: producers.clone(),
                }) {
                    ResolveVariablesResult::Resolved { value, secret, .. } => {
                        (unseal.is_some() || !secret).then_some(value)
                    }
                    ResolveVariablesResult::Cycle { path } => {
                        return Err(cycle(compiled, &path));
                    }
                },
                ServiceEnvValue::Secret {
                    encrypted_value, ..
                } => open(encrypted_value.as_ref())?,
            };
            if let Some(value) = value {
                env.insert(key.clone(), value);
            }
        }
        resolved.insert(node.node_id.clone(), env);
    }
    Ok(resolved)
}

/// A reference cycle, named by Service and variable.
fn cycle(compiled: &CompiledEnvironmentIntent, path: &[String]) -> RpcError {
    let names = path
        .iter()
        .map(|step| {
            let (owner, key) = step.split_once("::").unwrap_or(("", step));
            let service = compiled
                .node_snapshots
                .iter()
                .find(|node| node.node_id == owner)
                .and_then(|node| match &node.snapshot.0 {
                    ployz_core::config::CompiledNodeConfig::Service(config) => {
                        Some(config.settings.private_dns.to_string())
                    }
                    ployz_core::config::CompiledNodeConfig::Volume(_) => None,
                })
                .unwrap_or_default();
            format!("{service}.env.{key}")
        })
        .collect::<Vec<_>>();
    error::invalid(
        format!(
            "Variables reference each other in a cycle: {}",
            names.join(" -> ")
        ),
        json!({ "cycle": names }),
    )
}
