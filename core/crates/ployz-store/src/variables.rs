//! Variables: a Service's environment, addressed as `SERVICE.env.KEY`. A value is
//! text, where `${{ KEY }}` and `${{ service.KEY }}` reference other variables, or
//! a secret. Reads show a secret as `{"secret": true}`, and writing that back keeps
//! it; a new secret arrives as `{"secret": "…"}` and is sealed before it is stored.
//! `{"secret": {"generate": true}}` has the Store make a random secret and seal it, so
//! its value never leaves the Store; a variable that already holds a sealed secret keeps it.
//! `${{ secret(32, "a-zA-Z") }}` as the whole value has the Store make a plain value,
//! once per Environment: a copy, Sync or Follow brings only the [`Generator`].
//! `exported` marks a variable other Services are meant to reference.

use std::collections::BTreeMap;
use std::fmt;

use ployz_core::config::{
    ClientSealedValue, CompiledEnvironmentIntent, CompiledNodeConfig, Generator,
    ResolveVariablesInput, ResolveVariablesResult, ResolvedConfigFile, ResolverValue,
    SavedServiceIntent, SavedVariableIntent, SavedVariableValue, SecretCiphertext, ServiceEnvValue,
    ValuePart, ValuePartOwner, VariableProducer, is_deleted_owner, parse_variable_template,
    render_variable_parts, resolve_config_file, resolve_variables,
};
use ployz_core::{ConfigFileName, EnvPart, EnvValue, RpcError, SealedScope, ServiceName, Template};
use ring::rand::{SecureRandom as _, SystemRandom};
use serde_json::{Map, Value, json};

use crate::scope::Environment;
use crate::sealing::plain_fingerprint;
use crate::typed_address::{self, Typed};
use crate::{client_sealed, error, old_cli};

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
                "A variable name is letters, digits and _, not starting with a digit, at most 128 characters",
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
    /// `{"secret": true}` or `{"secret": false}`: keep the stored secret.
    Keep,
    Seal(String),
    /// `{"secret": {"sealedFor": ..., "value": ...}}`: a value the browser sealed for
    /// this Environment, stored as it came.
    ClientSealed(ClientSealedValue),
    /// `{"secret": {"generate": true}}`: a random secret, unless one is stored already.
    Generate,
    /// `${{ secret(…) }}`: a plain value the Store makes, unless one is stored already.
    Generated(Generator),
}

/// A variable's value as reads show it: text with references by Service name,
/// `{"secret": true}`, or `{"secret": false}` for a secret still without a value.
pub(crate) fn shown(variable: &SavedVariableIntent, names: &BTreeMap<String, String>) -> Value {
    shown_value(&variable.value, names)
}

