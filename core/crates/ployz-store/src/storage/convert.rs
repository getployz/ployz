//! `0009_offer_conditional_syncs`: every Conditional Sync still waiting for its pull
//! request becomes an offered proposal, and what the pull requests' facts said about
//! their merge becomes the sticky `merged`.
//!
//! What a Conditional Sync stored is copied as text: only its source Environment's ID
//! and name are read, so nothing is unsealed or sealed again. A row it cannot read,
//! or one that would overwrite a proposal already there, stops the migration and
//! names the row; the transaction rolls back and the legacy tables stay as they were.

use ployz_core::RpcError;
use serde_json::{Map, Value, json};

use super::{Backend, Param, Tx};
use crate::error;

/// What the conversion did, for the log.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Converted {
    /// Standing Conditional Syncs offered.
    pub(crate) standing: usize,
    /// Frozen ones offered, their pull request merged.
    pub(crate) frozen: usize,
    /// Landed ones, dropped: what landed is Saved, and any hint one left goes too.
    pub(crate) landed_dropped: usize,
    /// Held secret values no offer took, dropped with their table.
    pub(crate) orphan_held: usize,
    /// Waiting pushes that carried frozen Conditional Syncs: they deploy from Saved now.
    pub(crate) retired_attachments: usize,
}

pub(super) fn step(tx: &mut dyn Tx, backend: Backend) -> Result<(), RpcError> {
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
        standing = converted.standing,
        frozen = converted.frozen,
        landed_dropped = converted.landed_dropped,
        orphan_held = converted.orphan_held,
        retired_attachments = converted.retired_attachments,
        "Conditional Syncs converted to offers"
    );
    Ok(())
}

