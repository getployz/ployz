//! The walks that empty a plain copy of a sealed value from what the Branch tables
//! and the Sync ledger keep: what a Branch was made with, a pair's base and an
//! arrival's cells. Arrivals are always locked before bases, the order Discard and
//! Sync lock them in.

use std::collections::BTreeSet;

use ployz_core::RpcError;
use ployz_core::config::{Cell, SavedEnvironmentIntent, SealedCell};

use ployz_core::config::At;

use super::{empty, ledger};
use crate::branch::{document, json_of, row_id, rows};
use crate::error;
use crate::storage::Tx;

/// What a Sync's Undo puts back for the receiver's own value sealed since.
const NOT_SET: SealedCell = SealedCell::Cell(Cell::SecretWithoutValue);

/// Keep no plain value `environment` ever sealed in a base it shares: what a Branch
/// was made with, a pair's base, and an arrival's prior cell, what landed and what the
/// other side held. Each keeps the value's fingerprint, which a Sync compares by. The
/// receiver's own value a Sync's Undo keeps becomes sealed and Not set. It holds no
/// lock on the other side of a pair, so each write replaces only the document it read:
/// a base another writer moved since stays as that writer left it, for the next Save
/// to empty.
pub(crate) fn scrub_bases(tx: &mut dyn Tx, environment: &str) -> Result<(), RpcError> {
    let rows = ledger::sealed_rows(tx, environment)?;
    empty_in_bases(tx, environment, &rows)
}