/// `value` as [`shown`] shows a variable holding it.
pub(crate) fn shown_value(value: &SavedVariableValue, names: &BTreeMap<String, String>) -> Value {
    match value {
        SavedVariableValue::Secret { .. } => json!({ "secret": true }),
        SavedVariableValue::SecretWithoutValue => json!({ "secret": false }),
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
/// a secret, a value sealed in the browser, `{"secret": {"generate": true}}` for one
/// `sealer` seals, or `{"value": …, "exported": …}` with either field. A secret's
/// plain text is refused, through `guard` for the CLI. Returns whether Working State
/// changed, and the Typed Addresses in a new value.
pub(crate) fn set(
    environment: &mut Environment,
    service: &ServiceName,
    key: &VariableKey,
    value: Value,
    guard: old_cli::Guard,
    sealer: client_sealed::Sealer<'_>,
) -> Result<(bool, Option<Typed>), RpcError> {
    let (input, exported) = parse(key, value)?;
    let names = environment.names();
    let others: Vec<_> = environment
        .working
        .services
        .iter()
        .filter(|other| other.slug != service.as_str())
        .collect();
    let current = environment
        .service(service)?
        .variables
        .iter()
        .find(|variable| variable.key == key.as_str());
    let kept = |current: &SavedVariableIntent| {
        (
            current.value.clone(),
            current.value_fingerprint.clone(),
            current.generated.clone(),
            None,
        )
    };
    let (value, fingerprint, generated, typed) = match (input, current) {
        (None, None) => return Err(invalid(key, "a new variable needs a value")),
        (None, Some(current)) => kept(current),
        (Some(Input::Keep), Some(current)) if secret(current) => kept(current),
        (Some(Input::Keep), _) => return Err(invalid(key, "it holds no secret to keep")),
        // A retried edit must not rotate a secret its referrers and a database already use.
        (Some(Input::Generate), Some(current))
            if matches!(current.value, SavedVariableValue::Secret { .. }) =>
        {
            kept(current)
        }
        (Some(Input::Generate), _) => {
            let sealed = sealer.seal(
                &environment.summary.id,
                &environment.service(service)?.id,
                key.as_str(),
                &generated_secret()?,
            )?;
            (
                SavedVariableValue::Secret {
                    encrypted_value: Some(sealed.value),
                },
                sealed.fingerprint,
                None,
                None,
            )
        }
        (Some(Input::Text(_) | Input::Generated(_)), Some(current)) if secret(current) => {
            return Err(error::conflict(
                format!(
                    "{key} is a secret, and a secret never becomes plain text: set a new secret, or unset it first"
                ),
                json!({ "variable": key.as_str() }),
            ));
        }
        // Asking again for the generator a variable has keeps its value: a retried edit or
        // a re-applied patch must not rotate a password a database already initialized with.
        (Some(Input::Generated(generator)), Some(current))
            if current.generated.as_ref() == Some(&generator) && !current.awaits_value() =>
        {
            kept(current)
        }
        (Some(Input::Generated(generator)), _) => {
            let (value, fingerprint) = generated_value(&generator)?;
            (value, fingerprint, Some(generator), None)
        }
        (Some(Input::Text(text)), _) => {
            let old = current.map_or(&[][..], |current| current.value.parts());
            let (labels, removed) = (environment.shown_names(), environment.removed_names());
            let shown = Shown {
                labels: &labels,
                removed: &removed,
            };
            let (parts, fingerprint) = text_parts(key, &text, &names, shown, old)?;
            let typed = typed_address::in_plain(&parts, key, &others, &names)?;
            let value = stored(parts);
            // A generated variable's own value, as reads show it, set back stays generated.
            match current {
                Some(current) if current.generated.is_some() && current.value == value => {
                    kept(current)
                }
                Some(_) | None => (value, fingerprint, None, typed),
            }
        }
        (Some(Input::Seal(_)), _) => {
            return Err(guard.refuse(key.as_str(), invalid(key, client_sealed::PLAINTEXT_REFUSED)));
        }
        (Some(Input::ClientSealed(value)), _) => {
            let receiver = client_sealed::Receiver {
                environment: &environment.summary.id,
                service: environment.service(service)?,
            };
            let sealed =
                client_sealed::accept(value, key, &receiver, |message| invalid(key, message))?;
            (
                SavedVariableValue::Secret {
                    encrypted_value: Some(sealed.value),
                },
                sealed.fingerprint,
                None,
                None,
            )
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
        generated,
    };
    if current == Some(&next) {
        return Ok((false, typed));
    }
    let variables = &mut environment.service_mut(service)?.variables;
    match variables
        .iter_mut()
        .find(|variable| variable.key == next.key)
    {
        Some(variable) => *variable = next,
        None => variables.push(next),
    }
    Ok((true, typed))
}

/// What `value` asks [`set`] for `key`, read before Working State: a new value, an
/// exported flag, or both.
fn parse(key: &VariableKey, value: Value) -> Result<(Option<Input>, Option<bool>), RpcError> {
    refuse_reserved(key)?;
    match value {
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
            Ok((input.transpose()?, exported.transpose()?))
        }
        value @ (Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_)) => Ok((Some(input(key, value)?), None)),
    }
}

/// Ployz provides these; a Service's own value would lie about its name and address.
/// PORT is meant to be set. Only setting is refused, so an Environment that already
/// holds one can still unset it.
fn refuse_reserved(key: &VariableKey) -> Result<(), RpcError> {
    if key.as_str() != "PORT" && ployz_core::config::BUILT_IN_VARIABLES.contains(&key.as_str()) {
        return Err(error::invalid(
            format!("{key} is set by Ployz; pick another name"),
            json!({ "reserved": key.as_str() }),
        ));
    }
    Ok(())
}

/// Rename variable `key` of `service` to `to`, keeping its value, secret or not,
/// without opening it. Already renamed is no change, so a retried rename succeeds.
/// The variables in this Environment that reference it, as `${{ KEY }}` in its own
/// Service or `${{ service.KEY }}` anywhere, switch to `to` with
/// `update_references`; without it they refuse the rename with `conflict`.
/// Returns the variables it rewrote, or `None` when nothing changed.
pub(crate) fn rename(
    environment: &mut Environment,
    service: &ServiceName,
    key: &VariableKey,
    to: &VariableKey,
    update_references: bool,
) -> Result<Option<Vec<(ServiceName, VariableKey)>>, RpcError> {
    refuse_reserved(to)?;
    let node = environment.service(service)?;
    let has = |name: &VariableKey| node.variables.iter().any(|v| v.key == name.as_str());
    match (has(key), key != to && has(to)) {
        // A rename onto a name in use would lose one value: refuse, and change neither.
        (true, true) => {
            return Err(error::conflict(
                format!(
                    "{} already has a variable {to}; rename or unset that one first",
                    node.slug
                ),
                json!({ "variable": to.as_str() }),
            ));
        }
        (true, false) if key == to => return Ok(None),
        // Already renamed by an earlier try.
        (false, true) => return Ok(None),
        (false, false) => return find(node, key).map(|_| None),
        (true, false) => {}
    }
    let lineage = node.lineage_id.clone();
    let names = |referrers: &[(ServiceName, VariableKey)]| {
        referrers
            .iter()
            .map(|(service, key)| format!("{service}.{key}"))
            .collect::<Vec<_>>()
    };
    // A Config file doesn't follow a rename, and the rename would break it.
    let reads = |part: &ValuePart| {
        matches!(
            part,
            ValuePart::Ref {
                owner: ValuePartOwner::Service { lineage_id },
                key: reads,
            } if *lineage_id == lineage && reads == key.as_str()
        )
    };
    let files: Vec<String> = environment
        .working
        .configs
        .iter()
        .flat_map(|config| {
            config
                .files
                .iter()
                .filter(|(_, content)| content.content.iter().any(reads))
                .map(move |(file, _)| format!("{}/{file}", config.name))
        })
        .collect();
    if !files.is_empty() {
        return Err(error::conflict(
            format!(
                "{} references {service}.{key}; change those references first",
                files.join(", ")
            ),
            json!({ "files": files, "references": format!("{service}.{key}") }),
        ));
    }
    let referrers = referrers(environment, service, &lineage, key)?;
    if !referrers.is_empty() && !update_references {
        let summary = &environment.summary;
        return Err(error::conflict(
            format!(
                "{service}.{key} is used by {}; rename it with updated references to switch them to {to}",
                names(&referrers).join(", ")
            ),
            json!({
                "variables": names(&referrers),
                "next": format!(
                    "ployz set {service}.env.{key} --rename {to} --update-references --env {} --project {}",
                    summary.name, summary.project
                ),
            }),
        ));
    }
    for referrer in &mut environment.working.services {
        let own = referrer.slug == service.as_str();
        for variable in &mut referrer.variables {
            let SavedVariableValue::Template { parts } = &mut variable.value else {
                continue;
            };
            let mut rewrote = false;
            for part in parts.iter_mut() {
                if let ValuePart::Ref { owner, key: wanted } = part
                    && wanted == key.as_str()
                    && references(owner, own, &lineage)
                {
                    to.as_str().clone_into(wanted);
                    rewrote = true;
                }
            }
            if rewrote {
                variable.value_fingerprint = plain_fingerprint(parts);
            }
        }
    }
    let variable = environment
        .service_mut(service)?
        .variables
        .iter_mut()
        .find(|variable| variable.key == key.as_str())
        .ok_or_else(|| error::corrupt("variable"))?;
    to.as_str().clone_into(&mut variable.key);
    Ok(Some(referrers))
}

/// Whether a reference by `owner`, in a Service that is (`own`) or isn't the one
/// with lineage `lineage`, names that Service.
fn references(owner: &ValuePartOwner, own: bool, lineage: &str) -> bool {
    match owner {
        ValuePartOwner::Self_ => own,
        ValuePartOwner::Service { lineage_id } => lineage_id == lineage,
    }
}

/// The variables of Working State that reference variable `key` of `service`
/// (lineage `lineage`), Service by Service, each sorted by name.
fn referrers(
    environment: &Environment,
    service: &ServiceName,
    lineage: &str,
    key: &VariableKey,
) -> Result<Vec<(ServiceName, VariableKey)>, RpcError> {
    let mut found = Vec::new();
    for referrer in &environment.working.services {
        let own = referrer.slug == service.as_str();
        for variable in sorted(referrer) {
            let SavedVariableValue::Template { parts } = &variable.value else {
                continue;
            };
            let uses = parts.iter().any(|part| {
                matches!(part, ValuePart::Ref { owner, key: wanted }
                    if wanted == key.as_str() && references(owner, own, lineage))
            });
            if uses {
                let name = ServiceName::parse(&referrer.slug)
                    .map_err(|_| error::corrupt("Service name"))?;
                found.push((name, VariableKey::parse(&variable.key)?));
            }
        }
    }
    Ok(found)
}

/// How an edit reads the Services its references name.
#[derive(Clone, Copy)]
pub(crate) struct Shown<'a> {
    /// Each Service by lineage, as reads show it.
    pub(crate) labels: &'a BTreeMap<String, String>,
    /// The name each removed Service was last called, by lineage, where a record
    /// has one.
    pub(crate) removed: &'a BTreeMap<String, String>,
}

