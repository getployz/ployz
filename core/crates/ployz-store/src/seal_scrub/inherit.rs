//! Sealing, in every Branch below a receiver, the values a deploying Parent sealed
//! and the Branch still holds as it inherited them.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::RpcError;
use ployz_core::config::{Cell, SavedEnvironmentIntent, SavedVariableIntent, VariableValue};
use serde_json::Value;

use super::walk::{Took, below, held};
use ployz_core::config::{At, RowId};

use crate::Actor;
use crate::branch::{self, arrivals, base_of, marked, row, rows, share};
use crate::error;
use crate::id::EnvironmentId;
use crate::review;
use crate::scope::{self, Environment};
use crate::storage::{self, Tx};
use crate::trusted::Evidence;

/// Variables `parent` sealed and deployed stay hints in `receiver`, whose own change
/// to each wins, so Follow never carries those seals further down (#3760). Seal each
/// in every Branch below that still holds a value it was made with or took from its
/// Parent: a plain value whose fingerprint `parent` held, which it shares unchanged
/// with its own Parent. That Branch then holds it Not set, staged, as does that base,
/// so what `receiver` deploys later lands there as it would have. A Branch that
/// changed a variable keeps its value. Each Branch is one attempt, locked once for
/// every row and waiting at most `branch::follow::BRANCH_LOCK_WAIT`, so one that
/// can't take the seals keeps its values and never fails the deploy.
pub(crate) fn seal_inherited_below(
    tx: &mut dyn Tx,
    who: &Actor,
    parent: &EnvironmentId,
    receiver: &EnvironmentId,
    sealed_rows: &[RowId],
) -> Result<(), RpcError> {
    let rows: BTreeSet<(String, String)> = sealed_rows
        .iter()
        .filter_map(|row| {
            let At::Variable(key) = row.at() else {
                return None;
            };
            Some((row.lineage().to_owned(), key.clone()))
        })
        .collect();
    if rows.is_empty() {
        return Ok(());
    }
    let rows: Vec<((String, String), BTreeSet<String>)> =
        held(tx, parent.as_str(), &rows, None, Took::Sealer)?
            .into_iter()
            .collect();
    if rows.is_empty() {
        return Ok(());
    }
    let every: BTreeSet<usize> = (0..rows.len()).collect();
    // The rows each Branch fences from what's below it: one it marks Never sync never
    // takes the seal by Follow, so neither does any Branch below it.
    let mut fenced: BTreeMap<EnvironmentId, BTreeSet<usize>> = BTreeMap::new();
    for branch in below(tx, receiver.as_str())? {
        let branch = EnvironmentId::try_from(branch).map_err(|_| error::corrupt("Branch"))?;
        let sealed = storage::attempt(tx, |tx| {
            let mut fence = row(tx, &branch)?
                .and_then(|row| fenced.get(&row.parent).cloned())
                .unwrap_or_default();
            let marks = marked(tx, &branch)?;
            for (index, ((lineage, key), _)) in rows.iter().enumerate() {
                if marks.contains(&RowId::of(lineage, At::Variable(key.clone()))) {
                    fence.insert(index);
                }
            }
            fenced.insert(branch.clone(), fence.clone());
            if branch::stopped(tx, &branch)? {
                return Ok(());
            }
            // Locked only to write: a Branch holds the value until its Parent deploys.
            let mut unlocked = scope::load_by_id(tx, &branch)?;
            let mut due = Vec::new();
            for (index, row) in rows.iter().enumerate() {
                let ((lineage, key), fingerprints) = row;
                if !fence.contains(&index)
                    && inherited(tx, &mut unlocked, lineage, key, fingerprints)?.is_some()
                {
                    due.push(row);
                }
            }
            if due.is_empty() {
                return Ok(());
            }
            let mut branch = branch::lock_below(tx, who, &branch)?;
            for ((lineage, key), fingerprints) in due {
                seal_inherited(tx, &mut branch, lineage, key, fingerprints)?;
            }
            Ok(())
        });
        if let Err(error) = sealed {
            // Unread, it may mark the rows: what's below it waits as well.
            fenced.insert(branch.clone(), every.clone());
            tracing::warn!(
                branch = branch.as_str(),
                rows = ?rows.iter().map(|(row, _)| row).collect::<Vec<_>>(),
                code = error.code.as_str(),
                error = error.message.as_str(),
                "couldn't seal values a Branch inherited; it takes them at a later deploy"
            );
        }
    }
    Ok(())
}