/// [`scrub_bases`] for `rows` only, by Service lineage and key.
pub(crate) fn empty_in_bases(
    tx: &mut dyn Tx,
    environment: &str,
    rows: &BTreeSet<(String, String)>,
) -> Result<(), RpcError> {
    if rows.is_empty() {
        return Ok(());
    }
    for read in rows::made_with_around(tx, environment)? {
        let mut made_with = read.intent.clone();
        if empty::empty_plain_in(&mut made_with, rows) {
            read.replace(tx, &made_with)?;
        }
    }
    for row in tx.query(
        "SELECT environment_id, other_id, lineage, at, prior FROM config_sync_arrival \
         WHERE (environment_id = ?1 OR other_id = ?1) AND prior IS NOT NULL",
        &[environment.into()],
    )? {
        let (lineage, at) = (row.text(2)?, row.text(3)?);
        let At::Variable(key) = row_id(lineage, at)?.at().clone() else {
            continue;
        };
        if !rows.contains(&(lineage.to_owned(), key)) {
            continue;
        }
        let mut prior: Cell = row.json(4, "Sync")?;
        if empty::empty_plain_cell(&mut prior) {
            tx.execute(
                "UPDATE config_sync_arrival SET prior = ?5 \
                 WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
                 AND prior = ?6",
                &[
                    row.text(0)?.into(),
                    row.text(1)?.into(),
                    lineage.into(),
                    at.into(),
                    json_of(&prior).as_str().into(),
                    row.text(4)?.into(),
                ],
            )?;
        }
    }
    // What landed and what the other side held, in any state: a plain value sealed
    // since keeps only its fingerprint, which Undo and Deploy compare it by.
    for row in tx.query(
        "SELECT environment_id, other_id, lineage, at, value, source FROM config_sync_arrival \
         WHERE environment_id = ?1 OR other_id = ?1",
        &[environment.into()],
    )? {
        let (lineage, at) = (row.text(2)?, row.text(3)?);
        let At::Variable(key) = row_id(lineage, at)?.at().clone() else {
            continue;
        };
        if !rows.contains(&(lineage.to_owned(), key)) {
            continue;
        }
        let mut value: Cell = row.json(4, "Sync")?;
        let mut source: Option<Cell> = row
            .optional_text(5)?
            .map(|source| serde_json::from_str(source).map_err(|_| error::corrupt("Sync")))
            .transpose()?;
        let emptied_value = empty::empty_plain_cell(&mut value);
        let emptied_source = source.as_mut().is_some_and(empty::empty_plain_cell);
        if emptied_value || emptied_source {
            tx.execute(
                "UPDATE config_sync_arrival SET value = ?5, source = ?6 \
                 WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
                 AND value = ?7 AND COALESCE(source, '') = COALESCE(?8, '')",
                &[
                    row.text(0)?.into(),
                    row.text(1)?.into(),
                    lineage.into(),
                    at.into(),
                    json_of(&value).as_str().into(),
                    source.as_ref().map(json_of).as_deref().into(),
                    row.text(4)?.into(),
                    row.optional_text(5)?.into(),
                ],
            )?;
        }
    }
    // The receiver's own value a Sync's Undo puts back: once sealed, it comes back
    // sealed and Not set, as History brings it back, never as plain text. As in
    // History, a key the receiver holds plain now keeps it: sealed once, it was
    // added again plain since.
    let working = tx
        .query(
            "SELECT working FROM config_environment WHERE id = ?1",
            &[environment.into()],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Environment"))?
        .intent(0, "Working State")?;
    for row in tx.query(
        "SELECT other_id, lineage, at, was FROM config_sync_arrival \
         WHERE environment_id = ?1 AND was IS NOT NULL",
        &[environment.into()],
    )? {
        let (lineage, at) = (row.text(1)?, row.text(2)?);
        let At::Variable(key) = row_id(lineage, at)?.at().clone() else {
            continue;
        };
        let plain_now = working
            .services
            .iter()
            .find(|service| service.lineage_id == lineage)
            .and_then(|service| service.variables.iter().find(|v| v.key == key))
            .is_some_and(|variable| !ledger::sealed(&variable.value));
        if plain_now || !rows.contains(&(lineage.to_owned(), key)) {
            continue;
        }
        // A variable's only cell with a value is a plain one, generated or not.
        if let SealedCell::Cell(Cell::Value(_)) = row.json(3, "Sync")? {
            tx.execute(
                "UPDATE config_sync_arrival SET was = ?5 \
                 WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
                 AND was = ?6",
                &[
                    environment.into(),
                    row.text(0)?.into(),
                    lineage.into(),
                    at.into(),
                    json_of(&NOT_SET).as_str().into(),
                    row.text(3)?.into(),
                ],
            )?;
        }
    }
    // Arrivals before bases, the order Discard and Sync lock them in.
    for row in tx.query(
        "SELECT environment_id, other_id, base FROM config_sync_base \
         WHERE environment_id = ?1 OR other_id = ?1",
        &[environment.into()],
    )? {
        let mut base = row.intent(2, "Sync base")?;
        if empty::empty_plain_in(&mut base, rows) {
            tx.execute(
                "UPDATE config_sync_base SET base = ?3 \
                 WHERE environment_id = ?1 AND other_id = ?2 AND base = ?4",
                &[
                    row.text(0)?.into(),
                    row.text(1)?.into(),
                    document(&base).as_str().into(),
                    row.text(2)?.into(),
                ],
            )?;
        }
    }
    Ok(())
}

/// Empty each plain value `environment` held for `rows`, which it seals now, in
/// what every Branch below it was made with, shares and holds as arrived: a
/// Branch of a Branch can keep it where the seal never landed, as in a Branch that
/// changed the value itself. `before`, the Working State a write replaces, may
/// hold a value it seals that nothing saved. Run where a write seals a value or a
/// Save seals one again, never on every Save: it reads the whole tree below.
pub(crate) fn empty_sealed_below(
    tx: &mut dyn Tx,
    environment: &str,
    rows: &BTreeSet<(String, String)>,
    before: Option<&SavedEnvironmentIntent>,
) -> Result<(), RpcError> {
    if rows.is_empty() {
        return Ok(());
    }
    let below = below(tx, environment)?;
    let mut sealed = held(tx, environment, rows, before, Took::Sealer)?;
    // A Branch that took the seal held what it shares below as the sealer did:
    // its own write emptied that, but not one before Ployz 0.3.15.
    for branch in &below {
        for (row, fingerprints) in held(tx, branch, rows, None, Took::Seal)? {
            sealed.entry(row).or_default().extend(fingerprints);
        }
    }
    empty_below(tx, &below, &sealed)
}

/// Each Environment below `environment`, Parents before their Branches.
pub(super) fn below(tx: &mut dyn Tx, environment: &str) -> Result<Vec<String>, RpcError> {
    let mut below = Vec::new();
    let mut next = std::collections::VecDeque::from([environment.to_owned()]);
    while let Some(parent) = next.pop_front() {
        for branch in rows::children(tx, &parent)? {
            next.push_back(branch.clone());
            below.push(branch);
        }
    }
    Ok(below)
}

/// Which of `rows` an Environment's fingerprints count for in [`held`].
#[derive(Clone, Copy)]
pub(super) enum Took {
    /// The sealer: each row but one it holds plain now.
    Sealer,
    /// A Branch below it: each row it holds sealed now.
    Seal,
}

/// Each fingerprint `environment` held plain for each of `rows` before it sealed
/// it, as the seal's walk reads them: none for a row it holds plain now.
pub(crate) fn held_by_sealer(
    tx: &mut dyn Tx,
    environment: &str,
    rows: &BTreeSet<(String, String)>,
) -> Result<empty::Held, RpcError> {
    held(tx, environment, rows, None, Took::Sealer)
}

/// Each fingerprint `environment` held for `rows`: in what its Working State held
/// before the write, and what it was made with or shares with its Parent and
/// Branches, which keep a value's fingerprint once emptied. Its History holds none
/// once scrubbed, and a write that seals a value passes what it replaced.
pub(super) fn held(
    tx: &mut dyn Tx,
    environment: &str,
    rows: &BTreeSet<(String, String)>,
    before: Option<&SavedEnvironmentIntent>,
    took: Took,
) -> Result<empty::Held, RpcError> {
    // A row it holds plain now was added again since: the seal was another
    // variable's, whose copies History alone reads as sealed.
    let working = tx
        .query(
            "SELECT working FROM config_environment WHERE id = ?1",
            &[environment.into()],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Environment"))?
        .intent(0, "Working State")?;
    let rows: BTreeSet<(String, String)> = rows
        .iter()
        .filter(|(lineage, key)| {
            let mut now = working
                .services
                .iter()
                .filter(|service| service.lineage_id == *lineage)
                .flat_map(|service| &service.variables)
                .filter(|variable| variable.key == *key);
            match took {
                Took::Sealer => !now.any(|variable| !ledger::sealed(&variable.value)),
                Took::Seal => now.any(|variable| ledger::sealed(&variable.value)),
            }
        })
        .cloned()
        .collect();
    let rows = &rows;
    let mut held = empty::Held::new();
    if let Some(before) = before {
        empty::hold(&mut held, before, rows);
    }
    // Only what it held itself: what it and its Branches were made with, and
    // what it shares with its Parent or a Branch, which starts as what the Branch
    // was made with and moves only by what lands. A Sync base with any other
    // Environment can start as that one's value.
    for read in rows::made_with_around(tx, environment)? {
        empty::hold(&mut held, &read.intent, rows);
    }
    for row in tx.query(
        "SELECT b.base FROM config_sync_base b JOIN config_environment_branch e \
         ON (e.environment_id = b.environment_id AND e.parent_id = b.other_id) \
         OR (e.environment_id = b.other_id AND e.parent_id = b.environment_id) \
         WHERE b.environment_id = ?1 OR b.other_id = ?1",
        &[environment.into()],
    )? {
        empty::hold(&mut held, &row.intent(0, "Sync base")?, rows);
    }
    Ok(held)
}

/// [`empty_sealed_below`] by the fingerprints in `held`.
fn empty_below(tx: &mut dyn Tx, below: &[String], held: &empty::Held) -> Result<(), RpcError> {
    if held.is_empty() {
        return Ok(());
    }
    for branch in below {
        let branch = branch.as_str();
        if let Some(read) = rows::made_with_of(tx, branch)? {
            let mut made_with = read.intent.clone();
            if empty::empty_held_in(&mut made_with, held) {
                read.replace(tx, &made_with)?;
            }
        }
    }
    // Arrivals before bases, the order Discard and Sync lock them in.
    for branch in below {
        let branch = branch.as_str();
        for row in tx.query(
            "SELECT environment_id, other_id, lineage, at, value, source, prior \
             FROM config_sync_arrival WHERE environment_id = ?1 OR other_id = ?1",
            &[branch.into()],
        )? {
            let (lineage, at) = (row.text(2)?, row.text(3)?);
            let At::Variable(key) = row_id(lineage, at)?.at().clone() else {
                continue;
            };
            let Some(fingerprints) = held.get(&(lineage.to_owned(), key)) else {
                continue;
            };
            let cell = |index: usize| -> Result<Option<Cell>, RpcError> {
                row.optional_text(index)?
                    .map(|cell| serde_json::from_str(cell).map_err(|_| error::corrupt("Sync")))
                    .transpose()
            };
            let (mut value, mut source, mut prior) = (cell(4)?, cell(5)?, cell(6)?);
            let mut emptied = false;
            for cell in [&mut value, &mut source, &mut prior].into_iter().flatten() {
                emptied |= empty::empty_held_cell(cell, fingerprints);
            }
            if emptied {
                tx.execute(
                    "UPDATE config_sync_arrival SET value = ?5, source = ?6, prior = ?7 \
                     WHERE environment_id = ?1 AND other_id = ?2 AND lineage = ?3 AND at = ?4 \
                     AND value = ?8 AND COALESCE(source, '') = COALESCE(?9, '') \
                     AND COALESCE(prior, '') = COALESCE(?10, '')",
                    &[
                        row.text(0)?.into(),
                        row.text(1)?.into(),
                        lineage.into(),
                        at.into(),
                        value.as_ref().map(json_of).as_deref().into(),
                        source.as_ref().map(json_of).as_deref().into(),
                        prior.as_ref().map(json_of).as_deref().into(),
                        row.text(4)?.into(),
                        row.optional_text(5)?.into(),
                        row.optional_text(6)?.into(),
                    ],
                )?;
            }
        }
    }
    for branch in below {
        let branch = branch.as_str();
        for row in tx.query(
            "SELECT environment_id, other_id, base FROM config_sync_base \
             WHERE environment_id = ?1 OR other_id = ?1",
            &[branch.into()],
        )? {
            let mut base = row.intent(2, "Sync base")?;
            if empty::empty_held_in(&mut base, held) {
                tx.execute(
                    "UPDATE config_sync_base SET base = ?3 \
                     WHERE environment_id = ?1 AND other_id = ?2 AND base = ?4",
                    &[
                        row.text(0)?.into(),
                        row.text(1)?.into(),
                        document(&base).as_str().into(),
                        row.text(2)?.into(),
                    ],
                )?;
            }
        }
    }
    Ok(())
}