/// The lineage each `${{ name.KEY }}` of an edit reads: the Service `shown` labels
/// `name`. A removed Service's, such as `${{ <deleted redis>.KEY }}`, only where the
/// edited value, `old`, reads that `KEY` of it: an edit keeps the references it had
/// and adds none to a removed Service. A `<deleted NAME>` no label matches reads the
/// one Service `old` reads that `KEY` of and that was last called `NAME`, as reads
/// showed it before another removal of that name made both show by lineage.
pub(crate) fn lineage_of<'a>(
    shown: Shown<'a>,
    old: &'a [ValuePart],
) -> impl FnMut(&str, &str) -> Option<String> + 'a {
    move |name, key| {
        let removed = is_deleted_owner(name);
        let referenced = || {
            old.iter().filter_map(|part| match part {
                ValuePart::Ref {
                    owner: ValuePartOwner::Service { lineage_id },
                    key: reads,
                } => Some((lineage_id, reads)),
                ValuePart::Ref { .. } | ValuePart::Text { .. } => None,
            })
        };
        let labelled = shown
            .labels
            .iter()
            .filter(|(_, shown)| shown.as_str() == name)
            .map(|(lineage, _)| lineage)
            .find(|lineage| {
                !removed || referenced().any(|(read, reads)| read == *lineage && reads == key)
            });
        if let Some(lineage) = labelled {
            return Some(lineage.clone());
        }
        let last_called = name.strip_prefix("<deleted ")?.strip_suffix('>')?;
        let mut alike: Vec<&String> = referenced()
            .filter(|(_, reads)| *reads == key)
            .map(|(lineage, _)| lineage)
            .filter(|lineage| shown.removed.get(*lineage).map(String::as_str) == Some(last_called))
            .collect();
        alike.sort();
        alike.dedup();
        match alike.as_slice() {
            [lineage] => Some((*lineage).clone()),
            _ => None,
        }
    }
}

/// Text as a variable's parts, referencing Services as [`lineage_of`] reads them,
/// with its fingerprint.
pub(crate) fn text_parts(
    key: &VariableKey,
    text: &str,
    names: &BTreeMap<String, String>,
    shown: Shown<'_>,
    old: &[ValuePart],
) -> Result<(Vec<ValuePart>, String), RpcError> {
    validate_text(key, text)?;
    let template = parse_variable_template(text, lineage_of(shown, old));
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
    Ok((template.parts, fingerprint))
}

/// Text parts as a stored value: a literal unless they reference something.
pub(crate) fn stored(mut parts: Vec<ValuePart>) -> SavedVariableValue {
    match parts.as_mut_slice() {
        [] => SavedVariableValue::Literal {
            value: String::new(),
        },
        [ValuePart::Text { value }] => SavedVariableValue::Literal {
            value: std::mem::take(value),
        },
        _ => SavedVariableValue::Template { parts },
    }
}

/// The longest `KEY=value` string Linux passes a program, with its closing NUL:
/// `MAX_ARG_STRLEN`. A Container given a longer one never starts.
pub(crate) const MAX_ENV_STRING: usize = 128 * 1024;

pub(crate) const TOO_LONG: &str = "too long for a Container's environment: Linux passes at most 128 KiB per variable, name included";

pub(crate) const fn too_long(key: &str, value: usize) -> bool {
    key.len() + value + 2 > MAX_ENV_STRING
}

/// The most Linux passes a program for its variables and arguments together with
/// Docker's default 8 MiB stack: a quarter of it. Each `KEY=value` string counts with
/// its NUL and an 8-byte pointer to it.
const DEFAULT_ENV_TOTAL: usize = 2 * 1024 * 1024;

/// The most Linux passes whatever the stack limit: three quarters of `_STK_LIM`.
/// A Container given more never starts on any Server.
const MAX_ENV_TOTAL: usize = 6 * 1024 * 1024;

/// What `KEY=value` takes of a program's environment.
const fn env_bytes(key: usize, value: usize) -> usize {
    key + 1 + value + 1 + 8
}

/// Refuse `service`'s environment when its variables, at `bytes`, are past
/// [`MAX_ENV_TOTAL`].
fn too_big(service: &str, bytes: usize) -> Result<(), RpcError> {
    if bytes <= MAX_ENV_TOTAL {
        return Ok(());
    }
    Err(error::invalid(
        format!(
            "{service}: too big for a Container's environment: its variables take {} KiB, and Linux passes at most {} KiB of variables and arguments together, whatever the stack limit",
            bytes.div_ceil(1024),
            MAX_ENV_TOTAL / 1024
        ),
        json!({ "service": service, "bytes": bytes, "limit": MAX_ENV_TOTAL }),
    ))
}

