//! Which variables an Environment ever sealed: History never brings plain text
//! back for one.

use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{
    Cell, EnvironmentNodeType, SavedEnvironmentIntent, SavedServiceIntent, SavedVariableIntent,
    SavedVariableValue, ValuePart,
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

/// Show sealed each plain value `introduced` keeps that is sealed since: by the
/// Environment, now or before it deleted or added the variable again, or, for one
/// it was made with or copied, by its Parent, the node's owner or an Environment
/// above them. A Node Introduction keeps the value a Branch was made with, and a
/// review compares a new node with it, but never reads a sealed value back. The
/// fingerprint stays, so the comparison still sees whether the value changed.
pub(crate) fn hide_introduced(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    introduced: &mut SavedEnvironmentIntent,
    working: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    each_sealed_since(tx, environment, introduced, working, |was, _| {
        was.value = SavedVariableValue::Secret {
            encrypted_value: None,
        };
        was.generated = None;
    })
}

/// Read each plain variable of the `copied` Services in `running`, what `owner`
/// runs or tried to, as the owner holds it now: a seal saved but not yet deployed,
/// or whose deploy failed, leaves the plain value in Applied State, and one made
/// after a failed save leaves it in that revision. A copy of either never takes
/// it. A variable the owner's Working State holds sealed, by ID or else key, takes
/// that seal; one it sealed and deleted since is sealed without a value (Not set);
/// another added plain since under its key stays as it is, as History's
/// [`super::Scrub`] reads them.
pub(crate) fn seal_running(
    tx: &mut dyn Tx,
    owner: &scope::Environment,
    running: &mut SavedEnvironmentIntent,
    copied: &BTreeSet<String>,
) -> Result<(), RpcError> {
    let ever = EverSealed::load(tx, owner.summary.id.as_str())?;
    if ever.is_empty() {
        return Ok(());
    }
    for service in &mut running.services {
        if !copied.contains(&service.lineage_id) {
            continue;
        }
        let now = owner
            .working
            .services
            .iter()
            .find(|now| now.id == service.id)
            .map(|now| &now.variables);
        for variable in &mut service.variables {
            if sealed(&variable.value) {
                continue;
            }
            let now = now.and_then(|now| {
                now.iter()
                    .find(|now| now.id == variable.id)
                    .or_else(|| now.iter().find(|now| now.key == variable.key))
            });
            let (value, fingerprint) = match now {
                Some(now) if sealed(&now.value) => {
                    (now.value.clone(), now.value_fingerprint.clone())
                }
                Some(_) if !ever.holds_id(&service.id, variable) => continue,
                None if !ever.holds(&service.id, variable) => continue,
                Some(_) | None => (SavedVariableValue::SecretWithoutValue, String::new()),
            };
            variable.value = value;
            variable.value_fingerprint = fingerprint;
            variable.generated = None;
        }
    }
    Ok(())
}

/// Make Not set each plain value `introduced` keeps that [`hide_introduced`]
/// shows sealed: a discard that brings a variable back from a Node Introduction
/// brings it back sealed without a value, never as the earlier plain value. One
/// the Environment holds sealed now keeps its plain value, so the discard refuses
/// to overwrite the sealed one, as it always has.
pub(crate) fn unset_introduced(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    introduced: &mut SavedEnvironmentIntent,
    working: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    each_sealed_since(tx, environment, introduced, working, |was, held| {
        if held {
            return;
        }
        was.value = SavedVariableValue::SecretWithoutValue;
        was.value_fingerprint.clear();
        was.generated = None;
    })
}

/// Apply `seal` to each plain variable `introduced` keeps that is sealed since,
/// as [`hide_introduced`] says, with whether `working` holds it sealed now.
fn each_sealed_since(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    introduced: &mut SavedEnvironmentIntent,
    working: &SavedEnvironmentIntent,
    seal: impl Fn(&mut SavedVariableIntent, bool),
) -> Result<(), RpcError> {
    // Read only for a variable changed, deleted or added again: they scan every save.
    let mut ever = None;
    let mut chain = None;
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
                Some(now) if sealed(&now.value) => true,
                Some(now) if now.id == was.id && now.key == was.key && now.value == was.value => {
                    false
                }
                // Changed in place: never sealed here, as a sealed value never turns plain.
                Some(now) if now.id == was.id => sealed_above(
                    tx,
                    &mut chain,
                    environment,
                    (&service.id, &service.lineage_id),
                    was,
                )?,
                Some(_) | None => {
                    if ever.is_none() {
                        ever = Some(EverSealed::load(tx, environment.as_str())?);
                    }
                    let own = ever.as_ref().is_some_and(|ever| match now {
                        Some(_) => ever.holds_id(&service.id, was),
                        None => ever.holds(&service.id, was),
                    });
                    own || sealed_above(
                        tx,
                        &mut chain,
                        environment,
                        (&service.id, &service.lineage_id),
                        was,
                    )?
                }
            };
            if sealed_now {
                seal(was, now.is_some_and(|now| sealed(&now.value)));
            }
        }
    }
    Ok(())
}

