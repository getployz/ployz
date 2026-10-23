//! Values the browser sealed to one Environment's Sealing Key, which the Store
//! carries as ciphertext and never opens. Off for every Organization until its
//! `config_sealed_variables` row exists, and nothing here inserts one.

use ployz_core::RpcError;

use crate::OrganizationId;
use crate::storage::Tx;

/// Whether `organization` takes client-sealed values, read in the caller's transaction.
///
/// # Errors
/// Returns a storage error.
pub(crate) fn enabled(tx: &mut dyn Tx, organization: &OrganizationId) -> Result<bool, RpcError> {
    let rows = tx.query(
        "SELECT 1 FROM config_sealed_variables WHERE organization_id = ?1",
        &[organization.as_str().into()],
    )?;
    Ok(!rows.is_empty())
}
