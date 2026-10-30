//! Which variables an Environment ever sealed: History never brings plain text
//! back for one.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{
    Cell, SavedEnvironmentIntent, SavedVariableIntent, SavedVariableValue, ValuePart,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;
use crate::{Actor, EnvironmentId, EnvironmentSummary};

pub(crate) fn sealed(value: &SavedVariableValue) -> bool {
    match value {
        SavedVariableValue::Secret { encrypted_value: _ }
        | SavedVariableValue::SecretWithoutValue => true,
        SavedVariableValue::Literal { value: _ } | SavedVariableValue::Template { parts: _ } => {
            false
        }
    }
}

/// Show sealed each plain value `introduced` keeps for a variable the Environment
/// holds sealed now, or held sealed before deleting or adding it again, or one it
/// was made with or copied that its Parent or the node's owner sealed since: a
/// Node Introduction keeps the value a Branch was made with, and a review compares
/// a new node with it, but never reads a sealed value back. The fingerprint stays,
/// so the comparison still sees whether the value changed.
pub(crate) fn hide_introduced(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    introduced: &mut SavedEnvironmentIntent,
    working: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    // Read only for a variable deleted or added again: they scan every save.
    let mut ever = None;
    let mut parent = None;
    for service in &mut introduced.services {
        let now = working
            .services
            .iter()
            .find(|now| now.id == service.id)
            .map(|now| &now.variables);
        for was in &mut service.variables {
            if sealed(&was.value) {
                continue;
            }
            let now = now.and_then(|now| {
                now.iter()
                    .find(|now| now.id == was.id)
                    .or_else(|| now.iter().find(|now| now.key == was.key))
            });
            let sealed_now = match now {
                Some(now) if now.id == was.id || sealed(&now.value) => sealed(&now.value),
                Some(_) | None => {
                    if ever.is_none() {
                        ever = Some(EverSealed::load(tx, environment.as_str())?);
                    }
                    let own = ever.as_ref().is_some_and(|ever| match now {
                        Some(_) => ever.holds_id(&service.id, was),
                        None => ever.holds(&service.id, was),
                    });
                    own || {
                        if parent.is_none() {
                            parent = Some(parent_sealed(tx, environment)?);
                        }
                        parent.as_ref().is_some_and(|parent| {
                            parent.contains(&(service.lineage_id.clone(), was.key.clone()))
                        })
                    }
                }
            };
            if sealed_now {
                was.value = SavedVariableValue::Secret {
                    encrypted_value: None,
                };
                was.generated = None;
            }
        }
    }
    Ok(())
}

