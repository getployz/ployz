//! The migrations that scrub every Environment, which re-establish the target state
//! "no plain copy of a sealed value remains" and so run the scrub as it is today.

use ployz_core::RpcError;

use super::{history, walk};
use crate::EnvironmentId;
use crate::storage::Tx;

/// Run [`walk::scrub_bases`] over every Environment.
pub(crate) fn scrub_every_base(tx: &mut dyn Tx) -> Result<(), RpcError> {
    for row in tx.query("SELECT id FROM config_environment", &[])? {
        walk::scrub_bases(tx, row.text(0)?)?;
    }
    Ok(())
}

/// Run [`history::scrub_sealed`] over every Environment.
pub(crate) fn scrub_every_history(tx: &mut dyn Tx) -> Result<(), RpcError> {
    for row in tx.query("SELECT id FROM config_environment", &[])? {
        history::scrub_sealed(tx, &row.parse::<EnvironmentId>(0, "Environment")?)?;
    }
    Ok(())
}