/// Whether `environment` was made with or copied `was`, of its node with this ID
/// and lineage, and an Environment above it sealed that since. Reads the [`Chain`]
/// the first time.
fn sealed_above(
    tx: &mut dyn Tx,
    chain: &mut Option<Chain>,
    environment: &EnvironmentId,
    (node, lineage): (&str, &str),
    was: &SavedVariableIntent,
) -> Result<bool, RpcError> {
    let chain = match chain {
        Some(chain) => chain,
        None => {
            let mut read = Chain::default();
            read.load(tx, environment.as_str())?;
            chain.insert(read)
        }
    };
    Ok(chain.holds(environment.as_str(), node, lineage, &was.key))
}

/// Each variable a Branch was made with, or copied from a node's owner since, that
/// the Parent or owner sealed since, or kept as it was made with or copied and an
/// Environment above it sealed. The Branch's own copy has its own ID, so the seal
/// is matched by the ID of the variable the copy came from, never by key alone,
/// which a variable added since under that key shares.
#[derive(Default)]
struct SealedAbove {
    /// By Service lineage and key, for a node the Branch was made with.
    made_with: BTreeSet<(String, String)>,
    /// By the Branch's node ID, then key, for each Own Copy made since: a copy
    /// removed and made again has a new ID, and its own source.
    copied: BTreeMap<String, BTreeMap<String, CopiedFrom>>,
}

/// Where an Own Copy's variable was sealed. The copy's row keeps no lineage, so
/// one sealed above its owner is read when a review asks, with the lineage it has.
enum CopiedFrom {
    Owner,
    /// The owner's node `node` was made with or copied it under `key`, and kept it.
    Above {
        owner: String,
        node: String,
        key: String,
    },
}

/// The [`SealedAbove`] of an Environment and of every one above it a review
/// reaches: its Parent and each owner its Own Copies came from, up to the root.
/// Each is above the one that reaches it, so the chain never loops.
#[derive(Default)]
struct Chain {
    sealed: BTreeMap<String, SealedAbove>,
    ledgers: BTreeMap<String, EverSealed>,
    /// By Environment, its Node Introductions' variables: what it was made with or
    /// copied, never a variable added since.
    introduced: BTreeMap<String, Vec<Introduced>>,
}

/// A variable as a Node Introduction holds it.
#[derive(Clone)]
struct Introduced {
    node: String,
    id: String,
    key: String,
    fingerprint: String,
}

impl Chain {
    /// Whether the variable `environment`'s node `node` was made with or copied
    /// under `key` was sealed above it. `environment` is loaded.
    fn holds(&self, environment: &str, node: &str, lineage: &str, key: &str) -> bool {
        let Some(sealed) = self.sealed.get(environment) else {
            return false;
        };
        match sealed.copied.get(node) {
            Some(keys) => keys.get(key).is_some_and(|from| match from {
                CopiedFrom::Owner => true,
                CopiedFrom::Above {
                    owner,
                    node: at,
                    key: was,
                } => self.holds(owner, at, lineage, was),
            }),
            None => sealed
                .made_with
                .contains(&(lineage.to_owned(), key.to_owned())),
        }
    }