/// Each variable a Branch was made with, or copied from a node's owner since, that
/// the Parent or owner sealed since, by Service lineage and key: none for a root.
/// The Branch's own copy has its own ID, so the seal is matched by the ID of the
/// variable the copy came from, never by key alone, which a variable added since
/// under that key shares.
fn parent_sealed(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<BTreeSet<(String, String)>, RpcError> {
    // (source, source Service ID, lineage, variable ID, key)
    let mut came_from = Vec::new();
    for row in tx.query(
        "SELECT parent_id, made_with FROM config_environment_branch WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )? {
        let parent = row.text(0)?;
        for service in row.intent(1, "Branch")?.services {
            for variable in service.variables {
                came_from.push((
                    parent.to_owned(),
                    service.id.clone(),
                    service.lineage_id.clone(),
                    variable.id,
                    variable.key,
                ));
            }
        }
    }
    for row in tx.query(
        "SELECT source_id, source_node_id, lineage_id, variable_id, key \
         FROM config_node_copied_from WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )? {
        came_from.push((
            row.text(0)?.to_owned(),
            row.text(1)?.to_owned(),
            row.text(2)?.to_owned(),
            row.text(3)?.to_owned(),
            row.text(4)?.to_owned(),
        ));
    }
    let mut ledgers = BTreeMap::new();
    let mut sealed = BTreeSet::new();
    for (source, service, lineage, variable, key) in came_from {
        let ever = match ledgers.entry(source) {
            Entry::Occupied(ever) => ever.into_mut(),
            Entry::Vacant(at) => {
                let ever = EverSealed::load(tx, at.key())?;
                at.insert(ever)
            }
        };
        if ever.ids.contains(&(service, variable)) {
            sealed.insert((lineage, key));
        }
    }
    Ok(sealed)
}

/// Whether a template reads a name in `names`. By key alone, whatever Service owns
/// it, so a shared key counts.
fn reads_any(value: &SavedVariableValue, names: &BTreeSet<String>) -> bool {
    value.parts().iter().any(|part| match part {
        ValuePart::Ref { key, .. } => names.contains(key),
        ValuePart::Text { .. } => false,
    })
}

pub(crate) fn names(tx: &mut dyn Tx, environment: &str) -> Result<BTreeSet<String>, RpcError> {
    let mut names = BTreeSet::new();
    for row in tx.query(
        "SELECT key FROM config_sealed_variable WHERE environment_id = ?1",
        &[environment.into()],
    )? {
        names.insert(row.text(0)?.to_owned());
    }
    let mut intents = Vec::new();
    for row in tx.query(
        "SELECT intent FROM config_saved WHERE environment_id = ?1",
        &[environment.into()],
    )? {
        intents.push(row.intent(0, "Saved State")?);
    }
    for row in tx.query(
        "SELECT working FROM config_environment WHERE id = ?1",
        &[environment.into()],
    )? {
        intents.push(row.intent(0, "Working State")?);
    }
    let variables: Vec<_> = intents
        .iter()
        .flat_map(|intent| &intent.services)
        .flat_map(|service| &service.variables)
        .collect();
    names.extend(
        variables
            .iter()
            .filter(|variable| sealed(&variable.value))
            .map(|variable| variable.key.clone()),
    );
    // A Branch reads a Live Node's values from its Parent, which may seal them: a
    // template reading a Service its own intent doesn't own always counts.
    for intent in &intents {
        let live = crate::branch::used_live(intent);
        names.extend(
            intent
                .services
                .iter()
                .flat_map(|service| &service.variables)
                .filter(|variable| {
                    variable
                        .value
                        .referenced_lineages()
                        .any(|lineage| live.contains_key(lineage))
                })
                .map(|variable| variable.key.clone()),
        );
    }
    // A template carries a Sealed value only through a reference, maybe via another
    // template: one reading only plain values resolves plain and is shown.
    loop {
        let reading: Vec<_> = variables
            .iter()
            .filter(|variable| !names.contains(&variable.key) && reads_any(&variable.value, &names))
            .map(|variable| variable.key.clone())
            .collect();
        if reading.is_empty() {
            return Ok(names);
        }
        names.extend(reading);
    }
}

/// The variable names under which an Environment's Containers may hold Sealed values.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SealedNamesQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct SealedNamesView {
    /// The Environment, at the revision read.
    pub environment: EnvironmentSummary,
    /// Every name ever Sealed in it, and every template that reads one, across its
    /// Services.
    pub names: BTreeSet<String>,
}

pub(crate) fn names_view(
    tx: &mut dyn Tx,
    who: &Actor,
    q: &SealedNamesQuery,
) -> Result<SealedNamesView, RpcError> {
    let environment = scope::environment(tx, who, &q.environment)?;
    let names = names(tx, environment.summary.id.as_str())?;
    Ok(SealedNamesView {
        environment: environment.summary,
        names,
    })
}

/// Every variable an Environment ever held sealed, per Service: by stable ID, so
/// a rename keeps the seal, and by key, so a new variable under that key does.
#[derive(Default)]
pub(crate) struct EverSealed {
    ids: BTreeSet<(String, String)>,
    keys: BTreeSet<(String, String)>,
}

