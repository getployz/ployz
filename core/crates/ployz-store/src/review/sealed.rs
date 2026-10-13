//! Which variables an Environment ever sealed: History never brings plain text
//! back for one.

use std::collections::BTreeSet;

use ployz_core::RpcError;
use ployz_core::config::{SavedEnvironmentIntent, SavedVariableIntent, SavedVariableValue};

use crate::storage::Tx;

pub(crate) fn sealed(value: &SavedVariableValue) -> bool {
    match value {
        SavedVariableValue::Secret { encrypted_value: _ }
        | SavedVariableValue::SecretWithoutValue => true,
        SavedVariableValue::Literal { value: _ } | SavedVariableValue::Template { parts: _ } => {
            false
        }
    }
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