/// The Parent of `branch` when it holds variable `key` of `lineage` plain, at one of
/// `fingerprints`, as it last shared it with that Parent and as it came from above:
/// it was made with it, or it arrived from that Parent. Compared as Sync compares it,
/// by key and fingerprint, since each side's copy of a Service has its own variable
/// IDs. A value the Branch set itself and shared up holds the same base, but came
/// from neither. None for a row it marks Never sync.
fn inherited(
    tx: &mut dyn Tx,
    branch: &mut Environment,
    lineage: &str,
    key: &str,
    fingerprints: &BTreeSet<String>,
) -> Result<Option<EnvironmentId>, RpcError> {
    let Some(parent) = row(tx, &branch.summary.id)?.map(|row| row.parent) else {
        return Ok(None);
    };
    // A row it marks Never sync takes nothing from above, as Follow leaves it.
    let at = RowId::of(lineage, At::Variable(key.to_owned()));
    if marked(tx, &branch.summary.id)?.contains(&at) {
        return Ok(None);
    }
    let Some(fingerprint) = variable(&mut branch.working, lineage, key)
        .filter(|variable| {
            !review::sealed::sealed(&variable.value)
                && fingerprints.contains(variable.value.stored_fingerprint())
        })
        .map(|variable| variable.value.stored_fingerprint().to_owned())
    else {
        return Ok(None);
    };
    let mut base = base_of(tx, (&branch.summary.id, &parent))?;
    let shared = variable(&mut base, lineage, key)
        .is_some_and(|shared| shared.value.stored_fingerprint() == fingerprint);
    if !shared {
        return Ok(None);
    }
    let made = rows::made_with(tx, &branch.summary.id)?.is_some_and(|mut made| {
        variable(&mut made, lineage, key)
            .is_some_and(|made| made.value.stored_fingerprint() == fingerprint)
    });
    let arrived = arrivals(tx, &branch.summary.id, &parent)?
        .get(&at)
        .is_some_and(|cell| plain_fingerprint(cell) == Some(fingerprint.as_str()));
    Ok((made || arrived).then_some(parent))
}

/// The fingerprint of a plain variable's cell.
pub(crate) fn plain_fingerprint(cell: &Cell) -> Option<&str> {
    let Cell::Value(Value::Object(fields)) = cell else {
        return None;
    };
    fields.get("fingerprint")?.as_str()
}

/// [`seal_inherited_below`] in one Branch, which the caller locked.
fn seal_inherited(
    tx: &mut dyn Tx,
    branch: &mut Environment,
    lineage: &str,
    key: &str,
    fingerprints: &BTreeSet<String>,
) -> Result<(), RpcError> {
    let Some(parent) = inherited(tx, branch, lineage, key, fingerprints)? else {
        return Ok(());
    };
    if let Some(variable) = variable(&mut branch.working, lineage, key) {
        seal_without_value(variable);
    }
    scope::write_working(
        tx,
        Evidence::Store,
        branch,
        scope::WorkingWrite::Trusted(scope::Reason::Inherit),
    )?;
    // After the write, which empties the value in this base by fingerprint.
    let mut base = base_of(tx, (&branch.summary.id, &parent))?;
    if let Some(variable) = variable(&mut base, lineage, key) {
        seal_without_value(variable);
    }
    share(tx, (&branch.summary.id, &parent), &base)
}

/// The variable `key` of the Service of `lineage` in `intent`.
fn variable<'intent>(
    intent: &'intent mut SavedEnvironmentIntent,
    lineage: &str,
    key: &str,
) -> Option<&'intent mut SavedVariableIntent> {
    intent
        .services
        .iter_mut()
        .find(|service| service.lineage_id == lineage)?
        .variables
        .iter_mut()
        .find(|variable| variable.key == key)
}

/// `variable` sealed without a value: Not set.
fn seal_without_value(variable: &mut SavedVariableIntent) {
    variable.value = VariableValue::NotSet;
}
