//! What sealing a value takes out of History: every earlier saved version, and the
//! Sync bases, lose their plain copy.

use std::collections::BTreeSet;

use ployz_core::RpcError;
use ployz_core::config::{SavedEnvironmentIntent, VariableValue};

use super::{ledger, walk};
use crate::id::{EnvironmentId, Revision};
use crate::storage::Tx;
use crate::{review, scope};

/// Sealing a value removes every earlier copy of it from History: plain text
/// and older ciphertexts both become the sealed value, matched by stable ID so
/// a rename keeps it, and plain text of a value sealed then deleted becomes
/// sealed without a value. A revision a deployment still runs keeps its copy
/// until that deployment leaves flight, because it records what it applied
/// from that revision when it finishes.
pub(crate) struct Scrub {
    latest: Option<review::Saved>,
    ledger: ledger::SealLedger,
    running: BTreeSet<Revision>,
}

impl Scrub {
    pub(crate) fn load(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<Self, RpcError> {
        let ledger = ledger::SealLedger::load_with_saves(tx, environment.as_str())?;
        if ledger.is_empty() {
            return Ok(Self {
                latest: None,
                ledger,
                running: BTreeSet::new(),
            });
        }
        let running = tx
            .query(
                &format!(
                    "SELECT saved_revision FROM config_deployment WHERE environment_id = ?1 AND {}",
                    crate::deployment::in_flight_sql()
                ),
                &[environment.as_str().into()],
            )?
            .iter()
            .map(|row| row.number(0, "Deployment revision").map(Revision))
            .collect::<Result<BTreeSet<_>, _>>()?;
        Ok(Self {
            latest: review::latest_saved(tx, environment)?,
            ledger,
            running,
        })
    }

    pub(crate) fn ledger(&self) -> &ledger::SealLedger {
        &self.ledger
    }

    /// Remove from `saved`, the version saved at `revision`, what sealing took
    /// out of History; whether anything changed.
    pub(crate) fn apply(&self, revision: Revision, saved: &mut SavedEnvironmentIntent) -> bool {
        let Some(latest) = &self.latest else {
            return false;
        };
        if revision >= latest.revision || self.running.contains(&revision) {
            return false;
        }
        let mut scrubbed = false;
        for service in &mut saved.services {
            let now = latest
                .intent
                .services
                .iter()
                .find(|now| now.id == service.id)
                .map(|now| &now.variables);
            for was in &mut service.variables {
                if was.value == VariableValue::NotSet {
                    continue;
                }
                let now = now.and_then(|now| {
                    now.iter()
                        .find(|now| now.id == was.id)
                        .or_else(|| now.iter().find(|now| now.key == was.key))
                });
                let value = match now {
                    Some(now) if ledger::sealed(&now.value) => &now.value,
                    // Added again plain since: only this very variable was sealed.
                    Some(now)
                        if now.id != was.id
                            && !ledger::sealed(&was.value)
                            && self.ledger.holds_id(&service.id, was) =>
                    {
                        &VariableValue::NotSet
                    }
                    None if !ledger::sealed(&was.value) && self.ledger.holds(&service.id, was) => {
                        &VariableValue::NotSet
                    }
                    Some(_) | None => continue,
                };
                if !was.value.holds_same(value) {
                    was.value = value.clone();
                    scrubbed = true;
                }
            }
        }
        scrubbed
    }
}

/// Write [`Scrub`] into every earlier saved version, and keep no sealed plain value
/// in a Sync base, so storage holds no copy History hides.
pub(crate) fn scrub_sealed(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<(), RpcError> {
    walk::scrub_bases(tx, environment.as_str())?;
    let scrub = Scrub::load(tx, environment)?;
    let Some(latest) = &scrub.latest else {
        return Ok(());
    };
    let rows = tx.query(
        "SELECT revision, intent FROM config_saved WHERE environment_id = ?1 AND revision < ?2",
        &[
            environment.as_str().into(),
            scope::revision_param(latest.revision)?.into(),
        ],
    )?;
    for row in rows {
        let earlier = Revision(row.number(0, "revision")?);
        let mut saved = row.intent(1, "Saved State")?;
        if scrub.apply(earlier, &mut saved) {
            // A scrub matched by key seals a variable under its own ID.
            ledger::SealLedger::record(tx, environment.as_str(), &saved)?;
            review::saved::rewrite_intent(tx, environment, earlier, &saved)?;
        }
    }
    Ok(())
}