/// Refuse `service` holding more literal variable text than any Container's
/// environment takes, and note when it holds more than Docker's default stack lets
/// Linux pass. Counts only literal values, which always reach the Container whole;
/// what templates, references and secrets resolve to is judged at deploy.
pub(crate) fn environment_note(
    environment: &Environment,
    service: &ServiceName,
) -> Result<Option<String>, RpcError> {
    let bytes = environment
        .service(service)?
        .variables
        .iter()
        .filter_map(|variable| match &variable.value {
            SavedVariableValue::Literal { value } => {
                Some(env_bytes(variable.key.len(), value.len()))
            }
            SavedVariableValue::Template { .. }
            | SavedVariableValue::Secret { .. }
            | SavedVariableValue::SecretWithoutValue => None,
        })
        .sum();
    too_big(service.as_str(), bytes)?;
    Ok((bytes > DEFAULT_ENV_TOTAL).then(|| {
        format!(
            "{service}'s variables take {} KiB. With Docker's default 8 MiB stack, Linux passes a Container at most {} KiB of variables and arguments, so it won't start unless the Server raises that limit",
            bytes.div_ceil(1024),
            DEFAULT_ENV_TOTAL / 1024
        )
    }))
}

/// Refuse resolved environment `env` of `service` when no Container could take it.
pub(crate) fn refuse_too_big(
    service: &str,
    env: &BTreeMap<String, EnvValue>,
) -> Result<(), RpcError> {
    too_big(
        service,
        env.iter()
            .map(|(key, value)| env_bytes(key.len(), opened_len(value)))
            .sum(),
    )
}

/// The bytes `value` takes in the Container once its Sealed parts are opened.
fn opened_len(value: &EnvValue) -> usize {
    let part = |part: &EnvPart| match part {
        EnvPart::Text(text) => text.len(),
        EnvPart::Sealed(sealed) => client_sealed::plaintext_len(sealed),
    };
    match value {
        EnvValue::Text(text) => text.len(),
        EnvValue::Sealed(sealed) => client_sealed::plaintext_len(sealed),
        EnvValue::Template(template) => template.template.iter().map(part).sum(),
    }
}

/// Refuse plain text that [`set`] wouldn't seal as a new secret of `key`, as it
/// refuses it. A CLI that seals the text itself runs this first, since the Store
/// can't read what it seals.
///
/// # Errors
/// A name Ployz sets, or text [`validate_text`] refuses.
pub(crate) fn admit_secret(key: &VariableKey, plaintext: &str) -> Result<(), RpcError> {
    refuse_reserved(key)?;
    validate_text(key, plaintext)
}

/// Plain and secret values must survive the container's environment unchanged.
///
/// # Errors
/// Refuses NUL bytes, or a value too long for the environment, without echoing it.
pub(crate) fn validate_text(key: &VariableKey, text: &str) -> Result<(), RpcError> {
    if text.contains('\0') {
        return Err(invalid(key, "null characters are not allowed"));
    }
    if too_long(key.as_str(), text.len()) {
        return Err(invalid(key, TOO_LONG));
    }
    Ok(())
}

fn secret(variable: &SavedVariableIntent) -> bool {
    matches!(
        variable.value,
        SavedVariableValue::Secret { .. } | SavedVariableValue::SecretWithoutValue
    )
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

/// Whether `value` holds a secret's plain text: text anywhere under a `secret`
/// field, whatever its other fields and however deep, whether or not [`set`] takes
/// it.
pub(crate) fn holds_secret_text(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(field, value)| {
            (field == "secret" && holds_text(value)) || holds_secret_text(value)
        }),
        Value::Array(items) => items.iter().any(holds_secret_text),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

fn holds_text(value: &Value) -> bool {
    match value {
        Value::String(_) => true,
        Value::Object(fields) => fields.values().any(holds_text),
        Value::Array(items) => items.iter().any(holds_text),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// A new secret's plain text, as [`set`] reads it from a value, and the exported
/// flag asked beside it.
pub(crate) struct NewSecret {
    text: String,
    exported: Option<bool>,
}

impl NewSecret {
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// The value that asks [`set`] for `sealed` in place of this text, exported as
    /// the text was.
    pub(crate) fn sealed(&self, sealed: &ClientSealedValue) -> Value {
        match self.exported {
            Some(exported) => json!({ "value": { "secret": sealed }, "exported": exported }),
            None => json!({ "secret": sealed }),
        }
    }
}

/// The new secret `value` asks [`set`] to seal as `key`, read as [`set`] reads it.
///
/// # Errors
/// What [`set`] refuses in `value` before it reads Working State, such as a secret's
/// text beside other fields.
pub(crate) fn new_secret(key: &VariableKey, value: &Value) -> Result<Option<NewSecret>, RpcError> {
    match parse(key, value.clone())? {
        (Some(Input::Seal(text)), exported) => Ok(Some(NewSecret { text, exported })),
        (
            Some(
                Input::Text(_)
                | Input::Keep
                | Input::ClientSealed(_)
                | Input::Generate
                | Input::Generated(_),
            )
            | None,
            _,
        ) => Ok(None),
    }
}

/// The plain text `value` asks [`set`] to seal: `{"secret": "…"}` with text in it.
fn plaintext_secret(value: &Value) -> Option<&str> {
    match value {
        Value::Object(fields) if fields.len() == 1 => match fields.get("secret") {
            Some(Value::String(plaintext)) if !plaintext.is_empty() => Some(plaintext),
            Some(_) | None => None,
        },
        Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_) => None,
    }
}

fn input(key: &VariableKey, value: Value) -> Result<Input, RpcError> {
    if let Some(plaintext) = plaintext_secret(&value) {
        return Ok(Input::Seal(plaintext.to_owned()));
    }
    match value {
        Value::String(text) => match Generator::parse(&text) {
            Ok(Some(generator)) => Ok(Input::Generated(generator)),
            Ok(None) => Ok(Input::Text(text)),
            Err(refused) => Err(invalid(key, &refused.to_string())),
        },
        Value::Object(fields) if fields.len() == 1 => match fields.get("secret") {
            Some(Value::Bool(_)) => Ok(Input::Keep),
            Some(Value::Object(asked))
                if asked.len() == 1 && asked.get("generate") == Some(&Value::Bool(true)) =>
            {
                Ok(Input::Generate)
            }
            // A fixed message: a serde error would echo the value.
            Some(sealed @ Value::Object(asked)) if asked.contains_key("sealedFor") => {
                serde_json::from_value(sealed.clone())
                    .map(Input::ClientSealed)
                    .map_err(|_| {
                        invalid(key, "expected sealedFor and a value sealed in the browser")
                    })
            }
            Some(_) | None => Err(invalid(key, "expected {\"secret\": true} or a new secret")),
        },
        Value::Null => Err(invalid(key, "null never deletes a variable; unset it")),
        Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            Err(invalid(key, "expected text or {\"secret\": true}"))
        }
    }
}

/// 32 random letters, the shape of the dashboard's database passwords.
fn generated_secret() -> Result<String, RpcError> {
    let letters = Generator::new(32, "a-zA-Z")
        .map_err(|_| error::internal("the letters generator is valid"))?;
    generate(&letters)
}

/// A new value `generator` makes, with its fingerprint.
fn generated_value(generator: &Generator) -> Result<(SavedVariableValue, String), RpcError> {
    let value = generate(generator)?;
    let fingerprint = plain_fingerprint(&[ValuePart::Text {
        value: value.clone(),
    }]);
    Ok((SavedVariableValue::Literal { value }, fingerprint))
}

/// Make the value of every generated variable in `working` that awaits one: what a
/// copy, Sync or Follow brought arrives with only its [`Generator`], and each
/// Environment makes its own.
///
/// # Errors
/// `unavailable` when there is no randomness to make one from.
pub(crate) fn make_generated(
    working: &mut ployz_core::config::SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    for variable in working
        .services
        .iter_mut()
        .flat_map(|service| service.variables.iter_mut())
    {
        if let (true, Some(generator)) = (variable.awaits_value(), &variable.generated) {
            (variable.value, variable.value_fingerprint) = generated_value(generator)?;
        }
    }
    Ok(())
}

/// Give each generated variable in `working` that awaits a value the one `made` holds
/// for the same Service lineage, key and generator: what Working State takes from the
/// Saved State a merge published, so the two hold one value.
pub(crate) fn take_generated(
    working: &mut ployz_core::config::SavedEnvironmentIntent,
    made: &ployz_core::config::SavedEnvironmentIntent,
) {
    for service in &mut working.services {
        let Some(source) = made
            .services
            .iter()
            .find(|made| made.lineage_id == service.lineage_id)
        else {
            continue;
        };
        for variable in service.variables.iter_mut().filter(|v| v.awaits_value()) {
            if let Some(made) = source.variables.iter().find(|made| {
                made.key == variable.key
                    && made.generated == variable.generated
                    && !made.awaits_value()
            }) {
                variable.value.clone_from(&made.value);
                variable
                    .value_fingerprint
                    .clone_from(&made.value_fingerprint);
            }
        }
    }
}

/// Refuse to publish a generated variable that awaits its value: Saved State and what
/// deploys from it never hold one.
///
/// # Errors
/// `internal` naming the variable, never a value.
pub(crate) fn refuse_awaiting(
    intent: &ployz_core::config::SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    for service in &intent.services {
        if let Some(variable) = service.variables.iter().find(|v| v.awaits_value()) {
            return Err(error::internal(format!(
                "{}.{} would be saved before its generated value was made",
                service.slug, variable.key
            )));
        }
    }
    Ok(())
}

/// A value `generator` makes: each character drawn uniformly from its alphabet.
pub(crate) fn generate(generator: &Generator) -> Result<String, RpcError> {
    let alphabet = generator.alphabet();
    let length = usize::from(generator.length());
    if alphabet.is_empty() {
        return Err(error::internal("a generator without characters"));
    }
    // Bytes at or past the largest multiple of the alphabet's size a byte holds are
    // dropped, so every character is equally likely.
    let limit = 256 - 256 % alphabet.len();
    let random = SystemRandom::new();
    let mut value = String::with_capacity(length);
    let mut bytes = [0_u8; 64];
    while value.len() < length {
        random
            .fill(&mut bytes)
            .map_err(|_| error::unavailable("No randomness to make a secret from"))?;
        value.extend(
            bytes
                .iter()
                .map(|byte| usize::from(*byte))
                .filter(|byte| *byte < limit)
                .filter_map(|byte| alphabet.get(byte % alphabet.len()).copied().map(char::from))
                .take(length.saturating_sub(value.len())),
        );
    }
    Ok(value)
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
        json!({ "variable": key.as_str() }),
    )
}