pub(crate) fn convert(tx: &mut dyn Tx) -> Result<Converted, RpcError> {
    let mut converted = Converted::default();
    facts(tx)?;
    let rows = tx.query(
        "SELECT id, organization_id, environment_id, state, pr_environment_id, repository_id, \
         number, target_branch, working_revision, merge_commit, stored \
         FROM config_conditional_sync ORDER BY id",
        &[],
    )?;
    let mut held_taken = 0;
    for (done, row) in rows.iter().enumerate() {
        fault::at(done)?;
        let id = row.text(0)?;
        let corrupt = || error::corrupt(&format!("Conditional Sync {id}"));
        let state = row.text(3).map_err(|_| corrupt())?;
        match state {
            "landed" => {
                converted.landed_dropped += 1;
                continue;
            }
            "standing" => converted.standing += 1,
            "frozen" => converted.frozen += 1,
            _ => return Err(corrupt()),
        }
        let organization = row.text(1).map_err(|_| corrupt())?;
        let into = row.text(2).map_err(|_| corrupt())?;
        let repository = row.int(5).map_err(|_| corrupt())?;
        let number = row.int(6).map_err(|_| corrupt())?;
        let stored = row.text(10).map_err(|_| corrupt())?;
        let environment = serde_json::from_str::<Value>(stored)
            .ok()
            .and_then(|stored| stored.get("environment").cloned())
            .ok_or_else(corrupt)?;
        let (Some(source_id), Some(name)) = (
            environment.get("id").and_then(Value::as_str),
            environment.get("name").and_then(Value::as_str),
        ) else {
            return Err(corrupt());
        };
        let source = row
            .optional_text(4)
            .map_err(|_| corrupt())?
            .unwrap_or(source_id);
        let held = held(tx, into, repository, number)?;
        held_taken += held.len();
        let offered = json!({ "v": 1, "stored": stored, "held": held }).to_string();
        collide(tx, id, into, source, (repository, number))?;
        tx.execute(
            "INSERT INTO config_proposal (id, organization_id, environment_id, \
             source_environment_id, repository_id, number, source_name, source_revision, \
             first_sync, last_sync, offered) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?1, ?1, ?9)",
            &[
                id.into(),
                organization.into(),
                into.into(),
                source.into(),
                repository.into(),
                number.into(),
                name.into(),
                row.int(8).map_err(|_| corrupt())?.into(),
                offered.as_str().into(),
            ],
        )?;
        tx.execute(
            "INSERT INTO config_sync_receipt (organization_id, sync_id, environment_id, \
             source_environment_id, proposal_id) VALUES (?1, ?2, ?3, ?4, ?2) \
             ON CONFLICT DO NOTHING",
            &[organization.into(), id.into(), into.into(), source.into()],
        )?;
        if state == "frozen" {
            let commit = row.text(9).map_err(|_| corrupt())?;
            let target = row.text(7).map_err(|_| corrupt())?;
            tx.execute(
                "UPDATE config_pull_request SET merged = ?1 \
                 WHERE organization_id = ?2 AND repository_id = ?3 AND number = ?4 \
                 AND merged IS NULL",
                &[
                    json!({ "commit": commit, "into": target })
                        .to_string()
                        .as_str()
                        .into(),
                    organization.into(),
                    repository.into(),
                    number.into(),
                ],
            )?;
        }
    }
    fault::at(rows.len())?;
    let all_held = tx.query("SELECT COUNT(*) FROM config_held_secret", &[])?;
    converted.orphan_held = count(&all_held)?.saturating_sub(held_taken);
    let attached = tx.query(
        "SELECT COUNT(*) FROM config_waiting_deploy WHERE syncs <> '[]'",
        &[],
    )?;
    converted.retired_attachments = count(&attached)?;
    Ok(converted)
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

/// The Destination's held values for the pull request, by `<lineage>/<at>`, each
/// the sealed cell's text as held.
fn held(
    tx: &mut dyn Tx,
    into: &str,
    repository: i64,
    number: i64,
) -> Result<Map<String, Value>, RpcError> {
    let rows = tx.query(
        "SELECT lineage, at, value FROM config_held_secret \
         WHERE environment_id = ?1 AND repository_id = ?2 AND number = ?3 \
         ORDER BY lineage, at",
        &[into.into(), repository.into(), number.into()],
    )?;
    rows.iter()
        .map(|row| {
            Ok((
                format!("{}/{}", row.text(0)?, row.text(1)?),
                Value::String(row.text(2)?.to_owned()),
            ))
        })
        .collect()
}

/// Refuse to convert `id` over a proposal already there for the same Destination and
/// source or pull request, or with its ID: naming both, so an operator can remove it.
fn collide(
    tx: &mut dyn Tx,
    id: &str,
    into: &str,
    source: &str,
    (repository, number): (i64, i64),
) -> Result<(), RpcError> {
    let found = tx.query(
        "SELECT id FROM config_proposal WHERE id = ?1 OR (environment_id = ?2 \
         AND (source_environment_id = ?3 OR (repository_id = ?4 AND number = ?5))) \
         ORDER BY id",
        &[
            id.into(),
            into.into(),
            source.into(),
            Param::Int(repository),
            Param::Int(number),
        ],
    )?;
    let Some(row) = found.first() else {
        return Ok(());
    };
    let proposal = row.text(0)?;
    // The legacy table has no key on its pull request: two of its rows may wait on
    // the same one for the same Destination, and only one becomes the offer.
    let converted = !tx
        .query(
            "SELECT id FROM config_conditional_sync WHERE id = ?1",
            &[proposal.into()],
        )?
        .is_empty();
    let message = if converted {
        format!(
            "Conditional Syncs {proposal} and {id} wait on the same pull request in the same Environment: delete one, then open the Config Store again"
        )
    } else {
        format!(
            "Conditional Sync {id} can't become an offer: proposal {proposal} is already there. Remove it, then open the Config Store again"
        )
    };
    Err(error::conflict(
        message,
        json!({ "conditional_sync": id, "proposal": proposal }),
    ))
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
