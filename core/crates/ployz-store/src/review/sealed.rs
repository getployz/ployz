//! Which variables an Environment ever sealed: History never brings plain text
//! back for one.

use std::collections::BTreeSet;

use ployz_core::RpcError;
use ployz_core::config::{
    EnvironmentNodeType, SavedEnvironmentIntent, SavedServiceIntent, SavedVariableIntent,
    SavedVariableValue,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::scope::{self, EnvironmentRef};
use crate::storage::Tx;
use crate::{Actor, EnvironmentSummary};

pub(crate) fn sealed(value: &SavedVariableValue) -> bool {
    match value {
        SavedVariableValue::Secret { encrypted_value: _ }
        | SavedVariableValue::SecretWithoutValue => true,
        SavedVariableValue::Literal { value: _ } | SavedVariableValue::Template { parts: _ } => {
            false
        }
    }
}

fn may_carry_sealed(value: &SavedVariableValue) -> bool {
    sealed(value) || matches!(value, SavedVariableValue::Template { parts: _ })
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
    names.extend(
        intents
            .iter()
            .flat_map(|intent| &intent.services)
            .flat_map(|service| &service.variables)
            .filter(|variable| may_carry_sealed(&variable.value))
            .map(|variable| variable.key.clone()),
    );
    Ok(names)
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
    /// Every name ever Sealed or templated in it, across its Services.
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

    pub(crate) fn holds(&self, service: &str, variable: &SavedVariableIntent) -> bool {
        self.ids
            .contains(&(service.to_owned(), variable.id.clone()))
            || self
                .keys
                .contains(&(service.to_owned(), variable.key.clone()))
    }
}

/// Record each variable `working` holds sealed, at the write that seals it, and
/// scrub the plain text it held from the Node Introductions.
pub(crate) fn record(
    tx: &mut dyn Tx,
    environment: &str,
    working: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    let mut sealing = false;
    for (service, variable) in sealed_in(working) {
        sealing |= tx.execute(
            "INSERT INTO config_sealed_variable (environment_id, service_id, variable_id, key) \
             VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING",
            &[
                environment.into(),
                service.into(),
                variable.id.as_str().into(),
                variable.key.as_str().into(),
            ],
        )? > 0;
    }
    if sealing {
        scrub_introductions(tx, environment)?;
    }
    Ok(())
}

/// Seal, without its value, each plain value in `environment`'s Node Introductions of
/// a variable it ever held sealed. A node never deployed compares with its
/// introduction, so the seal still shows as a change, and Discard brings back no
/// plain text.
pub(crate) fn scrub_introductions(tx: &mut dyn Tx, environment: &str) -> Result<(), RpcError> {
    let ever = EverSealed::load(tx, environment)?;
    if ever.is_empty() {
        return Ok(());
    }
    for row in tx.query(
        "SELECT node_id, node FROM config_node_introduction \
         WHERE environment_id = ?1 AND node_type = ?2",
        &[
            environment.into(),
            crate::storage::name_of(EnvironmentNodeType::Service)
                .as_str()
                .into(),
        ],
    )? {
        let mut service: SavedServiceIntent = row.json(1, "Node Introduction")?;
        let mut scrubbed = false;
        for variable in &mut service.variables {
            if !sealed(&variable.value) && ever.holds(&service.id, variable) {
                variable.value = SavedVariableValue::SecretWithoutValue;
                variable.value_fingerprint.clear();
                scrubbed = true;
            }
        }
        if scrubbed {
            tx.execute(
                "UPDATE config_node_introduction SET node = ?3 \
                 WHERE environment_id = ?1 AND node_id = ?2",
                &[
                    environment.into(),
                    row.text(0)?.into(),
                    scope::Node::Service(&service).document().as_str().into(),
                ],
            )?;
        }
    }
    Ok(())
}

/// Scrub what an earlier Store kept beside sealed values: the receiver's own in each
/// arrival no Sync claims, and every Environment's Node Introductions.
pub(crate) fn scrub_kept(tx: &mut dyn Tx) -> Result<(), RpcError> {
    crate::branch::forget_unclaimed(tx)?;
    for row in tx.query("SELECT id FROM config_environment", &[])? {
        scrub_introductions(tx, row.text(0)?)?;
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
