//! What a write that may seal a value runs afterwards, so no plain copy of the value
//! is left in the ledger's reach. The order inside each is the order the writes ran
//! in before they shared it.

use ployz_core::RpcError;
use ployz_core::config::SavedEnvironmentIntent;

use super::{history, walk};
use crate::id::EnvironmentId;
use crate::review::sealed::{SealLedger, newly_sealed, sealed_again};
use crate::storage::Tx;

/// After Working State `working` replaced `before`: record what it seals, and empty
/// what the values it newly seals left plain in the bases around it and in every
/// Branch below it.
pub(crate) fn after_working_write(
    tx: &mut dyn Tx,
    environment: &str,
    before: &SavedEnvironmentIntent,
    working: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    SealLedger::record(tx, environment, working)?;
    let sealed = newly_sealed(before, working);
    walk::empty_in_bases(tx, environment, &sealed)?;
    walk::empty_sealed_below(tx, environment, &sealed, Some(before))
}

/// After `intent` became the next Saved revision over `previous`: record what it
/// seals, clear what saving a sealed value again leaves plain below, and scrub every
/// earlier version and base.
pub(crate) fn after_saved_write(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    previous: Option<&SavedEnvironmentIntent>,
    intent: &SavedEnvironmentIntent,
) -> Result<(), RpcError> {
    SealLedger::record(tx, environment.as_str(), intent)?;
    walk::empty_sealed_below(
        tx,
        environment.as_str(),
        &sealed_again(previous, intent),
        None,
    )?;
    history::scrub_sealed(tx, environment)
}
