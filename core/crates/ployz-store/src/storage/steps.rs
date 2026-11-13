//! Migration steps that call into the Store's feature code.

use ployz_core::RpcError;

use crate::branch::{UNKEPT, json_of};
use crate::storage::Tx;

/// Repair, before `0037_arrival_checks` adds its CHECKs, the stored arrivals it can
/// without inventing a value, each statement touching only the rows it changes: a
/// pending row no Sync claims keeps [`UNKEPT`] for a missing `was`; a Use hint or a
/// settled row keeps no `prior` or `was`, which only a pending row is read for; and
/// a Use hint is a Follow's. A row of another `state` keeps what it holds.
pub(crate) fn repair_arrivals(tx: &mut dyn Tx) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_sync_arrival SET was = ?1 WHERE state = 'pending' \
         AND sync_id IS NULL AND proposal_id IS NULL AND was IS NULL",
        &[json_of(&UNKEPT).as_str().into()],
    )?;
    tx.execute(
        "UPDATE config_sync_arrival SET prior = NULL, was = NULL \
         WHERE state IN ('hint', 'settled') AND (prior IS NOT NULL OR was IS NOT NULL)",
        &[],
    )?;
    tx.execute(
        "UPDATE config_sync_arrival SET how = 'follow' WHERE state = 'hint' AND how <> 'follow'",
        &[],
    )?;
    Ok(())
}
