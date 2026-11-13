//! The ledger of every variable an Environment ever sealed, which the scrubs and a
//! review read.

use std::collections::BTreeSet;

use ployz_core::RpcError;
use ployz_core::config::{SavedEnvironmentIntent, SavedVariableIntent, VariableValue};

use crate::review::saved;
use crate::storage::{Param, Tx};

pub(crate) const fn sealed(value: &VariableValue) -> bool {
    value.is_secret()
}

/// The ledger of every variable an Environment ever held sealed, per Service: by
/// stable ID, so a rename keeps the seal, and by key, so a new variable under that
/// key does. Each Save, History scrub and Working State write records the seals it
/// holds ([`Self::record`]), and `0033_record_saved_seals` and
/// `0038_record_late_saved_seals` recorded those saved before. A reseal records
/// nothing: it re-encrypts only seals already held.
#[derive(Default)]
pub(crate) struct SealLedger {
    pub(crate) ids: BTreeSet<(String, String)>,
    pub(crate) keys: BTreeSet<(String, String)>,
}

impl SealLedger {
    /// The ledger alone: what a review reads, once per Environment its walk reaches.
    pub(crate) fn load(tx: &mut dyn Tx, environment: &str) -> Result<Self, RpcError> {
        let mut ledger = Self::default();
        for row in tx.query(
            "SELECT service_id, variable_id, key FROM config_sealed_variable \
             WHERE environment_id = ?1",
            &[environment.into()],
        )? {
            let service = row.text(0)?;
            ledger
                .ids
                .insert((service.to_owned(), row.text(1)?.to_owned()));
            ledger
                .keys
                .insert((service.to_owned(), row.text(2)?.to_owned()));
        }
        Ok(ledger)
    }

    /// The ledger and every seal the saves hold, for the writes that remove or copy a
    /// plain value.
    // TODO(#3752): read the ledger alone at the next major. A Store from before 0033
    // shares the database during a rolling deploy and saves seals it never records.
    pub(crate) fn load_with_saves(tx: &mut dyn Tx, environment: &str) -> Result<Self, RpcError> {
        let mut ledger = Self::load(tx, environment)?;
        for saved in saved::intents(tx, environment)? {
            for (service, variable) in sealed_in(&saved) {
                ledger.ids.insert((service.to_owned(), variable.id.clone()));
                ledger
                    .keys
                    .insert((service.to_owned(), variable.key.clone()));
            }
        }
        Ok(ledger)
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

    /// Record each variable `intent` holds sealed, at the write of Working or Saved
    /// State that holds it.
    pub(crate) fn record(
        tx: &mut dyn Tx,
        environment: &str,
        intent: &SavedEnvironmentIntent,
    ) -> Result<(), RpcError> {
        let seals: Vec<_> = sealed_in(intent)
            .map(|(service, variable)| (service, variable.id.as_str(), variable.key.as_str()))
            .collect();
        insert(tx, environment, &seals)
    }

    /// Record each variable every Environment's Working State holds sealed: seals
    /// made before the ledger existed.
    pub(crate) fn record_working(tx: &mut dyn Tx) -> Result<(), RpcError> {
        for row in tx.query("SELECT id, working FROM config_environment", &[])? {
            Self::record(tx, row.text(0)?, &row.intent(1, "Working State")?)?;
        }
        Ok(())
    }

    /// Record each variable every Environment's Working State and saved versions
    /// hold sealed: seals saved before every save recorded them. Each Environment's
    /// seals are recorded once, however many saves repeat them.
    pub(crate) fn record_saved(tx: &mut dyn Tx) -> Result<(), RpcError> {
        for environment in tx.query("SELECT id, working FROM config_environment", &[])? {
            let id = environment.text(0)?;
            let mut seals = BTreeSet::new();
            let mut hold = |intent: &SavedEnvironmentIntent| {
                seals.extend(sealed_in(intent).map(|(service, variable)| {
                    (
                        service.to_owned(),
                        variable.id.clone(),
                        variable.key.clone(),
                    )
                }));
            };
            hold(&environment.intent(1, "Working State")?);
            // One Environment's saves at a time, so a Store of many holds only one in memory.
            for saved in saved::intents(tx, id)? {
                hold(&saved);
            }
            let seals: Vec<_> = seals
                .iter()
                .map(|(service, variable, key)| (service.as_str(), variable.as_str(), key.as_str()))
                .collect();
            insert(tx, id, &seals)?;
        }
        Ok(())
    }
}

/// Rows per `INSERT`: well under either database's limit on bound parameters.
const INSERT_ROWS: usize = 500;

/// Add each `(service, variable, key)` of `environment` to the ledger, in as few
/// statements as the batch allows.
fn insert(
    tx: &mut dyn Tx,
    environment: &str,
    seals: &[(&str, &str, &str)],
) -> Result<(), RpcError> {
    for batch in seals.chunks(INSERT_ROWS) {
        let rows = (0..batch.len())
            .map(|row| {
                let at = row * 4;
                format!("(?{}, ?{}, ?{}, ?{})", at + 1, at + 2, at + 3, at + 4)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let params: Vec<Param<'_>> = batch
            .iter()
            .flat_map(|&(service, variable, key)| {
                [
                    environment.into(),
                    service.into(),
                    variable.into(),
                    key.into(),
                ]
            })
            .collect();
        tx.execute(
            &format!(
                "INSERT INTO config_sealed_variable (environment_id, service_id, variable_id, key) \
                 VALUES {rows} ON CONFLICT DO NOTHING"
            ),
            &params,
        )?;
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
    let ledger = SealLedger::load_with_saves(tx, environment)?;
    if ledger.is_empty() {
        return Ok(BTreeSet::new());
    }
    let mut intents = Vec::new();
    for row in tx.query(
        "SELECT working FROM config_environment WHERE id = ?1",
        &[environment.into()],
    )? {
        intents.push(row.intent(0, "Working State")?);
    }
    intents.extend(saved::intents(tx, environment)?);
    Ok(intents
        .iter()
        .flat_map(|intent| &intent.services)
        .flat_map(|service| {
            service
                .variables
                .iter()
                .filter(|variable| ledger.holds(&service.id, variable))
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

/// Each variable `after` holds sealed with another value than `before` holds it,
/// by Service lineage and key: sealed, or sealed again.
pub(crate) fn sealed_again(
    before: Option<&SavedEnvironmentIntent>,
    after: &SavedEnvironmentIntent,
) -> BTreeSet<(String, String)> {
    let value = |intent: &SavedEnvironmentIntent, lineage: &str, key: &str| {
        intent
            .services
            .iter()
            .filter(|service| service.lineage_id == lineage)
            .flat_map(|service| &service.variables)
            .find(|variable| variable.key == key)
            .map(|variable| variable.value.clone())
    };
    after
        .services
        .iter()
        .flat_map(|service| {
            service
                .variables
                .iter()
                .filter(|variable| sealed(&variable.value))
                .map(|variable| (service.lineage_id.as_str(), variable))
        })
        .filter(|(lineage, variable)| {
            before.and_then(|before| value(before, lineage, &variable.key))
                != Some(variable.value.clone())
        })
        .map(|(lineage, variable)| (lineage.to_owned(), variable.key.clone()))
        .collect()
}