impl EverSealed {
    pub(crate) fn load(tx: &mut dyn Tx, environment: &str) -> Result<Self, RpcError> {
        let mut ever = Self::default();
        for row in tx.query(
            "SELECT service_id, variable_id, key FROM config_sealed_variable \
             WHERE environment_id = ?1",
            &[environment.into()],
        )? {
            let service = row.text(0)?;
            ever.ids
                .insert((service.to_owned(), row.text(1)?.to_owned()));
            ever.keys
                .insert((service.to_owned(), row.text(2)?.to_owned()));
        }
        // Saves hold seals made before Working State recorded them.
        for row in tx.query(
            "SELECT intent FROM config_saved WHERE environment_id = ?1",
            &[environment.into()],
        )? {
            for (service, variable) in sealed_in(&row.intent(0, "Saved State")?) {
                ever.ids.insert((service.to_owned(), variable.id.clone()));
                ever.keys.insert((service.to_owned(), variable.key.clone()));
            }
        }
        Ok(ever)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Whether this very variable was sealed: not one under its key since.
    pub(crate) fn holds_id(&self, service: &str, variable: &SavedVariableIntent) -> bool {
        self.ids
            .contains(&(service.to_owned(), variable.id.clone()))
    }

    pub(crate) fn holds(&self, service: &str, variable: &SavedVariableIntent) -> bool {
        self.ids
            .contains(&(service.to_owned(), variable.id.clone()))
            || self
                .keys
                .contains(&(service.to_owned(), variable.key.clone()))
    }
}

/// Record each variable `working` holds sealed, at the write that seals it.
pub(crate) fn record(
    tx: &mut dyn Tx,
    environment: &str,
    working: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    for (service, variable) in sealed_in(working) {
        tx.execute(
            "INSERT INTO config_sealed_variable (environment_id, service_id, variable_id, key) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
            &[
                environment.into(),
                service.into(),
                variable.id.as_str().into(),
                variable.key.as_str().into(),
            ],
        )?;
    }
    Ok(())
}

/// Record each variable every Environment's Working State holds sealed: seals made
/// before the ledger existed.
pub(crate) fn record_working(tx: &mut dyn Tx) -> Result<(), RpcError> {
    for row in tx.query("SELECT id, working FROM config_environment", &[])? {
        record(tx, row.text(0)?, &row.intent(1, "Working State")?)?;
    }
    Ok(())
}

fn sealed_in(
    intent: &SavedEnvironmentIntent,
) -> impl Iterator<Item = (&str, &SavedVariableIntent)> {
    intent.services.iter().flat_map(|service| {
        service
            .variables
            .iter()
            .filter(|variable| sealed(&variable.value))
            .map(move |variable| (service.id.as_str(), variable))
    })
}

/// Each variable an Environment ever held sealed, by Service lineage and key: how a
/// Sync base, shared with other Environments, names it.
pub(crate) fn sealed_rows(
    tx: &mut dyn Tx,
    environment: &str,
) -> Result<BTreeSet<(String, String)>, RpcError> {
    let ever = EverSealed::load(tx, environment)?;
    if ever.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut intents = Vec::new();
    for row in tx.query(
        "SELECT working FROM config_environment WHERE id = ?1",
        &[environment.into()],
    )? {
        intents.push(row.intent(0, "Working State")?);
    }
    for row in tx.query(
        "SELECT intent FROM config_saved WHERE environment_id = ?1",
        &[environment.into()],
    )? {
        intents.push(row.intent(0, "Saved State")?);
    }
    Ok(intents
        .iter()
        .flat_map(|intent| &intent.services)
        .flat_map(|service| {
            service
                .variables
                .iter()
                .filter(|variable| ever.holds(&service.id, variable))
                .map(|variable| (service.lineage_id.clone(), variable.key.clone()))
        })
        .collect())
}

/// Each variable `after` holds sealed that `before` didn't, by Service lineage and key.
pub(crate) fn newly_sealed(
    before: &SavedEnvironmentIntent,
    after: &SavedEnvironmentIntent,
) -> BTreeSet<(String, String)> {
    let rows = |intent: &SavedEnvironmentIntent| -> BTreeSet<(String, String)> {
        intent
            .services
            .iter()
            .flat_map(|service| {
                service
                    .variables
                    .iter()
                    .filter(|variable| sealed(&variable.value))
                    .map(|variable| (service.lineage_id.clone(), variable.key.clone()))
            })
            .collect()
    };
    let before = rows(before);
    rows(after).difference(&before).cloned().collect()
}

/// Empty the plain value of each of `rows` in a Sync base. It keeps its kind and
/// fingerprint, which is all a Sync compares a plain value by.
pub(crate) fn empty_plain_in(
    base: &mut SavedEnvironmentIntent,
    rows: &BTreeSet<(String, String)>,
) -> bool {
    let mut emptied = false;
    for service in &mut base.services {
        for variable in &mut service.variables {
            if rows.contains(&(service.lineage_id.clone(), variable.key.clone())) {
                emptied |= empty_plain(&mut variable.value);
            }
        }
    }
    emptied
}

fn empty_plain(value: &mut SavedVariableValue) -> bool {
    match value {
        SavedVariableValue::Literal { value } if !value.is_empty() => value.clear(),
        SavedVariableValue::Template { parts } if !parts.is_empty() => parts.clear(),
        SavedVariableValue::Literal { value: _ }
        | SavedVariableValue::Template { parts: _ }
        | SavedVariableValue::Secret { encrypted_value: _ }
        | SavedVariableValue::SecretWithoutValue => return false,
    }
    true
}

/// [`empty_plain_in`] for one base cell, as a pending arrival keeps it.
pub(crate) fn empty_plain_cell(cell: &mut Cell) -> bool {
    let Cell::Value(Value::Object(fields)) = cell else {
        return false;
    };
    let mut plain = fields.clone();
    let Some(fingerprint) = plain.remove("fingerprint") else {
        return false;
    };
    let Ok(mut value) = serde_json::from_value::<SavedVariableValue>(Value::Object(plain)) else {
        return false;
    };
    if !empty_plain(&mut value) {
        return false;
    }
    let Ok(Value::Object(emptied)) = serde_json::to_value(value) else {
        return false;
    };
    *fields = emptied;
    fields.insert("fingerprint".to_owned(), fingerprint);
    true
}