/// The JSON Schema of a variable in the catalog.
pub(crate) fn schema() -> Value {
    let secret = json!({
        "type": "object",
        "properties": { "secret": { "type": "boolean" } },
        "required": ["secret"],
        "additionalProperties": false,
    });
    json!({
        "title": "Variable",
        "description": "An environment variable. Text may reference variables: ${{ KEY }} for this Service's, ${{ service.KEY }} for another's; $${{ is a literal ${{. A secret reads as {\"secret\": true}, or {\"secret\": false} until it has a value, and sending that back keeps it. {\"value\": …, \"exported\": true} marks one other Services are meant to reference. Renaming one that other variables reference is refused, naming them, unless the rename updates references, which switches them to the new name in the same edit.",
        "anyOf": [
            { "type": "string" },
            secret,
            {
                "type": "object",
                "properties": {
                    "value": { "anyOf": [{ "type": "string" }, secret] },
                    "exported": { "type": "boolean", "default": false },
                },
                "additionalProperties": false,
            },
        ],
        "examples": [
            "postgres://${{ db.USER }}@${{ db.PLOYZ_PRIVATE_DOMAIN }}:5432/app",
            { "secret": true },
            { "value": "info", "exported": true },
        ],
        "x-ployz-apply": "staged",
        "x-ployz-secret": true,
        "x-ployz-data-loss": false,
    })
}

/// Compiled Saved State with every reference resolved.
pub(crate) struct Resolved {
    /// Each Service's environment, by Service ID. A value sealed in the browser is
    /// Sealed, or a Template holding it, for the Machine to open.
    pub(crate) env: BTreeMap<String, BTreeMap<String, EnvValue>>,
    /// Each Config's files, by Config ID.
    pub(crate) configs: BTreeMap<String, BTreeMap<ConfigFileName, ResolvedConfigFile>>,
    /// Each Service's variables left out because a secret without a value reaches
    /// them, by Service ID.
    pub(crate) not_set: BTreeMap<String, Vec<String>>,
    /// Each Service's variables left out because they reference, directly or through
    /// another variable, a Service or variable that isn't there, by Service ID.
    pub(crate) broken: BTreeMap<String, Vec<String>>,
    /// Each Legacy secret of this Environment's Services that reaches a Service's
    /// environment or a Config's files: Not set there, and a Deploy that ships the
    /// reader waits for the reseal instead.
    pub(crate) legacy: Vec<LegacyRead>,
}

/// A Legacy secret, by its owner's Service ID and key, that reaches `reader`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LegacyRead {
    pub(crate) reader: LegacyReader,
    pub(crate) owner: String,
    pub(crate) key: String,
}

