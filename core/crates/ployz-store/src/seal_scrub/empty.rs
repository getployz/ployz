//! Emptying the plain copy of a sealed value: in a Sync base, in a pending arrival's
//! cell, or wherever its fingerprint is held.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    Cell, PlainText, SavedEnvironmentIntent, SavedVariableValue, VariableValue,
};
use serde_json::Value;

use super::ledger::sealed;

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
                emptied |= empty_variable(&mut variable.value);
            }
        }
    }
    emptied
}

/// By Service lineage and key, each fingerprint of a plain value an Environment
/// held for a variable it seals: what a copy of that value elsewhere matches.
pub(crate) type Held = BTreeMap<(String, String), BTreeSet<String>>;

/// Add to `held` the fingerprint of each plain value `intent` keeps for `rows`. An
/// emptied value keeps the fingerprint of the text it had, so it counts.
pub(crate) fn hold(
    held: &mut Held,
    intent: &SavedEnvironmentIntent,
    rows: &BTreeSet<(String, String)>,
) {
    for service in &intent.services {
        for variable in &service.variables {
            let row = (service.lineage_id.clone(), variable.key.clone());
            if rows.contains(&row) && !sealed(&variable.value) {
                held.entry(row)
                    .or_default()
                    .insert(variable.value.stored_fingerprint().to_owned());
            }
        }
    }
}

/// [`empty_plain_in`] for only the values with a fingerprint `held` holds for their
/// row: a copy of a sealed value's text, never another value under its key.
pub(crate) fn empty_held_in(base: &mut SavedEnvironmentIntent, held: &Held) -> bool {
    let mut emptied = false;
    for service in &mut base.services {
        for variable in &mut service.variables {
            let row = (service.lineage_id.clone(), variable.key.clone());
            if held
                .get(&row)
                .is_some_and(|held| held.contains(variable.value.stored_fingerprint()))
            {
                emptied |= empty_variable(&mut variable.value);
            }
        }
    }
    emptied
}

/// [`empty_plain_cell`] for a cell whose fingerprint is one of `held`.
pub(crate) fn empty_held_cell(cell: &mut Cell, held: &BTreeSet<String>) -> bool {
    let Cell::Value(Value::Object(fields)) = &*cell else {
        return false;
    };
    let matches = fields
        .get("fingerprint")
        .and_then(Value::as_str)
        .is_some_and(|fingerprint| held.contains(fingerprint));
    matches && empty_plain_cell(cell)
}

/// Empty a variable's plain or generated text, keeping its kind and fingerprint.
fn empty_variable(value: &mut VariableValue) -> bool {
    match value {
        VariableValue::Plain {
            text: PlainText::Literal(value),
            ..
        }
        | VariableValue::Generated { value, .. }
            if !value.is_empty() =>
        {
            value.clear();
        }
        VariableValue::Plain {
            text: PlainText::Template(parts),
            ..
        } if !parts.is_empty() => parts.clear(),
        VariableValue::Plain { .. }
        | VariableValue::Generated { .. }
        | VariableValue::Arriving { .. }
        | VariableValue::Sealed { .. }
        | VariableValue::NotSet => return false,
    }
    true
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
