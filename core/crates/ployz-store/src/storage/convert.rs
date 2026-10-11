//! `0009_offer_conditional_syncs`: what the pull requests' facts said about their
//! merge becomes the sticky `merged`, and the Conditional Syncs go. A standing one's
//! changes land with its pull request's merge from the PR Environment, all of them;
//! a frozen one's pull request merged already, so its `merged` stops a late event
//! from landing it again.

use ployz_core::RpcError;
use serde::Serialize;
use serde_json::{Value, json};
use ts_rs::TS;

use super::{Backend, Tx};
use crate::error;

/// What retiring a Store's Conditional Syncs did: its one receipt, which the process
/// that opened the Store logs.
#[derive(Debug, Default, PartialEq, Eq, Serialize, TS)]
pub struct Converted {
    /// Conditional Syncs dropped, landed or not.
    pub dropped: usize,
    /// Held secret values dropped with their table.
    pub held_dropped: usize,
    /// Waiting pushes that carried frozen Conditional Syncs: they deploy from Saved now.
    pub retired_attachments: usize,
}

pub(super) fn step(tx: &mut dyn Tx, backend: Backend) -> Result<Converted, RpcError> {
    if backend == Backend::Postgres {
        // A process still on the old schema writes none of these while they convert.
        tx.execute(
            "LOCK TABLE config_conditional_sync, config_held_secret, config_waiting_deploy \
             IN SHARE ROW EXCLUSIVE MODE",
            &[],
        )?;
    }
    let converted = convert(tx)?;
    tracing::info!(
        dropped = converted.dropped,
        held_dropped = converted.held_dropped,
        retired_attachments = converted.retired_attachments,
        "Conditional Syncs retired"
    );
    Ok(converted)
}

fn convert(tx: &mut dyn Tx) -> Result<Converted, RpcError> {
    facts(tx)?;
    let rows = tx.query(
        "SELECT id, organization_id, state, repository_id, number, target_branch, merge_commit \
         FROM config_conditional_sync ORDER BY id",
        &[],
    )?;
    for (done, row) in rows.iter().enumerate() {
        fault::at(done)?;
        let id = row.text(0)?;
        let corrupt = || error::corrupt(&format!("Conditional Sync {id}"));
        match row.text(2).map_err(|_| corrupt())? {
            "landed" | "standing" => continue,
            "frozen" => {}
            _ => return Err(corrupt()),
        }
        let commit = row.text(6).map_err(|_| corrupt())?;
        let target = row.text(5).map_err(|_| corrupt())?;
        tx.execute(
            "UPDATE config_pull_request SET merged = ?1 \
             WHERE organization_id = ?2 AND repository_id = ?3 AND number = ?4 \
             AND merged IS NULL",
            &[
                json!({ "commit": commit, "into": target })
                    .to_string()
                    .as_str()
                    .into(),
                row.text(1).map_err(|_| corrupt())?.into(),
                row.int(3).map_err(|_| corrupt())?.into(),
                row.int(4).map_err(|_| corrupt())?.into(),
            ],
        )?;
    }
    fault::at(rows.len())?;
    let held = tx.query("SELECT COUNT(*) FROM config_held_secret", &[])?;
    let attached = tx.query(
        "SELECT COUNT(*) FROM config_waiting_deploy WHERE syncs <> '[]'",
        &[],
    )?;
    Ok(Converted {
        dropped: rows.len(),
        held_dropped: count(&held)?,
        retired_attachments: count(&attached)?,
    })
}

/// Every pull request's facts without `merge_reached`, which no longer exists, and
/// `merged` from the facts of one that merged.
fn facts(tx: &mut dyn Tx) -> Result<(), RpcError> {
    let rows = tx.query(
        "SELECT organization_id, repository_id, number, facts FROM config_pull_request \
         ORDER BY organization_id, repository_id, number",
        &[],
    )?;
    for row in rows {
        let (organization, repository, number) = (row.text(0)?, row.int(1)?, row.int(2)?);
        let corrupt = || error::corrupt(&format!("pull request {repository}#{number}"));
        let Ok(Value::Object(mut facts)) = serde_json::from_str::<Value>(row.text(3)?) else {
            return Err(corrupt());
        };
        facts.remove("merge_reached");
        let merged = match (facts.get("merge_commit"), facts.get("target_branch")) {
            (Some(Value::String(commit)), Some(Value::String(into))) => {
                Some(json!({ "commit": commit, "into": into }).to_string())
            }
            (None | Some(Value::Null), _) => None,
            _ => return Err(corrupt()),
        };
        tx.execute(
            "UPDATE config_pull_request SET facts = ?1, merged = COALESCE(merged, ?2) \
             WHERE organization_id = ?3 AND repository_id = ?4 AND number = ?5",
            &[
                Value::Object(facts).to_string().as_str().into(),
                merged.as_deref().into(),
                organization.into(),
                repository.into(),
                number.into(),
            ],
        )?;
    }
    Ok(())
}

fn count(rows: &[super::Row]) -> Result<usize, RpcError> {
    let n = rows
        .first()
        .ok_or_else(|| error::corrupt("count"))?
        .int(0)?;
    usize::try_from(n).map_err(|_| error::corrupt("count"))
}

/// A stop injected after some number of rows, so a test can see that a conversion
/// that fails part way leaves nothing behind.
#[cfg(test)]
pub(crate) mod fault {
    use std::cell::Cell;

    use ployz_core::RpcError;

    thread_local! {
        static AFTER: Cell<Option<usize>> = const { Cell::new(None) };
    }

    /// Fail the conversion once it has converted `rows` rows; `None` never.
    pub(crate) fn after(rows: Option<usize>) {
        AFTER.with(|after| after.set(rows));
    }

    pub(super) fn at(done: usize) -> Result<(), RpcError> {
        if AFTER.with(Cell::get) == Some(done) {
            return Err(crate::error::internal(format!(
                "injected stop after {done} rows"
            )));
        }
        Ok(())
    }
}

#[cfg(not(test))]
mod fault {
    use ployz_core::RpcError;

    #[expect(clippy::unnecessary_wraps, reason = "the test build can fail here")]
    pub(super) const fn at(_done: usize) -> Result<(), RpcError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