/// What a Legacy secret reaches: a Service's environment or a Config's files, by ID.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LegacyReader {
    Service(String),
    Config(String),
}

/// Every Service's environment and every Config's files, resolved from compiled
/// Saved State against the frozen producers, for a Deploy of Environment
/// `environment`. Nothing opens a secret here. One sealed for `environment` reaches
/// its Service as a Sealed part; one sealed for another Environment, or Legacy, is
/// Not set, and what a Legacy one reaches is listed. A variable a secret without a
/// value, or a broken reference, reaches is left out; in a file either is empty.
///
/// # Errors
/// Returns `invalid_argument` for a reference cycle, a value too long for the
/// environment or a Config file that reads a value sealed in the browser.
pub(crate) fn resolve(
    compiled: &CompiledEnvironmentIntent,
    environment: &str,
) -> Result<Resolved, RpcError> {
    let mut resolved = resolve_values(compiled, environment)?;
    resolved.legacy = legacy_reads(compiled);
    Ok(resolved)
}

fn resolve_values(
    compiled: &CompiledEnvironmentIntent,
    environment: &str,
) -> Result<Resolved, RpcError> {
    let environment = uuid::Uuid::parse_str(environment).ok();
    let stored = |sealed: Option<&SecretCiphertext>| match sealed {
        Some(SecretCiphertext::ClientSealed(client)) => match environment {
            Some(id) if client.sealed_for == id => Ok(ResolverValue::Sealed {
                value: client.value.clone(),
            }),
            _ => Ok(ResolverValue::NotSet),
        },
        Some(SecretCiphertext::Legacy(_)) => Ok(ResolverValue::NotSet),
        None => Err(error::corrupt("sealed secret")),
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
            SavedVariableValue::Secret { encrypted_value } => stored(encrypted_value.as_ref())?,
            SavedVariableValue::SecretWithoutValue => ResolverValue::NotSet,
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
    let mut resolved = Resolved {
        env: BTreeMap::new(),
        configs: BTreeMap::new(),
        not_set: BTreeMap::new(),
        broken: BTreeMap::new(),
        legacy: Vec::new(),
    };
    for node in &compiled.node_snapshots {
        let config = match &node.snapshot.0 {
            CompiledNodeConfig::Service(config) => config,
            CompiledNodeConfig::Config(config) => {
                let mut files = BTreeMap::new();
                for (name, file) in &config.files {
                    let file = resolve_config_file(name.as_str(), &file.content, &producers)
                        .map_err(|path| cycle(compiled, &path))?;
                    if let Some(reference) = file.sealed.first() {
                        return Err(sealed_in_file(compiled, &config.name, name, reference));
                    }
                    files.insert(name.clone(), file);
                }
                resolved.configs.insert(node.node_id.clone(), files);
                continue;
            }
            CompiledNodeConfig::Volume(_) => continue,
        };
        let mut env = BTreeMap::new();
        let mut not_set = Vec::new();
        let mut broken = Vec::new();
        for (key, value) in &config.env {
            let value = match value {
                ServiceEnvValue::Literal { value, parts: None } => {
                    Some(EnvValue::Text(value.clone()))
                }
                ServiceEnvValue::Literal {
                    parts: Some(parts), ..
                } => match resolve_variables(&ResolveVariablesInput {
                    parts: parts.clone(),
                    self_owner_id: node.node_id.clone(),
                    producers: producers.clone(),
                }) {
                    ResolveVariablesResult::Resolved { warnings, .. } if !warnings.is_empty() => {
                        broken.push(key.clone());
                        None
                    }
                    ResolveVariablesResult::Resolved { value, .. } => Some(value),
                    ResolveVariablesResult::NotSet { warnings } => {
                        not_set.push(key.clone());
                        if !warnings.is_empty() {
                            broken.push(key.clone());
                        }
                        None
                    }
                    ResolveVariablesResult::Cycle { path } => {
                        return Err(cycle(compiled, &path));
                    }
                },
                // Only a secret without a value has no fingerprint.
                ServiceEnvValue::Secret {
                    encrypted_value: None,
                    fingerprint,
                    ..
                } if fingerprint.is_empty() => {
                    not_set.push(key.clone());
                    None
                }
                ServiceEnvValue::Secret {
                    encrypted_value, ..
                } => match stored(encrypted_value.as_ref())? {
                    // Only a value sealed for this very variable is bound to its name. A
                    // renamed one keeps the key it was sealed under, so it reaches the
                    // Machine as a Template part, which opens under that key.
                    ResolverValue::Sealed { value }
                        if value.key == *key
                            && value.scope == SealedScope::Service(node_uuid(&node.node_id)) =>
                    {
                        Some(EnvValue::Sealed(value))
                    }
                    ResolverValue::Sealed { value } => Some(EnvValue::Template(Template {
                        template: vec![EnvPart::Sealed(value)],
                    })),
                    ResolverValue::NotSet => {
                        not_set.push(key.clone());
                        None
                    }
                    ResolverValue::Secret { .. }
                    | ResolverValue::Literal { .. }
                    | ResolverValue::Template { .. } => None,
                },
            };
            if let Some(value) = value {
                // A reference can bring a Sealed value into PORT after PORT is saved.
                if key == "PORT" && !matches!(value, EnvValue::Text(_)) {
                    return Err(error::invalid(
                        format!(
                            "{}.env.PORT {}",
                            config.settings.private_dns,
                            client_sealed::PORT_REFUSED
                        ),
                        json!({ "variable": key }),
                    ));
                }
                if too_long(key, opened_len(&value)) {
                    return Err(error::invalid(
                        format!("{}.env.{key} is {TOO_LONG}", config.settings.private_dns),
                        json!({ "variable": key }),
                    ));
                }
                env.insert(key.clone(), value);
            }
        }
        if !not_set.is_empty() {
            resolved.not_set.insert(node.node_id.clone(), not_set);
        }
        if !broken.is_empty() {
            resolved.broken.insert(node.node_id.clone(), broken);
        }
        resolved.env.insert(node.node_id.clone(), env);
    }
    Ok(resolved)
}

/// A Service ID as a UUID; one that isn't parses to nil, which no Sealed scope names.
fn node_uuid(id: &str) -> uuid::Uuid {
    uuid::Uuid::parse_str(id).unwrap_or_default()
}

/// A Config file reads a value sealed in the browser, which only the Machine opens.
fn sealed_in_file(
    compiled: &CompiledEnvironmentIntent,
    config: &ployz_core::ConfigName,
    file: &ConfigFileName,
    reference: &ployz_core::config::FragileReference,
) -> RpcError {
    let service = match &reference.owner {
        ValuePartOwner::Service { lineage_id } => compiled
            .variable_producers
            .iter()
            .find(|producer| producer.owner_lineage_id == *lineage_id)
            .and_then(|producer| service_name(compiled, &producer.owner_id)),
        ValuePartOwner::Self_ => None,
    };
    let variable = match service {
        Some(service) => format!("{service}.{}", reference.key),
        None => reference.key.clone(),
    };
    error::invalid(
        format!(
            "Config {config} file {file} reads {variable}, a value sealed in the browser: only a Service's environment carries one"
        ),
        json!({ "config": config, "file": file, "variable": variable }),
    )
}

/// What each Legacy secret of `compiled`'s own Services reaches, by reference
/// alone: a variable is tainted when it is one or references a tainted one, and a
/// Service's environment or a Config's file that holds or references a tainted
/// variable reads it. Nothing resolves here, so no limit on resolved values applies.
/// One a Branch reads live from its parent is left out: it is Not set in the Branch
/// even once resealed.
fn legacy_reads(compiled: &CompiledEnvironmentIntent) -> Vec<LegacyRead> {
    let is_legacy =
        |value: Option<&SecretCiphertext>| matches!(value, Some(SecretCiphertext::Legacy(_)));
    // Each tainted variable by owner lineage and key, with the Legacy secret it reads
    // by owner Service ID and key.
    let mut tainted: BTreeMap<(String, String), (String, String)> = BTreeMap::new();
    for producer in &compiled.variable_producers {
        if let SavedVariableValue::Secret { encrypted_value } = &producer.value
            && is_legacy(encrypted_value.as_ref())
            && service_name(compiled, &producer.owner_id).is_some()
        {
            tainted.insert(
                (producer.owner_lineage_id.clone(), producer.key.clone()),
                (producer.owner_id.clone(), producer.key.clone()),
            );
        }
    }
    let reads_tainted =
        |tainted: &BTreeMap<(String, String), (String, String)>, parts: &[ValuePart], own: &str| {
            parts
                .iter()
                .filter_map(|part| match part {
                    ValuePart::Ref { owner, key } => {
                        let lineage = match owner {
                            ValuePartOwner::Service { lineage_id } => lineage_id.as_str(),
                            ValuePartOwner::Self_ => own,
                        };
                        tainted.get(&(lineage.to_owned(), key.clone())).cloned()
                    }
                    ValuePart::Text { .. } => None,
                })
                .collect::<Vec<_>>()
        };
    loop {
        let mut spread = Vec::new();
        for producer in &compiled.variable_producers {
            let id = (producer.owner_lineage_id.clone(), producer.key.clone());
            if let SavedVariableValue::Template { parts } = &producer.value
                && !tainted.contains_key(&id)
                && let Some(origin) = reads_tainted(&tainted, parts, &producer.owner_lineage_id)
                    .into_iter()
                    .next()
            {
                spread.push((id, origin));
            }
        }
        if spread.is_empty() {
            break;
        }
        tainted.extend(spread);
    }
    let mut reads = Vec::new();
    let mut read = |reader: LegacyReader, (owner, key): (String, String)| {
        let found = LegacyRead { reader, owner, key };
        if !reads.contains(&found) {
            reads.push(found);
        }
    };
    for node in &compiled.node_snapshots {
        match &node.snapshot.0 {
            CompiledNodeConfig::Service(config) => {
                for (key, value) in &config.env {
                    let origins = match value {
                        ServiceEnvValue::Secret {
                            encrypted_value, ..
                        } if is_legacy(encrypted_value.as_ref()) => {
                            vec![(node.node_id.clone(), key.clone())]
                        }
                        ServiceEnvValue::Literal {
                            parts: Some(parts), ..
                        } => reads_tainted(&tainted, parts, &node.node_lineage_id),
                        ServiceEnvValue::Literal { parts: None, .. }
                        | ServiceEnvValue::Secret { .. } => Vec::new(),
                    };
                    for origin in origins {
                        read(LegacyReader::Service(node.node_id.clone()), origin);
                    }
                }
            }
            CompiledNodeConfig::Config(config) => {
                for file in config.files.values() {
                    for origin in reads_tainted(&tainted, &file.content, "") {
                        read(LegacyReader::Config(node.node_id.clone()), origin);
                    }
                }
            }
            CompiledNodeConfig::Volume(_) => {}
        }
    }
    reads
}

/// The name of Service `node_id`, as a refusal names it.
fn service_name(compiled: &CompiledEnvironmentIntent, node_id: &str) -> Option<String> {
    compiled
        .node_snapshots
        .iter()
        .find(|node| node.node_id == node_id)
        .and_then(|node| match &node.snapshot.0 {
            CompiledNodeConfig::Service(config) => Some(config.settings.private_dns.to_string()),
            CompiledNodeConfig::Volume(_) | CompiledNodeConfig::Config(_) => None,
        })
}

/// A reference cycle, named by Service and variable.
fn cycle(compiled: &CompiledEnvironmentIntent, path: &[String]) -> RpcError {
    let names = path
        .iter()
        .map(|step| {
            let (owner, key) = step.split_once("::").unwrap_or(("", step));
            let service = service_name(compiled, owner).unwrap_or_default();
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

#[cfg(test)]
mod tests {
    #[test]
    fn a_generated_value_has_the_length_and_charset_asked() {
        let generator = Generator::new(64, "a-f0-9").unwrap();
        let alphabet = generator.alphabet();
        let value = super::generate(&generator).unwrap();
        assert_eq!(value.len(), 64);
        assert!(value.bytes().all(|byte| alphabet.contains(&byte)));
    }

    #[test]
    fn generated_values_differ_and_reach_every_character() {
        // 3 characters over 256 × 256 draws: a skewed or stuck draw misses one or
        // repeats a value. Asserted without printing either value.
        let generator = Generator::new(256, "abc").unwrap();
        let first = super::generate(&generator).unwrap();
        let second = super::generate(&generator).unwrap();
        assert!(first != second, "two draws matched");
        let mut counts = [0_usize; 3];
        for _ in 0..256 {
            for byte in super::generate(&generator).unwrap().bytes() {
                if let Some(count) = counts.get_mut(usize::from(byte - b'a')) {
                    *count += 1;
                }
            }
        }
        // Each expects 21845 of 65536; 5% either way is far past any fair draw.
        assert!(counts.iter().all(|count| (20_750..=22_950).contains(count)));
    }

    use ployz_core::ConfigName;
    use ployz_core::config::{
        CompiledEnvironmentNode, CompiledNodeSnapshot, ConfigNodeConfig, EncryptedSecretValue,
        FileMode, SavedConfigFile, SavedVariableProducer,
    };

    use super::*;

    #[test]
    fn a_sealed_value_counts_toward_a_containers_environment_at_its_opened_length() {
        let sealed = |opened: usize| ployz_core::SealedValue {
            sealed: ployz_core::Ciphertext::from_bytes(vec![0; opened + 48]),
            scope: SealedScope::Service(uuid::Uuid::from_u128(3)),
            key: "K".into(),
        };
        let fits = MAX_ENV_TOTAL - env_bytes(1, 0);
        let env = |value: EnvValue| BTreeMap::from([("K".to_owned(), value)]);
        let template = |opened: usize| {
            EnvValue::Template(Template {
                template: vec![EnvPart::Text("ab".into()), EnvPart::Sealed(sealed(opened))],
            })
        };
        refuse_too_big("web", &env(EnvValue::Sealed(sealed(fits)))).unwrap();
        refuse_too_big("web", &env(EnvValue::Sealed(sealed(fits + 1)))).unwrap_err();
        refuse_too_big("web", &env(template(fits - 2))).unwrap();
        refuse_too_big("web", &env(template(fits - 1))).unwrap_err();
    }

    #[test]
    fn a_config_file_that_reads_a_value_sealed_in_the_browser_is_refused_by_name() {
        use base64::Engine as _;
        let environment = uuid::Uuid::from_u128(2);
        let ciphertext = base64::engine::general_purpose::STANDARD.encode([1; 48]);
        let sealed: ClientSealedValue = serde_json::from_value(json!({
            "sealedFor": environment,
            "value": { "sealed": ciphertext, "scope": { "service": uuid::Uuid::from_u128(3) }, "key": "P" },
        }))
        .unwrap();
        let compiled = CompiledEnvironmentIntent {
            node_snapshots: vec![CompiledEnvironmentNode {
                environment_id: environment.to_string(),
                node_id: "sentry".into(),
                node_lineage_id: "sentry".into(),
                snapshot: CompiledNodeSnapshot(CompiledNodeConfig::Config(ConfigNodeConfig {
                    version: 1,
                    name: ConfigName::parse("sentry").unwrap(),
                    files: BTreeMap::from([(
                        ConfigFileName::parse("secret.yml").unwrap(),
                        SavedConfigFile {
                            content: parse_variable_template("p: ${{ web.P }}", |_, _| {
                                Some("web-lineage".to_owned())
                            })
                            .parts,
                            mode: FileMode::READ_ONLY,
                            uid: 0,
                            gid: 0,
                        },
                    )]),
                })),
                encrypted_registry_username: None,
                encrypted_registry_secret: None,
            }],
            variable_producers: vec![SavedVariableProducer {
                owner_scope: "service".into(),
                owner_id: "web".into(),
                owner_lineage_id: "web-lineage".into(),
                key: "P".into(),
                value: SavedVariableValue::Secret {
                    encrypted_value: Some(SecretCiphertext::ClientSealed(sealed)),
                },
            }],
        };
        let error = resolve(&compiled, &environment.to_string())
            .map(|_| ())
            .unwrap_err();
        assert_eq!(error.code, ployz_core::RpcErrorCode::InvalidArgument);
        assert!(error.message.contains("sentry"), "{error:?}");
        assert!(error.message.contains("secret.yml"), "{error:?}");
        assert!(error.message.contains("reads P"), "{error:?}");
        assert!(!format!("{error:?}").contains(&ciphertext), "{error:?}");
    }

    #[test]
    fn a_config_file_that_reads_a_legacy_secret_reads_it_as_not_set() {
        let file = |text: &str| SavedConfigFile {
            content: parse_variable_template(text, |_, _| Some("web-lineage".to_owned())).parts,
            mode: FileMode::READ_ONLY,
            uid: 0,
            gid: 0,
        };
        let compiled = CompiledEnvironmentIntent {
            node_snapshots: vec![CompiledEnvironmentNode {
                environment_id: "env".into(),
                node_id: "sentry".into(),
                node_lineage_id: "sentry".into(),
                snapshot: CompiledNodeSnapshot(CompiledNodeConfig::Config(ConfigNodeConfig {
                    version: 1,
                    name: ConfigName::parse("sentry").unwrap(),
                    files: BTreeMap::from([
                        (
                            ConfigFileName::parse("secret.yml").unwrap(),
                            file("p: ${{ web.P }}"),
                        ),
                        (ConfigFileName::parse("plain.yml").unwrap(), file("plain")),
                    ]),
                })),
                encrypted_registry_username: None,
                encrypted_registry_secret: None,
            }],
            variable_producers: vec![SavedVariableProducer {
                owner_scope: "service".into(),
                owner_id: "web".into(),
                owner_lineage_id: "web-lineage".into(),
                key: "P".into(),
                value: SavedVariableValue::Secret {
                    encrypted_value: Some(SecretCiphertext::Legacy(EncryptedSecretValue {
                        version: 1,
                        iv: "P06pur97O+q8KRa4".into(),
                        tag: "WX+lTmeP37rpdHHvD6iOEQ==".into(),
                        ciphertext: "LcZhKYl1ZWoTpzn9xXoetYCxr8Iy".into(),
                    })),
                },
            }],
        };
        let mut resolved = resolve(&compiled, "env").unwrap();
        assert!(
            resolved.legacy.is_empty(),
            "web is no Service here, as a live value"
        );
        let files = resolved.configs.remove("sentry").unwrap();
        assert_eq!(
            files
                .iter()
                .map(|(name, file)| (name.to_string(), file.content.clone()))
                .collect::<Vec<_>>(),
            [
                ("plain.yml".into(), "plain".into()),
                ("secret.yml".into(), "p: ".into())
            ]
        );
        let secret = files
            .get(&ConfigFileName::parse("secret.yml").unwrap())
            .unwrap();
        assert!(secret.secret);
        assert_eq!(secret.not_set.len(), 1);
    }
}