    /// Read what `environment` was made with and copied, and what sealed it.
    fn load(&mut self, tx: &mut dyn Tx, environment: &str) -> Result<(), RpcError> {
        if self.sealed.contains_key(environment) {
            return Ok(());
        }
        let mut sealed = SealedAbove::default();
        for row in tx.query(
            "SELECT parent_id, made_with FROM config_environment_branch WHERE environment_id = ?1",
            &[environment.into()],
        )? {
            let parent = row.text(0)?;
            for service in row.intent(1, "Branch")?.services {
                for variable in &service.variables {
                    if ledger(tx, &mut self.ledgers, parent)?.holds_id(&service.id, variable)
                        || self.kept_and_sealed(tx, parent, &service, variable)?
                    {
                        sealed
                            .made_with
                            .insert((service.lineage_id.clone(), variable.key.clone()));
                    }
                }
            }
        }
        for row in tx.query(
            "SELECT node_id, source_id, source_node_id, variable_id, key \
             FROM config_node_copied_from WHERE environment_id = ?1",
            &[environment.into()],
        )? {
            let (copy, owner, node, variable, key) = (
                row.text(0)?,
                row.text(1)?,
                row.text(2)?,
                row.text(3)?,
                row.text(4)?,
            );
            let from = if ledger(tx, &mut self.ledgers, owner)?
                .ids
                .contains(&(node.to_owned(), variable.to_owned()))
            {
                Some(CopiedFrom::Owner)
            } else {
                // Only the value the owner kept as it was made with or copied it.
                let came = self.introduced(tx, environment, copy, |was| was.key == key)?;
                match came {
                    Some(came) => self
                        .introduced(tx, owner, node, |was| {
                            was.id == variable && was.fingerprint == came.fingerprint
                        })?
                        .map(|was| CopiedFrom::Above {
                            owner: owner.to_owned(),
                            node: node.to_owned(),
                            key: was.key,
                        }),
                    None => None,
                }
            };
            if matches!(from, Some(CopiedFrom::Above { .. })) {
                self.load(tx, owner)?;
            }
            let keys = sealed.copied.entry(copy.to_owned()).or_default();
            if let Some(from) = from {
                keys.insert(key.to_owned(), from);
            }
        }
        self.sealed.insert(environment.to_owned(), sealed);
        Ok(())
    }

    /// Whether `environment` kept `variable` of its node `service` as it was made
    /// with or copied, and an Environment above it sealed that.
    fn kept_and_sealed(
        &mut self,
        tx: &mut dyn Tx,
        environment: &str,
        service: &SavedServiceIntent,
        variable: &SavedVariableIntent,
    ) -> Result<bool, RpcError> {
        let Some(was) = self.introduced(tx, environment, &service.id, |was| {
            was.id == variable.id && was.fingerprint == variable.value_fingerprint
        })?
        else {
            return Ok(false);
        };
        self.load(tx, environment)?;
        // By the key it was made with or copied under: a rename since keeps the ID.
        Ok(self.holds(environment, &service.id, &service.lineage_id, &was.key))
    }

    /// The variable of `environment`'s Node Introduction of `node` that `matches`.
    fn introduced(
        &mut self,
        tx: &mut dyn Tx,
        environment: &str,
        node: &str,
        matches: impl Fn(&Introduced) -> bool,
    ) -> Result<Option<Introduced>, RpcError> {
        let introduced = match self.introduced.entry(environment.to_owned()) {
            Entry::Occupied(introduced) => introduced.into_mut(),
            Entry::Vacant(at) => {
                let mut introduced = Vec::new();
                for row in tx.query(
                    "SELECT node FROM config_node_introduction \
                     WHERE environment_id = ?1 AND node_type = ?2",
                    &[
                        environment.into(),
                        crate::storage::name_of(EnvironmentNodeType::Service)
                            .as_str()
                            .into(),
                    ],
                )? {
                    let service: SavedServiceIntent = row.json(0, "Node Introduction")?;
                    introduced.extend(service.variables.into_iter().map(|variable| Introduced {
                        node: service.id.clone(),
                        id: variable.id,
                        key: variable.key,
                        fingerprint: variable.value_fingerprint,
                    }));
                }
                at.insert(introduced)
            }
        };
        Ok(introduced
            .iter()
            .find(|was| was.node == node && matches(was))
            .cloned())
    }
}

/// `source`'s [`EverSealed`], loaded once per review.
fn ledger<'a>(
    tx: &mut dyn Tx,
    ledgers: &'a mut BTreeMap<String, EverSealed>,
    source: &str,
) -> Result<&'a EverSealed, RpcError> {
    Ok(match ledgers.entry(source.to_owned()) {
        Entry::Occupied(ever) => ever.into_mut(),
        Entry::Vacant(at) => {
            let ever = EverSealed::load(tx, source)?;
            at.insert(ever)
        }
    })
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
